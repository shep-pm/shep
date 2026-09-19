//! What the link task reads the shepherd through, and the real implementation
//! over `shep-client`.
//!
//! Traits rather than one concrete type, so [`super::link::run_link`] is
//! drivable with no socket: a bus that genuinely drops frames and a shepherd
//! that will not answer are not reachable through a real connection on
//! demand.
//!
//! Reading the bus needs `&mut` and issuing a `ListFlock` needs a shared
//! reference, and `tokio::select!` cannot hold both against one value, so
//! the two halves are separate traits.

use core::fmt;
use core::future::Future;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use shep_client::{Client, ConnectError, EventStream, Lagged, RequestError};
use shep_core::protocol::{BusEvent, ProcessInfo, Request, Response};
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

use crate::exit::ExitCode;

/// The topics lookout subscribes to.
///
/// `process.*` is what the flock table is made of; `daemon.*` carries
/// `BusEvent::Dropped` and `BusEvent::DaemonShutdown`.
///
/// Not `log.*`: the bleats feed reads log files from disk
/// ([`super::tail`]). Subscribing would make lookout the bus's
/// highest-volume subscriber, and `super::link::run_connected` answers a
/// lag with an immediate `ListFlock`, so log traffic would turn into
/// shepherd request load exactly when the shepherd is busiest.
pub const TOPICS: &[&str] = &["process.*", "daemon.*"];

/// Reading the flock. `&self`, so [`super::link::run_connected`] can hold it
/// across the same `select!` that holds an [`EventSource`] mutably.
pub trait FlockSource: Send + Sync {
    /// The flock as it stands.
    ///
    /// # Errors
    /// Whatever `Request::ListFlock` failed with.
    fn flock(&self) -> impl Future<Output = Result<Vec<ProcessInfo>, RequestError>> + Send;

    /// Sends one request over this connection and returns the shepherd's
    /// answer, whatever it is.
    ///
    /// Unlike [`Self::flock`], an unrecognised [`Response`] is not swallowed
    /// into an empty success: an action or a lamb fetch has no next poll.
    ///
    /// # Errors
    ///
    /// Whatever the underlying connection failed the request with.
    fn send(&self, request: Request)
    -> impl Future<Output = Result<Response, RequestError>> + Send;
}

/// One source of bus frames.
pub trait EventSource: Send {
    /// The next frame; `Err(`[`Lagged`]`)` when this client's own receiver fell
    /// behind and discarded frames; `None` when the subscription ends, which
    /// is how a dead connection announces itself.
    fn next_event(&mut self) -> impl Future<Output = Option<Result<BusEvent, Lagged>>> + Send;
}

/// Opens a connection and hands back both halves of it together.
///
/// One factory rather than two parameters: a reconnect rebuilds the request
/// path and the subscription at the same moment.
pub trait Shepherd: Send {
    /// This connection's request half.
    type Flock: FlockSource;
    /// This connection's subscription half.
    type Events: EventSource;

    /// Connects and subscribes.
    ///
    /// # Errors
    /// [`LinkError::Unreachable`] when the socket would not answer or the
    /// handshake failed, [`LinkError::Refused`] when it answered and then
    /// refused the subscription.
    fn link(
        &mut self,
    ) -> impl Future<Output = Result<(Self::Flock, Self::Events), LinkError>> + Send;
}

/// Why opening a connection failed.
///
/// No `#[non_exhaustive]`: shep-cli is `[[bin]]`-only and every match on
/// this type is in this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// Nothing answered at the socket, or the handshake did not complete.
    Unreachable(String),
    /// The shepherd answered and speaks a different wire version.
    ///
    /// Its own exit code, as in `main.rs`'s `connect_client`: reporting a
    /// skew as "the shepherd did not answer" would send the operator to
    /// check a daemon that is running.
    Protocol(String),
    /// The shepherd answered but refused the subscription.
    Refused(String),
}

impl LinkError {
    /// The exit code for a failure on the first dial, before the dashboard
    /// exists. A later failure is a rung on [`super::link::run_link`]'s
    /// ladder, never an exit. Derived from `ExitCode::from(&ConnectError)`
    /// at conversion time, so verbs cannot drift apart.
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Protocol(_) => ExitCode::ProtocolMismatch,
            Self::Unreachable(_) | Self::Refused(_) => ExitCode::DaemonUnreachable,
        }
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(why) => write!(f, "the shepherd did not answer: {why}"),
            Self::Protocol(why) => write!(f, "{why}"),
            Self::Refused(why) => write!(f, "the shepherd refused the subscription: {why}"),
        }
    }
}

impl core::error::Error for LinkError {}

impl From<ConnectError> for LinkError {
    fn from(err: ConnectError) -> Self {
        if ExitCode::from(&err) == ExitCode::ProtocolMismatch {
            return Self::Protocol(err.to_string());
        }
        Self::Unreachable(err.to_string())
    }
}

/// The request half of a live connection.
#[derive(Debug)]
pub struct ClientFlock(Client);

impl ClientFlock {
    /// The wrapped connection, for [`super::lookout`]'s version guard.
    ///
    /// The guard cannot live inside [`UnixShepherd::link`]: that Future is
    /// `+ Send`, and [`super::link::run_link`] holds the `Shepherd` for the
    /// whole reconnect ladder as `'static`. A borrowed `Streams` crosses
    /// neither bound.
    pub(crate) fn client(&self) -> &Client {
        &self.0
    }
}

impl FlockSource for ClientFlock {
    async fn flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        match self.0.request(Request::ListFlock).await? {
            Response::Flock(flock) => Ok(flock),
            // `Response` is `#[non_exhaustive]`; the next poll asks again.
            _unrecognised => Ok(Vec::new()),
        }
    }

    async fn send(&self, request: Request) -> Result<Response, RequestError> {
        // The client's own default deadline, the same one `commands::lifecycle`
        // passes for stop, restart and reload.
        self.0.request(request).await
    }
}

impl EventSource for EventStream {
    async fn next_event(&mut self) -> Option<Result<BusEvent, Lagged>> {
        self.next().await
    }
}

/// The real thing: a socket path that can be dialled again.
#[derive(Debug)]
pub struct UnixShepherd {
    socket: PathBuf,
}

impl UnixShepherd {
    /// Watches the shepherd listening at `socket`.
    #[must_use]
    pub fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
        }
    }
}

impl Shepherd for UnixShepherd {
    type Flock = ClientFlock;
    type Events = EventStream;

    async fn link(&mut self) -> Result<(Self::Flock, Self::Events), LinkError> {
        // `Client::connect`, never `connect_or_spawn`: opening a dashboard, or
        // reconnecting one, must not start a shepherd, and a reconnect that
        // did would resurrect one the operator may have just killed.
        let client = Client::connect(&self.socket).await?;
        let topics = TOPICS.iter().map(|topic| (*topic).to_string()).collect();
        let stream = client
            .subscribe(topics)
            .await
            .map_err(|err| LinkError::Refused(err.to_string()))?;
        Ok((ClientFlock(client), stream))
    }
}

/// One reading of the machine this lookout is running on.
///
/// Not `dog::metrics::HostReading`: that one walks the process table for a
/// process count and carries no load average. This is read every second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostSample {
    /// One-, five- and fifteen-minute load averages.
    pub load: (f64, f64, f64),
    /// How many cores that load is spread across, from
    /// `std::thread::available_parallelism`. `None` when the platform would
    /// not say; the strip drops the segment rather than guessing 1.
    pub cores: Option<usize>,
    /// Total physical memory in bytes.
    pub memory_total_bytes: u64,
    /// Memory in use, as the platform reports it.
    pub memory_used_bytes: u64,
    /// Seconds since the host booted.
    pub uptime_seconds: u64,
}

/// Everything lookout reads that does not come off the socket.
///
/// `&mut self`: the tail reader remembers each file's length at the previous
/// read, and `super::run_ui` owns this outright.
pub trait Local {
    /// This machine's load, memory and uptime, or `None` on a platform
    /// `sysinfo` does not support.
    fn host(&mut self) -> Option<HostSample>;

    /// The tail of one sheep's two log files. See [`super::tail`].
    fn tail(&mut self, out: Option<&Path>, err: Option<&Path>) -> super::tail::Tail;

    /// How many bytes one sheep's two log files hold together.
    ///
    /// `None` unless both paths are reported and both files can be sized: half
    /// a total would read as a smaller log rather than as a partial answer. A
    /// log rotated away between the poll and the read is one of the cases that
    /// lands here.
    fn log_sizes(&mut self, out: Option<&Path>, err: Option<&Path>) -> Option<u64>;
}

/// The real one: a `sysinfo` handle and the tail reader's memory of each
/// file's length.
#[derive(Debug)]
pub struct LocalReader {
    cores: Option<usize>,
    /// Held across ticks rather than built per sample: constructing a
    /// `System` sets up the platform state a refresh then updates in place,
    /// and [`Local::host`] runs every second for the life of the session.
    /// Memory only, which is what makes the refresh as narrow as the
    /// construction.
    system: System,
    seen: std::collections::BTreeMap<PathBuf, u64>,
}

impl LocalReader {
    /// Reads the core count once, builds the one `sysinfo` handle every
    /// sample refreshes, and starts with no memory of any log file.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cores: std::thread::available_parallelism()
                .ok()
                .map(NonZeroUsize::get),
            // Memory only. `.with_processes(..)` is what makes `dog::metrics`'
            // own sampler a process-table walk.
            system: System::new_with_specifics(
                RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
            ),
            seen: std::collections::BTreeMap::new(),
        }
    }
}

/// Same as [`LocalReader::new`].
///
/// `clippy::new_without_default` denies an argument-less `new` with no
/// `Default`.
impl Default for LocalReader {
    fn default() -> Self {
        Self::new()
    }
}

impl Local for LocalReader {
    fn host(&mut self) -> Option<HostSample> {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return None;
        }
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::everything());
        let load = System::load_average();
        Some(HostSample {
            load: (load.one, load.five, load.fifteen),
            cores: self.cores,
            memory_total_bytes: self.system.total_memory(),
            memory_used_bytes: self.system.used_memory(),
            uptime_seconds: System::uptime(),
        })
    }

    fn tail(&mut self, out: Option<&Path>, err: Option<&Path>) -> super::tail::Tail {
        super::tail::read(&mut self.seen, out, err)
    }

    fn log_sizes(&mut self, out: Option<&Path>, err: Option<&Path>) -> Option<u64> {
        let out = std::fs::metadata(out?).ok()?.len();
        let err = std::fs::metadata(err?).ok()?.len();
        Some(out.saturating_add(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2 ms: a memory-only sample runs ~8 µs on this machine and a
    /// process-table walk ~8 ms. Timed rather than asserted on the
    /// `RefreshKind`, which is what a regression would change.
    #[test]
    fn a_host_sample_is_cheap_enough_for_a_one_second_heartbeat() {
        let mut local = LocalReader::new();
        // One warm sample first: the heartbeat runs in the steady state, and
        // a platform's own first read is not what it costs per second.
        let _ = local.host();

        let started = std::time::Instant::now();
        for _ in 0..10 {
            let _ = local.host();
        }
        let each = started.elapsed() / 10;
        // Windows' `sysinfo` backend is heavier: 2.8ms there against 2ms
        // on unix. 15ms still fails loudly on a sample that would make
        // the dashboard stutter.
        #[cfg(unix)]
        let budget = std::time::Duration::from_millis(2);
        #[cfg(windows)]
        let budget = std::time::Duration::from_millis(15);
        assert!(
            each < budget,
            "one host sample took {each:?} (budget {budget:?}); \
             the heartbeat fires every second"
        );
    }

    /// Branched on `IS_SUPPORTED_SYSTEM` rather than asserted:
    /// `clippy::assertions_on_constants` denies both forms.
    #[test]
    fn an_unsupported_platform_reports_nothing_rather_than_zero() {
        let mut local = LocalReader::new();
        if sysinfo::IS_SUPPORTED_SYSTEM {
            let sample = local.host().expect("a supported platform samples");
            assert!(sample.memory_total_bytes > 0, "a supported host has memory");
            assert!(sample.memory_used_bytes < sample.memory_total_bytes);
            assert!(sample.cores.is_some_and(|cores| cores >= 1));
        } else {
            assert!(
                local.host().is_none(),
                "no numbers where there is nothing to read"
            );
        }
    }

    /// `sysinfo`'s own cpu count can disagree with std's under an affinity
    /// mask or a cgroup quota; the load average is spread across std's.
    #[test]
    fn the_core_count_comes_from_std_and_not_from_sysinfo() {
        let mut local = LocalReader::new();
        assert_eq!(
            local.host().and_then(|sample| sample.cores),
            std::thread::available_parallelism()
                .ok()
                .map(NonZeroUsize::get),
        );
    }

    #[tokio::test]
    async fn a_version_skewed_shepherd_is_refused_through_client_flock() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let ack = shep_core::protocol::HelloAck {
            daemon_version: "0.1.8".to_string(),
            protocol: shep_core::protocol::PROTOCOL_VERSION,
            pid: 4242,
            min_supported: None,
        };
        let (client, _fake) = shep_client::testing::fake_client_with_ack(&addr, ack).await;
        let flock = ClientFlock(client);

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = crate::output::Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: crate::cli::Format::Table,
        };
        let code = crate::version_guard::refuse_version_skew(
            &mut streams,
            flock.client(),
            crate::version_guard::VersionGuard::Enforce,
        )
        .expect_err("a differing crate version must be refused");
        assert_eq!(code, crate::exit::ExitCode::VersionSkew);
    }

    #[tokio::test]
    async fn a_matching_version_proceeds_through_client_flock() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let ack = shep_core::protocol::HelloAck {
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: shep_core::protocol::PROTOCOL_VERSION,
            pid: 4242,
            min_supported: None,
        };
        let (client, _fake) = shep_client::testing::fake_client_with_ack(&addr, ack).await;
        let flock = ClientFlock(client);

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = crate::output::Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: crate::cli::Format::Table,
        };
        crate::version_guard::refuse_version_skew(
            &mut streams,
            flock.client(),
            crate::version_guard::VersionGuard::Enforce,
        )
        .expect("a matching version is not a skew");
    }

    /// The two files together, since the detail pane's cell says "on disk"
    /// about the pair.
    #[test]
    fn two_readable_logs_size_to_their_sum() {
        let dir = tempfile::Builder::new()
            .prefix("shep-logsize-")
            .tempdir()
            .expect("a tempdir for the logs");
        let out = dir.path().join("web-out.log");
        let err = dir.path().join("web-err.log");
        std::fs::write(&out, b"listening on :8080\n").expect("write the out log");
        std::fs::write(&err, b"warn\n").expect("write the err log");

        let mut local = LocalReader::new();
        assert_eq!(local.log_sizes(Some(&out), Some(&err)), Some(19 + 5));
    }

    /// A log rotated away between two polls leaves exactly one of the two
    /// reads failing. Half a total would read as a smaller log rather than as
    /// a partial answer, so the answer is no size at all. This is the rule
    /// `view::detail::log_row` used to carry when it read the filesystem
    /// itself.
    #[test]
    fn a_missing_log_sizes_to_nothing_rather_than_to_one_file() {
        let dir = tempfile::Builder::new()
            .prefix("shep-logsize-")
            .tempdir()
            .expect("a tempdir for the logs");
        let out = dir.path().join("only-out.log");
        std::fs::write(&out, b"listening on :8080\n").expect("write the out log");
        let rotated_away = dir.path().join("rotated-away-err.log");

        let mut local = LocalReader::new();
        assert_eq!(local.log_sizes(Some(&out), Some(&rotated_away)), None);
        assert_eq!(
            local.log_sizes(Some(&rotated_away), Some(&out)),
            None,
            "either side missing is the same answer"
        );
    }

    /// A shepherd predating the `out_file`/`err_file` fields reports neither.
    #[test]
    fn an_unreported_log_path_sizes_to_nothing() {
        let mut local = LocalReader::new();
        assert_eq!(local.log_sizes(None, None), None);
    }
}
