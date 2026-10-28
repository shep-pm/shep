//! The metrics dog: [`MetricsConfig`], [`run`], and the data types
//! [`exposition::render`] turns into Prometheus text and the `otel`
//! feature's exporter turns into an OTLP payload.
//!
//! [`Reading`] is one snapshot, and it is built two ways. The flock and host
//! series are polled fresh on every scrape, from `Request::ListFlock` and the
//! handshake. The channel samples in [`Reading::samples`] are kept between
//! scrapes by [`samples::SampleStore`], which [`feed::run`] fills off the bus
//! and which drops a sender's samples when that sender stops or restarts.
//! [`collect_reading`] is the poll, and both renderers call it.

pub mod exposition;

mod config;
mod feed;
mod grammar;
pub mod samples;

// The whole of the `otel` feature. A build without it compiles none of
// this, and refuses a config that asks for export instead.
#[cfg(feature = "otel")]
mod otlp;

pub use config::MetricsConfig;

use std::sync::Arc;
use std::time::Duration;

use shep_client::ReconnectingClient;
use shep_client::RequestError;
use shep_client::dogs::Stop;
use shep_core::protocol::ProcessInfo;
use sysinfo::{MemoryRefreshKind, ProcessRefreshKind, RefreshKind, System};
use tokio::net::{TcpListener, TcpStream};

use super::DogRuntime;
use crate::exit::ExitCode;
use crate::http::{self, HttpError};
use samples::{SampleFamily, SampleStore};

/// How long [`http::read_request`] waits for a connected peer to finish
/// sending its request. Generous for a scraper on the same host, small
/// enough that a peer that connects and says nothing cannot hold a task
/// open indefinitely.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// What a `[dog.metrics]` section that asks for OTLP export gets on a build
/// compiled without the `otel` feature. Naming the feature is the point: a
/// dog that exports nothing and says nothing looks healthy.
#[cfg(not(feature = "otel"))]
const OTLP_FEATURE_REFUSAL: &str = concat!(
    "`otlp_endpoint` is set, but this build has no `otel` feature and exports nothing; ",
    "install it with `cargo install shep --features otel`",
);

/// What the shepherd and the host looked like when the reading was taken,
/// plus the channel samples the dog has kept since their senders went away.
#[derive(Debug, Default)]
pub struct Reading {
    /// Every registered entry, sheep and dogs alike, as `ListFlock`
    /// answered.
    pub flock: Vec<ProcessInfo>,
    /// The shepherd's crate version, from the handshake rather than from a
    /// request: `HelloAck` already answered it.
    pub daemon_version: String,
    /// The shepherd's pid, from the same handshake.
    pub daemon_pid: u32,
    /// Host totals, `None` where the sampler could not read them.
    pub host: Option<HostReading>,
    /// The samples sheep and dogs sent on their shepherd channels, grouped
    /// one family per metric name. Rendered after the built-in groups.
    pub samples: Vec<SampleFamily>,
}

/// The machine the flock is running on, read through `sysinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReading {
    /// Total physical memory in bytes.
    pub memory_total_bytes: u64,
    /// Memory in use, as the platform reports it.
    pub memory_used_bytes: u64,
    /// How many processes the host is running, the flock included.
    pub processes: usize,
    /// Seconds since the host booted.
    pub uptime_seconds: u64,
}

/// Reads the host's memory, process count and uptime through one
/// short-lived `sysinfo::System` per call.
///
/// `None` on a target `sysinfo` does not support, which is expected
/// rather than an error a scrape should fail over.
///
/// `pub(crate)`: `whistle::read` also calls this for its own host sample.
pub(crate) fn sample_host() -> Option<HostReading> {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    let system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            // `nothing()` alone still leaves `tasks` on: sysinfo's Linux
            // backend then counts every thread as a process.
            .with_processes(ProcessRefreshKind::nothing().without_tasks()),
    );
    Some(HostReading {
        memory_total_bytes: system.total_memory(),
        memory_used_bytes: system.used_memory(),
        processes: system.processes().len(),
        uptime_seconds: System::uptime(),
    })
}

/// [`sample_host`] on the blocking pool, awaited.
///
/// The walk is one long blocking sweep of the process table. Run inline
/// inside an `async fn` it stalls every other task on that tokio worker,
/// so both async callers come here instead. A panic inside the pool reads
/// as `None`.
pub(crate) async fn sample_host_off_worker() -> Option<HostReading> {
    tokio::task::spawn_blocking(sample_host)
        .await
        .unwrap_or_default()
}

/// Asks the shepherd for the flock and pairs that answer with the
/// handshake's own version and pid, plus a host sample and the samples the
/// dog has kept.
///
/// The one reading both paths take: [`handle_connection`] renders it as
/// Prometheus text and the OTLP exporter encodes it as a POST body.
/// `store` is snapshotted against the listing this call just took.
///
/// # Errors
/// - [`RequestError`], as [`ReconnectingClient::list_flock`] gives it: the
///   shepherd did not answer the listing.
pub(crate) async fn collect_reading(
    client: &ReconnectingClient,
    store: &SampleStore,
) -> Result<Reading, RequestError> {
    let flock = client.list_flock().await?;
    let samples = store.snapshot(&flock);
    // Read the daemon from the generation that just answered `ListFlock`.
    // `ReconnectingClient::daemon` reports the daemon answering now, so two
    // reads either side of a handover could publish one daemon's version
    // beside another's pid.
    let ack = client.daemon();
    Ok(Reading {
        flock,
        daemon_version: ack.daemon_version,
        daemon_pid: ack.pid,
        samples,
        // The host sample is the one blocking step between two awaits here,
        // so it goes to the blocking pool rather than stalling this worker
        // for the length of the process-table walk.
        host: sample_host_off_worker().await,
    })
}

/// Runs the metrics dog until it is signalled.
///
/// Binds [`MetricsConfig::bind`] and serves until `SIGINT` or `SIGTERM`, or
/// until its shepherd has been gone for a whole
/// [`SHEPHERD_RETURN_BUDGET`](super::SHEPHERD_RETURN_BUDGET). A refused bind
/// or a refused OTLP endpoint is fatal, the latter taking `InvalidConfig`.
pub async fn run(runtime: DogRuntime) -> ExitCode {
    let config = match runtime.config::<MetricsConfig>() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("shep dog metrics: {err}; see `shep dogs`");
            return ExitCode::InvalidConfig;
        }
    };
    // The OTLP keys are read once, here, like `bind`. A section that asks
    // for export on a build that cannot do it is refused rather than run.
    #[cfg(not(feature = "otel"))]
    if config.otlp_endpoint.is_some() {
        eprintln!("shep dog metrics: {OTLP_FEATURE_REFUSAL}");
        return ExitCode::InvalidConfig;
    }
    #[cfg(feature = "otel")]
    let exporter = match otlp::exporter_for(&config) {
        Ok(exporter) => exporter,
        Err(err) => {
            eprintln!("shep dog metrics: {err}");
            return ExitCode::InvalidConfig;
        }
    };
    let listener = match TcpListener::bind(config.bind).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("shep dog metrics: could not bind {}: {err}", config.bind);
            return ExitCode::Failure;
        }
    };
    let mut stop = Stop::on_stop_signals();
    let client = Arc::new(runtime.into_client());
    // One store for both paths that read it: the scrape server renders its
    // snapshot and the exporter encodes the same one.
    let store = Arc::new(samples::SampleStore::new());
    // The exporter runs beside the scrape server on its own task, sharing
    // its client, and is never awaited: a failing export is a log line in
    // this dog's own log, never a reason for it to exit.
    #[cfg(feature = "otel")]
    if let Some(exporter) = exporter {
        let _exporter_task = otlp::spawn(exporter, Arc::clone(&client), Arc::clone(&store));
    }
    tokio::select! {
        () = stop.wait() => ExitCode::Success,
        () = accept_forever(listener, Arc::clone(&client), Arc::clone(&store)) => ExitCode::Success,
        // The feed is what keeps the samples, so it is also what notices the
        // shepherd is gone: a handover ends its subscription and it re-arms
        // one, and only a shepherd that never answers again leaves this arm
        // waiting past the budget. A handover never reaches here.
        lost = client.link_lost(super::SHEPHERD_RETURN_BUDGET) => {
            eprintln!("shep dog metrics: {lost}");
            ExitCode::from(&lost)
        }
        // A shepherd that refused this dog's subscription. The feed parks
        // instead of returning for any other failure, so this arm never
        // competes with the `link_lost` one above.
        code = feed::run(Arc::clone(&client), Arc::clone(&store)) => code,
    }
}

/// Accepts connections off `listener` forever, one task per connection.
/// Never returns: [`run`] races it against a stop request.
async fn accept_forever(
    listener: TcpListener,
    client: Arc<ReconnectingClient>,
    store: Arc<SampleStore>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let client = Arc::clone(&client);
                let store = Arc::clone(&store);
                tokio::spawn(handle_connection(stream, client, store));
            }
            Err(err) => {
                eprintln!("shep dog metrics: accept failed: {err}");
            }
        }
    }
}

/// Serves exactly one request on `stream`: `/metrics` answers the
/// exposition, everything else answers 404 naming `/metrics`.
async fn handle_connection(
    mut stream: TcpStream,
    client: Arc<ReconnectingClient>,
    store: Arc<SampleStore>,
) {
    let request = match http::read_request(&mut stream, READ_TIMEOUT).await {
        Ok(request) => request,
        // A peer that never finished a request gets no reply at all:
        // there is no well-formed request to answer.
        Err(_err) => return,
    };
    let path = request
        .target
        .split_once('?')
        .map_or(request.target.as_str(), |(path, _query)| path);
    if path != "/metrics" {
        let _: Result<(), HttpError> = http::write_response(
            &mut stream,
            404,
            "text/plain",
            b"not found; metrics are served at /metrics\n",
        )
        .await;
        return;
    }

    let reading = match collect_reading(&client, &store).await {
        Ok(reading) => reading,
        // A failed `ListFlock` answers 503, not a 200 with nothing in it:
        // a scraper reads that as a real empty flock, where a 503 is
        // `up == 0` for this target.
        Err(_) => {
            let _: Result<(), HttpError> = http::write_response(
                &mut stream,
                503,
                "text/plain",
                b"the shepherd did not answer\n",
            )
            .await;
            return;
        }
    };
    let body = exposition::render(&reading);
    let _: Result<(), HttpError> = http::write_response(
        &mut stream,
        200,
        "text/plain; version=0.0.4",
        body.as_bytes(),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use shep_client::testing::{
        Handshake, fake_daemon_across_handovers, fake_reconnecting_client_on, sample_ack,
    };
    use shep_core::protocol::{HelloAck, PROTOCOL_VERSION, ProcessInfo};
    use shep_core::status::ProcStatus;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::task::JoinHandle;

    use super::*;

    /// A minimal, online sheep fixture, enough for the exposition to name
    /// it in a `sheep="..."` label.
    pub(super) fn sample_info(name: &str) -> ProcessInfo {
        ProcessInfo::builder(1, name, ProcStatus::Online)
            .pid(Some(4242))
            .uptime_ms(1_000)
            .cpu_percent(Some(0.5))
            .memory_bytes(Some(1024))
            .build()
    }

    /// Tests that wait out a real [`crate::dog::SHEPHERD_RETURN_BUDGET`]. Five seconds
    /// of elapsed time is the point, so a paused clock would test nothing.
    mod slow {
        use super::*;

        /// An unused loopback port, found by binding one and letting it go.
        ///
        /// Port `0` is what every other test here uses, but this one has to
        /// name the dog's port in a config section before the dog exists,
        /// so it cannot read the port back off a listener the dog owns.
        async fn free_port() -> u16 {
            let probe = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            probe.local_addr().unwrap().port()
        }

        /// fails if a metrics dog whose shepherd is gone waits for it
        /// forever. This dog touches its client only when something scrapes
        /// it, so an unscraped one sits reconnecting against an address
        /// nobody answers.
        ///
        /// Also fails if it exits the moment the connection drops: a reload
        /// drops it on purpose and this dog crosses it.
        #[tokio::test]
        async fn a_metrics_dog_whose_shepherd_is_gone_waits_its_budget_and_then_exits() {
            let dir = tempfile::tempdir().unwrap();
            let socket = shep_client::testing::control_address(dir.path());
            let port = free_port().await;
            let shepherds =
                fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
            shepherds.reply_to_dog_config(&format!("bind = \"127.0.0.1:{port}\"\n"));
            shepherds.reply_to_list(vec![sample_info("web")]);
            let paths = crate::dog::tests::test_paths(dir.path(), socket);

            let dog = tokio::spawn(crate::dog::run_dog("metrics", paths));

            // Up, bound and answering from the shepherd's own flock. A test
            // that dropped the shepherd before this would be watching a dog
            // that never started, which exits for its own reasons and would
            // satisfy every assertion below.
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let bound = tokio::time::timeout(Duration::from_secs(10), async {
                // `scrape` unwraps its connect, so the wait for the bind is
                // its own loop rather than a scrape that retries.
                while TcpStream::connect(addr).await.is_err() {
                    assert!(!dog.is_finished(), "the dog exited before it bound");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            assert!(bound.is_ok(), "the dog never bound its port");

            let served = scrape(addr, "/metrics").await;
            assert!(
                served.starts_with("HTTP/1.1 200 "),
                "the dog must be answering from the shepherd's own flock: {served}"
            );
            assert!(served.contains("web"), "{served}");

            // Gone for good, listener and all, which is what a stopped
            // shepherd leaves behind. A handover leaves the listener bound.
            drop(shepherds);
            let went = tokio::time::Instant::now();

            let code = tokio::time::timeout(crate::dog::SHEPHERD_RETURN_BUDGET * 3, dog)
                .await
                .expect("a dog whose shepherd is gone must exit, not linger")
                .unwrap();
            let waited = went.elapsed();

            assert_eq!(
                code,
                ExitCode::DaemonUnreachable,
                "exiting 0 would read as a dog that finished its work"
            );
            let budget = crate::dog::SHEPHERD_RETURN_BUDGET;
            assert!(
                waited >= budget,
                "gave up after {waited:?}, inside the {budget:?} a handover \
                 is allowed to take, which is the restart-per-reload this rule exists to avoid"
            );
        }
    }

    /// A running metrics dog bound to an OS-assigned loopback port, backed
    /// by `client`. Aborts its serving task on drop, so a dog left
    /// listening past its own test cannot hold a port for the rest of the
    /// binary's run.
    pub(super) struct RunningDog {
        addr: SocketAddr,
        handle: JoinHandle<()>,
    }

    impl RunningDog {
        pub(super) fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    impl Drop for RunningDog {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    /// Binds `config.bind` (port `0` in every test below, never a fixed
    /// one), reads back the OS-assigned address, and serves `client` on it
    /// in the background.
    pub(super) async fn serve_on_free_port(
        client: ReconnectingClient,
        config: MetricsConfig,
    ) -> RunningDog {
        let listener = TcpListener::bind(config.bind).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(accept_forever(
            listener,
            Arc::new(client),
            Arc::new(samples::SampleStore::new()),
        ));
        RunningDog { addr, handle }
    }

    /// Connects to `addr`, sends a bare `GET <path>` and returns the raw
    /// response text: status line, headers and body together. Every
    /// read/write is wrapped in a generous timeout, so a hang here is a bug
    /// in the dog under test, not a reason to hang the suite.
    pub(super) async fn scrape(addr: SocketAddr, path: &str) -> String {
        let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .expect("connect must not hang")
            .unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        tokio::time::timeout(Duration::from_secs(5), stream.write_all(request.as_bytes()))
            .await
            .expect("write must not hang")
            .unwrap();
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
            .await
            .expect("read must not hang")
            .unwrap();
        String::from_utf8(buf).expect("the exposition is ASCII/UTF-8")
    }

    /// The fake shepherd answers a different flock to the second
    /// `ListFlock`, so a dog that polled once at startup serves the first
    /// one twice.
    #[tokio::test]
    async fn every_scrape_asks_the_shepherd_again() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        daemon.reply_to_list_sequence(vec![
            vec![sample_info("web")],
            vec![sample_info("web"), sample_info("api")],
        ]);
        let dog = serve_on_free_port(client, MetricsConfig::default_on_port(0)).await;

        let first = scrape(dog.addr(), "/metrics").await;
        assert!(first.contains(r#"sheep="web""#), "{first}");
        assert!(!first.contains(r#"sheep="api""#), "{first}");

        let second = scrape(dog.addr(), "/metrics").await;
        assert!(
            second.contains(r#"sheep="api""#),
            "the second scrape must see the second listing: {second}"
        );
        assert_eq!(daemon.list_flock_count(), 2, "one ListFlock per scrape");
    }

    /// `cut_on_next_request` makes the shepherd read exactly one envelope
    /// and drop the connection without a reply: accepted, then never
    /// answered. This is also the shape a daemon handover produces, so it
    /// pins that the request in flight still fails; only the connection
    /// is re-established.
    #[tokio::test]
    async fn a_shepherd_that_will_not_answer_produces_a_503() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let shepherds =
            fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        let client = ReconnectingClient::connect(&socket).await.unwrap();
        let dog = serve_on_free_port(client, MetricsConfig::default_on_port(0)).await;
        shepherds.cut_on_next_request();

        let response = scrape(dog.addr(), "/metrics").await;
        assert!(response.starts_with("HTTP/1.1 503 "), "{response}");
    }

    /// A dog's process crosses the shepherd's `execve` for free, but only
    /// the listening socket crosses with it, so the accepted connection
    /// this dog holds on the old shepherd dies. A pid check cannot see
    /// that; the assertion here is a scrape instead.
    #[tokio::test]
    async fn a_scrape_after_a_handover_serves_the_successors_flock() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let successor = HelloAck {
            daemon_version: "0.2.0".into(),
            protocol: PROTOCOL_VERSION,
            pid: 5150,
            min_supported: None,
        };
        let shepherds = fake_daemon_across_handovers(
            &socket,
            vec![
                Handshake::Accept(sample_ack()),
                Handshake::Accept(successor),
            ],
        );
        shepherds.reply_to_list(vec![sample_info("web")]);
        let client = ReconnectingClient::connect(&socket).await.unwrap();
        let dog = serve_on_free_port(client, MetricsConfig::default_on_port(0)).await;

        let before = scrape(dog.addr(), "/metrics").await;
        assert!(before.starts_with("HTTP/1.1 200 "), "{before}");
        assert!(
            before.contains(r#"shep_daemon_up{version="9.9.9"}"#),
            "{before}"
        );

        shepherds.cut().await;

        // Scraped until the successor answers: a scrape landing inside the
        // reconnect window legitimately 503s, and the contract is that the
        // dog recovers, not that it never misses a beat.
        let recovered = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let body = scrape(dog.addr(), "/metrics").await;
                if body.starts_with("HTTP/1.1 200 ") {
                    return body;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the dog must answer 200 again after its shepherd is replaced");

        assert!(
            recovered.contains(r#"sheep="web""#),
            "the exposition must carry the successor's flock: {recovered}"
        );
        assert!(
            recovered.contains(r#"shep_daemon_up{version="0.2.0"}"#),
            "the exposition must name the daemon now running, not the one              that was replaced: {recovered}"
        );
    }

    #[tokio::test]
    async fn only_the_metrics_path_serves_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
        let dog = serve_on_free_port(client, MetricsConfig::default_on_port(0)).await;

        let root = scrape(dog.addr(), "/").await;
        assert!(root.starts_with("HTTP/1.1 404 "), "{root}");
        assert!(root.contains("/metrics"), "{root}");

        let metrics = scrape(dog.addr(), "/metrics").await;
        assert!(metrics.starts_with("HTTP/1.1 200 "), "{metrics}");
    }

    /// The host sample is the one blocking step on the scrape path and it
    /// crosses a `spawn_blocking` boundary to get into the response: a
    /// scrape must still carry it. Without this, a plumbing mistake — an
    /// unawaited handle, an error arm that drops the sample — would leave
    /// every other assertion above green and the host series silently
    /// gone.
    #[tokio::test]
    async fn a_scrape_carries_the_host_sample() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_reconnecting_client_on(&socket).await;
        daemon.reply_to_list_sequence(vec![vec![sample_info("web")]]);
        let dog = serve_on_free_port(client, MetricsConfig::default_on_port(0)).await;

        let body = scrape(dog.addr(), "/metrics").await;
        assert!(
            body.contains("shep_host_memory_total_bytes"),
            "the host sample must survive the trip to the blocking pool: {body}"
        );
        assert!(
            body.contains("shep_host_uptime_seconds"),
            "uptime is part of the same sample: {body}"
        );
        assert_eq!(
            daemon.list_flock_count(),
            1,
            "one ListFlock for the one scrape"
        );
    }
}

/// The half of this dog that only exists when the `otel` feature is off.
///
/// Its own module, with its own imports, so a build with the feature on
/// compiles none of it and reports no unused import.
#[cfg(all(test, not(feature = "otel")))]
mod otlp_off_tests {
    use std::time::Duration;

    use shep_client::testing::{sample_ack, serve_one_request};
    use shep_core::protocol::{Request, Response};

    use crate::exit::ExitCode;

    /// A `[dog.metrics]` section that asks for OTLP export must refuse on a
    /// build that cannot do it. Fails if the dog starts anyway, exporting
    /// nothing while `shep list` reports it online, which is the failure
    /// mode this refusal exists to avoid.
    #[tokio::test]
    async fn an_endpoint_without_the_feature_refuses_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let response = Response::DogSection {
            toml: "otlp_endpoint = \"http://127.0.0.1:4318\"\n"
                .to_string()
                .into(),
        };
        let handle = serve_one_request(&socket, sample_ack(), response).await;
        let paths = crate::dog::tests::test_paths(dir.path(), socket);
        let dog = tokio::spawn(crate::dog::run_dog("metrics", paths));

        // The dog reached the socket and asked for its section, which is
        // what makes the wait below one for a refusal rather than a hope.
        let envelope = tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("the dog must reach the wire")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfigResolved {
                name: "metrics".to_string()
            }
        );

        let code = tokio::time::timeout(Duration::from_secs(10), dog)
            .await
            .expect("a refused config is an exit, not a hang")
            .unwrap();
        assert_eq!(code, ExitCode::InvalidConfig);
    }
}
