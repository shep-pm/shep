// One crate-root fixture module: every test module in this crate shares these
// helpers instead of hand-rolling its own. The scripted doubles live in
// `doubles`, re-exported below so every caller keeps writing
// `crate::testing::Name`. The loopback HTTP fake lives in `crate::testing_http`.
mod doubles;

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use shep_core::config::{AppConfig, ResolvedApp, normalize};
use shep_core::paths::ShepPaths;
use shep_core::secrets::SecretView;
use shep_core::status::ProcStatus;
use tokio::sync::{broadcast, mpsc, watch};

pub(crate) use doubles::{
    ArmCall, RecordingEnforcer, RecordingPidSignals, ScriptedProber, ScriptedSampler, TestClock,
    probe_config, rss, rss_cpu,
};

use crate::assemble::assemble;
use crate::bus::SharedEvent;
use crate::cron::DEFAULT_MAX_CRON_SLEEP;
use crate::entry::{ProcessEntry, ReloadState, RestartBudget};
use crate::extras::{Extras, ExtrasReports, LivenessReport};
use crate::fake::{FIRST_SCRIPTED_PID, ProcScript, ScriptedRunner, idle_sweep};
use crate::limits::LimitBreach;
use crate::limits::sample::{MemorySampler, ProcessIdentity, ProcessRss};
use crate::limits::stats::StatsState;
use crate::privilege::SpawnIdentity;
use crate::rpc::RpcContext;
use crate::runner::{Preflight, ProcIo, ProcessRunner, RunnerError, SpawnSpec};
use crate::snapshot::{FlockRegistry, FlockSnapshot, SavedApp};
use crate::supervisor::{SupervisorBuilder, SupervisorHandle};

// A hand-rolled `MakeWriter` over one shared buffer, not
// `fmt::layer().with_test_writer()`: the test writer hands its output to
// libtest's capture, where the test itself cannot read it back.
#[derive(Debug, Clone, Default)]
pub(crate) struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    /// Everything rendered into this capture so far, as one string.
    fn rendered(&self) -> String {
        let buffer = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A second [`tracing::Dispatch`], kept alive for the life of the process, so
/// a callsite's cached `Interest` is a union over every registered dispatcher.
///
/// `tracing` caches each callsite's `Interest` process-wide on first reach.
/// While `Dispatchers::has_just_one` holds, that value is read off whichever
/// thread registered first, which in a test binary is routinely a sibling with
/// no subscriber, and [`capture_logs`] comes back empty.
static SECOND_DISPATCH: LazyLock<tracing::Dispatch> =
    LazyLock::new(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));

/// Runs `f` with a subscriber scoped to this thread, returning what it recorded.
///
/// Scoped rather than global, since a global subscriber installs once per
/// process. `f` must be synchronous and stay on this thread: a record written
/// by a `tokio::spawn`ed task carries no thread-local dispatcher. Forcing
/// [`SECOND_DISPATCH`] first is load-bearing; see its doc. ANSI is off so
/// assertions match text, and the level is `TRACE`.
pub(crate) fn capture_logs(f: impl FnOnce()) -> String {
    LazyLock::force(&SECOND_DISPATCH);
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    capture.rendered()
}

/// The one `warn!` [`a_sibling_thread_reaching_a_callsite_first_cannot_empty_the_capture`]
/// races over, in its own helper so exactly two threads share one callsite.
fn racing_warn() {
    tracing::warn!("a callsite two threads reach");
}

/// fails if [`SECOND_DISPATCH`] stops being forced.
///
/// The two channels put the sibling's registration inside the capture's scope
/// on every run: the sibling waits to be told the scope is open, and the
/// capture waits to be told registration is done. Under the full binary's
/// parallelism another live capture can make `has_just_one` false anyway, so
/// the negative control has to be run with `--exact`.
#[test]
fn a_sibling_thread_reaching_a_callsite_first_cannot_empty_the_capture() {
    let (scope_open, await_scope) = std::sync::mpsc::channel();
    let (registered, await_registration) = std::sync::mpsc::channel();

    let sibling = std::thread::spawn(move || {
        await_scope.recv().expect("the capture must open its scope");
        // No subscriber on this thread: this is the registration that decides
        // the callsite's cached `Interest` for the whole process.
        racing_warn();
        registered
            .send(())
            .expect("the capture must still be waiting");
    });

    let rendered = capture_logs(|| {
        scope_open
            .send(())
            .expect("the sibling must still be waiting");
        await_registration
            .recv()
            .expect("the sibling must register before this emit");
        racing_warn();
    });

    sibling.join().expect("the sibling thread must not panic");
    assert!(
        rendered.contains("a callsite two threads reach"),
        "a sibling thread registering first must not disable the callsite for \
         this capture: {rendered:?}"
    );
}

/// Every sheep the supervisor holds, by name and status, in name order.
///
/// A listing is only assertable once it is sorted, and the flock a restore
/// installs is not in the order the roll named.
pub(crate) async fn sorted_status(handle: &SupervisorHandle) -> Vec<(String, ProcStatus)> {
    let mut listed = handle.list().await;
    listed.sort_by(|left, right| left.name.cmp(&right.name));
    listed
        .into_iter()
        .map(|info| (info.name, info.status))
        .collect()
}

/// A muster roll holding `apps`, every one of them recorded as running.
///
/// The rolls whose instance counts a test is actually about build their own
/// `SavedApp` entries and go through [`FlockSnapshot::with_apps`].
pub(crate) fn roll_of(apps: Vec<AppConfig>) -> FlockSnapshot {
    let saved = apps.into_iter().map(|app| SavedApp {
        app,
        instances_running: 1,
        disabled: false,
    });
    FlockSnapshot::with_apps(saved.collect())
}

// The tempdir root is `$SHEP_HOME` with no extra nesting: `sun_path` caps a
// socket path at 104 bytes on macOS, and macOS temp paths are already long.
pub(crate) fn test_paths(dir: &tempfile::TempDir) -> ShepPaths {
    let home = dir.path().to_path_buf();
    ShepPaths::resolve(
        &|key| (key == "SHEP_HOME").then(|| home.display().to_string()),
        std::path::Path::new("/nonexistent"),
    )
}

/// A [`ScriptedRunner`] a test can still read after the engine has taken
/// ownership of it. [`ProcessRunner::spawn`] takes `&self`, so sharing one
/// costs nothing but this forwarding impl.
///
/// The supervisor's tests and `boot`'s both hand a runner away and then assert
/// on its counters.
#[derive(Debug)]
pub(crate) struct SharedRunner(pub(crate) Arc<ScriptedRunner>);

impl ProcessRunner for SharedRunner {
    type Proc = crate::fake::FakeProc;

    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        self.0.spawn(spec)
    }

    fn preflight(&self, spec: &SpawnSpec) -> Preflight {
        self.0.preflight(spec)
    }
}

/// A [`ProcessRunner`] that announces each spawn on a unix datagram socket
/// before delegating, so a test can assert on the order of two events rather
/// than on the presence of one.
///
/// For `boot`'s readiness-ordering case: the announcement and `boot`'s own
/// `READY=1` land on one socket, and AF_UNIX `SOCK_DGRAM` enqueues
/// synchronously, so the queue order read back is the program order.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct AnnouncingRunner<R> {
    inner: R,
    target: PathBuf,
}

#[cfg(unix)]
impl<R> AnnouncingRunner<R> {
    /// Wraps `inner`, announcing every spawn to the socket bound at `target`.
    pub(crate) fn new(inner: R, target: &Path) -> Self {
        Self {
            inner,
            target: target.to_path_buf(),
        }
    }
}

#[cfg(unix)]
impl<R: ProcessRunner> ProcessRunner for AnnouncingRunner<R> {
    type Proc = R::Proc;

    /// # Panics
    ///
    /// If the announcement cannot be sent, because the socket the test bound
    /// is gone or was never bound. Swallowing it would report a timeout
    /// instead of the real fault.
    #[track_caller]
    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        let socket = std::os::unix::net::UnixDatagram::unbound()
            .expect("a test must be able to open an unbound datagram socket");
        socket
            .send_to(b"SPAWNED\n", &self.target)
            .expect("the test's listening socket must be bound before the spawn");
        self.inner.spawn(spec)
    }
}

/// A proptest configuration running `local_cases` by default, and whatever
/// `PROPTEST_CASES` names when the environment sets it.
///
/// `Config::default()` already reads `PROPTEST_CASES`, but a struct-update
/// literal that then writes `cases:` overwrites what it read. Deferring to the
/// default whenever the variable is set keeps both: a source-tuned count
/// locally, an environment-set ceiling in CI.
pub(crate) fn proptest_config(local_cases: u32) -> proptest::test_runner::Config {
    let default = proptest::test_runner::Config::default();
    if std::env::var_os("PROPTEST_CASES").is_some() {
        default
    } else {
        proptest::test_runner::Config {
            cases: local_cases,
            ..default
        }
    }
}

/// Creates `root.join(rel)`, making its parent directories first, and writes
/// one byte so the file actually exists on disk. Returns the absolute path
/// written.
///
/// One byte, because watch tests care that a create or modify event fires and
/// never about the contents.
///
/// # Errors
///
/// Whatever `std::fs::create_dir_all` or `std::fs::write` returns.
pub(crate) fn touch(root: &Path, rel: &str) -> std::io::Result<PathBuf> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, b"x")?;
    Ok(path)
}

// The dispatch tests and the connection server's need the same fixture.
pub(crate) struct Harness {
    pub(crate) ctx: RpcContext,
    // Kept alive only: dropping the tempdir would remove the paths `ctx`
    // still points at.
    _dir: tempfile::TempDir,
    // Kept alive only: dropping the sender's last receiver would turn
    // every future `events.send()` into a silent no-op.
    _events_rx: broadcast::Receiver<SharedEvent>,
    pub(crate) shutdown_rx: watch::Receiver<bool>,
    /// Breach reports the supervisor's extras produce, for tests that assert
    /// a memory limit fired.
    pub(crate) breaches: mpsc::Receiver<LimitBreach>,
    /// Liveness-failure reports, for tests that assert a probe threshold
    /// tripped.
    pub(crate) liveness: mpsc::Receiver<LivenessReport>,
    /// The same [`StatsState`] the extras and [`Self::ctx`] share, so a test
    /// can plant the periodic CPU baseline an on-demand reading measures
    /// against instead of waiting a poll interval for the enforcer's own
    /// tick to plant one.
    pub(crate) stats: Arc<StatsState>,
    /// The runner the engine spawns through, so a test can read a sheep's
    /// channel with [`ScriptedRunner::io_handles`].
    pub(crate) runner: Arc<ScriptedRunner>,
    /// Every signal a lamb path sent, in place of the operating system.
    pub(crate) pid_signals: Arc<RecordingPidSignals>,
}

/// Capacity of the harness's two report channels. A test needing more unread
/// reports than this is asserting on something else.
const HARNESS_REPORT_CAPACITY: usize = 16;

/// Builds one supervisor engine (a [`ScriptedRunner`] replaying `scripts`)
/// plus a fresh [`RpcContext`] wired to it, with neutral lifecycle extras: a
/// harness nobody configured arms nothing and reports nothing.
pub(crate) fn harness(scripts: Vec<ProcScript>) -> Harness {
    // A machine with no visible processes, so nothing breaches. Not
    // `ScriptedSampler::new(vec![])`, which the constructor asserts on: the
    // neutral value is one reading holding an empty table.
    harness_sampling(scripts, vec![vec![]])
}

/// [`harness`], over a process table that really holds the first sheep's
/// tree.
///
/// The pid it describes is the one [`ScriptedRunner`] hands the first spawn,
/// so a sheep started through this harness joins against a real reading. Every
/// reading reports the same 4 KiB tree and a CPU counter that advances 1500 ms
/// between the first two, enough to baseline off one and measure the next.
pub(crate) fn harness_with_stats(scripts: Vec<ProcScript>) -> Harness {
    let tree = |cpu_ms| {
        vec![ProcessRss {
            pid: FIRST_SCRIPTED_PID,
            parent: None,
            bytes: SCRIPTED_TREE_BYTES,
            cpu_ms,
        }]
    };
    harness_sampling(scripts, vec![tree(1_000), tree(2_500), tree(2_501)])
}

/// Resident size every reading [`harness_with_stats`] scripts reports, small
/// and distinctive so a test asserting on it cannot be reading a default.
pub(crate) const SCRIPTED_TREE_BYTES: u64 = 4096;

/// [`harness`], with every spawn reporting `pid` rather than a number
/// derived from spawn order.
///
/// For a test whose scripted proc has to be the same process as a real
/// socket's peer; see [`ScriptedRunner::spawning_at`].
pub(crate) fn harness_at_pid(scripts: Vec<ProcScript>, pid: u32) -> Harness {
    harness_sampling_with(ScriptedRunner::new(scripts).spawning_at(pid), vec![vec![]])
}

/// [`harness`], with every named app refused by the runner's preflight.
///
/// For a test that needs one app of a batch to be provably unstartable, which
/// is the supervisor's pre-registration pass and the only door to
/// `SupervisorError::CannotStart`; see [`ScriptedRunner::refusing`].
pub(crate) fn harness_refusing(scripts: Vec<ProcScript>, names: &[&str]) -> Harness {
    harness_sampling_with(ScriptedRunner::new(scripts).refusing(names), vec![vec![]])
}

/// [`harness`], with every named app failing at the spawn itself.
///
/// The half [`harness_refusing`] cannot express: a preflight refusal happens
/// before anything in the batch is registered, while a spawn failure happens
/// after the apps ahead of it are already running.
pub(crate) fn harness_failing_to_spawn(scripts: Vec<ProcScript>, names: &[&str]) -> Harness {
    harness_sampling_with(
        ScriptedRunner::new(scripts).failing_to_spawn(names),
        vec![vec![]],
    )
}

/// [`harness`] over a scripted process table, the body both spellings share.
fn harness_sampling(scripts: Vec<ProcScript>, readings: Vec<Vec<ProcessRss>>) -> Harness {
    harness_sampling_with(ScriptedRunner::new(scripts), readings)
}

/// [`harness_sampling`], over a runner the caller built.
fn harness_sampling_with(runner: ScriptedRunner, readings: Vec<Vec<ProcessRss>>) -> Harness {
    harness_with_runner(runner, |reports| {
        standard_extras(Arc::new(ScriptedSampler::new(readings)), reports)
    })
}

/// The [`Extras`] every scripted harness runs with, over a caller-chosen
/// sampler.
///
/// The sampler is the only part a fixture varies. Everything else is the
/// fixed test environment: a clock that does not move on its own, an
/// enforcer polling that sampler, and the defaults a daemon nobody
/// configured would run with.
fn standard_extras(sampler: Arc<dyn MemorySampler>, reports: ExtrasReports) -> Extras {
    let stats = Arc::new(StatsState::new(Arc::clone(&sampler)));
    Extras {
        clock: Arc::new(TestClock::starting_at(
            "2026-01-01T00:00:00Z"
                .parse()
                .expect("a valid RFC3339 timestamp"),
        )),
        enforcer: Arc::new(crate::limits::PollingEnforcer::start(
            sampler,
            reports.breaches.clone(),
            Arc::clone(&stats),
        )),
        lamb_sweep: idle_sweep(),
        // A fixture nobody configured behaves like a daemon nobody configured.
        max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
        reports,
        stats,
    }
}

/// [`harness`], over a process table that reports process identities.
///
/// The one fixture that can answer a lamb walk. [`harness_sampling`]'s sampler
/// takes the trait's default `identify`, which returns nothing, so a
/// `describe` through [`harness`] finds no lambs however the walk is written.
pub(crate) fn harness_identifying(
    scripts: Vec<ProcScript>,
    identities: Vec<ProcessIdentity>,
) -> Harness {
    harness_with_extras(scripts, |reports| {
        standard_extras(
            Arc::new(ScriptedSampler::identifying(vec![identities])),
            reports,
        )
    })
}

/// One row of a scripted identity table, shared by [`harness_identifying`]
/// and `stats.rs`'s and `rpc/dispatch.rs`'s test modules.
pub(crate) fn identity(pid: u32, parent: Option<u32>, name: &str) -> ProcessIdentity {
    ProcessIdentity {
        pid,
        parent,
        name: name.to_string(),
    }
}

/// A [`StatsState`] over a machine with no visible processes.
///
/// The neutral value for a fixture that has to hand [`Extras`] a `stats` but
/// asserts nothing about resource readings. Watching still works against it,
/// since the watch map is the fixture's own bookkeeping.
pub(crate) fn idle_stats() -> Arc<StatsState> {
    Arc::new(StatsState::new(Arc::new(ScriptedSampler::new(vec![
        vec![],
    ]))))
}

/// [`harness`], with the caller deciding the extras.
///
/// Takes a builder rather than a finished [`Extras`], because the harness has
/// to own both report receivers: no reporter is spawned, so a test asserts the
/// report itself rather than racing a restart. A caller-built `Extras` carries
/// senders whose receivers the harness could not recover.
pub(crate) fn harness_with_extras(
    scripts: Vec<ProcScript>,
    build_extras: impl FnOnce(ExtrasReports) -> Extras,
) -> Harness {
    harness_with_runner(ScriptedRunner::new(scripts), build_extras)
}

/// [`harness`], over a [`ScriptedRunner`] the caller built.
///
/// A `Vec<ProcScript>` cannot say anything about the runner itself: which pid
/// its spawns report, which sheep it refuses.
pub(crate) fn harness_with_runner(
    runner: ScriptedRunner,
    build_extras: impl FnOnce(ExtrasReports) -> Extras,
) -> Harness {
    build_harness(runner, build_extras, crate::supervisor::DEFAULT_ENVIRONMENT)
}

/// [`harness`], on a host whose `[daemon] environment` is `environment`.
///
/// The shepherd's environment is what a sheep naming none of its own
/// resolves its secrets in, held by the supervisor and by the RPC layer.
pub(crate) fn harness_in_environment(scripts: Vec<ProcScript>, environment: &str) -> Harness {
    build_harness(
        ScriptedRunner::new(scripts),
        |reports| standard_extras(Arc::new(ScriptedSampler::new(vec![vec![]])), reports),
        environment,
    )
}

/// The one body behind every harness, on a host in `environment`.
fn build_harness(
    runner: ScriptedRunner,
    build_extras: impl FnOnce(ExtrasReports) -> Extras,
    environment: &str,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (events, events_rx) = crate::bus::test_bus(256);
    let (breach_tx, breaches) = mpsc::channel(HARNESS_REPORT_CAPACITY);
    let (live_tx, liveness) = mpsc::channel(HARNESS_REPORT_CAPACITY);
    let extras = build_extras(ExtrasReports {
        breaches: breach_tx,
        liveness: live_tx,
    });
    // Taken before `extras` is moved, as `boot` takes it: the RPC layer and
    // the extras must share one state, or a listing would read a watch set
    // nothing ever wrote to.
    let stats = Arc::clone(&extras.stats);
    // Shared for `boot`'s reason: a `PutSecrets` through the harness has to
    // reach the same registry a spawn resolves against.
    let provider_secrets = Arc::new(crate::secrets::ProviderSecrets::load(&paths.secrets_cache));
    let runner = Arc::new(runner);
    let supervisor = SupervisorBuilder::new(
        SharedRunner(Arc::clone(&runner)),
        paths.clone(),
        events.clone(),
    )
    .extras(extras)
    .provider_secrets(Arc::clone(&provider_secrets))
    .environment(environment.to_string())
    .spawn();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let pid_signals = Arc::new(RecordingPidSignals::default());
    Harness {
        ctx: RpcContext {
            supervisor,
            events,
            registry: FlockRegistry::new(),
            snapshot_path: paths.snapshot.clone(),
            dogs_config: paths.dogs_config.clone(),
            // The two built-in dogs, which is what a `$SHEP_HOME` with
            // nothing adopted hands a real boot. A test wanting an adopted
            // name assigns its own set over this one.
            known_dogs: crate::rpc::KnownDogs::new(
                ["metrics".to_string(), "bark".to_string()]
                    .into_iter()
                    .collect(),
            ),
            dog_names: Vec::new(),
            boot_first_dogs: Vec::new(),
            paths: paths.clone(),
            daemon_version: "0.1.0".to_string(),
            dog_refusals: crate::dogs::DogRefusals::new(),
            peer_contacts: crate::dogs::PeerContacts::new(),
            pid: 4242,
            shutdown,
            stats: Arc::clone(&stats),
            // Nothing read, as when `sysinfo` cannot see the platform. A test
            // that needs a reading assigns its own `HostState::fixed`.
            host: crate::host::HostState::fixed(None),
            provider_secrets,
            host_environment: environment.to_string(),
            pid_signals: Arc::clone(&pid_signals) as Arc<dyn crate::rpc::PidSignals>,
        },
        _dir: dir,
        _events_rx: events_rx,
        shutdown_rx,
        breaches,
        liveness,
        stats,
        runner,
        pid_signals,
    }
}

/// `AppConfig::minimal(name, "./srv")` with `mutate` applied, normalized.
///
/// The one place a fixture app is built for the lifecycle-extra tests, so a
/// case that needs `cron_restart` or `watch` says only that.
///
/// # Panics
/// Panics if the mutated config does not normalize, which is a fixture bug.
#[track_caller]
pub(crate) fn app_with(name: &str, mutate: impl FnOnce(&mut AppConfig)) -> ResolvedApp {
    let mut app = AppConfig::minimal(name, "./srv");
    mutate(&mut app);
    normalize(app).expect("the fixture app must normalize")
}

/// A [`ProcessEntry`] carrying nothing but its `id` and `spec`: `Online`,
/// `pid` `None`, zero restarts, `instance` 0, empty log paths, nothing
/// parked, no reload, no dog, no recorded exit, and
/// [`SpawnIdentity::Resolved(None)`].
///
/// A case names the fields it is about and takes the rest, as
/// `ProcessEntry { status: ProcStatus::Stopped, ..plain_entry(id, spec) }`.
pub(crate) fn plain_entry(id: u32, spec: ResolvedApp) -> ProcessEntry {
    ProcessEntry {
        id,
        spec,
        pending: None,
        pending_reidentifies: false,
        overridden: Vec::new(),
        instance: 0,
        status: ProcStatus::Online,
        pid: None,
        restarts: 0,
        started_at: None,
        budget: RestartBudget::default(),
        reload: ReloadState::None,
        // A settled identity, which is what a registration through `Start`
        // leaves behind once the entry is `Online`.
        credentials: SpawnIdentity::Resolved(None),
        out_file: PathBuf::new(),
        err_file: PathBuf::new(),
        dog: None,
        last_exit: None,
        spawn_failure: None,
    }
}

/// An `Online` [`ProcessEntry`] shaped like one the actor really registered.
///
/// Its two log paths come from [`assemble`] rather than being invented, so a
/// registry-tier test's entry cannot drift from what a spawn produces.
pub(crate) fn armed_entry(
    id: u32,
    instance: u32,
    pid: u32,
    app: ResolvedApp,
    paths: &ShepPaths,
) -> ProcessEntry {
    let spec = assemble(
        &app,
        instance,
        paths,
        None,
        &SecretView::empty("production".to_string()),
    )
    .expect("the fixture app must assemble");
    ProcessEntry {
        instance,
        pid: Some(pid),
        out_file: spec.out_file,
        err_file: spec.err_file,
        ..plain_entry(id, app)
    }
}
