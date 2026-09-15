//! Tests for the supervisor actor.
//!
//! These live under the module they exercise rather than in `tests/`, so
//! they can reach its private items directly, the way the rest of the crate
//! does. The shared harness is here; each sibling file holds one concern.

use shep_core::config::{AppConfig, LevelRule, LineLevel, ProbeConfig, ProbeKind, normalize};
use shep_core::protocol::DogSource;
use shep_core::status::ProcStatus;
use shep_core::values::{MemSize, UpDuration};

use super::*;
use crate::cron::{DEFAULT_MAX_CRON_SLEEP, SystemClock};
use crate::extras::{ExtrasReports, spawn_extras_reporter};
use crate::fake::{ProcScript, ScriptedRunner};
use crate::limits::LimitEnforcer;
use crate::testing::capture_logs;
use crate::testing::{
    Harness, RecordingEnforcer, ScriptedProber, SharedRunner, app_with, armed_entry, harness,
    idle_stats, probe_config, test_paths,
};
#[cfg(unix)]
use crate::tokio_runner::TokioRunner;
use tokio::sync::watch;
// aliased: `Ordering` in this module means `cmp::Ordering`
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

// --- module tree ---
mod actions;
#[cfg(unix)]
mod adopt;
#[cfg(unix)]
mod adopt_restart;
#[cfg(unix)]
mod adopt_swap;
mod config_load;
mod config_promote;
mod config_rearm;
mod config_report;
mod config_reset;
mod credentials;
mod dogs;
mod env_batch;
mod flush;
#[cfg(unix)]
mod handover;
mod interleaving;
mod readiness;
mod reload_bus;
mod reload_drain;
mod reload_drainee;
mod reload_swap;
mod reopen;
mod restart_races;
mod scale;
mod secrets;
mod shutdown;
mod signals;
mod start_stop;
mod triggers;
// --- end module tree ---

/// Every process event queued right now, in order, for a case whose
/// handler is synchronous.
fn drained_process_kinds(
    rx: &mut tokio::sync::broadcast::Receiver<SharedEvent>,
) -> Vec<ProcessEventKind> {
    let mut kinds = Vec::new();
    while let Ok(BusEvent::Process { event, .. }) = rx.try_recv().map(|event| event.to_event()) {
        kinds.push(event);
    }
    kinds
}

async fn await_event(
    rx: &mut tokio::sync::broadcast::Receiver<SharedEvent>,
    id: u32,
    kind: ProcessEventKind,
) -> bool {
    loop {
        match rx.recv().await.map(|event| event.to_event()) {
            Ok(BusEvent::Process {
                event,
                info,
                manually,
                ..
            }) if info.id == id && event == kind => {
                return manually;
            }
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(e) => panic!("event stream closed before {kind:?} for id {id}: {e}"),
        }
    }
}

/// Waits up to `window` for `kind` targeting `id`; panics if it arrives.
///
/// Bounded `timeout` + `recv` rather than `try_recv`: a message already due
/// may not have reached this receiver's queue yet.
async fn assert_no_event_within(
    rx: &mut tokio::sync::broadcast::Receiver<SharedEvent>,
    id: u32,
    kind: ProcessEventKind,
    window: Duration,
) {
    match tokio::time::timeout(window, await_event(rx, id, kind)).await {
        Err(_elapsed) => {} // window elapsed with nothing arriving: expected
        Ok(_manually) => panic!("unexpected {kind:?} for id {id} within {window:?}"),
    }
}

/// A bare actor over `sheep`, running `scripts` and reachable at `tx`.
///
/// The bus receiver is dropped here, as every fixture already dropped its
/// own: a bus with no subscriber still takes every send.
///
/// `next_id` is one past the slots, which the contiguous ids every fixture
/// assigns make right. A case that needs another value, or `extras`,
/// writes it with struct-update syntax over this.
fn test_actor(
    paths: ShepPaths,
    scripts: Vec<ProcScript>,
    sheep: HashMap<u32, SheepSlot>,
    tx: mpsc::Sender<Msg>,
) -> Actor<ScriptedRunner> {
    let (events, _events_rx) = crate::bus::test_bus(64);
    let provider_secrets = Arc::new(ProviderSecrets::load(&paths.secrets_cache));
    Actor {
        runner: ScriptedRunner::new(scripts),
        next_id: sheep.len() as u32,
        paths,
        events,
        host_environment: DEFAULT_ENVIRONMENT.to_string(),
        provider_secrets,
        tx,
        sheep,
        next_deadline: 0,
        next_action_stamp: 0,
        pending: Vec::new(),
        shutting_down: false,
        extras: None,
        registry: ExtrasRegistry::default(),
        reloads: HashMap::new(),
        smits: Smits::new(),
    }
}

// --- The readiness gate ---

// --- Concurrency regression guards ---

// --- `Stopping`: the drainee, against the guards it must never pass ---
//
// These cases call the guarded handlers directly, so a failure names the
// guard rather than a later consequence.

/// One sheep already `Stopping`, wired the way a reload's drainee is: a live
/// `ctl` sender and a pid a stale report can be raised against. No scripts,
/// so a broken guard's spawn attempt fails loudly.
fn actor_with_stopping_drainee(
    dir: &tempfile::TempDir,
    pid: u32,
    epoch: u64,
) -> (Actor<ScriptedRunner>, mpsc::Receiver<SheepCtl>) {
    let paths = test_paths(dir);
    let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
    let mut entry = armed_entry(0, 0, pid, app, &paths);
    entry.status = ProcStatus::Stopping;
    let (ctl_tx, ctl_rx) = mpsc::channel(1);
    let slot = SheepSlot {
        ctl: Some(ctl_tx),
        epoch,
        ..SheepSlot::new(entry)
    };
    let mut sheep = HashMap::new();
    sheep.insert(0, slot);
    let (tx, _rx) = mpsc::channel(16);
    let actor = test_actor(paths, Vec::new(), sheep, tx);
    (actor, ctl_rx)
}

/// One sheep marked as a reload's drainee, `Stopping` on `status` and
/// `ReloadState::Drainee` on `reload`, holding a live signal mailbox whose
/// receiver the caller keeps. `begin_action` filters on that marker and
/// `begin_signal` must not.
fn actor_with_a_drainee_holding_a_signal_mailbox(
    dir: &tempfile::TempDir,
) -> (Actor<ScriptedRunner>, mpsc::Receiver<SignalRequest>) {
    // No scripts: an unwanted spawn fails loudly.
    let (mut actor, _mailbox) = actor_with_one_online_sheep(dir, vec![]);
    let slot = actor.sheep.get_mut(&0).expect("the fixture registers id 0");
    slot.entry.status = ProcStatus::Stopping;
    slot.entry.reload = ReloadState::Drainee { new_id: Some(1) };
    // Wide enough that a `try_send` returning `Full` means a bug.
    let (signals, signal_rx) = mpsc::channel(16);
    slot.signals = Some(signals);
    (actor, signal_rx)
}

// --- Reload: which of the two orderings an app gets ---

// --- Reload: the post-drain check an overlap still owes ---

// --- Reload: the per-instance swap machine ---

/// A window covering a whole swap (`listen_timeout` + `graceful_timeout` +
/// room), so a case whose event never arrives fails instead of parking the
/// suite. Virtual time, so an early swap costs nothing.
const SWAP_WINDOW: Duration = Duration::from_secs(30);

/// Drives virtual time until `kind` arrives for `id`, failing rather than
/// hanging if it never does.
async fn expect_event(
    rx: &mut tokio::sync::broadcast::Receiver<SharedEvent>,
    id: u32,
    kind: ProcessEventKind,
) {
    assert!(
        tokio::time::timeout(SWAP_WINDOW, await_event(rx, id, kind))
            .await
            .is_ok(),
        "no {kind:?} for id {id} within {SWAP_WINDOW:?}"
    );
}

/// One started app, the runner behind it and a bus subscriber.
///
/// The runner is shared rather than moved so a case can read
/// `kill_counts().len()`, the number of spawns that succeeded.
async fn started(
    dir: &tempfile::TempDir,
    app: AppConfig,
    scripts: Vec<ProcScript>,
) -> (
    SupervisorHandle,
    Arc<ScriptedRunner>,
    tokio::sync::broadcast::Receiver<SharedEvent>,
) {
    let (events, rx) = crate::bus::test_bus(256);
    let runner = Arc::new(ScriptedRunner::new(scripts));
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(dir), events);
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    (handle, runner, rx)
}

/// A bare actor holding one `Online` sheep, for the cases that drive a
/// handler directly: a swap's ownership lives in `ProcessEntry::reload`,
/// which is crate-internal and never on the wire.
fn actor_with_one_online_sheep(
    dir: &tempfile::TempDir,
    scripts: Vec<ProcScript>,
) -> (Actor<ScriptedRunner>, mpsc::Receiver<Msg>) {
    actor_with_one_online_sheep_of(dir, AppConfig::minimal("web", "./srv"), scripts)
}

/// [`actor_with_one_online_sheep`] for a case that needs a particular app:
/// a `readiness_probe`, a `reuse_port`, or both, which between them decide
/// which reload the instance gets.
fn actor_with_one_online_sheep_of(
    dir: &tempfile::TempDir,
    app: AppConfig,
    scripts: Vec<ProcScript>,
) -> (Actor<ScriptedRunner>, mpsc::Receiver<Msg>) {
    let paths = test_paths(dir);
    let app = normalize(app).unwrap();
    let mut sheep = HashMap::new();
    sheep.insert(0, SheepSlot::new(armed_entry(0, 0, 1111, app, &paths)));
    let (tx, rx) = mpsc::channel(MAILBOX_CAPACITY);
    let actor = test_actor(paths, scripts, sheep, tx);
    (actor, rx)
}

/// [`ProcessEntry::id`] of the fixture's sheep, and of its dog.
const SHEEP_ID: u32 = 0;
const DOG_ID: u32 = 1;

/// A bare actor holding one `Online` sheep and one `Online` dog.
///
/// Alike in everything a selector can read, so the dog marker is the only
/// difference a case can attribute an answer to.
fn actor_with_a_sheep_and_a_dog(
    dir: &tempfile::TempDir,
) -> (Actor<ScriptedRunner>, mpsc::Receiver<Msg>) {
    let paths = test_paths(dir);
    let mut sheep = HashMap::new();
    for (id, name, dog) in [
        (SHEEP_ID, "web", None),
        (DOG_ID, "bark", Some(DogSource::BuiltIn)),
    ] {
        let app = app_with(name, |config| config.fold = Some("svc".to_string()));
        let mut entry = armed_entry(id, 0, 1111 + id, app, &paths);
        entry.dog = dog;
        sheep.insert(id, SheepSlot::new(entry));
    }
    let (tx, rx) = mpsc::channel(MAILBOX_CAPACITY);
    let actor = test_actor(paths, Vec::new(), sheep, tx);
    (actor, rx)
}

/// Without the last two assertions, a helper that excluded dogs from
/// everything passes and `shep disable bark` would match nothing.
#[test]
fn a_wildcard_passes_a_dog_by_and_its_own_name_still_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let (actor, _mailbox) = actor_with_a_sheep_and_a_dog(&dir);

    assert_eq!(
        actor.matching_ids(&ProcessSelector::All),
        vec![SHEEP_ID],
        "`all` is the flock, not the kennel"
    );
    assert_eq!(
        actor.matching_ids(&ProcessSelector::parse("/^(web|bark)$/").unwrap()),
        vec![SHEEP_ID],
        "a sweep that spells both names out is still a sweep"
    );
    assert_eq!(
        actor.matching_ids(&ProcessSelector::Fold("svc".into())),
        vec![SHEEP_ID],
        "a dog shares its fold with the flock and is still not swept by it"
    );
    assert_eq!(
        actor.matching_ids(&ProcessSelector::Name("bark".into())),
        vec![DOG_ID]
    );
    assert_eq!(
        actor.matching_ids(&ProcessSelector::Id(DOG_ID)),
        vec![DOG_ID]
    );
}

#[test]
fn a_listing_reports_where_a_dog_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let (actor, _mailbox) = actor_with_a_sheep_and_a_dog(&dir);

    assert_eq!(
        to_info(&actor.sheep[&DOG_ID].entry, &actor.smits).dog,
        Some(DogSource::BuiltIn)
    );
    assert_eq!(
        to_info(&actor.sheep[&SHEEP_ID].entry, &actor.smits).dog,
        None
    );
}

/// Starts `app` (normalized) through `h`'s supervisor and hands back the
/// snapshot the start answers with.
///
/// # Panics
///
/// Panics if `app` does not normalize, or if the actor refuses the start.
/// No `#[track_caller]`: it is a no-op on an async fn.
async fn start_app(h: &Harness, app: AppConfig) -> Vec<ProcessInfo> {
    h.ctx
        .supervisor
        .start(vec![normalize(app).unwrap()])
        .await
        .unwrap()
}

/// One dog's app spec. The path is a label: [`ScriptedRunner`] replays a
/// script instead of exec'ing anything, so nothing has to exist there.
fn dog_app(name: &str) -> ResolvedApp {
    normalize(AppConfig::minimal(name, "/nonexistent/shep")).unwrap()
}

/// One process event, flattened to what a reload's bus claims are made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    id: u32,
    kind: ProcessEventKind,
    status: ProcStatus,
    manually: bool,
}

/// Every process event in arrival order, up to and including `kind` for
/// `id`.
///
/// Bounded by [`SWAP_WINDOW`]. A `Lagged` is fatal: a hole in the stream is
/// a hole in every claim read off it.
async fn events_through(
    rx: &mut tokio::sync::broadcast::Receiver<SharedEvent>,
    id: u32,
    kind: ProcessEventKind,
) -> Vec<Seen> {
    let collect = async {
        let mut seen = Vec::new();
        loop {
            match rx.recv().await.map(|event| event.to_event()) {
                Ok(BusEvent::Process {
                    event,
                    info,
                    manually,
                    ..
                }) => {
                    seen.push(Seen {
                        id: info.id,
                        kind: event,
                        status: info.status,
                        manually,
                    });
                    if info.id == id && event == kind {
                        return seen;
                    }
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    panic!("the event stream lagged by {n}; no ordering claim survives that")
                }
                Err(e) => panic!("event stream closed before {kind:?} for id {id}: {e}"),
            }
        }
    };
    tokio::time::timeout(SWAP_WINDOW, collect)
        .await
        .unwrap_or_else(|_| panic!("no {kind:?} for id {id} within {SWAP_WINDOW:?}"))
}

/// Where `seen` first records `kind` for `id`, or a panic naming the run.
fn at(seen: &[Seen], id: u32, kind: ProcessEventKind) -> usize {
    seen.iter()
        .position(|e| e.id == id && e.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} for id {id} in {seen:?}"))
}

// --- Custom actions: one action out, one answer back or none ---

/// What an action gets to answer in. Virtual time, and long enough that no
/// scheduling order inside a case reaches it by accident.
const ACTION_TIMEOUT: Duration = Duration::from_secs(20);

/// A window generous enough for any action wait to report home, so a case
/// whose result never arrives fails instead of parking the suite.
const ACTION_WINDOW: Duration = Duration::from_secs(120);

/// A bare actor holding one sheep whose shepherd channel is open, plus the
/// mailbox every spawned wait reports to and the child's end of the
/// channel. Driven by hand so a case can put a reply on the channel at an
/// exact point relative to a wait's deadline.
fn actor_with_an_open_channel(
    dir: &tempfile::TempDir,
) -> (
    Actor<ScriptedRunner>,
    mpsc::Receiver<Msg>,
    mpsc::Receiver<ShepherdMessage>,
) {
    // No scripts: a spawn that should not have happened fails loudly.
    let (mut actor, mailbox) = actor_with_one_online_sheep(dir, vec![]);
    // Wide enough that no case can fill it, so a blocking `send` is a bug.
    let (to_child, child_rx) = mpsc::channel(16);
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers id 0")
        .to_child = Some(to_child);
    (actor, mailbox, child_rx)
}

/// Puts one action on the fixture's sheep and hands back the receiver its
/// answer will arrive on. Arms the wait directly rather than through
/// `Command::Trigger`.
fn trigger_action(
    actor: &mut Actor<ScriptedRunner>,
    action: &str,
) -> oneshot::Receiver<ActionOutcome> {
    let to_child = actor.sheep[&0]
        .to_child
        .clone()
        .expect("the fixture's sheep holds the daemon's end of a channel");
    actor.arm_action(0, to_child, action.to_string(), None, ACTION_TIMEOUT)
}

/// Drives the one message an action wait sends home and applies it,
/// returning what it carried.
async fn settle_action(
    actor: &mut Actor<ScriptedRunner>,
    mailbox: &mut mpsc::Receiver<Msg>,
) -> ActionOutcome {
    let msg = tokio::time::timeout(ACTION_WINDOW, mailbox.recv())
        .await
        .expect("an action wait reported nothing within the window")
        .expect("the actor's mailbox closed");
    match msg {
        Msg::ActionResult { id, stamp, outcome } => {
            actor.handle_action_result(id, stamp, outcome.clone());
            outcome
        }
        other => panic!("expected an action result, got {other:?}"),
    }
}

/// Reads the action the daemon put on the child's end of the channel,
/// failing rather than hanging if nothing was sent.
async fn sent_action(child_rx: &mut mpsc::Receiver<ShepherdMessage>) -> ShepherdMessage {
    tokio::time::timeout(ACTION_WINDOW, child_rx.recv())
        .await
        .expect("nothing reached the child's end of the channel")
        .expect("the child's end of the channel closed")
}

// --- Custom actions: one selector in, one row per matched sheep out ---

/// Reads one trigger's whole answer, failing rather than hanging if it
/// never comes. A request that armed a wait nothing resolves never answers.
async fn triggered(
    answer: oneshot::Receiver<Result<Vec<ActionReply>, SupervisorError>>,
) -> Result<Vec<ActionReply>, SupervisorError> {
    tokio::time::timeout(ACTION_WINDOW, answer)
        .await
        .expect("a trigger reported nothing within the window")
        .expect("the trigger's reply channel was dropped")
}

/// One expected row, spelled out at the call site.
fn row(id: u32, name: &str, outcome: ActionOutcome) -> ActionReply {
    ActionReply {
        id,
        name: name.to_string(),
        outcome,
    }
}

/// A [`ScriptedRunner`] whose spawns hand out a log-control channel that
/// accepts requests and never answers them.
///
/// Each request is held rather than dropped, so the `oneshot` sender inside
/// stays owed instead of resolving `Err`. The `watch` counts requests that
/// reached a pump.
struct SilentPumpRunner {
    inner: ScriptedRunner,
    seen: watch::Sender<u32>,
}

impl SilentPumpRunner {
    fn new(scripts: Vec<ProcScript>) -> (Self, watch::Receiver<u32>) {
        let (seen, requests) = watch::channel(0);
        (
            Self {
                inner: ScriptedRunner::new(scripts),
                seen,
            },
            requests,
        )
    }
}

impl fmt::Debug for SilentPumpRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SilentPumpRunner").finish_non_exhaustive()
    }
}

impl ProcessRunner for SilentPumpRunner {
    type Proc = crate::fake::FakeProc;

    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
        let (proc, mut io) = self.inner.spawn(spec)?;
        let (tx, mut rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
        // Replacing the sender drops the fake's own, ending the control
        // task it spawned. This runner exists so nothing answers.
        io.log_ctl = tx;
        let seen = self.seen.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Some(request) = rx.recv().await {
                held.push(request);
                seen.send_modify(|count| *count += 1);
            }
        });
        Ok((proc, io))
    }
}

/// What [`FailingPumpRunner`]'s pump answers every reopen with. One owner
/// for the string: the case below asserts the whole error it ends up in.
const PUMP_REFUSAL: &str = "/gone/web-out.log: No such file or directory";

/// The sheep [`FailingPumpRunner`] gives a failing pump to.
const REFUSING_SHEEP: &str = "web";

/// A [`ScriptedRunner`] whose spawn of [`REFUSING_SHEEP`] gets a pump that
/// answers every reopen with a failure. Every other sheep keeps the
/// scripted fake's own answering pump. By name rather than by spawn order,
/// so one case can hold a failed reopen and a healthy sheep beside it.
#[derive(Debug)]
struct FailingPumpRunner {
    inner: Arc<ScriptedRunner>,
}

impl FailingPumpRunner {
    fn new(inner: Arc<ScriptedRunner>) -> Self {
        Self { inner }
    }
}

impl ProcessRunner for FailingPumpRunner {
    type Proc = crate::fake::FakeProc;

    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
        let (proc, mut io) = self.inner.spawn(spec)?;
        if spec.name != REFUSING_SHEEP {
            return Ok((proc, io));
        }
        let (tx, mut rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
        // Replacing the sender drops the fake's own, ending the control
        // task it spawned. This pump answers in its place.
        io.log_ctl = tx;
        tokio::spawn(async move {
            while let Some(ctl) = rx.recv().await {
                // Both variants, so this pump keeps serving whichever
                // arrives.
                match ctl {
                    LogCtl::Reopen { done } => {
                        let _ = done.send(Err(ReopenError {
                            message: PUMP_REFUSAL.to_string(),
                        }));
                    }
                    LogCtl::Flush { done } => {
                        let _ = done.send(Err(FlushError {
                            message: PUMP_REFUSAL.to_string(),
                        }));
                    }
                    // This runner exists for the reopen and flush refusals.
                    #[cfg(unix)]
                    LogCtl::ReportFds { done } => {
                        let _ = done.send(CarriedFds::none());
                    }
                    // Nothing to start reading again: this runner reads no
                    // streams.
                    #[cfg(unix)]
                    LogCtl::Resume => {}
                }
            }
        });
        Ok((proc, io))
    }
}

// --- Signal: `shep signal`, one selector in, one row per matched sheep
// out ---

// --- SendLine: `shep whisper`, one selector in, one row per matched
// sheep out ---

// --- flush -------------------------------------------------------
//
// That the path and not the pump's current inode is what gets emptied needs
// a real handle on a real file, and lives in `tests/daemon_e2e.rs`.

// --- the log plane mid-reload ------------------------------------
//
// A swap's drainee and its replacement derive identical log paths. Both
// cases name the replacement by id, the form that cannot match the drainee.

/// A spawn spec for the cases that drive [`run_sheep`] directly. The
/// scripted fake reads none of it; [`ProcessRunner::spawn`] takes one.
fn log_ctl_spec() -> SpawnSpec {
    SpawnSpec {
        name: "svc".to_string(),
        program: "./svc".to_string(),
        args: Vec::new(),
        cwd: None,
        env: std::collections::BTreeMap::new(),
        out_file: std::path::PathBuf::from("out.log"),
        err_file: std::path::PathBuf::from("err.log"),
        channel: false,
        stdin: false,
        credentials: None,
    }
}

// --- Identity: which user a spawn actually runs as ----------------
//
// Nothing on the wire reports the uid a child comes up under, so the cases
// below read it off the `SpawnSpec` through `ScriptedRunner::spawned_as`.

/// A bare actor holding nothing at all, for the cases that drive
/// registration and respawn directly. Direct because
/// [`ProcessEntry::credentials`] is crate-internal.
fn actor_with_an_empty_flock(
    dir: &tempfile::TempDir,
    scripts: Vec<ProcScript>,
) -> Actor<ScriptedRunner> {
    let (tx, _rx) = mpsc::channel(MAILBOX_CAPACITY);
    let paths = test_paths(dir);
    test_actor(paths, scripts, HashMap::new(), tx)
}

/// The name this test process is already running under, the only user a
/// non-root test can ask for: `privilege::resolve` refuses any request that
/// would change identity unless the daemon is root.
#[cfg(unix)]
fn own_user_name() -> String {
    nix::unistd::User::from_uid(nix::unistd::geteuid())
        .unwrap()
        .expect("this process has a passwd entry")
        .name
}

/// A user name no passwd database has an entry for, so `resolve` fails
/// the same way whether or not the test runs as root.
const NO_SUCH_USER: &str = "definitely-not-a-real-shep-user";

// ---------------------------------------------------------------
// Supervisor proptest
// ---------------------------------------------------------------

// The command script and the process script are generated independently;
// their interleaving emerges from the runtime. Invariants are read off
// successive `list()` snapshots and the event stream, never off tick
// counts.

// No `Shutdown` step: it closes the actor's mailbox, so nothing composes
// after it. Each step is fully awaited before the next, so manual-vs-manual
// races belong in this file's dedicated race tests.

/// Two descriptors standing in for the daemon's own, so a snapshot taken
/// in a test names numbers that are really open. The actor never learns
/// the real listener and pidfile, so they are an argument.
#[cfg(unix)]
fn daemon_fds(dir: &tempfile::TempDir) -> (DaemonFds, [std::fs::File; 2]) {
    use std::os::fd::AsRawFd as _;

    let listener = std::fs::File::create(dir.path().join("listener.stand-in")).unwrap();
    let pidfile = std::fs::File::create(dir.path().join("pidfile.stand-in")).unwrap();
    let fds = DaemonFds {
        listener: listener.as_raw_fd(),
        pidfile: pidfile.as_raw_fd(),
    };
    // Returned alongside so the caller holds both files open: a closed
    // descriptor's number is free to be handed to the next open.
    (fds, [listener, pidfile])
}

/// A runner that takes an inherited sheep without a real process behind
/// it, so the install path can be driven under the paused clock.
///
/// `wait` never resolves: these cases assert on what an install puts in
/// the flock, and an exit that arrived on its own would race them.
#[cfg(unix)]
#[derive(Debug, Default)]
struct AdoptingRunner;

/// The pid [`AdoptingRunner`] gives anything it spawns fresh. Not a pid
/// any carried sheep in these cases holds.
#[cfg(unix)]
const STAND_IN_SPAWN_PID: u32 = 7000;

/// A proc with a pid and no process: it reports what it was built with
/// and never exits.
#[cfg(unix)]
#[derive(Debug)]
struct StandInProc {
    pid: u32,
}

#[cfg(unix)]
impl RunningProcess for StandInProc {
    fn pid(&self) -> u32 {
        self.pid
    }

    async fn wait(&mut self) -> ExitOutcome {
        core::future::pending().await
    }

    fn signal(&mut self, _sig: crate::runner::StopSignal) -> Result<(), RunnerError> {
        Ok(())
    }

    fn kill_tree(&mut self) -> Result<(), RunnerError> {
        Ok(())
    }
}

/// Four channels shaped the way an adopted sheep's are: logs and
/// shepherd traffic closed, since nothing here writes either.
#[cfg(unix)]
fn stand_in_io() -> ProcIo {
    let (_logs_tx, logs) = mpsc::channel(1);
    let (_from_child_tx, from_child) = mpsc::channel(1);
    let (to_child, to_child_rx) = mpsc::channel(1);
    drop(to_child_rx);
    let (log_ctl, log_ctl_rx) = mpsc::channel(1);
    drop(log_ctl_rx);
    let (to_stdin, to_stdin_rx) = mpsc::channel(1);
    drop(to_stdin_rx);
    ProcIo {
        logs,
        from_child,
        to_child,
        log_ctl,
        to_stdin,
    }
}

#[cfg(unix)]
impl ProcessRunner for AdoptingRunner {
    type Proc = StandInProc;

    fn spawn(&self, _spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        Ok((
            StandInProc {
                pid: STAND_IN_SPAWN_PID,
            },
            stand_in_io(),
        ))
    }

    fn adopt(&self, spec: crate::runner::AdoptSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        Ok((StandInProc { pid: spec.pid }, stand_in_io()))
    }
}

/// One sheep as a blob describes it, with `mutate` free to give it a
/// history no fresh registration could have.
#[cfg(unix)]
fn carried(
    name: &str,
    id: u32,
    pid: Option<u32>,
    mutate: impl FnOnce(&mut ProcessEntry),
) -> CarriedSheep {
    carried_marked(name, id, pid, false, None, false, mutate)
}

/// [`carried`], for an instance an earlier reload's readiness
/// verification failed against.
#[cfg(unix)]
fn carried_ready_failed(
    name: &str,
    id: u32,
    pid: Option<u32>,
    mutate: impl FnOnce(&mut ProcessEntry),
) -> CarriedSheep {
    carried_marked(name, id, pid, false, None, true, mutate)
}

/// [`carried`], with the slot facts a blob carries beside the entry set
/// explicitly: a pending delete, the manual command that owns this
/// sheep's next exit, and an earlier reload's failed readiness verdict.
#[cfg(unix)]
fn carried_marked(
    name: &str,
    id: u32,
    pid: Option<u32>,
    pending_delete: bool,
    manual: Option<PendingManual>,
    ready_failed: bool,
    mutate: impl FnOnce(&mut ProcessEntry),
) -> CarriedSheep {
    let mut app = AppConfig::minimal(name, "./srv");
    // Nothing here wants a respawn: an automatic restart would spawn a
    // second process behind the assertions.
    app.autorestart = false;
    let mut entry = ProcessEntry {
        id,
        spec: normalize(app).unwrap(),
        pending: None,
        pending_reidentifies: false,
        overridden: Vec::new(),
        instance: 0,
        status: ProcStatus::Online,
        pid,
        restarts: 0,
        started_at: None,
        budget: RestartBudget::default(),
        reload: ReloadState::None,
        credentials: SpawnIdentity::Resolved(None),
        out_file: PathBuf::new(),
        err_file: PathBuf::new(),
        dog: None,
        last_exit: None,
    };
    mutate(&mut entry);
    CarriedSheep::from_entry(
        &entry,
        0,
        CarriedFds::none(),
        pending_delete,
        manual,
        ready_failed,
        None,
    )
}

/// One sheep as a blob describes it, owed a respawn at a named moment.
///
/// Needs a `restart_delay` on the app, since a deadline means nothing
/// without a configured delay to be shorter than. `autorestart` is left
/// on, unlike [`carried_marked`]'s: these cases want the respawn.
#[cfg(unix)]
fn carried_owed_a_restart(
    name: &str,
    id: u32,
    delay: shep_core::values::UpDuration,
    due: Option<SystemTime>,
) -> CarriedSheep {
    let mut app = AppConfig::minimal(name, "./srv");
    app.restart_delay = Some(delay);
    let entry = ProcessEntry {
        id,
        spec: normalize(app).unwrap(),
        pending: None,
        pending_reidentifies: false,
        overridden: Vec::new(),
        instance: 0,
        status: ProcStatus::WaitingRestart,
        pid: None,
        restarts: 1,
        started_at: None,
        budget: RestartBudget::default(),
        reload: ReloadState::None,
        credentials: SpawnIdentity::Resolved(None),
        out_file: PathBuf::new(),
        err_file: PathBuf::new(),
        dog: None,
        last_exit: None,
    };
    CarriedSheep::from_entry(&entry, 0, CarriedFds::none(), false, None, false, due)
}

/// A carried sheep with no descriptors to rebuild, which is every case
/// below that does not open real pipes.
#[cfg(unix)]
fn without_handles(carried: CarriedSheep) -> crate::handover::adopt::AdoptedSheep {
    crate::handover::adopt::AdoptedSheep {
        carried,
        out_pipe: None,
        err_pipe: None,
        out_log: None,
        err_log: None,
        stdin_pipe: None,
        channel: None,
    }
}

/// Counters as a blob carries them, with `next_id` the one a case cares
/// about.
#[cfg(unix)]
const fn counters(next_id: u32) -> Counters {
    Counters {
        next_id,
        next_deadline: 0,
        next_action_stamp: 0,
    }
}

/// A real child for the adoption cases below to take over, running
/// `script` under `/bin/sh`, and its pid.
///
/// Its own process group is load-bearing: `TokioProc::signal` and
/// `kill_tree` both address the group, so a child that inherited this test
/// binary's group is not a group leader, `killpg` answers `ESRCH`, and the
/// ladder delivers nothing. The `Child` handle is dropped rather than
/// waited on, so the adopted flock's own reaper collects the status.
#[cfg(unix)]
fn adoptable_child(script: &str) -> u32 {
    use std::os::unix::process::CommandExt as _;

    std::process::Command::new("/bin/sh")
        .args(["-c", script])
        .process_group(0)
        // Null, not inherited: under `cargo test ... | <anything>` the
        // harness's stdout is a pipe, and a child that outlives a failing
        // case holds it open, turning the assertion into a hang.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a test host can spawn a shell")
        .id()
}

/// The app an adopted sheep in these cases runs, respawnable for real:
/// `mutate` gets it before it is normalized onto the entry.
#[cfg(unix)]
fn respawnable(mutate: impl FnOnce(&mut AppConfig)) -> impl FnOnce(&mut ProcessEntry) {
    move |entry: &mut ProcessEntry| {
        let mut app = AppConfig::minimal("web", "/bin/sh");
        // Real: one case lets the successor respawn the sheep and asserts
        // on the pid it comes back with. `./srv` would land in `Errored`.
        app.args = vec!["-c".to_owned(), "sleep 30".to_owned()];
        mutate(&mut app);
        entry.spec = normalize(app).unwrap();
    }
}

/// Polls the flock until `done` accepts it, or fails the case.
///
/// Real children and real signals, so the clock is real too: nothing here
/// can advance a paused one on the child's behalf. The bound sits past
/// anything these cases ask for, so a stall fails rather than hangs.
#[cfg(unix)]
async fn flock_until(
    sup: &SupervisorHandle,
    done: impl Fn(&[ProcessInfo]) -> bool,
    what: &str,
) -> Vec<ProcessInfo> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = sup.list().await;
            if done(&info) {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"))
}

// --- a swap in flight, carried across the exec ----------------------

// --- `ApplyConfig`: a Flockfile merged onto a running flock ---
//
// Actor-tier, all but one: a load leaves behind `spec`, `pending` and a pid
// that must not have moved, and no reply reports those three together.

/// The pid the first fixture instance carries. Distinctive, so a case
/// asserting the child was left alone cannot be reading a default.
const APPLY_FIRST_PID: u32 = 7100;

/// An actor over one `Online` instance of each app, plus the recording
/// enforcer its extras are armed against.
///
/// One slot per instance the config declares, ids in order. The extras are
/// real but nothing is armed, since no instance went through
/// `went_online`. Every slot is `Online` with a pid and no `ctl`, so a
/// stopped instance written onto the entry is removed synchronously.
fn actor_over(
    dir: &tempfile::TempDir,
    apps: &[ResolvedApp],
) -> (Actor<ScriptedRunner>, Arc<RecordingEnforcer>) {
    let paths = test_paths(dir);
    let mut sheep = HashMap::new();
    let mut next_id = 0;
    for app in apps {
        for instance in 0..app.config().instances {
            let id = next_id;
            next_id += 1;
            sheep.insert(
                id,
                SheepSlot::new(armed_entry(
                    id,
                    instance,
                    APPLY_FIRST_PID + id,
                    app.clone(),
                    &paths,
                )),
            );
        }
    }
    let enforcer = Arc::new(RecordingEnforcer::default());
    let (breach_tx, _breaches) = mpsc::channel(1);
    let (live_tx, _liveness) = mpsc::channel(1);
    let extras = Extras {
        clock: Arc::new(SystemClock),
        enforcer: Arc::clone(&enforcer) as Arc<dyn LimitEnforcer>,
        max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
        reports: ExtrasReports {
            breaches: breach_tx,
            liveness: live_tx,
        },
        stats: idle_stats(),
    };
    let (tx, _rx) = mpsc::channel(MAILBOX_CAPACITY);
    // Enough scripts for a scale-up to come up: without them a case
    // that scales would assert on a shortfall rather than the apply.
    let scripts = vec![ProcScript::never_exits(); 4];
    let actor = Actor {
        extras: Some(extras),
        ..test_actor(paths, scripts, sheep, tx)
    };
    (actor, enforcer)
}

/// A [`DeclaredApp`] whose document wrote exactly `keys`, plus every key
/// of its own `env` table when `keys` names `env`.
fn declared_app(config: AppConfig, keys: &[&str]) -> DeclaredApp {
    let declared: BTreeSet<String> = keys.iter().map(|key| (*key).to_string()).collect();
    let declared_env = if declared.contains("env") {
        config.env.keys().cloned().collect()
    } else {
        BTreeSet::new()
    };
    DeclaredApp {
        config,
        declared,
        declared_env,
    }
}

/// The override record an earlier load of `keys` would have left, with
/// `fields` set since by an operator.
fn established(
    keys: &[&str],
    fields: Vec<(&str, serde_json::Value)>,
) -> shep_core::overrides::AppOverrides {
    shep_core::overrides::AppOverrides {
        fields: fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        declared: keys.iter().map(|key| (*key).to_string()).collect(),
        declared_env: BTreeSet::new(),
    }
}

/// `handle_command` answers before it returns, so the `await` here
/// resolves rather than hopes.
async fn apply_config(
    actor: &mut Actor<ScriptedRunner>,
    apps: Vec<DeclaredApp>,
    reset: ResetDepth,
) -> Vec<Applied> {
    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::ApplyConfig { apps, reset, reply });
    answer
        .await
        .expect("the actor answers an apply before it returns")
        .expect("the fixture flock is registered")
}

// --- Promotion: a parked config reaching its replacement process ---

// Actor-tier: a promotion moves `spec`, `pending` and `credentials`, and no
// reply reports those three together.
