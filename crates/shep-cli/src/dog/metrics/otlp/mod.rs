//! OTLP/HTTP export: `[dog.metrics] otlp_*`, the interval loop that pushes
//! the dog's series to a collector, and the refusals a bad config gets.
//!
//! The transport is [`crate::fetch`]'s, the same hand-rolled HTTP/1.1 over
//! `tokio-rustls` bark delivers webhooks with, so export adds no
//! dependency. The body is [`encode()`]'s OTLP/JSON, read by [`reply::rejection`].
//!
//! One export per interval, no retry and no queue: a failed export is
//! reported once and the next tick sends current values anyway. Nothing here
//! can take the scrape server or the dog's exit code down with it.

pub mod encode;
mod reply;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use shep_client::ReconnectingClient;
use shep_client::RequestError;
use tokio::task::JoinHandle;

use super::MetricsConfig;
use crate::fetch;

pub use encode::{ExportContext, encode};
use reply::rejection;

/// The path every OTLP/HTTP collector serves metrics at.
const METRICS_PATH: &str = "/v1/metrics";

/// Why a `[dog.metrics]` section cannot run an OTLP exporter.
///
/// Not `#[non_exhaustive]`: every module here is private to this binary, so
/// no out-of-tree match exists to break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OtlpConfigError {
    /// `otlp_endpoint` is not an `http://` or `https://` URL this build can
    /// connect to. Carries the reason, which never quotes a URL holding an
    /// `@`: see [`crate::fetch::url_for_message`].
    Url(String),
    /// `otlp_interval` is `0`. [`tokio::time::interval`] refuses a zero
    /// period outright.
    IntervalZero,
    /// `otlp_timeout` is `0`: every export would be over before it
    /// connected.
    TimeoutZero,
    /// An `otlp_headers` key is empty, or carries a control byte that would
    /// end the request line early. Carries the key.
    BadHeaderKey(String),
    /// An `otlp_headers` value carries a control byte. Carries the key, and
    /// never the value: the value is the credential.
    BadHeaderValue(String),
}

impl fmt::Display for OtlpConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(reason) => write!(f, "otlp_endpoint is not usable: {reason}"),
            Self::IntervalZero => f.write_str("otlp_interval must be more than 0"),
            Self::TimeoutZero => f.write_str("otlp_timeout must be more than 0"),
            Self::BadHeaderKey(key) => {
                write!(
                    f,
                    "otlp_headers key {key:?} is empty or carries a control byte"
                )
            }
            Self::BadHeaderValue(key) => {
                write!(
                    f,
                    "otlp_headers value for key {key:?} carries a control byte"
                )
            }
        }
    }
}

impl core::error::Error for OtlpConfigError {}

/// Why one export did not land.
// No `Clone`/`PartialEq`: [`fetch::FetchError`] carries a `std::io::Error`,
// which is neither.
#[derive(Debug)]
enum ExportError {
    /// The shepherd did not answer `ListFlock`, so there was no reading to
    /// send.
    Collect(RequestError),
    /// The collector refused the POST, or the connect, the TLS handshake or
    /// the deadline failed it.
    Send(fetch::FetchError),
    /// The collector answered 2xx and said in its body that it took less than
    /// it was sent, or answered something this could not read. Carries the
    /// reason: the collector's own text, sanitised and cut. A 2xx here is not
    /// a clean export, and points are missing.
    Partial(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collect(source) => write!(f, "no reading to export: {source}"),
            Self::Send(source) => write!(f, "{source}"),
            Self::Partial(reason) => {
                write!(f, "the collector took less than it was sent: {reason}")
            }
        }
    }
}

impl core::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Collect(source) => Some(source),
            Self::Send(source) => Some(source),
            // The collector's own words, not an error this one came from.
            Self::Partial(_reason) => None,
        }
    }
}

/// One configured collector, and what the interval loop needs to reach it.
pub struct Exporter {
    target: fetch::Target,
    interval: Duration,
    timeout: Duration,
    headers: Vec<(String, String)>,
    /// When this dog started, in nanoseconds since the Unix epoch: the
    /// `startTimeUnixNano` the built-in sums and the dog's own refusal
    /// counter carry. A channel counter carries its sender's start instead.
    started_unix_nano: u64,
    host_name: Option<String>,
}

/// Manual: a derived `Debug` would print `headers`, and an OTLP collector's
/// auth header is a bearer token (IR-41). `target`'s own `Debug` already
/// withholds its path.
impl fmt::Debug for Exporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Exporter {{ target: {:?}, interval: {:?}, timeout: {:?}, headers: <redacted>, \
             started_unix_nano: {}, host_name: {:?} }}",
            self.target, self.interval, self.timeout, self.started_unix_nano, self.host_name,
        )
    }
}

/// The exporter `[dog.metrics]` describes, or `None` when it names no
/// endpoint, which is the default and means no export at all.
///
/// Read once at startup, like [`MetricsConfig::bind`]: the endpoint, the
/// cadence and the headers are what the dog was started with.
///
/// # Errors
/// - [`OtlpConfigError::Url`]: `otlp_endpoint` is not a fetchable URL.
/// - [`OtlpConfigError::IntervalZero`] / [`OtlpConfigError::TimeoutZero`]: a
///   zero duration where one does not work.
/// - [`OtlpConfigError::BadHeaderKey`] / [`OtlpConfigError::BadHeaderValue`]:
///   a header name or value that could split the request.
pub fn exporter_for(config: &MetricsConfig) -> Result<Option<Exporter>, OtlpConfigError> {
    let Some(endpoint) = &config.otlp_endpoint else {
        return Ok(None);
    };
    let url = metrics_url(endpoint);
    let target = match fetch::parse_url(&url) {
        Ok(target) => target,
        Err(fetch::FetchError::Url(reason)) => return Err(OtlpConfigError::Url(reason)),
        Err(source) => return Err(OtlpConfigError::Url(source.to_string())),
    };
    let interval = config.otlp_interval.as_duration();
    if interval.is_zero() {
        return Err(OtlpConfigError::IntervalZero);
    }
    let timeout = config.otlp_timeout.as_duration();
    if timeout.is_zero() {
        return Err(OtlpConfigError::TimeoutZero);
    }
    let mut headers = Vec::with_capacity(config.otlp_headers.len());
    for (key, value) in &config.otlp_headers {
        if key.is_empty() || has_control_byte(key) {
            return Err(OtlpConfigError::BadHeaderKey(key.clone()));
        }
        if has_control_byte(value) {
            // Refused naming the key: the value is the thing a control byte
            // is being refused for, and it is a credential.
            return Err(OtlpConfigError::BadHeaderValue(key.clone()));
        }
        headers.push((key.clone(), value.clone()));
    }
    Ok(Some(Exporter {
        target,
        interval,
        timeout,
        headers,
        started_unix_nano: unix_nanos(),
        host_name: sysinfo::System::host_name(),
    }))
}

/// Whether `value` holds a character that would end the request line, or a
/// header line, early.
fn has_control_byte(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// The full URL to POST to: `endpoint`, minus any trailing slash, plus
/// [`METRICS_PATH`].
///
/// The query is split off before the slash is trimmed. Trimming the whole
/// string leaves a slash that sits just before the `?`, and a collector that
/// does not normalise its path answers 404 for `//v1/metrics` while the
/// operator's endpoint was valid.
fn metrics_url(endpoint: &str) -> String {
    let (base, query) = match endpoint.split_once('?') {
        Some((head, tail)) => (head.trim_end_matches('/'), Some(tail)),
        None => (endpoint.trim_end_matches('/'), None),
    };
    match query {
        // A query belongs after the appended segment. Appending to the whole
        // string would put `/v1/metrics` inside the query's value.
        Some(query) => format!("{base}{METRICS_PATH}?{query}"),
        None => format!("{base}{METRICS_PATH}"),
    }
}

/// Nanoseconds since the Unix epoch, saturating rather than wrapping on a
/// clock set before 1970.
fn unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Runs `exporter` on its own task. `run` never awaits this handle: an
/// exporter that stops exporting is a log line, not a reason for the dog to
/// exit.
pub fn spawn(
    exporter: Exporter,
    client: Arc<ReconnectingClient>,
    store: Arc<super::samples::SampleStore>,
) -> JoinHandle<()> {
    tokio::spawn(export_loop(exporter, client, store))
}

/// One export per `interval`, until the process ends.
///
/// A fresh [`tokio::time::interval`] ticks immediately, and consuming that
/// first tick puts the first export one whole interval after the dog starts.
///
/// `Delay` rather than the default `Burst`: queueing missed ticks would send
/// stale readings back to back.
async fn export_loop(
    exporter: Exporter,
    client: Arc<ReconnectingClient>,
    store: Arc<super::samples::SampleStore>,
) {
    let mut ticker = tokio::time::interval(exporter.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    // `true` to start, so the first failure is a transition and logs.
    let mut healthy = true;
    loop {
        ticker.tick().await;
        match exporter.export(&client, &store).await {
            Ok(()) => {
                if !healthy {
                    eprintln!("shep dog metrics: otlp export recovered");
                    healthy = true;
                }
            }
            Err(err) => {
                if healthy {
                    eprintln!("shep dog metrics: otlp export failed: {err}");
                    healthy = false;
                }
            }
        }
    }
}

impl Exporter {
    /// Collects one reading and pushes it.
    ///
    /// A 2xx whose body names no `partialSuccess` is a success. A 2xx that
    /// names one is a failed export: the collector took some of the points
    /// and said so, and a tick that reported that as clean would lose the
    /// dropped points with nothing in the log.
    async fn export(
        &self,
        client: &ReconnectingClient,
        store: &super::samples::SampleStore,
    ) -> Result<(), ExportError> {
        let reading = super::collect_reading(client, store)
            .await
            .map_err(ExportError::Collect)?;
        let context = ExportContext {
            now_unix_nano: unix_nanos(),
            exporter_start_unix_nano: self.started_unix_nano,
            host_name: self.host_name.as_deref(),
        };
        let body = encode(&reading, &context);
        let reply = fetch::post_json(&self.target, &self.headers, body.as_bytes(), self.timeout)
            .await
            .map_err(ExportError::Send)?;
        match rejection(&reply.body) {
            Some(reason) => Err(ExportError::Partial(reason)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use shep_client::testing::fake_reconnecting_client_on;

    use crate::dog::metrics::accept_forever;
    use crate::dog::metrics::samples::SampleStore;

    use super::super::{Reading, tests as metrics_tests};
    use super::*;
    use crate::host_sample::HostReading;

    /// A listener that answers every request it accepts with `responses`,
    /// cycling, and records each request's full bytes. Aborted on drop, so a
    /// collector cannot hold its port for the rest of the run.
    struct Collector {
        addr: SocketAddr,
        seen: Arc<Mutex<Vec<String>>>,
        handle: JoinHandle<()>,
    }

    impl Collector {
        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }

        /// Waits until `want` requests have arrived, looking every 20ms. It
        /// returns the moment the count is reached, so it can carry more than
        /// `want`: a caller that asserts an exact count needs an export
        /// cadence wider than this poll, or two ticks can land between two
        /// looks. The bound is the test's own, so a collector that never
        /// answers fails here rather than hanging.
        async fn wait_for(&self, want: usize) -> Vec<String> {
            let budget = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                let seen = self.seen();
                if seen.len() >= want {
                    return seen;
                }
                assert!(
                    tokio::time::Instant::now() < budget,
                    "the collector answered {} of {want} requests",
                    seen.len()
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    impl Drop for Collector {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    /// A listener answering `responses` verbatim, one per request, cycling.
    async fn collector_answering(responses: Vec<String>) -> Collector {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let handle = tokio::spawn(async move {
            let mut answered = 0;
            loop {
                let (stream, _peer) = listener.accept().await.unwrap();
                let mut reader = tokio::io::BufReader::new(stream);
                let mut request = String::new();
                let mut content_length = None;
                loop {
                    let mut line = String::new();
                    let read = reader.read_line(&mut line).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    if let Some((_name, value)) = line.split_once(':')
                        && line.to_ascii_lowercase().starts_with("content-length")
                    {
                        content_length = value.trim().parse().ok();
                    }
                    request.push_str(&line);
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                if let Some(length) = content_length {
                    let mut body = vec![0u8; length];
                    if reader.read_exact(&mut body).await.is_ok() {
                        request.push_str(&String::from_utf8_lossy(&body));
                    }
                }
                recorder.lock().unwrap().push(request);
                let response = &responses[answered % responses.len()];
                answered += 1;
                let _ = reader.write_all(response.as_bytes()).await;
                let _ = reader.shutdown().await;
            }
        });
        Collector { addr, seen, handle }
    }

    /// [`collector_answering`] with `statuses` and no body, which is what a
    /// collector that took every point answers.
    async fn collector(statuses: Vec<u16>) -> Collector {
        collector_answering(
            statuses
                .iter()
                .map(|status| {
                    format!(
                        "HTTP/1.1 {status} ok\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                })
                .collect(),
        )
        .await
    }

    /// An [`Exporter`] pointed at `addr` on a `cadence` interval, with the
    /// headers a real collector needs.
    fn exporter_on(addr: SocketAddr, cadence: Duration) -> Exporter {
        Exporter {
            target: fetch::parse_url(&format!("http://{}:{}/v1/metrics", addr.ip(), addr.port()))
                .unwrap(),
            interval: cadence,
            timeout: Duration::from_secs(5),
            headers: vec![("Authorization".to_owned(), "Bearer s3cr3t".to_owned())],
            started_unix_nano: 1_699_999_900_000_000_000,
            host_name: Some("sheepbox".to_owned()),
        }
    }

    /// A [`Reading`] an exporter can be pointed at: no flock, one host
    /// sample, so a test's assertions are about the transport.
    fn reading() -> Reading {
        Reading {
            flock: vec![],
            daemon_version: "9.9.9".to_string(),
            daemon_pid: 12345,
            samples: vec![],
            host: Some(HostReading {
                memory_total_bytes: 16_000_000_000,
                memory_used_bytes: 8_000_000_000,
                processes: 200,
                uptime_seconds: 3600,
            }),
        }
    }

    /// Real time, not a paused clock: these tests drive real sockets, and a
    /// paused clock would advance past the point a request is mid-flight.
    #[tokio::test]
    async fn a_post_carries_the_headers_the_path_and_the_json_content_type() {
        let collector = collector(vec![200]).await;
        let exporter = exporter_on(collector.addr, Duration::from_secs(60));
        let body = serde_json::to_string(&serde_json::json!({"resourceMetrics": []})).unwrap();
        fetch::post_json(
            &exporter.target,
            &exporter.headers,
            body.as_bytes(),
            Duration::from_secs(5),
        )
        .await
        .expect("a collector that answers 200 is a success");

        let request = collector.wait_for(1).await.remove(0);
        assert!(
            request.starts_with("POST /v1/metrics HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.contains("\r\nAuthorization: Bearer s3cr3t\r\n"),
            "{request}"
        );
        assert!(
            request.contains("\r\nContent-Type: application/json\r\n"),
            "{request}"
        );
        assert!(request.contains("\r\nConnection: close\r\n"), "{request}");
        assert!(
            request.contains(&format!("\r\nContent-Length: {}\r\n", body.len())),
            "{request}"
        );
        let (_head, sent_body) = request
            .split_once("\r\n\r\n")
            .expect("a blank line ends the head");
        assert_eq!(sent_body, body);
    }

    /// A 500 is a failed export, and a failed export is a log line: the
    /// loop keeps ticking, and the next tick sends current values.
    ///
    /// The 250ms cadence is against `wait_for`'s 20ms look, not a taste: at
    /// 20ms a third tick can land before the poll has seen the second, and
    /// the count is wrong on a runner with a coarse clock.
    #[tokio::test]
    async fn a_5xx_is_a_failed_export_and_the_next_tick_still_runs() {
        let collector = collector(vec![500]).await;
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
        let exporter = exporter_on(collector.addr, Duration::from_millis(250));
        let store = SampleStore::new();

        let err = exporter
            .export(&client, &store)
            .await
            .expect_err("a collector answering 500 is a failed export");
        assert!(
            matches!(err, ExportError::Send(fetch::FetchError::Status(500))),
            "{err:?}"
        );

        let handle = tokio::spawn(export_loop(
            exporter,
            Arc::new(client),
            Arc::new(SampleStore::new()),
        ));
        let seen = collector.wait_for(3).await;
        assert_eq!(seen.len(), 3, "the tick after a failed export must run");
        assert!(!handle.is_finished(), "a failing exporter must not end");
        handle.abort();
    }

    /// A 200 whose body names a `partialSuccess` is a failed export. The
    /// collector took some of the points and said which: a tick that called
    /// that clean would lose the dropped points with nothing in the log, and
    /// the next tick would do the same.
    #[tokio::test]
    async fn a_2xx_naming_a_partial_success_is_a_failed_export() {
        let body = r#"{"partialSuccess":{"rejectedDataPoints":"1","errorMessage":"bad point"}}"#;
        let collector = collector_answering(vec![format!(
            "HTTP/1.1 200 ok\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
        let exporter = exporter_on(collector.addr, Duration::from_secs(60));

        let err = exporter
            .export(&client, &SampleStore::new())
            .await
            .expect_err("a 2xx naming dropped points is not a clean export");
        assert!(
            matches!(&err, ExportError::Partial(reason) if reason == "1 data point rejected: bad point"),
            "{err:?}"
        );
    }

    /// A 204 with no `Content-Length` at all is how a collector answers a
    /// payload it took whole, and it is the shape [`crate::fetch`]'s GET
    /// reader refuses. Fails if it started reading as a failed export.
    #[tokio::test]
    async fn a_204_with_no_content_length_is_a_clean_export() {
        let collector = collector_answering(vec![
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string(),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
        let exporter = exporter_on(collector.addr, Duration::from_secs(60));

        exporter
            .export(&client, &SampleStore::new())
            .await
            .expect("a 204 that named nothing took everything");
        assert_eq!(collector.wait_for(1).await.len(), 1);
    }

    /// A collector that took every point and echoes `partialSuccess` as an
    /// empty object is a clean export. Fails if that answer started reading
    /// as a failure, which is also the answer that never lets the loop log a
    /// recovery: it would sit unhealthy from the first tick.
    #[tokio::test]
    async fn a_200_echoing_an_empty_partial_success_is_a_clean_export() {
        let body = r#"{"partialSuccess":{}}"#;
        let collector = collector_answering(vec![format!(
            "HTTP/1.1 200 ok\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
        let exporter = exporter_on(collector.addr, Duration::from_secs(60));

        exporter
            .export(&client, &SampleStore::new())
            .await
            .expect("an empty `partialSuccess` names nothing the collector dropped");
        assert_eq!(collector.wait_for(1).await.len(), 1);
    }

    /// The exporter shares its client with the scrape server and runs on a
    /// task beside it. A collector nobody listens on must cost the scrape
    /// nothing.
    #[tokio::test]
    async fn an_unreachable_endpoint_does_not_stop_the_scrape_server() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
        let client = Arc::new(client);

        // The scrape server this test must keep answering, built here
        // rather than through `serve_on_free_port`, which takes the client
        // by value and would leave the exporter without one.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let served = listener.local_addr().unwrap();
        let server = tokio::spawn(accept_forever(
            listener,
            Arc::clone(&client),
            Arc::new(SampleStore::new()),
        ));

        // A port that was bound and let go: a connect to it is refused.
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = probe.local_addr().unwrap();
        drop(probe);

        let handle = tokio::spawn(export_loop(
            exporter_on(dead, Duration::from_millis(100)),
            Arc::clone(&client),
            Arc::new(SampleStore::new()),
        ));

        // `shep_daemon_up` needs no flock and no host sample, so this says
        // the scrape still reached the shepherd without depending on which
        // of the two paths asked it first.
        let response = metrics_tests::scrape(served, "/metrics").await;
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        assert!(
            response.contains(r#"shep_daemon_up{version="9.9.9"} 1"#),
            "{response}"
        );
        assert!(!handle.is_finished(), "the exporter must still be trying");
        assert!(
            !server.is_finished(),
            "the scrape server must still be accepting"
        );
        handle.abort();
        server.abort();
    }

    /// A paused clock, because the thing under test is a delay and a real
    /// one would spend a minute of wall clock to prove it.
    #[tokio::test(start_paused = true)]
    async fn the_first_export_lands_one_interval_after_start() {
        let collector = collector(vec![200]).await;
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
        let handle = tokio::spawn(export_loop(
            exporter_on(collector.addr, Duration::from_secs(60)),
            Arc::new(client),
            Arc::new(SampleStore::new()),
        ));

        // One second in, nothing has been sent: the immediate first tick of
        // a fresh interval is consumed without exporting.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            collector.seen().is_empty(),
            "an export landed inside the first interval"
        );

        // Steady 1s steps, so the paused clock walks to the 60s deadline
        // rather than the test waiting on it in one await.
        let mut landed = false;
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if !collector.seen().is_empty() {
                landed = true;
                break;
            }
        }
        assert!(landed, "no export landed inside two intervals");
        handle.abort();
    }

    /// `tokio::time::interval` panics on a zero period. Refusing it as a bad
    /// config is what keeps an operator's typo a startup message rather than
    /// a dog panic.
    #[test]
    fn a_zero_interval_is_refused_rather_than_panicking_the_loop() {
        let config = MetricsConfig {
            otlp_endpoint: Some("http://127.0.0.1:4318".to_owned()),
            otlp_interval: shep_core::values::UpDuration::from_millis(0),
            ..MetricsConfig::default()
        };
        assert_eq!(
            exporter_for(&config).unwrap_err(),
            OtlpConfigError::IntervalZero
        );

        let config = MetricsConfig {
            otlp_endpoint: Some("http://127.0.0.1:4318".to_owned()),
            otlp_timeout: shep_core::values::UpDuration::from_millis(0),
            ..MetricsConfig::default()
        };
        assert_eq!(
            exporter_for(&config).unwrap_err(),
            OtlpConfigError::TimeoutZero
        );
    }

    /// Both halves matter: the refusal, and that naming the key did not name
    /// the value it is refusing.
    #[test]
    fn a_header_with_a_newline_is_refused_naming_the_key_not_the_value() {
        let config = MetricsConfig {
            otlp_endpoint: Some("http://127.0.0.1:4318".to_owned()),
            otlp_headers: BTreeMap::from([(
                "Authorization".to_owned(),
                "Bearer s3cr3t\r\nX-Injected: yes".to_owned(),
            )]),
            ..MetricsConfig::default()
        };
        let err = exporter_for(&config).unwrap_err();
        assert_eq!(
            err,
            OtlpConfigError::BadHeaderValue("Authorization".to_owned())
        );
        let shown = format!("{err} {err:?}");
        assert!(shown.contains("Authorization"), "{shown}");
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert!(!shown.contains("X-Injected"), "{shown}");

        let config = MetricsConfig {
            otlp_headers: BTreeMap::from([("Bad\nKey".to_owned(), "value".to_owned())]),
            ..config
        };
        let err = exporter_for(&config).unwrap_err();
        assert_eq!(err, OtlpConfigError::BadHeaderKey("Bad\nKey".to_owned()));
        assert!(
            !format!("{err}").contains("value"),
            "a refused key should not carry the value either: {err}"
        );
    }

    #[test]
    fn an_endpoint_that_is_not_a_url_is_refused() {
        let config = MetricsConfig {
            otlp_endpoint: Some("collector.internal:4318".to_owned()),
            ..MetricsConfig::default()
        };
        let err = exporter_for(&config).unwrap_err();
        let OtlpConfigError::Url(reason) = &err else {
            panic!("wrong variant: {err:?}");
        };
        assert!(
            reason.contains("does not start with http:// or https://"),
            "{reason}"
        );
    }

    /// A credentials-bearing endpoint is refused by `fetch::parse_url`
    /// without echoing it, which is the same rule bark's sinks follow.
    #[test]
    fn an_endpoint_carrying_credentials_is_refused_without_echoing_them() {
        let config = MetricsConfig {
            otlp_endpoint: Some("https://user:hunter2@collector.internal:4318".to_owned()),
            ..MetricsConfig::default()
        };
        let err = exporter_for(&config).unwrap_err();
        assert!(matches!(err, OtlpConfigError::Url(_)), "{err:?}");
        assert!(!format!("{err} {err:?}").contains("hunter2"), "{err}");
    }

    #[test]
    fn the_endpoint_gets_the_metrics_path_appended() {
        for (endpoint, path) in [
            ("http://127.0.0.1:4318", "/v1/metrics"),
            ("http://127.0.0.1:4318/", "/v1/metrics"),
            ("http://127.0.0.1:4318///", "/v1/metrics"),
            ("https://otlp.internal/collect", "/collect/v1/metrics"),
            (
                // A query stays a query: appending to the whole string would
                // bury `/v1/metrics` inside the parameter's value.
                "https://otlp.internal?token=abc",
                "/v1/metrics?token=abc",
            ),
            (
                // The same with the slash the operator put before the query:
                // trimming after splitting is what keeps one `/` in front of
                // the appended path.
                "http://127.0.0.1:4318/?token=abc",
                "/v1/metrics?token=abc",
            ),
            ("http://127.0.0.1:4318/collect/", "/collect/v1/metrics"),
        ] {
            let config = MetricsConfig {
                otlp_endpoint: Some(endpoint.to_owned()),
                ..MetricsConfig::default()
            };
            let exporter = exporter_for(&config)
                .unwrap_or_else(|err| panic!("{endpoint}: {err}"))
                .expect("an endpoint is an exporter");
            assert_eq!(exporter.target.path, path, "{endpoint}");
        }
    }

    /// The exact string, with the header's value named in the test as the
    /// thing that must not appear in it.
    #[test]
    fn an_exporters_debug_redacts_its_headers() {
        let exporter = exporter_on(
            "127.0.0.1:4318".parse::<SocketAddr>().unwrap(),
            Duration::from_secs(60),
        );
        let rendered = format!("{exporter:?}");
        assert_eq!(
            rendered,
            r#"Exporter { target: Target { https: false, host: "127.0.0.1", port: 4318, path: <redacted> }, interval: 60s, timeout: 5s, headers: <redacted>, started_unix_nano: 1699999900000000000, host_name: Some("sheepbox") }"#
        );
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(!rendered.contains("Authorization"), "{rendered}");
    }

    /// The reading an export sends is the one the scrape server would send,
    /// and `encode` is a pure function of it, so a body built from a fixed
    /// reading is a fixed string.
    #[tokio::test]
    async fn an_export_sends_the_body_the_encoder_made() {
        let collector = collector(vec![200]).await;
        let exporter = exporter_on(collector.addr, Duration::from_secs(60));
        let expected = encode(
            &reading(),
            &ExportContext {
                now_unix_nano: exporter.started_unix_nano,
                exporter_start_unix_nano: exporter.started_unix_nano,
                host_name: exporter.host_name.as_deref(),
            },
        );
        let body = expected.as_bytes();
        fetch::post_json(
            &exporter.target,
            &exporter.headers,
            body,
            Duration::from_secs(5),
        )
        .await
        .expect("a collector that answers 200 is a success");
        let request = collector.wait_for(1).await.remove(0);
        let (_head, sent_body) = request
            .split_once("\r\n\r\n")
            .expect("a blank line ends the head");
        assert_eq!(sent_body, expected);
    }
}
