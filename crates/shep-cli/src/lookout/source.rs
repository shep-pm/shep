//! What the link task reads the shepherd through, and the real implementation
//! over `shep-client`.
//!
//! Traits rather than one concrete type, so [`super::link::run_link`] is
//! drivable with no socket: a bus that genuinely drops frames and a shepherd
//! that will not answer are not reachable through a real connection on demand.
//!
//! Reading the bus needs `&mut` and issuing a `ListFlock` needs a shared
//! reference, which `tokio::select!` cannot hold against one value, so the
//! two halves are separate traits.

use core::fmt;
use core::future::Future;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use shep_client::{Client, ConnectError, EventStream, Lagged, RequestError, SshOptions};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{BusEvent, ProcessInfo, Request, Response};
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

use crate::commands::rpc::deadline_for;
use crate::dog_settings::{ask_dog, dog_write};
use crate::exit::ExitCode;

/// The topics lookout subscribes to.
///
/// `process.*` is what the flock table is made of; `daemon.*` carries
/// `BusEvent::Dropped` and `BusEvent::DaemonShutdown`.
///
/// Not `log.*`: the bleats feed reads log files from disk ([`super::tail`]),
/// and a lag on that bus already costs an immediate `ListFlock`.
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
pub struct ClientFlock {
    client: Client,
    /// This machine's `$SHEP_HOME`, or `None` for a shepherd on another one.
    ///
    /// The manifest cache under that home is what says which dogs want to be
    /// asked before a write to their settings lands, and this binary holds
    /// manifests only for the dogs it has adopted here. A dog on a watched
    /// host is not this machine's to ask, and its own `shep set` on that host
    /// is where its checks run.
    home: Option<ShepPaths>,
}

impl ClientFlock {
    /// The wrapped connection, for [`super::lookout`]'s version guard.
    ///
    /// The guard cannot live inside [`UnixShepherd::link`]: that Future is
    /// `+ Send`, and [`super::link::run_link`] holds the `Shepherd` for the
    /// whole reconnect ladder as `'static`. A borrowed `Streams` crosses
    /// neither bound.
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }
}

impl FlockSource for ClientFlock {
    async fn flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        match self.client.list_flock().await {
            // `Response` is `#[non_exhaustive]`; the next poll asks again.
            Err(RequestError::UnexpectedReply { .. }) => Ok(Vec::new()),
            answer => answer,
        }
    }

    /// Sends `request`, asking the dog first when the request writes a dog's
    /// settings and this machine has a manifest saying it wants to be asked.
    ///
    /// A refusal comes back as the same [`RequestError::Rpc`] the shepherd
    /// would have answered a refused write with, so the pane reports a dog's
    /// "no" and a shepherd's alike, and nothing is written either way.
    async fn send(&self, request: Request) -> Result<Response, RequestError> {
        if let Some(write) = dog_write(&request)
            && let Some(paths) = &self.home
        {
            ask_dog(&self.client, paths, write.dog, &write.table, write.sheep)
                .await
                .map_err(|failure| failure.request_error())?;
        }
        let deadline = deadline_for(&request);
        self.client.request_with_deadline(request, deadline).await
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
    home: ShepPaths,
}

impl UnixShepherd {
    /// Watches the shepherd listening at `socket`, with `paths` as this
    /// machine's `$SHEP_HOME`.
    #[must_use]
    pub fn new(socket: &Path, paths: &ShepPaths) -> Self {
        Self {
            socket: socket.to_path_buf(),
            home: paths.clone(),
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
        subscribed(client, Some(self.home.clone())).await
    }
}

/// A shepherd on another machine, dialled through ssh.
///
/// The first [`Shepherd::link`] runs `options` as given, so `lookout` can make
/// it before raw mode and ssh may prompt, as a single `--host` verb does. Every
/// later one is a redial on [`super::link::run_link`]'s ladder, inside the
/// alternate screen, where a prompt would land unseen: those run batch mode.
#[derive(Debug)]
pub struct HostShepherd {
    dest: String,
    options: SshOptions,
    dialled: bool,
}

impl HostShepherd {
    /// Watches the shepherd on `dest`, dialled through `options`.
    #[must_use]
    pub fn new(dest: &str, options: SshOptions) -> Self {
        Self {
            dest: dest.to_owned(),
            options,
            dialled: false,
        }
    }
}

impl Shepherd for HostShepherd {
    type Flock = ClientFlock;
    type Events = EventStream;

    async fn link(&mut self) -> Result<(Self::Flock, Self::Events), LinkError> {
        // Set before the dial, so a first dial that fails still makes the next
        // one a redial.
        let options = if core::mem::replace(&mut self.dialled, true) {
            self.options.clone().batch_mode(true)
        } else {
            self.options.clone()
        };
        let client = Client::connect_ssh_with(&self.dest, &options).await?;
        subscribed(client, None).await
    }
}

/// Subscribes `client` to [`TOPICS`] and pairs the two halves.
async fn subscribed(
    client: Client,
    home: Option<ShepPaths>,
) -> Result<(ClientFlock, EventStream), LinkError> {
    let topics = TOPICS.iter().map(|topic| (*topic).to_string()).collect();
    let stream = client
        .subscribe(topics)
        .await
        .map_err(|err| LinkError::Refused(err.to_string()))?;
    Ok((ClientFlock { client, home }, stream))
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

/// What a lookout over `--host` reads in place of this machine: nothing.
///
/// A backstop. The host's rows name log files on the host, and reading those
/// paths here would show this machine's files under the host's name. The UI
/// says so in words before it gets this far, since an empty tail reads as a
/// sheep that wrote nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLocal;

impl Local for NoLocal {
    fn host(&mut self) -> Option<HostSample> {
        None
    }

    fn tail(&mut self, _out: Option<&Path>, _err: Option<&Path>) -> super::tail::Tail {
        super::tail::Tail::default()
    }

    fn log_sizes(&mut self, _out: Option<&Path>, _err: Option<&Path>) -> Option<u64> {
        None
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
        let flock = ClientFlock { client, home: None };

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams =
            crate::output::Streams::bare(&mut out, &mut err, crate::cli::Format::Table);
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
        let flock = ClientFlock { client, home: None };

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams =
            crate::output::Streams::bare(&mut out, &mut err, crate::cli::Format::Table);
        crate::version_guard::refuse_version_skew(
            &mut streams,
            flock.client(),
            crate::version_guard::VersionGuard::Enforce,
        )
        .expect("a matching version is not a skew");
    }

    /// Fails if lookout's stop goes out on the client's 5s default, which a
    /// kill ladder plus a lamb sweep can outlast.
    #[tokio::test]
    async fn a_stop_sent_through_client_flock_asks_for_the_stop_budget() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let (client, _fake) = shep_client::testing::fake_client_needing_budget(
            &addr,
            std::time::Duration::from_secs(7),
            Response::Stopped(Vec::new()),
        )
        .await;
        let flock = ClientFlock { client, home: None };

        let answer = flock
            .send(Request::Stop {
                selector: shep_core::protocol::SelectorSpec::All,
            })
            .await;

        assert_eq!(answer, Ok(Response::Stopped(Vec::new())));
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
    /// a partial answer, so the answer is no size at all.
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

    /// Stand-in `ssh` for [`HostShepherd`], unix only. It appends its argv to
    /// `argv`, one line per dial, then answers like a shepherd: it reads a
    /// frame and replies, twice over (the handshake, then the subscribe as
    /// request 1), from the canned `hello` and `subscribed` files.
    #[cfg(unix)]
    mod stand_in {
        use std::os::unix::fs::PermissionsExt as _;
        use std::path::{Path, PathBuf};

        use shep_core::protocol::{
            HelloAck, HelloReply, PROTOCOL_VERSION, Reply, Response, ServerFrame, encode_frame,
        };

        /// `payload` behind the four-byte length every frame carries.
        fn framed(payload: &impl serde::Serialize) -> Vec<u8> {
            let payload = encode_frame(payload).unwrap();
            let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
            bytes.extend_from_slice(&payload);
            bytes
        }

        pub(super) fn write(dir: &Path) -> PathBuf {
            let ack: HelloReply = Ok(HelloAck {
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol: PROTOCOL_VERSION,
                pid: 4242,
                min_supported: None,
            });
            let subscribed = ServerFrame::Reply(Reply {
                id: 1,
                result: Ok(Response::Subscribed),
            });
            std::fs::write(dir.join("hello"), framed(&ack)).unwrap();
            std::fs::write(dir.join("subscribed"), framed(&subscribed)).unwrap();
            let program = dir.join("ssh");
            std::fs::write(
                &program,
                r#"#!/bin/sh
dir=$(dirname "$0")
echo "$@" >> "$dir/argv"
skip() {
  n=0
  for b in $(dd bs=1 count=4 2>/dev/null | od -An -v -tu1); do n=$((n * 256 + b)); done
  [ "$n" -eq 0 ] || dd bs=1 count="$n" of=/dev/null 2>/dev/null
}
skip; cat "$dir/hello"
skip; cat "$dir/subscribed"
cat >/dev/null
"#,
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            program
        }

        /// One line per dial, as the stand-in was called.
        pub(super) fn dials(dir: &Path) -> Vec<String> {
            std::fs::read_to_string(dir.join("argv"))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_shepherd_dials_a_stand_in_and_subscribes() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in::write(dir.path());
        let mut shepherd = HostShepherd::new("build-01", SshOptions::default().program(program));

        let (flock, _events) = shepherd
            .link()
            .await
            .expect("the stand-in answers the handshake and the subscribe");

        assert_eq!(flock.client().host(), Some("build-01"));
        assert_eq!(
            stand_in::dials(dir.path()),
            ["-T -- build-01 shep dial-stdio"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_second_dial_is_batch_mode_and_the_first_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in::write(dir.path());
        let mut shepherd = HostShepherd::new("build-01", SshOptions::default().program(program));

        let _first = shepherd.link().await.expect("the first dial");
        let _second = shepherd.link().await.expect("the redial");

        assert_eq!(
            stand_in::dials(dir.path()),
            [
                "-T -- build-01 shep dial-stdio",
                "-T -o BatchMode=yes -- build-01 shep dial-stdio",
            ]
        );
    }

    #[tokio::test]
    async fn a_host_that_cannot_be_dialled_is_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("no-such-ssh");
        let mut shepherd = HostShepherd::new("build-01", SshOptions::default().program(program));

        let err = shepherd.link().await.expect_err("there is no ssh to run");

        assert!(matches!(err, LinkError::Unreachable(_)), "{err:?}");
    }

    #[test]
    fn no_local_reads_nothing_of_this_machine() {
        let log = Path::new(file!());
        let mut local = NoLocal;
        assert_eq!(local.host(), None);
        assert_eq!(
            local.tail(Some(log), Some(log)),
            crate::lookout::tail::Tail::default()
        );
        assert_eq!(local.log_sizes(Some(log), Some(log)), None);
    }
}

/// The gate every dashboard write to a dog's settings passes through, against
/// a shepherd that answers the ask.
#[cfg(test)]
mod validate_tests;
