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
mod dogs;
mod readiness;
mod reload_bus;
mod reload_drain;
mod reload_drainee;
mod reload_swap;
mod reopen;
mod restart_races;
mod scale;
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
    while let Ok(BusEvent::Process { event, .. }) = rx.try_recv().map(|event| event.to_event())
    {
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

    fn spawn(
        &self,
        spec: &SpawnSpec,
    ) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
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

    fn spawn(
        &self,
        spec: &SpawnSpec,
    ) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
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

/// Fails if a flush skips a matched sheep's pump, reaches a sheep the
/// selector never named, or answers with the wrong set.
///
/// The counts are what make this more than a smoke test, as in
/// [`a_reopen_reaches_every_matched_sheep_and_no_others`].
#[tokio::test(start_paused = true)]
async fn a_flush_reaches_every_matched_sheep_and_no_others() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Three scripts for three instances: a fourth spawn would land that
    // sheep pumpless, which reads like the skip being looked for.
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);

    let mut web = AppConfig::minimal("web", "./srv");
    web.instances = 2;
    handle
        .start(vec![
            normalize(web).unwrap(),
            normalize(AppConfig::minimal("api", "./api")).unwrap(),
        ])
        .await
        .unwrap();

    let flushed = handle
        .flush(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0, 1],
        "the reply must carry both `web` instances, id-sorted, and no `api`"
    );
    assert_eq!(runner.flushes(0), 1, "web's first instance");
    assert_eq!(runner.flushes(1), 1, "web's second instance");
    assert_eq!(runner.flushes(2), 0, "api was never named");
    assert_eq!(
        runner.reopens(0),
        0,
        "a flush must push `LogCtl::Flush`, never `LogCtl::Reopen` — a \
         flush wired to the neighbouring variant would swap the flock's \
         handles and empty nothing"
    );
}

/// Fails if the actor awaits a flush's acknowledgement inside its own loop,
/// the cycle
/// [`the_actor_keeps_answering_while_a_reopen_waits_on_a_silent_pump`]
/// describes, reached through the other verb. `list` is the probe: it is
/// answered from the actor loop and nowhere else.
#[tokio::test(start_paused = true)]
async fn the_actor_keeps_answering_while_a_flush_waits_on_a_silent_pump() {
    let (events, _rx) = crate::bus::test_bus(64);
    let (runner, mut requests) = SilentPumpRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let flushing = tokio::spawn({
        let handle = handle.clone();
        async move { handle.flush(ProcessSelector::All).await }
    });

    tokio::time::timeout(Duration::from_secs(5), requests.wait_for(|seen| *seen == 1))
        .await
        .expect("the flush must reach the pump")
        .expect("the runner outlives this wait, so its sender cannot have closed");

    let listed = tokio::time::timeout(Duration::from_secs(5), handle.list())
        .await
        .expect("the actor must keep answering while a flush is outstanding");
    assert_eq!(listed.len(), 1);
    assert!(
        !flushing.is_finished(),
        "sanity: nothing can acknowledge this flush, so `list` answering \
         above is not just the flush having finished first"
    );
    flushing.abort();
}

/// What [`LateWritingPumpRunner`]'s pump appends as it answers a flush. One
/// owner: the cases below assert the file it lands in is empty.
const LATE_LINE: &str = "landed-while-the-flush-was-being-answered\n";

/// The sheep [`LateWritingPumpRunner`] gives a late-writing pump to.
const LATE_WRITING_SHEEP: &str = "latecomer";

/// A [`ScriptedRunner`] whose spawn of [`LATE_WRITING_SHEEP`] gets a pump
/// that appends [`LATE_LINE`] to that sheep's stdout log path as it
/// acknowledges a flush. Every other sheep keeps the fake's own pump.
///
/// A real [`tokio::fs::File`] hands its `write(2)` to the blocking pool, so
/// a `Flush` can arrive with bytes not yet in the file; the acknowledgement
/// is what says they landed.
struct LateWritingPumpRunner {
    inner: ScriptedRunner,
}

impl LateWritingPumpRunner {
    fn new(inner: ScriptedRunner) -> Self {
        Self { inner }
    }
}

impl fmt::Debug for LateWritingPumpRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LateWritingPumpRunner")
            .finish_non_exhaustive()
    }
}

impl ProcessRunner for LateWritingPumpRunner {
    type Proc = crate::fake::FakeProc;

    fn spawn(
        &self,
        spec: &SpawnSpec,
    ) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
        let (proc, mut io) = self.inner.spawn(spec)?;
        if spec.name != LATE_WRITING_SHEEP {
            return Ok((proc, io));
        }
        let (tx, mut rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
        // Replacing the sender drops the fake's own, ending the control
        // task it spawned. This pump answers in its place.
        io.log_ctl = tx;
        let out_file = spec.out_file.clone();
        tokio::spawn(async move {
            while let Some(ctl) = rx.recv().await {
                match ctl {
                    LogCtl::Flush { done } => {
                        if let Some(parent) = out_file.parent() {
                            std::fs::create_dir_all(parent).unwrap();
                        }
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&out_file)
                            .unwrap();
                        std::io::Write::write_all(&mut file, LATE_LINE.as_bytes()).unwrap();
                        // Answered only once the bytes are on disk.
                        let _ = done.send(Ok(()));
                    }
                    LogCtl::Reopen { done } => {
                        let _ = done.send(Ok(()));
                    }
                    // This runner exists for the flush ordering.
                    #[cfg(unix)]
                    LogCtl::ReportFds { done } => {
                        let _ = done.send(CarriedFds::none());
                    }
                    // Nothing to start reading again: no streams are read.
                    #[cfg(unix)]
                    LogCtl::Resume => {}
                }
            }
        });
        Ok((proc, io))
    }
}

/// Fails if the truncate runs before the pump has acknowledged the flush:
/// the file is emptied with a line still in flight, and the line lands at
/// offset 0 afterwards under `O_APPEND`.
///
/// That the file exists proves the pump was flushed at all, since the
/// truncate does not create a missing path; that it is empty proves the
/// truncate came second.
#[tokio::test(start_paused = true)]
async fn a_flush_truncates_only_after_its_pump_has_answered() {
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(
        LateWritingPumpRunner::new(ScriptedRunner::new(vec![ProcScript::never_exits()])),
        test_paths(&dir),
        events,
    );
    handle
        .start(vec![
            normalize(AppConfig::minimal(LATE_WRITING_SHEEP, "./srv")).unwrap(),
        ])
        .await
        .unwrap();

    // Read off the daemon's own snapshot, so the test cannot disagree with
    // the assembler about the path.
    let out_file = PathBuf::from(
        handle.list().await[0]
            .out_file
            .clone()
            .expect("the daemon reports its own resolved log paths"),
    );

    handle.flush(ProcessSelector::All).await.unwrap();

    assert!(
        out_file.exists(),
        "the pump never wrote, so this flush never reached one: {}",
        out_file.display()
    );
    assert_eq!(
        std::fs::read_to_string(&out_file).unwrap(),
        "",
        "a line the pump landed as it answered the flush must not survive \
         the truncate that follows it"
    );
}

/// Fails if a flush leaves a stopped sheep's log file alone. The operation
/// addresses recorded paths, not open handles, and `shep bleats
/// --no-follow` reads a stopped sheep's logs. The fake's control task ends
/// with its proc, so the truncate is reached through the no-pump leg.
#[tokio::test(start_paused = true)]
async fn a_stopped_sheeps_log_file_is_truncated_too() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.autorestart = false;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    handle
        .stop(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();

    let listed = handle.list().await;
    assert_eq!(listed[0].status, ProcStatus::Stopped);
    let out_file = PathBuf::from(listed[0].out_file.clone().unwrap());
    std::fs::create_dir_all(out_file.parent().unwrap()).unwrap();
    std::fs::write(&out_file, "what the sheep logged before it stopped\n").unwrap();

    let flushed =
        tokio::time::timeout(Duration::from_secs(5), handle.flush(ProcessSelector::All))
            .await
            .expect("a flush aimed at a stopped sheep must not wait for an acknowledgement")
            .expect("a stopped sheep has no pump to flush, which is not a failure");

    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].status, ProcStatus::Stopped);
    assert_eq!(
        std::fs::read_to_string(&out_file).unwrap(),
        "",
        "a stopped sheep's log is still readable, so it is still emptied"
    );
}

/// Fails if instances sharing one log path answer with one row per file
/// rather than one per sheep, or leave the shared file unemptied.
///
/// The answer is keyed by sheep, since the selector named sheep; the work
/// is keyed by path, since one truncate empties the file for every handle
/// open on it. The shared path is asserted rather than assumed.
#[tokio::test(start_paused = true)]
async fn instances_sharing_one_log_path_answer_one_row_each() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner =
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);

    let mut web = AppConfig::minimal("web", "./srv");
    web.instances = 2;
    web.merge_logs = true;
    handle.start(vec![normalize(web).unwrap()]).await.unwrap();

    let listed = handle.list().await;
    assert_eq!(
        listed[0].out_file, listed[1].out_file,
        "fixture check: `merge_logs` must really point both instances at \
         one path, or this case proves nothing"
    );
    let shared = PathBuf::from(listed[0].out_file.clone().unwrap());
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    std::fs::write(&shared, "both instances wrote here\n").unwrap();

    let flushed = handle.flush(ProcessSelector::All).await.unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0, 1],
        "one row per sheep, not per file emptied"
    );
    assert_eq!(std::fs::read_to_string(&shared).unwrap(), "");
}

/// Fails if the set of pumps a flush drains is narrowed back to the sheep
/// the selector matched, leaving a sibling sharing that file unflushed.
///
/// [`LateWritingPumpRunner`] is pointed at the unmatched sheep and the
/// shared file is not created up front, so existence proves the sibling's
/// pump was reached and emptiness that the truncate waited.
#[tokio::test(start_paused = true)]
async fn a_sibling_sharing_a_path_is_flushed_even_when_the_selector_skips_it() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Two scripts for two apps of one instance each: a third spawn would
    // land that sheep pumpless, which reads like the skipped pump.
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(
        LateWritingPumpRunner::new(ScriptedRunner::new(vec![
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ])),
        test_paths(&dir),
        events,
    );

    // Two apps pointed at one file rather than one app's two instances
    // under `merge_logs`: the sibling needs a name to aim the pump at.
    let shared = dir.path().join("shared-out.log");
    let mut named = AppConfig::minimal("web", "./srv");
    named.out_file = Some(shared.display().to_string());
    let mut sibling = AppConfig::minimal(LATE_WRITING_SHEEP, "./api");
    sibling.out_file = Some(shared.display().to_string());
    handle
        .start(vec![normalize(named).unwrap(), normalize(sibling).unwrap()])
        .await
        .unwrap();

    let listed = handle.list().await;
    assert_eq!(
        listed[0].out_file, listed[1].out_file,
        "fixture check: both apps must really resolve to one path, or \
         this case proves nothing"
    );

    let flushed = handle
        .flush(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0],
        "the reply answers the selector: the sibling's file was emptied \
         too, but the operator never named the sibling and it is not a \
         row here"
    );
    assert!(
        shared.exists(),
        "the sibling's pump writes as it answers a flush, so a path that \
         is not there means it was never asked: {}",
        shared.display()
    );
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "",
        "a line the unmatched sibling landed as it answered must not \
         survive the truncate of the path it shares"
    );
}

/// Fails if a pump that could not land what it owed is reported as a
/// success. The failure is keyed by path rather than by sheep: see
/// [`SupervisorError::FlushFailed`]. The healthy sheep is the second half:
/// a failure must not stop the rest of the flock being flushed.
#[tokio::test(start_paused = true)]
async fn a_pump_that_could_not_flush_fails_the_request() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Two scripts for two instances: a third spawn would land that sheep
    // pumpless.
    let scripted = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(
        FailingPumpRunner::new(Arc::clone(&scripted)),
        test_paths(&dir),
        events,
    );
    handle
        .start(vec![
            normalize(AppConfig::minimal(REFUSING_SHEEP, "./srv")).unwrap(),
            normalize(AppConfig::minimal("api", "./api")).unwrap(),
        ])
        .await
        .unwrap();

    let error =
        tokio::time::timeout(Duration::from_secs(5), handle.flush(ProcessSelector::All))
            .await
            .expect("a pump that answers must not leave the flush waiting")
            .expect_err("a flush a pump could not carry out must not answer Ok");

    assert_eq!(
        error,
        SupervisorError::FlushFailed(PUMP_REFUSAL.to_string()),
        "the failure must carry the path, and only the path that failed"
    );
    assert_eq!(
        scripted.flushes(1),
        1,
        "the healthy sheep must still have been flushed"
    );
}

/// Fails if a selector that matches nothing is answered as a success.
/// `flush` demands an explicit selector, so a zero exit would tell the
/// operator the logs they named are empty when nothing was touched.
#[tokio::test(start_paused = true)]
async fn a_flush_matching_nothing_is_not_found() {
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(ScriptedRunner::new(vec![]), test_paths(&dir), events);

    assert_eq!(
        handle.flush(ProcessSelector::All).await,
        Err(SupervisorError::NotFound)
    );
}

/// Fails if [`truncate_log`] gains a `create(true)`, or treats a missing
/// path as an error. A log file that is not there is already empty, and a
/// stray empty log at a path a rotator just renamed away is worse.
#[tokio::test]
async fn truncating_a_path_that_is_not_there_creates_nothing_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("never-started-out.log");

    assert_eq!(truncate_log(&missing).await, Ok(()));
    assert!(
        !missing.exists(),
        "a flush must not create the log file it did not find"
    );
}

/// Fails if [`truncate_log`]'s last arm swallows its error: a `_ => Ok(())`
/// beside the `NotFound` one, or a `NotFound` guard widened to every kind.
/// The case above cannot see it, since a missing path answers `Ok`.
///
/// A directory in the log's place fails `open(2)` for writing for every
/// uid, root included, so this cannot pass for the wrong reason.
#[tokio::test]
async fn truncating_a_path_that_is_a_directory_reports_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("web-out.log");
    std::fs::create_dir(&blocked).unwrap();

    let error = truncate_log(&blocked)
        .await
        .expect_err("a path that could not be truncated must not answer Ok");
    assert!(
        error
            .message
            .starts_with(&format!("{}: ", blocked.display())),
        "the failure must name the path it could not empty: {error}"
    );
}

/// Fails if [`truncate_log`] stops opening through [`open_log_path`]: drop
/// the `O_NOFOLLOW` it adds and `shep flush` empties whatever the symlink
/// points at, with the daemon's privileges.
///
/// The target's bytes prove nothing was emptied, the link still being a
/// link proves the open did not replace it, and the message names the
/// symlink rather than `ELOOP`. `#[cfg(unix)]`: `O_NOFOLLOW` and the
/// refusal are both unix-only.
#[cfg(unix)]
#[tokio::test]
async fn truncating_a_symlinked_log_path_refuses_and_leaves_its_target_alone() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("precious.txt");
    let link = dir.path().join("web-out.log");
    std::fs::write(&target, b"do not empty me").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let error = truncate_log(&link)
        .await
        .expect_err("a symlinked log path must not be truncated through");

    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"do not empty me",
        "the symlink's target must still hold every byte it did"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the refusal must leave the symlink itself in place, not replace it"
    );
    assert_eq!(
        error.message,
        format!("{}: {}", link.display(), crate::runner::SYMLINK_REFUSED),
        "the failure must name the path and say the word symlink: {error}"
    );
}

// --- the log plane mid-reload ------------------------------------
//
// A swap's drainee and its replacement derive identical log paths. Both
// cases name the replacement by id, the form that cannot match the drainee.

/// Fails if the set of pumps a flush drains is narrowed back to the sheep
/// the selector matched, leaving a reload's drainee appending to a file
/// being emptied under it. The same mechanism as
/// [`a_sibling_sharing_a_path_is_flushed_even_when_the_selector_skips_it`],
/// reached without configuring anything.
#[tokio::test(start_paused = true)]
async fn a_flush_naming_a_replacement_still_drains_the_drainee_sharing_its_path() {
    let dir = tempfile::tempdir().unwrap();
    // Two scripts: the original spawn and the one replacement. A third
    // would abandon the reload, leaving one entry and no overlap to see.
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    let mid = handle.list().await;
    assert_eq!(
        mid.len(),
        2,
        "fixture check: both halves of the swap must be registered, or \
         there is no shared path to widen to"
    );
    assert_eq!(
        mid[0].out_file, mid[1].out_file,
        "fixture check: one instance slot must really give both entries \
         one out path, or this case proves nothing"
    );
    assert_eq!(
        mid[0].err_file, mid[1].err_file,
        "fixture check: and one err path"
    );

    let flushed = handle.flush(ProcessSelector::Id(1)).await.unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![1],
        "the reply answers the selector: the drainee's pump was drained \
         too, but the operator named only the replacement"
    );
    assert_eq!(
        runner.flushes(0),
        1,
        "the drainee's pump is what this case exists for — it is still \
         holding the file the truncate is about to empty"
    );
    assert_eq!(
        runner.flushes(1),
        1,
        "the replacement's pump, which the selector did name"
    );
}

/// Fails if a reopen is keyed on the selector alone, leaving a reload's
/// drainee holding the inode an external rotator has just renamed. The
/// drainee goes on appending to the archive while the recreated path takes
/// only the replacement's lines. The counts are the whole case.
#[tokio::test(start_paused = true)]
async fn a_reopen_naming_a_replacement_still_reaches_the_drainee_sharing_its_path() {
    let dir = tempfile::tempdir().unwrap();
    // Two scripts, counted, for the reason the flush case above gives.
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    let mid = handle.list().await;
    assert_eq!(
        mid.len(),
        2,
        "fixture check: both halves of the swap must be registered"
    );
    assert_eq!(
        mid[0].out_file, mid[1].out_file,
        "fixture check: one instance slot must really give both entries \
         one out path, or this case proves nothing"
    );
    assert_eq!(
        mid[0].err_file, mid[1].err_file,
        "fixture check: and one err path"
    );

    let reopened = handle.reopen(ProcessSelector::Id(1)).await.unwrap();

    assert_eq!(
        reopened.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![1],
        "the reply answers the selector, the same way `flush`'s does"
    );
    assert_eq!(
        runner.reopens(0),
        1,
        "the drainee's pump is what this case exists for — unasked, it \
         keeps the renamed inode open and goes on filling the archive"
    );
    assert_eq!(
        runner.reopens(1),
        1,
        "the replacement's pump, which the selector did name"
    );
    assert_eq!(
        runner.flushes(0),
        0,
        "a reopen must push `LogCtl::Reopen`, never `LogCtl::Flush` — the \
         neighbouring variant would land the drainee's owed bytes and \
         leave it on the renamed inode regardless"
    );
}

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

// `register_at_rest` records membership and resolves nothing, so the
// identity is unresolved until the first spawn; reading that as "asked for
// nobody" hands the child the daemon's own identity.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_restored_app_restarts_under_its_configured_user() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(own_user_name()));

    let info = actor.register_at_rest(&app);
    assert_eq!(
        actor.sheep[&info.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a membership record is not a `Start`: nothing has looked this app's user up yet"
    );

    actor.respawn(info.id, true);

    let wanted = Credentials {
        uid: nix::unistd::geteuid().as_raw(),
        gid: None,
    };
    assert_eq!(
        actor.runner.spawned_as(0),
        Some(wanted),
        "the restarted child must carry the identity its `user` resolves to; `None` here is \
         the child running as the shepherd, which is the downgrade"
    );
    assert_eq!(
        actor.sheep[&info.id].entry.credentials,
        SpawnIdentity::Resolved(Some(wanted)),
        "resolved once at the first spawn and stored, so no later restart looks it up again"
    );
}

// A `Start` refuses this config outright; reaching a spawn through the
// muster roll must not be a way around that.
#[tokio::test(start_paused = true)]
async fn a_restored_app_whose_user_cannot_be_resolved_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    let info = actor.register_at_rest(&app);
    let after = actor.respawn(info.id, true);

    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may be spawned for an app whose identity could not be resolved"
    );
    assert_eq!(after.status, ProcStatus::Errored);
    assert_eq!(
        actor.sheep[&info.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a failed resolution settles nothing: the next restart must ask again"
    );
}

// The pinned uid below is one no lookup of this app's `user` could produce,
// so a respawn that touched the passwd database again would be seen.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_restart_reuses_a_resolved_identity_rather_than_looking_it_up_again() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _rx) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    let pinned = Credentials {
        uid: 4242,
        gid: Some(4243),
    };
    let entry = &mut actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers id 0")
        .entry;
    entry.spec = app_with("web", |app| app.user = Some(own_user_name()));
    entry.credentials = SpawnIdentity::Resolved(Some(pinned));

    actor.respawn(0, true);

    assert_eq!(
        actor.runner.spawned_as(0),
        Some(pinned),
        "a running app's identity must survive its restart untouched"
    );
}

// `PerApp` is the muster restore and the dog, where nobody reads the
// shepherd's log, so a refused app has to be visible as `Errored` rather
// than missing. `#[cfg(unix)]`: the refusal needs a passwd database.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_start_refused_over_credentials_leaves_an_errored_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    let err = actor
        .do_start(vec![app], None, BatchPolicy::PerApp, &BTreeSet::new())
        .expect_err("an unresolvable user must refuse the start");
    assert!(
        err.to_string().contains(NO_SUCH_USER),
        "the refusal must name the user it could not resolve: {err}"
    );

    assert_eq!(actor.runner.spawn_count(), 0);
    let slot = actor
        .sheep
        .values()
        .next()
        .expect("the refusal must leave a row rather than vanishing");
    assert_eq!(slot.entry.status, ProcStatus::Errored);
    assert_eq!(
        slot.entry.credentials,
        SpawnIdentity::Unresolved,
        "the row must not claim a settled identity: a `restart` would reuse it and bring the \
         sheep up as the shepherd, which is the bug the row was added to make visible"
    );

    // And the row is inert: restarting it meets the same refusal.
    let id = slot.entry.id;
    let after = actor.respawn(id, true);
    assert_eq!(after.status, ProcStatus::Errored);
    assert_eq!(actor.runner.spawn_count(), 0);
}

// One `Errored` row and no others is the half-registered flock
// `AllOrNothing` exists to prevent. `#[cfg(unix)]`: the refusal needs a
// passwd database.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_all_or_nothing_start_refused_over_credentials_registers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    let err = actor
        .do_start(vec![app], None, BatchPolicy::AllOrNothing, &BTreeSet::new())
        .expect_err("an unresolvable user must refuse the start");
    assert!(
        err.to_string().contains(NO_SUCH_USER),
        "the refusal must name the user it could not resolve: {err}"
    );
    assert!(
        actor.sheep.is_empty(),
        "`AllOrNothing` registers nothing at all, this app included"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
}

// An app registered at rest has never resolved its identity, so the scale
// is the first call that asks.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn scaling_an_app_whose_user_will_not_resolve_leaves_the_flock_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 4]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
    let registered = actor.register_at_rest(&app);

    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 3, reply);
    let err = answer
        .await
        .expect("handle_scale answers every call")
        .expect_err("an unresolvable user must refuse the scale");

    let SupervisorError::CannotStart(message) = &err else {
        panic!("expected CannotStart, got {err:?}");
    };
    assert!(
        message.contains("web") && message.contains(NO_SUCH_USER),
        "the refusal must name the app and the user it could not resolve: {message}"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
    assert_eq!(
        actor.sheep.len(),
        1,
        "no instance may be registered by a scale that could not resolve an identity"
    );
    assert_eq!(
        actor.sheep[&registered.id].entry.spec.config().instances,
        1,
        "the stored count must not be written back for a scale that did not happen"
    );
    assert_eq!(
        actor.sheep[&registered.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a failed resolution settles nothing: the next attempt must ask again"
    );
}

// `register_without_spawning` is idempotent: the second restore finds the
// row it made, so nothing transitioned and no event is owed. An emit keyed
// on the row's status cannot tell the two apart, both being `Errored`.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_second_restore_does_not_re_announce_an_errored_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 2]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
    let mut events = actor.events.subscribe();

    for attempt in 1..=2 {
        assert!(
            actor
                .do_start(
                    vec![app.clone()],
                    None,
                    BatchPolicy::PerApp,
                    &BTreeSet::new()
                )
                .is_err(),
            "restore {attempt} must refuse the app"
        );
    }

    assert_eq!(
        actor.sheep.len(),
        1,
        "the second restore must find the row, not add a second one"
    );

    let mut errored = 0;
    while let Ok(event) = events.try_recv().map(|event| event.to_event()) {
        if let BusEvent::Process {
            event: ProcessEventKind::Errored,
            info,
            ..
        } = event
            && info.name == "web"
        {
            errored += 1;
        }
    }
    assert_eq!(
        errored, 1,
        "one Errored event, for the restore that actually registered the row: a second is a \
         transition that did not happen"
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn scaling_up_the_row_a_refused_start_left_meets_the_same_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 4]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    // The row, made the way an unattended boot makes it.
    actor
        .do_start(vec![app], None, BatchPolicy::PerApp, &BTreeSet::new())
        .expect_err("an unresolvable user must refuse the start");
    let id = actor
        .sheep
        .values()
        .next()
        .expect("the refused start leaves a row")
        .entry
        .id;
    assert_eq!(actor.sheep[&id].entry.status, ProcStatus::Errored);

    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 3, reply);
    let err = answer
        .await
        .expect("handle_scale answers every call")
        .expect_err("the row's user still will not resolve");

    assert!(
        matches!(err, SupervisorError::CannotStart(_)),
        "expected CannotStart, got {err:?}"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
    assert_eq!(
        actor.sheep.len(),
        1,
        "no instance may be registered by a scale that could not resolve an identity"
    );
}

// The user is resolvable, so an identity still `Unresolved` afterwards is
// what says nothing asked.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_no_op_scale_asks_no_passwd_question() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 2]);
    let app = app_with("web", |app| app.user = Some(own_user_name()));
    let registered = actor.register_at_rest(&app);

    // `register_at_rest` registers `instance: 0` whatever `instances`
    // says, so `current` is 1 and this is the `Ordering::Equal` arm.
    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 1, reply);
    answer
        .await
        .expect("handle_scale answers every call")
        .expect("scaling to the count an app already has is a no-op");

    assert_eq!(
        actor.sheep[&registered.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a scale that spawns nothing must not resolve an identity: nothing would use it, \
         and asking is what lets the answer refuse the call"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
}

// The user cannot be resolved, so a scale that asks is a scale that fails.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_no_op_scale_survives_a_user_that_will_not_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 2]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
    let registered = actor.register_at_rest(&app);

    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 1, reply);
    let scaled = answer
        .await
        .expect("handle_scale answers every call")
        .expect("a no-op scale must not be refused over an identity it never needed");

    assert_eq!(scaled.instances.len(), 1);
    assert_eq!(actor.runner.spawn_count(), 0);
    assert_eq!(
        actor.sheep[&registered.id].entry.status,
        ProcStatus::Stopped,
        "a no-op must leave the sheep exactly as it found it"
    );
}

// The `Errored` event carries no reason and the deferred reply has no
// per-id error slot, so this log line is all an operator has. `#[test]`
// with a `block_on` inside: `capture_logs` scopes its subscriber to one
// thread and needs a synchronous closure.
#[cfg(unix)]
#[test]
fn a_restart_that_starts_nothing_says_why_in_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // Route one: no script to pop, so the runner refuses the spawn.
    let refused = capture_logs(|| {
        let mut actor = actor_with_an_empty_flock(&dir, Vec::new());
        let info = actor.register_at_rest(&app_with("web", |_| {}));
        rt.block_on(async { actor.respawn(info.id, true) });
    });
    assert!(
        refused.contains("script exhausted"),
        "the runner's own reason must reach the log: {refused}"
    );

    // Route two: the identity cannot be resolved, so no spawn is
    // attempted at all.
    let unresolved = capture_logs(|| {
        let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
        let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
        let info = actor.register_at_rest(&app);
        rt.block_on(async { actor.respawn(info.id, true) });
    });
    assert!(
        unresolved.contains(NO_SUCH_USER),
        "the unresolvable user must reach the log by name: {unresolved}"
    );
}

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

#[derive(Debug, Clone, Copy)]
enum Step {
    List,
    StopAll,
    RestartAll,
    DeleteFirst,
    StartOne,
    /// A memory breach or a liveness failure raised against the pid the
    /// first listed sheep is running right now.
    Report,
    /// The same report, raised against a pid that sheep does not have.
    StaleReport,
}

fn step_strategy() -> impl proptest::strategy::Strategy<Value = Step> {
    proptest::prop_oneof![
        proptest::strategy::Just(Step::List),
        proptest::strategy::Just(Step::StopAll),
        proptest::strategy::Just(Step::RestartAll),
        proptest::strategy::Just(Step::DeleteFirst),
        proptest::strategy::Just(Step::StartOne),
        proptest::strategy::Just(Step::Report),
        proptest::strategy::Just(Step::StaleReport),
    ]
}

/// How a generated app gates its own `starting -> online` transition.
#[derive(Debug, Clone, Copy)]
enum Gate {
    /// Neither `wait_ready` nor `readiness_probe`: `spawn_fresh` marks the
    /// sheep `Online` inline.
    Ungated,
    /// `wait_ready = true` with this `listen_timeout` in milliseconds. No
    /// scripted child writes `{"kind":"ready"}`, so every wait ends at its
    /// deadline, and the deadline decides which later step lands under it.
    Channel(u64),
}

fn gate_strategy() -> impl proptest::strategy::Strategy<Value = Gate> {
    use proptest::strategy::Strategy as _; // `prop_map` below
    proptest::prop_oneof![
        2 => proptest::strategy::Just(Gate::Ungated),
        // Spans the driver's own durations, a 1600ms kill ladder and a
        // 2000ms `stable_then_exit`, so a deadline drawn here lands
        // before, during and after them.
        1 => (1u64..4_000u64).prop_map(Gate::Channel),
    ]
}

fn script_strategy() -> impl proptest::strategy::Strategy<Value = ProcScript> {
    // Weighted toward long-lived children so a run explores command
    // handling rather than only exhausting the restart budget.
    proptest::prop_oneof![
        6 => proptest::strategy::Just(ProcScript::never_exits()),
        2 => proptest::strategy::Just(ProcScript::const_exit(1)),
        1 => proptest::strategy::Just(ProcScript::stable_then_exit(2_000, 0)),
        1 => proptest::strategy::Just(ProcScript::ignores_signals()),
    ]
}

/// How many scripted procs one generated case may spawn.
///
/// A 9-command run reaches at most 30 command-driven spawns plus
/// crash-loop respawns capped at 16 per sheep: under 200 all told. An
/// exhausted `ScriptedRunner` answers `SpawnFailed`, which the actor turns
/// into `Errored`, so a claim about a restart that must not happen would
/// pass for the wrong reason. Still finite: a `stable_then_exit` script
/// resets the restart budget, and the pool running dry ends the chain.
const SCRIPT_POOL: usize = 512;

/// How long the steady-state drain waits for one more transition before
/// concluding there are none left.
///
/// Longer than every deadline a run can leave pending (a 4000ms readiness
/// wait, a 1600ms kill ladder, a 2000ms `stable_then_exit` script) and far
/// shorter than `fake::NEVER_MS`, so a `never_exits` proc stays alive
/// across it.
const QUIET_WINDOW: Duration = Duration::from_secs(60);

/// Ceiling on transitions observed after the last command. Each spawn
/// produces at most a start/restart, an online and a terminal event, so
/// `3 * SCRIPT_POOL` bounds a correct run; anything past this ceiling is
/// a flock that never settles.
const EVENT_BUDGET: usize = 3 * SCRIPT_POOL;

proptest::proptest! {
    // 128 cases: 24 misses the 3-step `[StartOne, StopAll, DeleteFirst]`
    // sequence that equally-weighted draws rarely land. ~0.6s under the
    // paused clock. `PROPTEST_CASES` overrides.
    #![proptest_config(crate::testing::proptest_config(128))]

    #[test]
    fn supervisor_upholds_its_invariants_under_any_interleaving(
        steps in proptest::collection::vec(step_strategy(), 1..10),
        gates in proptest::collection::vec(gate_strategy(), 1..10),
        scripts in proptest::collection::vec(script_strategy(), SCRIPT_POOL..SCRIPT_POOL + 1),
    ) {
        // Paused clock: every backoff, kill ladder and readiness delay is
        // virtual, so a 128-case run stays cheap whatever it draws.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        runtime.block_on(async move {
            // Capacity above `EVENT_BUDGET`: the drain below treats a
            // `Lagged` as a failure rather than skipping past it, since a
            // hole in the stream is a hole in every claim read off it.
            let (events, mut rx) = crate::bus::test_bus(8192);
            let handle = spawn_supervisor(
                ScriptedRunner::new(scripts),
                test_paths(&dir),
                events,
            );
            let mut started = 0u32;
            let mut highest_restarts = std::collections::HashMap::<u32, u32>::new();
            // `extra_restart` is the one command with no reply, so it is
            // the only way a restart is still mid-kill-ladder when the next
            // step is issued. `Step::StopAll` keeps its claim across that:
            // `claim_manual` takes the `manual` marker off it.

            for step in steps {
                match step {
                    Step::StartOne => {
                        let gate = gates[started as usize % gates.len()];
                        started += 1;
                        let mut app = AppConfig::minimal(&format!("sheep-{started}"), "./s");
                        if let Gate::Channel(ms) = gate {
                            app.wait_ready = true;
                            app.listen_timeout = UpDuration::from_millis(ms);
                        }
                        let _ = handle.start(vec![normalize(app).unwrap()]).await;
                    }
                    Step::StopAll => {
                        if let Ok(stopped) = handle.stop(ProcessSelector::All).await {
                            for info in stopped {
                                // A deferred reply means every match is
                                // terminal.
                                proptest::prop_assert_eq!(info.status, ProcStatus::Stopped);
                            }
                        }
                    }
                    Step::RestartAll => {
                        let _ = handle.restart(ProcessSelector::All).await;
                    }
                    Step::DeleteFirst => {
                        if let Some(first) = handle.list().await.first() {
                            let id = first.id;
                            if let Ok(deleted) = handle.delete(ProcessSelector::Id(id)).await {
                                proptest::prop_assert_eq!(deleted, vec![id]);
                            }
                            proptest::prop_assert!(
                                handle.list().await.iter().all(|i| i.id != id)
                            );
                        }
                    }
                    Step::Report => {
                        if let Some(first) = handle.list().await.first()
                            && let Some(pid) = first.pid
                        {
                            handle.extra_restart(first.id, pid, None, None).await;
                        }
                    }
                    Step::StaleReport => {
                        if let Some(first) = handle.list().await.first() {
                            // Never this sheep's own pid. A pid belonging
                            // to another sheep is just as stale: the guard
                            // compares against this id's entry.
                            let stale = first.pid.unwrap_or(0).wrapping_add(1);
                            handle.extra_restart(first.id, stale, None, None).await;
                        }
                    }
                    Step::List => {}
                }

                let listed = handle.list().await;
                // (1) ids are unique and the listing is sorted by id.
                let ids: Vec<u32> = listed.iter().map(|i| i.id).collect();
                let mut sorted = ids.clone();
                sorted.sort_unstable();
                sorted.dedup();
                proptest::prop_assert_eq!(&ids, &sorted);
                for info in &listed {
                    // (2) restart counts never decrease for a given id.
                    let seen = highest_restarts.entry(info.id).or_default();
                    proptest::prop_assert!(info.restarts >= *seen);
                    *seen = info.restarts;
                    // (3) no status outside the spec's set ever surfaces.
                    proptest::prop_assert!(matches!(
                        info.status,
                        ProcStatus::Starting | ProcStatus::Online | ProcStatus::Stopping
                            | ProcStatus::Stopped | ProcStatus::Errored | ProcStatus::WaitingRestart
                    ));
                }
            }

            // (4) steady state: with no further commands, the flock stops
            // transitioning. A bounded window, not `try_recv`: a run ends
            // with deadlines still pending, and the window walks the
            // paused clock over them.
            let mut observed = Vec::new();
            loop {
                match tokio::time::timeout(QUIET_WINDOW, rx.recv()).await {
                    Ok(Ok(event)) => {
                        observed.push(event.to_event());
                        proptest::prop_assert!(
                            observed.len() <= EVENT_BUDGET,
                            "the flock never reached steady state: {} transitions after \
                             the last command",
                            observed.len()
                        );
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped))) => {
                        return Err(proptest::test_runner::TestCaseError::fail(format!(
                            "event stream lagged by {skipped}: the invariants below cannot \
                             be read off a stream with holes in it"
                        )));
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                    Err(_elapsed) => break, // nothing left to transition
                }
            }

            // (5) never two live processes for one id, and never an
            // `Online` for an id with no live process. `spawn_fresh` never
            // reuses an id, and `handle_ready_result` resolves long after
            // the spawn it belongs to.
            let mut live = std::collections::HashSet::<u32>::new();
            let mut event_restarts = std::collections::HashMap::<u32, u32>::new();
            for event in observed {
                let BusEvent::Process { event, info, .. } = event else {
                    // LogOut/LogErr carry no lifecycle transition.
                    continue;
                };
                match event {
                    ProcessEventKind::Start => {
                        proptest::prop_assert!(
                            live.insert(info.id),
                            "two live spawns for id {}",
                            info.id
                        );
                    }
                    // One out and one in: the predecessor's `Msg::Exited`
                    // is what caused the respawn, so the id stays live.
                    ProcessEventKind::Restart => {
                        live.insert(info.id);
                    }
                    ProcessEventKind::Online => {
                        proptest::prop_assert!(
                            live.contains(&info.id),
                            "id {} was marked online with no live process: a readiness \
                             wait resolved onto a sheep that had already gone terminal",
                            info.id
                        );
                    }
                    ProcessEventKind::Exit
                    | ProcessEventKind::Stop
                    | ProcessEventKind::Errored
                    | ProcessEventKind::Delete => {
                        live.remove(&info.id);
                    }
                    // `ProcessEventKind` is `#[non_exhaustive]`, so E0004
                    // never fires here. Leaving `live` untouched for a
                    // later variant only makes the assertions stricter.
                    _ => {}
                }
                // (2) again, off the event stream rather than `list()`: a
                // snapshot only sees the counter between two commands.
                let seen = event_restarts.entry(info.id).or_default();
                proptest::prop_assert!(
                    info.restarts >= *seen,
                    "restart count for id {} went backwards: {} after {}",
                    info.id,
                    info.restarts,
                    *seen
                );
                *seen = info.restarts;
            }
            // The async block's error type is proptest's, so `?` above and
            // this tail agree.
            Ok::<(), proptest::test_runner::TestCaseError>(())
        })?;
    }
}

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

/// Whether `fd` names something open in this process.
#[cfg(unix)]
fn is_open(fd: std::os::fd::RawFd) -> bool {
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).is_ok()
}

/// One of the two sheep has a shepherd channel and the other does not. The
/// channel is the one number a snapshot may drop, so a case with only
/// channelled sheep could not tell "every number is open" from "every
/// number is present".
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_blob_from_a_live_flock_names_open_descriptors_per_sheep() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner =
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut talkative = AppConfig::minimal("api", "./srv");
    talkative.channel = true;
    handle
        .start(vec![
            normalize(AppConfig::minimal("web", "./srv")).unwrap(),
            normalize(talkative).unwrap(),
        ])
        .await
        .unwrap();

    let (candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    assert_eq!(candidates.len(), 2, "both sheep must reach the gate");
    assert_eq!(blob.sheep().len(), 2);
    for sheep in blob.sheep() {
        for fd in sheep.fds().all().into_iter().flatten() {
            assert!(is_open(fd), "blob names a closed descriptor: {fd}");
        }
    }
}

/// The pump reports the number it was told at the spawn and cannot learn
/// the channel is gone, so the number can name whatever the kernel has
/// since handed to the next `open` and the successor would write a
/// shepherd message into a log file. `SheepSlot::open_channel` decides
/// delivery, and the snapshot masks the field with it. The fake reports a
/// number for every field, so without the mask this case sees one.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_snapshot_names_no_channel_for_a_sheep_that_has_none() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner =
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut talkative = AppConfig::minimal("api", "./srv");
    talkative.channel = true;
    handle
        .start(vec![
            normalize(AppConfig::minimal("web", "./srv")).unwrap(),
            normalize(talkative).unwrap(),
        ])
        .await
        .unwrap();

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let named: Vec<(&str, bool)> = blob
        .sheep()
        .iter()
        .map(|sheep| (sheep.name(), sheep.fds().channel.is_some()))
        .collect();
    assert!(
        named.contains(&("web", false)),
        "a sheep with no channel must carry no channel descriptor: {named:?}"
    );
    assert!(
        named.contains(&("api", true)),
        "a sheep with a live channel must carry its descriptor: {named:?}"
    );
}

/// A pump that never answers has no deadline above it but this one: the
/// SIGHUP path awaits the snapshot before it can fall back, so one stall
/// takes out the handover and the graceful stop with it. An empty
/// `CarriedFds` is what a stopped sheep reports, so collapsing a wedged
/// live pump into one would pass the gate with descriptors dropped. Two
/// sheep, one of each kind, so a gate that refused every flock with a pump
/// in it would pass here for the wrong reason.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_pump_that_never_reports_refuses_the_snapshot_instead_of_hanging() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 2])
        .with_a_pump_that_never_reports(&["wedged"]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(vec![
            normalize(AppConfig::minimal("answering", "./srv")).unwrap(),
            normalize(AppConfig::minimal("wedged", "./srv")).unwrap(),
        ])
        .await
        .unwrap();

    // Far longer than the deadline under test, and never actually waited:
    // it buys a failure rather than a hung suite if the snapshot has no
    // deadline of its own.
    let snapshot =
        tokio::time::timeout(Duration::from_secs(3600), handle.handover_snapshot(fds))
            .await
            .expect("a snapshot over a wedged pump must answer rather than hang")
            .unwrap();
    let (candidates, _blob, _parked) = snapshot;

    let borrowed: Vec<crate::handover::Candidate<'_>> = candidates
        .iter()
        .map(crate::handover::OwnedCandidate::as_candidate)
        .collect();
    assert_eq!(
        crate::handover::fitness(&borrowed),
        crate::handover::Fitness::Refused(crate::handover::RefusedReason::PumpUnresponsive {
            sheep: "wedged".to_string(),
        }),
        "the gate must refuse, and must name the sheep whose pump went quiet"
    );
    for candidate in &candidates {
        assert_eq!(
            candidate.pump_unresponsive,
            candidate.entry.spec.config().name == "wedged",
            "exactly the wedged sheep is unresponsive, not its neighbour"
        );
    }
}

/// A report parks the pump that answers it, and only that one: a pump that
/// missed the deadline is still reading its streams. An abandoned handover
/// is audited by the resume counter, so a spurious resume reads as a
/// repaired pump forever after. Asserted by sending the resumes, so this
/// is a claim about which pump is told, not how many senders are held.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_pump_that_missed_the_deadline_is_not_in_the_parked_set() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(
        ScriptedRunner::new(vec![ProcScript::never_exits(); 2])
            .with_a_pump_that_never_reports(&["wedged"]),
    );
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    handle
        .start(vec![
            normalize(AppConfig::minimal("answering", "./srv")).unwrap(),
            normalize(AppConfig::minimal("wedged", "./srv")).unwrap(),
        ])
        .await
        .unwrap();

    let (_candidates, _blob, parked) =
        tokio::time::timeout(Duration::from_secs(3600), handle.handover_snapshot(fds))
            .await
            .expect("a snapshot over a wedged pump must answer rather than hang")
            .unwrap();
    parked.resume().await;

    // By name, because spawn order is the supervisor's business and
    // every counter here is indexed by it.
    let answering = runner.spawn_index_of("answering").expect("started above");
    let wedged = runner.spawn_index_of("wedged").expect("started above");
    // A resume carries no acknowledgement, so a send that has returned
    // has only been queued. Bounded, and instant under the paused clock.
    let delivered = async {
        while runner.resumes(answering) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), delivered)
        .await
        .expect("the pump that answered was parked, and is owed a resume");
    assert_eq!(
        runner.resumes(wedged),
        0,
        "a pump that never answered never parked, so nothing may resume it"
    );
}

/// Six wedged pumps is the size this test pins, and the bound it asserts
/// is about the shape rather than about any one budget: a serial sweep
/// scales with N and a concurrent one does not. Under the paused clock a
/// serial sweep reads six [`REPORT_DEADLINE`]s (12s) and a concurrent one
/// about one (2s); the bound below sits in that gap. Six was also where a
/// serial sweep first outlasted `shep-cli`'s `admin::KILL_TEARDOWN_WAIT`
/// while that was 10s, past which the client gives up first, falls back
/// to a predecessor still serving, and exits 0 before the gate refuses
/// for real. At 60s that crossing moved to thirty; the failure it names
/// did not go away, it got further off.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_sweep_of_six_wedged_pumps_costs_one_deadline_not_six() {
    const WEDGED: usize = 6;
    let names: Vec<String> = (0..WEDGED).map(|i| format!("wedged{i}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); WEDGED])
        .with_a_pump_that_never_reports(&name_refs);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(
            names
                .iter()
                .map(|name| normalize(AppConfig::minimal(name, "./srv")).unwrap())
                .collect(),
        )
        .await
        .unwrap();

    let started_at = tokio::time::Instant::now();
    let (candidates, blob, _parked) =
        tokio::time::timeout(Duration::from_secs(3600), handle.handover_snapshot(fds))
            .await
            .expect("a snapshot over six wedged pumps must answer rather than hang")
            .unwrap();
    let elapsed = started_at.elapsed();

    assert!(
        elapsed < REPORT_DEADLINE * 2,
        "six wedged pumps swept concurrently should cost about one \
         REPORT_DEADLINE, not six; took {elapsed:?}"
    );

    assert_eq!(candidates.len(), WEDGED);
    for candidate in &candidates {
        assert!(
            candidate.pump_unresponsive,
            "{} must still be reported unresponsive; racing the reports \
             must not blur which pump answered",
            candidate.entry.spec.config().name
        );
    }
    // `join_all` returns in input order and `handle_handover_snapshot`
    // sorted `drafts` by id, so `spawn_handover_task` needs no re-sort.
    assert!(
        candidates.windows(2).all(|w| w[0].entry.id < w[1].entry.id),
        "candidates must stay in id order across a concurrent sweep"
    );
    assert!(
        blob.sheep().windows(2).all(|w| w[0].id() < w[1].id()),
        "the blob must stay in id order too: it is a file an operator may read"
    );
}

/// A successor that reissued a live id would collide with a caller still
/// holding it, and a manual command the successor never sees is an
/// operator's `stop` that comes back as a running sheep. The manual marker
/// is asserted whole, since kind and origin decide different things on the
/// far side. The sheep has no live pump, which is also the
/// registered-but-not-running case: no descriptors, and not a refusal.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_snapshot_carries_the_actors_counters_and_slot_state() {
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let (mut actor, _ctl_rx) = actor_with_stopping_drainee(&dir, 4242, 7);
    actor.sheep.get_mut(&0).unwrap().manual = Some(PendingManual {
        kind: ManualKind::Stop,
        origin: CommandOrigin::Operator,
    });
    actor.sheep.get_mut(&0).unwrap().ready_failed = true;

    let (reply, rx) = oneshot::channel();
    actor.handle_handover_snapshot(fds, reply);
    let (candidates, blob, _parked) = rx.await.unwrap().unwrap();

    assert!(
        !candidates.is_empty(),
        "the flock must still reach the gate at all"
    );
    assert_eq!(
        blob.sheep()[0].manual(),
        Some(PendingManual {
            kind: ManualKind::Stop,
            origin: CommandOrigin::Operator,
        }),
        "a manual stop must reach the successor, kind and origin both"
    );
    assert_eq!(
        blob.sheep()[0].pending_delete(),
        Some(false),
        "nothing asked for this sheep to be deleted"
    );
    assert_eq!(
        blob.sheep()[0].ready_failed(),
        Some(true),
        "an earlier reload's failed verdict must reach the successor, or the rollback that \
         follows it has nothing left to replace"
    );
    assert!(
        blob.next_id() > 0,
        "a successor that reissues a live id collides"
    );
    assert_eq!(blob.sheep()[0].epoch(), 7, "a stale timer must stay stale");
    assert_eq!(
        blob.sheep()[0].fds(),
        CarriedFds::none(),
        "a sheep with no pump has no descriptors to carry"
    );
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

    fn adopt(
        &self,
        spec: crate::runner::AdoptSpec,
    ) -> Result<(Self::Proc, ProcIo), RunnerError> {
        Ok((StandInProc { pid: spec.pid }, stand_in_io()))
    }
}

/// The wait that would resolve it belonged to a task of the predecessor's,
/// which the `execve` took. Nothing else moves a sheep off `Starting`
/// except its own exit, so a successor that adopts one without re-arming
/// leaves it there with none of `arm_extras`'s watch, cron or memory
/// limits, which fire at the `Online` transition. Nothing here writes
/// `{"kind":"ready"}` and `handle_ready_result` puts a timed-out sheep
/// `Online` anyway, so reaching `Online` at all proves a wait was armed.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_that_was_still_starting_reaches_online() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |entry| {
                let mut app = AppConfig::minimal("web", "./srv");
                app.autorestart = false;
                app.wait_ready = true;
                entry.spec = normalize(app).unwrap();
                entry.status = ProcStatus::Starting;
            }))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    assert_eq!(
        sup.list().await[0].status,
        ProcStatus::Starting,
        "the blob said this sheep was still starting, so the successor must agree"
    );

    // Polled, not advanced by a fixed amount: the wait runs on a task, so
    // its result reaches the actor a message later than the deadline. The
    // bound sits past the app's 3s `listen_timeout`.
    let goes_online = async {
        while sup.list().await[0].status != ProcStatus::Online {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), goes_online)
        .await
        .expect("an adopted sheep left `Starting` must still have a readiness wait over it");
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

/// Every row of a listing by id, which is the order these cases name
/// their sheep in.
#[cfg(unix)]
fn by_id(mut info: Vec<ProcessInfo>) -> Vec<ProcessInfo> {
    info.sort_unstable_by_key(|row| row.id);
    info
}

/// A sheep whose pid moved was respawned.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_flock_keeps_its_pids_and_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried("web", 7, Some(4242), |_| {})),
                without_handles(carried("api", 8, Some(4243), |_| {})),
            ],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = by_id(sup.list().await);

    assert_eq!(
        info.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![7, 8],
        "the successor reissued ids instead of carrying them"
    );
    assert_eq!(
        info.iter().map(|row| row.pid).collect::<Vec<_>>(),
        vec![Some(4242), Some(4243)],
        "a sheep whose pid moved was respawned"
    );
}

/// Losing the marker is invisible to a pid check: the dog runs on. What
/// changes is that `matching_ids` stops passing it by, so `shep restart
/// all` reaches it and `shep dogs` loses it entirely.
///
/// A `stop` over a kennel with no sheep in it, so nothing waits on a kill
/// ladder and `NotFound` answers the wildcard. With the marker dropped the
/// wildcard matches the dog and parks on an exit `StandInProc` never
/// delivers, so the timeout turns a hung suite into a failure.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_dog_keeps_its_marker_and_stays_out_of_a_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried(
                "log-rotate",
                7,
                Some(4242),
                |entry| {
                    entry.dog = Some(DogSource::Adopted {
                        path: "/opt/bin/shep-log-rotate".to_string(),
                    });
                },
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = sup.list().await;
    assert_eq!(
        info[0].dog,
        Some(DogSource::Adopted {
            path: "/opt/bin/shep-log-rotate".to_string(),
        }),
        "a dog adopted as an ordinary sheep is one `shep dogs` has lost"
    );

    let swept = tokio::time::timeout(Duration::from_secs(5), sup.stop(ProcessSelector::All))
        .await
        .expect("a wildcard over a flock of nothing but dogs must answer rather than hang");
    assert!(
        matches!(swept, Err(SupervisorError::NotFound)),
        "`all` is the flock, not the kennel: {swept:?}"
    );
}

/// Losing these is silent. `restarts` resetting to zero hands a
/// crash-looping app amnesty it did not earn, and a lost `last_exit`
/// answers "why did it stop" with nothing.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_keeps_its_counters_and_last_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |entry| {
                entry.restarts = 4;
                entry.last_exit = Some(ExitInfo {
                    code: Some(2),
                    signal: None,
                });
            }))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = sup.list().await;

    assert_eq!(info[0].restarts, 4, "the restart count was reset");
    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: Some(2),
            signal: None
        }),
        "the last exit was lost"
    );
}

/// An adopted sheep that exits must reach `handle_exited` and be judged by
/// `decide_on_exit` like any other, not sit there `Online` forever.
///
/// A real child and the real runner, since a scripted exit would prove the
/// fake rather than the targeted `waitpid` an adopted pid needs. `/bin/sh`
/// is spawned by `std::process::Command`, so tokio holds no `Child` for it
/// and nothing but the reaper waits on it.
#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the adopted flock's reaper collects this status; a Child::wait would take it first"
)]
async fn an_adopted_sheeps_exit_flows_through_the_ordinary_path() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("a test host can spawn a shell");
    let pid = child.id();
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(pid), |_| {}))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("the adopted child is signalable");

    let info = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = sup.list().await;
            if info[0].status == ProcStatus::Stopped {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("an adopted sheep's exit reaches the actor");

    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(9)
        }),
        "the exit must be recorded, not lost"
    );
}

/// [`handover::fitness`] carries a pending delete rather than refusing it,
/// so a sheep whose delete was in flight when the predecessor exec'd must
/// still be deregistered on its next exit, not left registered or
/// respawned. A real child and the real runner: an adopted pid has no
/// `Child` handle, and only the reaper's `Msg::Exited` proves the carried
/// marker reaches `handle_exited`.
#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the adopted flock's reaper collects this status; a Child::wait would take it first"
)]
async fn a_carried_pending_delete_deregisters_on_the_next_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("a test host can spawn a shell");
    let pid = child.id();
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_marked(
                "web",
                7,
                Some(pid),
                true,
                None,
                false,
                |_| {},
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("the adopted child is signalable");

    let info = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = sup.list().await;
            if info.is_empty() {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect(
        "a carried pending delete must deregister the sheep on its next exit, not merely \
         stop it",
    );

    assert!(
        info.is_empty(),
        "the deleted sheep must be gone, not just stopped"
    );
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

/// The ladder ran inside the predecessor's sheep task and the `execve`
/// took it, so the sheep reaching a terminal status is what proves the
/// re-arm. `decide_on_exit` reads the marker as `manual_stop`, and this
/// app has `autorestart` on, so a ladder armed without it would kill the
/// sheep and then respawn it. A real child and the real runner: only the
/// reaper's `Msg::Exited` proves a carried marker reaches `handle_exited`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_manual_stop_stops_the_sheep_instead_of_respawning_it() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_marked(
                "web",
                7,
                Some(pid),
                false,
                Some(PendingManual {
                    kind: ManualKind::Stop,
                    origin: CommandOrigin::Operator,
                }),
                false,
                respawnable(|app| app.autorestart = true),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = flock_until(
        &sup,
        |info| info[0].status == ProcStatus::Stopped,
        "a carried manual stop must still stop the sheep after the exec",
    )
    .await;

    assert_eq!(
        info[0].restarts, 0,
        "a stop must not come back as a respawn"
    );
    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(15)
        }),
        "the re-armed ladder starts at its polite rung, not at SIGKILL"
    );
    assert_eq!(info[0].pid, None, "a stopped sheep holds no pid");
}

/// The escalation is a `tokio::time::timeout` inside `kill_process`, which
/// runs on the sheep task the `execve` takes. A successor that re-sends
/// only the polite signal leaves a child that traps `SIGTERM` running
/// forever. The signal in `last_exit` is the whole assertion: a pid check
/// cannot tell a ladder that escalated from one that got lucky.
/// `kill_timeout` is shortened to a second, well inside [`flock_until`].
#[cfg(unix)]
#[tokio::test]
async fn a_carried_manual_stop_still_escalates_to_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    // A loop, not a bare `sleep`: the polite rung reaches the whole group,
    // so a shell waiting on one `sleep` would exit carrying that sleep's
    // status and look like it had obeyed. The touchfile closes the startup
    // race, since a `SIGTERM` arriving before `trap` runs kills the shell.
    let armed = dir.path().join("trap-armed");
    // Bounded, so a case that fails before the ladder reaches it still
    // leaves a child that goes away inside `flock_until`'s bound.
    let pid = adoptable_child(&format!(
        "trap '' TERM; : > {}; i=0; while [ $i -lt 60 ]; do sleep 1; i=$((i+1)); done",
        armed.display()
    ));
    tokio::time::timeout(Duration::from_secs(20), async {
        while !armed.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the defiant child must arm its trap before anything signals it");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_marked(
                "web",
                7,
                Some(pid),
                false,
                Some(PendingManual {
                    kind: ManualKind::Stop,
                    origin: CommandOrigin::Operator,
                }),
                false,
                respawnable(|app| {
                    app.autorestart = false;
                    app.kill_timeout = "1000".parse().unwrap();
                }),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = flock_until(
        &sup,
        |info| info[0].status == ProcStatus::Stopped,
        "a sheep carried mid-ladder must still die on its own, without a hand-sent SIGKILL",
    )
    .await;

    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(9)
        }),
        "the ladder must escalate: this child ignores SIGTERM, so anything else means it \
         was never killed"
    );
}

/// Kind and origin are two separate facts on one marker and neither may be
/// defaulted: a hardcoded `Stop` leaves the sheep down when an operator
/// asked for it back, and a hardcoded `Operator` broadcasts a memory
/// breach as a user action. `Automatic` here, so the flag can come back
/// `false`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_manual_restart_respawns_and_keeps_its_origin() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_marked(
                "web",
                7,
                Some(pid),
                false,
                Some(PendingManual {
                    kind: ManualKind::Restart,
                    origin: CommandOrigin::Automatic,
                }),
                false,
                // Off, so the respawn below can only be the carried
                // `Restart`: an `autorestart` app would come back anyway.
                respawnable(|app| app.autorestart = false),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let manually = tokio::time::timeout(
        Duration::from_secs(20),
        await_event(&mut rx, 7, ProcessEventKind::Restart),
    )
    .await
    .expect("a carried manual restart must respawn the sheep after the exec");
    assert!(
        !manually,
        "an automatic restart carried across a handover must not be broadcast as a user \
         action"
    );

    let info = flock_until(
        &sup,
        |info| info[0].restarts == 1,
        "the respawn must be counted against the sheep that was carried",
    )
    .await;
    assert_ne!(
        info[0].pid,
        Some(pid),
        "a restart is a new process, not the adopted one"
    );
    assert_eq!(
        info[0].status,
        ProcStatus::Online,
        "an ungated app is Online the moment it respawns"
    );

    // The respawn is a real `sleep 30`; the shutdown stops it being left
    // behind when the tempdir goes.
    sup.shutdown().await;
}

/// Counters are restored before any slot is installed. A successor that
/// starts `next_id` at zero hands a new sheep an id the predecessor
/// already gave out.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_successor_does_not_reissue_a_live_id() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |_| {}))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let fresh = sup
        .start(vec![normalize(AppConfig::minimal("api", "./srv")).unwrap()])
        .await
        .expect("a fresh app starts under a successor");

    assert!(fresh[0].id >= 9, "reissued a live id: {}", fresh[0].id);
}

/// `schedule_restart`'s timer task dies with the process image, and
/// `handle_restart_due` is the only thing that moves a sheep off
/// [`ProcStatus::WaitingRestart`]. The pid is the assertion, not the
/// status: [`STAND_IN_SPAWN_PID`] is what [`AdoptingRunner`] hands a fresh
/// spawn and is no carried sheep's pid, so a row reporting it can only
/// have been respawned by the re-armed timer.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_owed_a_restart_still_gets_one() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, None, |entry| {
                entry.status = ProcStatus::WaitingRestart;
            }))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    // Virtual, and instant: the paused clock advances itself once every
    // task is idle, so this waits out the re-armed backoff for free.
    tokio::time::sleep(Duration::from_secs(5)).await;

    let info = sup.list().await;
    assert_eq!(
        info[0].status,
        ProcStatus::Online,
        "a sheep owed a respawn was left waiting for a timer that died with the exec: {info:?}"
    );
    assert_eq!(
        info[0].pid,
        Some(STAND_IN_SPAWN_PID),
        "the restart must be a real respawn, not a status edit: {info:?}"
    );
}

/// Both sheep are paced by a fixed hour and owed a respawn. One carries a
/// due moment two seconds out, the other carries none, and they share one
/// paused clock and one advance: only the pair tells "it waited out what
/// was left" from "it did not wait at all". Ten virtual seconds is five
/// times the carried deadline and a fraction of the hour, so each status
/// after it can only have come from one of the two behaviours.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_waits_out_only_what_is_left_of_its_delay() {
    let hour = "1h".parse().unwrap();
    let anchored_dir = tempfile::tempdir().unwrap();
    let (anchored_events, _anchored_rx) = crate::bus::test_bus(64);
    let anchored =
        SupervisorBuilder::new(AdoptingRunner, test_paths(&anchored_dir), anchored_events)
            .spawn_adopted(
                vec![without_handles(carried_owed_a_restart(
                    "web",
                    7,
                    hour,
                    Some(SystemTime::now() + Duration::from_secs(2)),
                ))],
                counters(9),
                Vec::new(),
            )
            .expect("a carried flock installs");

    let control_dir = tempfile::tempdir().unwrap();
    let (control_events, _control_rx) = crate::bus::test_bus(64);
    let control =
        SupervisorBuilder::new(AdoptingRunner, test_paths(&control_dir), control_events)
            .spawn_adopted(
                vec![without_handles(carried_owed_a_restart(
                    "web", 7, hour, None,
                ))],
                counters(9),
                Vec::new(),
            )
            .expect("a carried flock installs");

    tokio::time::sleep(Duration::from_secs(10)).await;

    let anchored = anchored.list().await;
    assert_eq!(
        anchored[0].status,
        ProcStatus::Online,
        "a sheep two seconds from its own due time must not be made to wait another hour by \
         the handover: {anchored:?}"
    );
    assert_eq!(
        anchored[0].pid,
        Some(STAND_IN_SPAWN_PID),
        "the restart must be a real respawn, not a status edit: {anchored:?}"
    );

    let control = control.list().await;
    assert_eq!(
        control[0].status,
        ProcStatus::WaitingRestart,
        "with no carried deadline the whole delay is re-armed, so an hourly app must still \
         be waiting ten seconds in — a control that respawned would mean the case above \
         proved nothing: {control:?}"
    );
}

/// Drives a real exit through `decide_on_exit`, the only thing that writes
/// the moment. A window, not an equality: the deadline is wall-clock,
/// since a monotonic instant cannot cross an `execve`, and a paused tokio
/// clock does not pause the wall clock.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_snapshot_of_a_waiting_sheep_carries_the_moment_its_respawn_falls_due() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(1)]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    // An hour, so the sheep is still waiting when the snapshot is taken
    // rather than racing its own respawn.
    app.restart_delay = Some("1h".parse().unwrap());
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    // The sheep starts `Online`, so waiting for `WaitingRestart` stops
    // this passing on the state before the crash.
    let waits = async {
        while handle.list().await[0].status != ProcStatus::WaitingRestart {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), waits)
        .await
        .expect("an app that exits immediately with autorestart on must reach WaitingRestart");

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let due = blob.sheep()[0]
        .restart_due()
        .expect("a sheep owed a respawn must carry the moment it is owed at");
    let left = due
        .duration_since(SystemTime::now())
        .expect("a deadline an hour out has not passed");
    assert!(
        left > Duration::from_secs(3595) && left <= Duration::from_secs(3600),
        "the carried moment must be this sheep's own exit plus its own delay: {left:?}"
    );
}

/// The successor has to restore the moment onto its own slot, not merely
/// read it on the way past. An absolute moment keeps the chain flat: a
/// carried remainder would have each hop add its own handover duration
/// back on, which is the drift the window asserts is absent.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_second_handover_inside_one_delay_still_names_the_original_moment() {
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let due = SystemTime::now() + Duration::from_secs(3000);
    let handle = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_owed_a_restart(
                "web",
                7,
                "1h".parse().unwrap(),
                Some(due),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let carried = blob.sheep()[0]
        .restart_due()
        .expect("a successor must hand its own successor the moment it was given");
    let drift = carried
        .duration_since(due)
        .or_else(|_| due.duration_since(carried))
        .unwrap();
    assert!(
        drift < Duration::from_secs(1),
        "the second hop must name the same moment as the first, not a fresh one: {drift:?} \
         of drift"
    );
}

// --- a swap in flight, carried across the exec ----------------------

/// A serial drain is what holds still long enough to be snapshotted: a
/// `readiness_probe` with no `reuse_port` is the one arrangement
/// `ReloadMode::of` sends down the serial ordering, and
/// [`ProcScript::never_reports_its_exit`] models the child a kill ladder
/// cannot end, so the swap stays in [`ReloadPhase::DrainFirst`].
///
/// A real clock, unlike its neighbours: a paused one auto-advances
/// whenever every task is idle, and the awaits inside `handover_snapshot`
/// are such a window.
#[cfg(unix)]
#[tokio::test]
async fn a_snapshot_taken_mid_swap_carries_the_job_and_the_markers() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_reports_its_exit(); 2]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.listen_timeout = UpDuration::from_millis(200);
    app.readiness_probe = Some(probe_config(ProbeKind::Tcp, "127.0.0.1:9"));
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    // A probed app is `Starting` until its probe answers or
    // `listen_timeout` elapses, and `reload_eligible` refuses anything not
    // serving, so without this the reload skips the only instance there is.
    let online = loop {
        let info = handle.list().await;
        if info[0].status == ProcStatus::Online {
            break info;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let old_id = online[0].id;

    handle
        .reload(ProcessSelector::All)
        .await
        .expect("an online app reloads");

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    assert_eq!(
        blob.reloads(),
        &[CarriedReload {
            app: "web".to_owned(),
            queue: Vec::new(),
            mode: ReloadMode::Serial,
            swap: ReloadSwap {
                old_id,
                new_id: None,
                phase: ReloadPhase::DrainFirst,
            },
        }],
        "the job the successor continues has to be in the blob, whole"
    );
    assert_eq!(
        blob.sheep()[0].reload(),
        Some(ReloadState::Drainee { new_id: None }),
        "and the marker that routes this instance's exit with it"
    );
}

/// A carried sheep that is half of a swap, with the role and the status
/// the predecessor's own entry carried.
#[cfg(unix)]
fn carried_in_swap(
    name: &str,
    id: u32,
    pid: Option<u32>,
    role: ReloadState,
    status: ProcStatus,
    manual: Option<PendingManual>,
    mutate: impl FnOnce(&mut ProcessEntry),
) -> CarriedSheep {
    carried_marked(name, id, pid, false, manual, false, move |entry| {
        mutate(entry);
        entry.reload = role;
        entry.status = status;
    })
}

/// One app's in-flight reload, as a blob carries it, with an empty queue.
///
/// Empty because a queue behind the swap would let a case pass on the next
/// instance's swap rather than on the carried one.
#[cfg(unix)]
fn carried_job(
    app: &str,
    mode: ReloadMode,
    old_id: u32,
    new_id: Option<u32>,
    phase: ReloadPhase,
) -> CarriedReload {
    CarriedReload {
        app: app.to_owned(),
        queue: Vec::new(),
        mode,
        swap: ReloadSwap {
            old_id,
            new_id,
            phase,
        },
    }
}

/// The watchdog is a `tokio::spawn`ed sleep of the predecessor's that the
/// `execve` takes. With the job restored and no timer over it,
/// `handle_reload` refuses on the map key forever, and the refusal is
/// whole-selector, so `shep reload all` goes with it. That refusal is the
/// assertion both ways. Nothing here ever exits, since
/// [`AdoptingRunner`]'s `wait` never resolves, so only a timer this image
/// armed can produce the second half.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_carried_swap_that_cannot_finish_is_still_abandoned_on_time() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(4242),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    None,
                    |_| {},
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(4243),
                    ReloadState::Replacement,
                    ProcStatus::Online,
                    None,
                    |_| {},
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    let refused = sup.reload(ProcessSelector::All).await;
    assert!(
        matches!(refused, Err(SupervisorError::ReloadInFlight(ref name)) if name == "web"),
        "the carried job must be in the map, or this case proves nothing: {refused:?}"
    );

    // Parking on `recv` advances the paused clock, so this waits out the
    // 16s watchdog for free. The bound is load-bearing: with no timer
    // armed the clock has nothing to advance to, so an unbounded wait
    // hangs the suite instead of failing the case.
    tokio::time::timeout(
        Duration::from_secs(3600),
        await_event(&mut rx, 8, ProcessEventKind::ReloadAbandoned),
    )
    .await
    .expect("a carried swap must still be bounded by a watchdog this image armed");

    sup.reload(ProcessSelector::All)
        .await
        .expect("once the watchdog has ended the job, the app must be reloadable again");
}

/// A snapshot cannot produce one, but the blob is a file, the same
/// residual `refuse_repeated_fds` guards on the descriptor side. Arming a
/// watchdog against an absent entry panics, so a build without the guard
/// fails the reload below by aborting.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_carried_reload_naming_no_registered_instance_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |_| {}))],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                98,
                Some(99),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    sup.reload(ProcessSelector::All)
        .await
        .expect("a job naming nothing must be dropped, not left refusing every later reload");
}

/// `spawn_verify_task` is a task of the predecessor's too, so a successor
/// that re-arms only the watchdog abandons a deploy that worked: the
/// replacement serves on, but `Reloaded` never fires and the rest of a
/// clustered app's queue is dropped.
///
/// A real listener and a real clock. The probe answers in microseconds
/// where the watchdog is 16s, so the bound below separates the two by an
/// order of magnitude whatever the host's speed.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_swap_in_verify_is_asked_again_rather_than_abandoned() {
    let probe_target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe_target.local_addr().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    // Bound, never used: dropping the handle would take a sender off the
    // actor's mailbox.
    let _sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_in_swap(
                "web",
                8,
                Some(4243),
                ReloadState::Replacement,
                ProcStatus::Online,
                None,
                move |entry| {
                    let mut app = AppConfig::minimal("web", "./srv");
                    app.autorestart = false;
                    // `Probe` readiness alone takes the serial ordering,
                    // which has no post-drain probe to re-arm. `reuse_port`
                    // puts this app on the overlapping one.
                    app.reuse_port = true;
                    app.readiness_probe = Some(probe_config(ProbeKind::Tcp, &addr.to_string()));
                    entry.spec = normalize(app).unwrap();
                },
            ))],
            counters(9),
            // No drainee: `Verify` is entered once the drainee is reaped,
            // so the re-ask happens with one process left.
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::Verify,
            )],
        )
        .expect("a carried flock installs");

    tokio::time::timeout(
        Duration::from_secs(5),
        await_event(&mut rx, 8, ProcessEventKind::Reloaded),
    )
    .await
    .expect("a carried swap in Verify must be probed again, not left to its watchdog");
}

/// [`ReloadPhase::DrainFirst`] is the one phase with no replacement yet:
/// `Drainee { new_id: None }` routes the exit to `reap_drainee` and so to
/// `spawn_serial_replacement`, not to `decide_on_exit`. A successor that
/// dropped it deregisters a `Stopping` sheep and leaves the slot empty
/// with the job still in the map. A real child and the real runner: only
/// the reaper's `Msg::Exited` proves a carried marker reaches
/// `handle_exited`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_serial_drain_still_spawns_its_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_in_swap(
                "web",
                7,
                Some(pid),
                ReloadState::Drainee { new_id: None },
                ProcStatus::Stopping,
                Some(PendingManual {
                    kind: ManualKind::Stop,
                    origin: CommandOrigin::Operator,
                }),
                swappable,
            ))],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Serial,
                7,
                None,
                ReloadPhase::DrainFirst,
            )],
        )
        .expect("a carried flock installs");

    let info = flock_until(
        &sup,
        |info| info.len() == 1 && info[0].id != 7,
        "a carried serial drain must spawn its replacement once the instance it drained goes",
    )
    .await;

    assert_eq!(
        info[0].id, 9,
        "the replacement takes the next carried id, not a reissued one"
    );
    assert_eq!(
        info[0].restarts, 0,
        "a reload is not a restart, so the count carries across the swap unchanged"
    );
    assert_ne!(info[0].pid, Some(pid), "the replacement is a new process");

    sup.shutdown().await;
}

/// The readiness wait, the `Replacement` marker and the job all have to
/// survive. Without the wait the replacement sits `Starting` forever
/// beside a drainee still serving: two live instances of a one-instance
/// app. Without the marker or the job, `handle_ready_result` takes the
/// ordinary path and the swap never commits; the `manually` flag below is
/// that route's fingerprint, since `spawn_replacement` passes `true` and
/// an adopted sheep that is not a replacement is armed with `false`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_replacement_awaiting_readiness_commits_its_swap() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let drainee = adoptable_child("sleep 30");
    let replacement = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(drainee),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    // No marker: an overlapping swap does not ask the
                    // drainee to go until its replacement is serving.
                    None,
                    swappable,
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(replacement),
                    ReloadState::Replacement,
                    ProcStatus::Starting,
                    None,
                    swappable,
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::AwaitReady,
            )],
        )
        .expect("a carried flock installs");

    let manually = tokio::time::timeout(
        Duration::from_secs(20),
        await_event(&mut rx, 8, ProcessEventKind::Online),
    )
    .await
    .expect("a carried replacement must still resolve its readiness after the exec");
    assert!(
        manually,
        "a replacement's Online is an operator's doing; reporting otherwise broadcasts a \
         deploy as the daemon's own"
    );

    let info = flock_until(
        &sup,
        |info| info.len() == 1,
        "committing the swap must drain the instance it replaced",
    )
    .await;
    assert_eq!(info[0].id, 8, "the replacement is what is left: {info:?}");
    assert_eq!(info[0].pid, Some(replacement));

    sup.shutdown().await;
}

/// [`ReloadPhase::DrainOld`] is the committed phase: the replacement is
/// serving and the instance it replaced is on its ladder. The `Drainee`
/// marker sends that instance's exit to `reap_drainee` rather than to
/// `decide_on_exit`, and this app has `autorestart` on, so a successor
/// that dropped the marker would respawn the old code into an instance
/// slot the replacement owns.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_drainee_still_finishes_its_swap() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let drainee = adoptable_child("sleep 30");
    let replacement = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(drainee),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    Some(PendingManual {
                        kind: ManualKind::Stop,
                        origin: CommandOrigin::Operator,
                    }),
                    |entry| swappable_with(entry, |app| app.autorestart = true),
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(replacement),
                    ReloadState::Replacement,
                    ProcStatus::Online,
                    None,
                    |entry| swappable_with(entry, |app| app.autorestart = true),
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    tokio::time::timeout(
        Duration::from_secs(20),
        await_event(&mut rx, 8, ProcessEventKind::Reloaded),
    )
    .await
    .expect("a carried drainee's exit must finish the swap it was half of");

    let info = sup.list().await;
    assert_eq!(
        info.len(),
        1,
        "the drainee is deregistered by the swap, never respawned: {info:?}"
    );
    assert_eq!(info[0].id, 8);
    assert_eq!(info[0].pid, Some(replacement));

    sup.shutdown().await;
}

/// The cap is not recorded anywhere, so it is derived from the role: both
/// sites that pass `LadderCap::Drain` leave the `Drainee` marker on the
/// entry. The child ignores `SIGTERM`, so only the escalation ends it, and
/// the two timeouts are three orders of magnitude apart: under the drain's
/// cap the `SIGKILL` lands a quarter of a second in, under the stop's five
/// minutes later, past [`flock_until`]'s bound.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_drainee_is_capped_by_graceful_timeout_not_kill_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let drainee = adoptable_child("trap '' TERM; sleep 300");
    let replacement = adoptable_child("sleep 30");
    let capped = |entry: &mut ProcessEntry| {
        swappable_with(entry, |app| {
            app.graceful_timeout = UpDuration::from_millis(250);
            app.kill_timeout = UpDuration::from_millis(300_000);
        });
    };
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(drainee),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    Some(PendingManual {
                        kind: ManualKind::Stop,
                        origin: CommandOrigin::Operator,
                    }),
                    capped,
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(replacement),
                    ReloadState::Replacement,
                    ProcStatus::Online,
                    None,
                    capped,
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    flock_until(
        &sup,
        |info| info.len() == 1,
        "a carried drainee must escalate at graceful_timeout, not at kill_timeout",
    )
    .await;

    sup.shutdown().await;
}

/// An abandoned reload leaves its replacement `Starting`, and a reload
/// replaces `Online` instances, so `SheepSlot::ready_failed` is the whole
/// of what keeps the leftover reachable. Asserted through a real reload
/// rather than by reading the slot back: `handle_reload` replies `Ok` with
/// the row in it either way, before its selector pass has run.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_ready_failed_instance_is_still_replaceable() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_ready_failed(
                "web",
                7,
                Some(pid),
                |entry| {
                    swappable(entry);
                    entry.status = ProcStatus::Starting;
                },
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    sup.reload(ProcessSelector::All)
        .await
        .expect("a registered app is one a reload can name");

    let info = flock_until(
        &sup,
        |info| info.len() == 1 && info[0].id != 7,
        "a carried `ready_failed` instance must still be replaceable by the reload that \
         rolls its release back",
    )
    .await;

    assert_eq!(
        info[0].id, 9,
        "the replacement takes the next carried id, not a reissued one"
    );
    assert_ne!(info[0].pid, Some(pid), "the replacement is a new process");

    sup.shutdown().await;
}

/// Both sheep are `Starting` and only the carried flag tells them apart:
/// an ordinary one is mid-wait and owed a fresh one, this one's wait
/// already ran and failed. `handle_ready_result`'s `TimedOut` arm goes
/// `Online` anyway and `went_online` clears `ready_failed` on its way
/// past, so arming one would report an abandoned release as serving. The
/// clock runs past the app's `listen_timeout`, so any wait that was armed
/// has fired by the time the status is read.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_carried_ready_failed_instance_gets_no_fresh_readiness_wait() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_ready_failed(
                "web",
                7,
                Some(4242),
                |entry| {
                    let mut app = AppConfig::minimal("web", "./srv");
                    app.autorestart = false;
                    app.wait_ready = true;
                    entry.spec = normalize(app).unwrap();
                    entry.status = ProcStatus::Starting;
                },
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    tokio::time::sleep(Duration::from_secs(120)).await;

    assert_eq!(
        sup.list().await[0].status,
        ProcStatus::Starting,
        "an instance whose readiness already failed must not be handed a second verdict by \
         the successor that adopted it"
    );
}

/// The app a carried swap's two halves run: a real `sleep`, so a
/// replacement can be spawned and a drainee signalled, with a
/// `listen_timeout` short enough for a real-clock case.
#[cfg(unix)]
fn swappable(entry: &mut ProcessEntry) {
    swappable_with(entry, |_| {});
}

/// [`swappable`], with `mutate` free to change the app first.
#[cfg(unix)]
fn swappable_with(entry: &mut ProcessEntry, mutate: impl FnOnce(&mut AppConfig)) {
    let mut app = AppConfig::minimal("web", "/bin/sh");
    app.args = vec!["-c".to_owned(), "sleep 30".to_owned()];
    app.autorestart = false;
    app.listen_timeout = UpDuration::from_millis(200);
    mutate(&mut app);
    entry.spec = normalize(app).unwrap();
}

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

/// Additive is the default because a Flockfile arrives from the app's own
/// repository: a merged pull request must not change a running flock.
#[tokio::test(start_paused = true)]
async fn a_file_load_does_not_overwrite_an_established_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |app| app.max_restarts = 3)]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script", "max_restarts"],
            vec![("max_restarts", serde_json::json!(3))],
        ),
    )
    .unwrap();

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 99;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().max_restarts,
        3,
        "the file overwrote a key the operator had set"
    );
    assert!(
        reply[0].applied.is_empty(),
        "nothing applied, so nothing may be reported as applied: {reply:?}"
    );
}

/// Appending an unestablished key is what makes a template update reach an
/// app at all.
#[tokio::test(start_paused = true)]
async fn a_file_load_appends_a_key_nobody_had_established() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(&["name", "script"], Vec::new()),
    )
    .unwrap();

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_memory = Some(MemSize::from_bytes(512 << 20));
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_memory"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().max_memory,
        Some(MemSize::from_bytes(512 << 20)),
        "a key nobody had established must be appended"
    );
    assert_eq!(reply[0].applied, vec!["max_memory".to_string()]);
}

#[tokio::test(start_paused = true)]
async fn a_live_field_lands_on_the_stored_spec() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 42;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(actor.sheep[&0].entry.spec.config().max_restarts, 42);
    assert_eq!(reply[0].applied, vec!["max_restarts".to_string()]);
    assert!(reply[0].pending.is_empty(), "{reply:?}");
}

/// `depends_on` is `ApplyGroup::NextSpawn` and reports as in force
/// anyway, the carve-out `autostart` already had and for the same reason:
/// nothing reads either at a spawn. `plan_for_names` reads `depends_on`
/// when a batch is ordered, off the stored spec, so the new value already
/// governs the next restart, the next shutdown and the next boot.
/// Reporting it pending would send an operator to `shep reload` to
/// promote a value that is already promoted.
#[tokio::test(start_paused = true)]
async fn a_loaded_depends_on_is_in_force_and_never_reports_as_pending() {
    // fails if `depends_on` is left in the ordinary NextSpawn arm, which
    // shows the sheep as `!1` in `shep flock` and tells `shep describe`
    // to reload for a value the next ordered walk is already reading.
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.depends_on = vec!["db".to_string()];
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "depends_on"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().depends_on,
        vec!["db".to_string()],
        "the edge has to be on the stored spec for the walk to read it"
    );
    assert_eq!(reply[0].applied, vec!["depends_on".to_string()]);
    assert!(
        reply[0].pending.is_empty(),
        "an edge already being read is not pending: {reply:?}"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_none(),
        "nothing may be parked for a respawn to promote"
    );
}

/// A load must never kill a process.
#[tokio::test(start_paused = true)]
async fn a_needs_respawn_field_parks_as_pending_and_leaves_the_child_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.pid,
        Some(APPLY_FIRST_PID),
        "the running child must not have been replaced"
    );
    assert!(
        entry.spec.config().env.is_empty(),
        "the running child's own config must keep describing what it was spawned from"
    );
    assert_eq!(
        entry
            .pending
            .as_ref()
            .expect("a NeedsRespawn change parks as pending")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue")
    );
    assert_eq!(reply[0].pending, vec!["env".to_string()]);
    assert!(reply[0].applied.is_empty(), "{reply:?}");
    assert_eq!(
        reply[0]
            .app
            .as_ref()
            .expect("an applied app is recorded")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "a reboot spawns everything afresh, so what it comes up on is the \
         full merge and not what the running child is on"
    );
}

/// An app whose merge is invalid refuses whole; the rest of the flock
/// still applies.
#[tokio::test(start_paused = true)]
async fn an_unnormalizable_merge_refuses_one_app_and_applies_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |_| {}), app_with("worker", |_| {})]);

    // Two instances sharing one explicit log path, with no `{{instance}}`
    // in it and no `merge_logs`: the one refusal a merge can produce out
    // of two individually-legal keys.
    let mut broken = AppConfig::minimal("web", "./srv");
    broken.instances = 2;
    broken.out_file = Some("/tmp/web.log".to_string());
    let mut worker = AppConfig::minimal("worker", "./srv");
    worker.max_restarts = 7;

    // `Policy`, not `None`: a plain load holds `instances` out of the
    // merge, so the two keys could not meet and the merge would be valid.
    let reply = apply_config(
        &mut actor,
        vec![
            declared_app(broken, &["name", "script", "instances", "out_file"]),
            declared_app(worker, &["name", "script", "max_restarts"]),
        ],
        ResetDepth::Policy,
    )
    .await;

    assert!(
        reply[0].refused.is_some(),
        "an unnormalizable merge must refuse: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0].entry.spec.config().instances,
        1,
        "a refused app's stored config must be untouched"
    );
    assert!(actor.sheep[&0].entry.spec.config().out_file.is_none());
    assert_eq!(actor.sheep[&1].entry.spec.config().max_restarts, 7);
    assert_eq!(reply[1].applied, vec!["max_restarts".to_string()]);
}

/// A load never prunes: the daemon has no record of which Flockfile an app
/// came from, so `shep start ./a/Flockfile.toml` followed by
/// `./b/Flockfile.toml` would have the second wipe the first's flock.
#[tokio::test(start_paused = true)]
async fn an_app_absent_from_the_file_is_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |_| {}), app_with("worker", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 5;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    let worker = &actor.sheep[&1].entry;
    assert_eq!(worker.spec.config().name, "worker");
    assert_eq!(worker.status, ProcStatus::Online);
    assert_eq!(worker.pid, Some(APPLY_FIRST_PID + 1));
    assert!(
        reply.iter().all(|applied| applied.name != "worker"),
        "a load must not claim to have touched an app the file never named: {reply:?}"
    );
}

/// The drainee holds the lower id, so `ids.first()` reaches the instance on
/// its way out, and a spec derived from it lands on the live replacement.
#[tokio::test(start_paused = true)]
async fn a_load_during_a_reload_reads_the_replacement_and_not_the_drainee() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.instances = 2;
            app.cwd = Some("/srv/new".to_string());
        })],
    );
    // Instance 0 is the drainee: the lower id, still on the config the
    // reload is replacing, and already `Stopping`.
    {
        let slot = actor
            .sheep
            .get_mut(&0)
            .expect("the fixture registered two slots");
        slot.entry.status = ProcStatus::Stopping;
        slot.entry.reload = ReloadState::Drainee { new_id: Some(1) };
        slot.entry.spec = app_with("web", |app| {
            app.instances = 2;
            app.cwd = Some("/srv/old".to_string());
        });
    }

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 7;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&1].entry.spec.config().cwd.as_deref(),
        Some("/srv/new"),
        "the replacement's spec must keep describing what the replacement \
         was spawned from: {reply:?}"
    );
    assert_eq!(actor.sheep[&1].entry.spec.config().max_restarts, 7);
    assert_eq!(reply[0].applied, vec!["max_restarts".to_string()]);
    assert!(reply[0].pending.is_empty(), "{reply:?}");
}

/// A dog runs at the daemon's own trust level, so a file naming one and
/// carrying a `script` would replace an adopted binary without adopting
/// anything, while `shep dogs` went on reporting the previous dog.
#[tokio::test(start_paused = true)]
async fn a_file_naming_a_dog_is_refused_rather_than_merged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("metrics", |_| {})]);
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registered one slot")
        .entry
        .dog = Some(DogSource::BuiltIn);

    let mut file = AppConfig::minimal("metrics", "/opt/evil");
    file.max_restarts = 42;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        reply[0].refused.as_deref(),
        Some(
            "metrics is a dog, and a dog's config comes from `shep adopt` rather than \
             from a Flockfile"
        ),
        "{reply:?}"
    );
    let entry = &actor.sheep[&0].entry;
    assert_eq!(entry.spec.config().script, "./srv");
    assert_eq!(
        entry.spec.config().max_restarts,
        AppConfig::default().max_restarts
    );
    assert!(entry.pending.is_none(), "a refused app parks nothing");
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "metrics")
            .unwrap()
            .is_none(),
        "a refused app establishes nothing"
    );
}

/// A reset resolves an undeclared key to the file as loaded, not to the
/// compiled default. The CLI defaults `cwd` to the Flockfile's own
/// directory without the document declaring it, so the compiled default
/// would park `cwd: None` and the next restart could not find the script.
/// `fold` and `interpreter` arrive the same way.
#[tokio::test(start_paused = true)]
async fn a_reset_against_an_unchanged_file_keeps_a_defaulted_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.cwd = Some("/srv/web".to_string());
            app.fold = Some("edge".to_string());
        })],
    );

    // What `shep start Flockfile.toml --reset` sends for an unmodified
    // two-line file: the resolved config carries the defaulted `cwd` and
    // the `--fold`, and the document declared neither.
    let mut file = AppConfig::minimal("web", "./srv");
    file.cwd = Some("/srv/web".to_string());
    file.fold = Some("edge".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script"])],
        ResetDepth::Policy,
    )
    .await;

    // `cwd` is `NeedsRespawn`, so the damage lands in `pending` rather
    // than on the running spec.
    assert!(
        actor.sheep[&0].entry.pending.is_none(),
        "an unchanged file has nothing to park: {reply:?}"
    );
    assert!(reply[0].pending.is_empty(), "{reply:?}");
    let config = actor.sheep[&0].entry.spec.config().clone();
    assert_eq!(config.cwd.as_deref(), Some("/srv/web"));
    assert_eq!(config.fold.as_deref(), Some("edge"));
}

/// `--reset=policy` restores settings, declared or not, and leaves env;
/// `--reset=all` takes env with it and drops the record.
#[tokio::test(start_paused = true)]
async fn reset_restores_every_setting_and_only_reset_all_takes_env() {
    // The operator's three edits since the file established name, script
    // and max_restarts: one over a key the file declares, one env key and
    // one field the file has never mentioned.
    let stored = |name: &str| {
        app_with(name, |app| {
            app.max_restarts = 3;
            app.env = BTreeMap::from([("OPERATOR".to_string(), "1".to_string())]);
            app.min_uptime = UpDuration::from_millis(9000);
        })
    };
    let record = || {
        established(
            &["name", "script", "max_restarts"],
            vec![
                ("max_restarts", serde_json::json!(3)),
                ("env", serde_json::json!({ "OPERATOR": "1" })),
                ("min_uptime", serde_json::json!("9000ms")),
            ],
        )
    };
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.max_restarts = 10;
        declared_app(file, &["name", "script", "max_restarts"])
    };

    let settings_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&settings_dir, &[stored("web")]);
    shep_core::overrides::put(&actor.paths.overrides, "web", &record()).unwrap();
    apply_config(&mut actor, vec![file()], ResetDepth::Policy).await;
    let settings = actor.sheep[&0].entry.spec.config().clone();
    let settings_pending = actor.sheep[&0].entry.pending.clone();
    assert_eq!(
        settings.max_restarts, 10,
        "--reset=policy puts a declared setting back to the file's"
    );
    assert_eq!(
        settings.min_uptime,
        AppConfig::default().min_uptime,
        "--reset=policy puts a field the file never declared back to the \
         file's own value, which for an undeclared key is the compiled \
         default"
    );
    assert_eq!(
        settings_pending
            .as_ref()
            .map_or(&settings.env, |app| &app.config().env)
            .get("OPERATOR")
            .map(String::as_str),
        Some("1"),
        "--reset=policy keeps env"
    );

    let all_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&all_dir, &[stored("web")]);
    shep_core::overrides::put(&actor.paths.overrides, "web", &record()).unwrap();
    apply_config(&mut actor, vec![file()], ResetDepth::All).await;
    let all = actor.sheep[&0].entry.spec.config().clone();
    let all_pending = actor.sheep[&0].entry.pending.clone();
    assert_eq!(all.max_restarts, 10);
    assert_eq!(
        all.min_uptime,
        AppConfig::default().min_uptime,
        "--reset=all drops a field the operator added"
    );
    assert!(
        all_pending
            .as_ref()
            .expect("dropping an env key needs a respawn")
            .config()
            .env
            .is_empty(),
        "--reset=all drops an env key the operator added"
    );
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "--reset=all removes the override record"
    );
}

/// The four-mode fixture: a template declaring `max_restarts` and `env`,
/// against a sheep whose operator has overridden `max_restarts`, added
/// `max_memory` (undeclared) and edited the one declared env key.
///
/// Returns the stored config, the template and the override record, in the
/// order [`merge_declared`] takes them. Every mode is asserted on the same
/// three values, since the four are not a two-by-two grid.
fn reset_grid() -> (AppConfig, DeclaredApp, shep_core::overrides::AppOverrides) {
    let mut stored = AppConfig::minimal("web", "./srv");
    stored.max_restarts = 3;
    stored.max_memory = Some(MemSize::from_bytes(2 << 30));
    stored.env = BTreeMap::from([("DB".to_string(), "operator".to_string())]);

    let mut template = AppConfig::minimal("web", "./srv");
    template.max_restarts = 10;
    template.env = BTreeMap::from([("DB".to_string(), "template".to_string())]);
    let incoming = declared_app(template, &["name", "script", "max_restarts", "env"]);

    let mut record = established(
        &["name", "script", "max_restarts", "env"],
        vec![
            ("max_restarts", serde_json::json!(3)),
            ("max_memory", serde_json::json!("2G")),
            ("env", serde_json::json!({ "DB": "operator" })),
        ],
    );
    // The template established `DB` on the load before this one, which is
    // how an operator's later edit to it became an override at all.
    record.declared_env = BTreeSet::from(["DB".to_string()]);
    (stored, incoming, record)
}

/// The merged config a mode produces over [`reset_grid`].
fn merged_over_grid(reset: ResetDepth) -> AppConfig {
    let (stored, incoming, record) = reset_grid();
    merge_declared(&stored, &incoming, &record, reset)
        .expect("the grid fixture travels through serde")
        .0
}

/// The override record a mode leaves over [`reset_grid`].
fn record_over_grid(reset: ResetDepth) -> shep_core::overrides::AppOverrides {
    let (stored, incoming, record) = reset_grid();
    merge_declared(&stored, &incoming, &record, reset)
        .expect("the grid fixture travels through serde")
        .1
}

/// Both axes in one test: the mode is the pair, and a test checking one
/// axis would pass for two different modes.
#[test]
fn file_puts_back_what_the_template_declares_and_leaves_the_rest() {
    let merged = merged_over_grid(ResetDepth::File);
    assert_eq!(merged.max_restarts, 10, "a declared key goes back");
    assert_eq!(
        merged.max_memory,
        Some(MemSize::from_bytes(2 << 30)),
        "a key the template never declares is not the template's to reset"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("operator"),
        "`file` keeps env"
    );
}

#[test]
fn policy_puts_back_every_setting_declared_or_not_and_keeps_env() {
    let merged = merged_over_grid(ResetDepth::Policy);
    assert_eq!(merged.max_restarts, 10, "a declared key goes back");
    assert_eq!(
        merged.max_memory,
        AppConfig::default().max_memory,
        "`policy` resets a key the template is silent about, which for an \
         undeclared key means the value a fresh start off that template gives it"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("operator"),
        "`policy` keeps env"
    );
}

/// An operator typing `--reset=env` does not expect their restart budget
/// put back because the template happens to mention it.
#[test]
fn env_resets_env_and_touches_no_setting_at_all() {
    let merged = merged_over_grid(ResetDepth::Env);
    assert_eq!(
        merged.max_restarts, 3,
        "a declared policy field is not `env`'s to reset"
    );
    assert_eq!(
        merged.max_memory,
        Some(MemSize::from_bytes(2 << 30)),
        "and neither is one the template is silent on"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("template"),
        "`env` puts env back to the template"
    );
}

/// `all` is the widest mode: every setting back, declared or not, and env
/// with it.
#[test]
fn all_puts_back_every_setting_and_env_with_it() {
    let merged = merged_over_grid(ResetDepth::All);
    assert_eq!(merged.max_restarts, 10, "a declared key goes back");
    assert_eq!(
        merged.max_memory,
        AppConfig::default().max_memory,
        "`all` resets a key the template is silent about"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("template"),
        "`all` puts env back to the template"
    );
}

/// An override is spent exactly where the merge overwrote it, so `file`
/// spends the declared setting and keeps both the undeclared one and env.
#[test]
fn file_spends_only_the_override_it_put_back() {
    let record = record_over_grid(ResetDepth::File);
    let mut held: Vec<&String> = record.fields.keys().collect();
    // Sorted: `serde_json::Map` is insertion-ordered, and this is a set.
    held.sort();
    assert_eq!(
        held,
        vec!["env", "max_memory"],
        "`file` keeps the undeclared override and env, and spends the rest"
    );
}

/// Every setting is in scope, so every setting override is spent; env is
/// untouched, so the env override stands.
#[test]
fn policy_spends_every_setting_override_and_keeps_env() {
    let record = record_over_grid(ResetDepth::Policy);
    let mut held: Vec<&String> = record.fields.keys().collect();
    // Sorted: `serde_json::Map` is insertion-ordered, and this is a set.
    held.sort();
    assert_eq!(
        held,
        vec!["env"],
        "`policy` spends both setting overrides and keeps env"
    );
}

/// An override is spent exactly where the merge overwrote it, and this mode
/// overwrites no setting, so both survive. Those record entries keep a
/// later plain load from appending the template's values over them.
#[test]
fn env_spends_only_the_env_override() {
    let record = record_over_grid(ResetDepth::Env);
    let mut held: Vec<&String> = record.fields.keys().collect();
    // Sorted: `serde_json::Map` is insertion-ordered, and this is a set.
    held.sort();
    assert_eq!(
        held,
        vec!["max_memory", "max_restarts"],
        "`env` spends env and keeps every setting override"
    );
}

/// Everything is in scope, so nothing is still overridden.
#[test]
fn all_spends_every_override() {
    let record = record_over_grid(ResetDepth::All);
    assert!(
        record.fields.is_empty(),
        "`all` holds nothing back: {:?}",
        record.fields.keys().collect::<Vec<_>>()
    );
}

/// Resetting a key's value and dropping its record entry are different
/// operations, and only `all` does both: under `env` the record still holds
/// `max_memory`, which is what stands between the operator's ceiling and
/// the next plain load.
#[tokio::test(start_paused = true)]
async fn an_env_reset_keeps_the_override_record() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(2 << 30));
            app.env = BTreeMap::from([("DB".to_string(), "operator".to_string())]);
        })],
    );
    let mut record = established(
        &["name", "script", "env"],
        vec![
            ("max_memory", serde_json::json!("2G")),
            ("env", serde_json::json!({ "DB": "operator" })),
        ],
    );
    record.declared_env = BTreeSet::from(["DB".to_string()]);
    shep_core::overrides::put(&actor.paths.overrides, "web", &record).unwrap();

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("DB".to_string(), "template".to_string())]);
    let file = declared_app(file, &["name", "script", "env"]);
    let reply = apply_config(&mut actor, vec![file], ResetDepth::Env).await;

    let written = shep_core::overrides::get(&actor.paths.overrides, "web")
        .unwrap()
        .unwrap_or_else(|| panic!("`env` keeps the record: {reply:?}"));
    assert!(
        written.fields.contains_key("max_memory"),
        "the undeclared override must survive an env reset: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0].entry.overridden,
        vec!["max_memory".to_string()],
        "and `shep flock`'s CFG column must still say so"
    );
}

/// The flag widens a load and never narrows one, so the additive default
/// underneath it still runs. The second assertion keeps that from reading
/// as an overwrite: `max_restarts` is established, and does not move.
#[test]
fn an_env_reset_still_appends_a_key_nobody_established() {
    let (stored, _, record) = reset_grid();
    let mut template = AppConfig::minimal("web", "./srv");
    template.max_restarts = 10;
    template.min_uptime = UpDuration::from_millis(9000);
    template.env = BTreeMap::from([("DB".to_string(), "template".to_string())]);
    let incoming = declared_app(
        template,
        &["name", "script", "max_restarts", "min_uptime", "env"],
    );

    let (merged, _) = merge_declared(&stored, &incoming, &record, ResetDepth::Env)
        .expect("the grid fixture travels through serde");
    assert_eq!(
        merged.min_uptime,
        UpDuration::from_millis(9000),
        "a declared key nobody established is appended under `env` too"
    );
    assert_eq!(
        merged.max_restarts, 3,
        "and an established one is still not overwritten"
    );
}

/// `instances` is held out of this depth as it is out of a plain load: the
/// store cannot tell a stocked count from a count nobody has touched, so
/// taking the file's would delete instances.
#[tokio::test(start_paused = true)]
async fn an_env_reset_never_reshapes_a_flock() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 4)]);
    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    let file = declared_app(file, &["name", "script", "instances"]);
    let reply = apply_config(&mut actor, vec![file], ResetDepth::Env).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        4,
        "`env` scaled a flock: {reply:?}"
    );
    // Exact, not a `contains("instances")`: the sentence an `env` operator
    // reads is the assertion, down to which mode it names and what that
    // mode costs.
    assert_eq!(
        reply[0].refused.as_deref(),
        Some(
            "instances: this load never reshapes a flock; no mode scales without also \
             putting back every setting the file declares, and `--reset=file` is the \
             narrowest that does, taking the file's count of 2"
        ),
        "an operator whose count did not move must be told why, and what the \
         mode they are pointed at would cost them: {reply:?}"
    );
}

/// An app stocked to four against a template carrying no `instances` line
/// keeps four. Under `policy` it drops to one, since the compiled default
/// wins an argument the file never entered. The second half keeps this from
/// passing on a `file` that refuses to scale at all.
#[tokio::test(start_paused = true)]
async fn a_file_reset_does_not_scale_an_app_the_template_says_nothing_about() {
    let stocked = || app_with("web", |app| app.instances = 4);

    let silent_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&silent_dir, &[stocked()]);
    let silent = declared_app(AppConfig::minimal("web", "./srv"), &["name", "script"]);
    let reply = apply_config(&mut actor, vec![silent], ResetDepth::File).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        4,
        "`file` scaled against a file with no `instances` line: {reply:?}"
    );
    assert!(
        !reply[0].applied.contains(&"instances".to_string()),
        "{reply:?}"
    );

    let declaring_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&declaring_dir, &[stocked()]);
    let mut declaring = AppConfig::minimal("web", "./srv");
    declaring.instances = 2;
    let declaring = declared_app(declaring, &["name", "script", "instances"]);
    let reply = apply_config(&mut actor, vec![declaring], ResetDepth::File).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        2,
        "`file` must apply a count the template does declare: {reply:?}"
    );
    assert!(
        reply[0].applied.contains(&"instances".to_string()),
        "{reply:?}"
    );
}

/// The `Policy` depth touches env not at all, so a template that has grown
/// `NEW_KEY` reports nothing and merges nothing. Recording the key as
/// established anyway would leave no plain load able to append it, with
/// only `--reset=all` to recover, taking every other env value.
#[tokio::test(start_paused = true)]
async fn a_settings_reset_does_not_establish_an_env_key_it_never_merged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(&["name", "script"], Vec::new()),
    )
    .unwrap();

    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.env = BTreeMap::from([("NEW_KEY".to_string(), "1".to_string())]);
        declared_app(file, &["name", "script", "env"])
    };
    let reset = apply_config(&mut actor, vec![file()], ResetDepth::Policy).await;

    assert!(
        actor.sheep[&0].entry.spec.config().env.is_empty(),
        "a `--reset=policy` merges no env at all: {reset:?}"
    );
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .expect("the load records what it established")
            .declared_env
            .is_empty(),
        "a key that never merged is not established"
    );

    // The plain load after it can still append.
    let plain = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("an env change parks for the next spawn")
            .config()
            .env
            .get("NEW_KEY")
            .map(String::as_str),
        Some("1"),
        "{plain:?}"
    );
}

/// `max_memory`, `watch` and the cron pair are read when a worker is
/// armed, so a spec write alone leaves the old value enforced for as long
/// as that arming lives.
#[tokio::test(start_paused = true)]
async fn a_changed_extras_field_rearms_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(100 << 20));
        })],
    );

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_memory = Some(MemSize::from_bytes(512 << 20));
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_memory"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        enforcer
            .arms()
            .last()
            .expect("a changed ceiling re-arms the name")
            .limit,
        MemSize::from_bytes(512 << 20),
        "the registry was re-armed with the old ceiling"
    );
}

/// `PollingEnforcer` computes a breach under its lock and sends it after
/// releasing that lock, so a re-arm landing in between leaves a report in
/// flight speaking for a limit nobody enforces. Reachable only because a
/// load re-arms an id that is already armed.
#[tokio::test(start_paused = true)]
async fn a_breach_measured_under_a_since_raised_ceiling_does_not_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(100 << 20));
        })],
    );

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_memory = Some(MemSize::from_bytes(512 << 20));
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_memory"])],
        ResetDepth::None,
    )
    .await;

    // Over the ceiling that was armed when the sample was taken, well
    // under the one the load just put in force.
    actor.handle_extra_restart(
        0,
        APPLY_FIRST_PID,
        None,
        Some(MemSize::from_bytes(200 << 20)),
    );

    let slot = &actor.sheep[&0];
    assert!(
        slot.manual.is_none(),
        "a breach against a ceiling the operator has raised must never claim the manual marker"
    );
    assert_eq!(slot.entry.pid, Some(APPLY_FIRST_PID));
}

/// The merge builds on the app's intended config, the parked one when
/// there is one, not on what the running child was spawned from: on the
/// second load the key is established, a plain load skips it, and a merge
/// based on the running config would carry the old value forward over the
/// parked one.
#[tokio::test(start_paused = true)]
async fn a_second_load_of_the_same_file_keeps_the_first_loads_parked_config() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
        declared_app(file, &["name", "script", "env"])
    };

    let first = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert_eq!(first[0].pending, vec!["env".to_string()]);

    let second = apply_config(&mut actor, vec![file()], ResetDepth::None).await;

    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("the parked config survives a second load")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the second load erased what the first parked"
    );
    assert_eq!(
        second[0]
            .app
            .as_ref()
            .expect("an applied app is recorded")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the recorded app lost it too, so a reboot would come up without it"
    );
    assert!(
        second[0].pending.is_empty(),
        "the second load changes nothing that was not already coming: {second:?}"
    );
}

/// An instance with no live task is deregistered synchronously inside
/// `handle_scale`, so the id list read before the scale can name a slot
/// that is already gone by the time the spec write walks it.
#[tokio::test(start_paused = true)]
async fn a_scale_down_removing_a_non_running_instance_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // One instance exited and never came back. No `ctl`, so its delete
    // resolves on the spot rather than through a kill ladder.
    let stopped = actor.sheep.get_mut(&1).expect("the fixture registers two");
    stopped.entry.status = ProcStatus::Stopped;
    stopped.entry.pid = None;

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 1;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "instances"])],
        ResetDepth::Policy,
    )
    .await;

    assert_eq!(actor.ids_of_name("web"), vec![0]);
    assert_eq!(reply[0].applied, vec!["instances".to_string()]);
    assert_eq!(actor.sheep[&0].entry.spec.config().instances, 1);
}

/// `Applied::refused` with two empty lists promises nothing happened, so a
/// reply with `app` as `None` leaves the muster roll on the old count while
/// a second instance runs. A file declaring `watch` and `cwd` together
/// merges cleanly and cannot be reached by a running instance, since `cwd`
/// needs a respawn and `watch` does not.
#[tokio::test(start_paused = true)]
async fn a_load_that_scales_and_cannot_reach_the_running_spec_reports_what_landed() {
    let root = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    file.watch = true;
    file.cwd = Some(root.path().display().to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "instances", "watch", "cwd"],
        )],
        ResetDepth::Policy,
    )
    .await;

    assert_eq!(
        actor.ids_of_name("web").len(),
        2,
        "the scale really did happen"
    );
    assert!(
        reply[0].refused.is_none(),
        "a merge that normalizes is not an invalid file: {reply:?}"
    );
    assert!(
        reply[0].app.is_some(),
        "the muster roll must not be left on the pre-load config: {reply:?}"
    );
    assert_eq!(reply[0].applied, vec!["instances".to_string()]);
    assert!(
        reply[0].pending.contains(&"watch".to_string())
            && reply[0].pending.contains(&"cwd".to_string()),
        "a change no running instance can take must park, not vanish: {reply:?}"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "and it must be parked on the entry, not only reported"
    );
}

/// Group membership is decided by the config rather than by what is
/// running, and both group triggers restart every member, so arming a
/// stopped instance lets a cron occurrence or a file save start it again.
/// Nothing heals that: the member is terminal, so no transition calls
/// `disarm` for it.
#[tokio::test(start_paused = true)]
async fn a_load_does_not_arm_a_stopped_instance() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })],
    );
    let stopped = actor.sheep.get_mut(&0).expect("the fixture registers one");
    stopped.entry.status = ProcStatus::Stopped;
    stopped.entry.pid = None;

    let mut file = AppConfig::minimal("web", "./srv");
    file.cron_restart = Some("*/5 * * * *".to_string());
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "cron_restart"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.registry.group_members("web"),
        None,
        "a load armed a schedule that will start a sheep the operator stopped"
    );
}

/// A plain load skips the count and says why; a `--reset` takes it. The
/// override store cannot tell a stocked count from an untouched one, so a
/// plain load acting on the field would delete instances.
#[tokio::test(start_paused = true)]
async fn a_plain_load_never_scales_and_a_reset_does() {
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.instances = 1;
        declared_app(file, &["name", "script", "instances"])
    };

    let plain_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&plain_dir, &[app_with("web", |app| app.instances = 2)]);
    let reply = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        2,
        "a plain load deleted an instance"
    );
    assert!(!reply[0].applied.contains(&"instances".to_string()));
    assert_eq!(
        reply[0].refused.as_deref(),
        Some(
            "instances: this load never reshapes a flock; no mode scales without also \
             putting back every setting the file declares, and `--reset=file` is the \
             narrowest that does, taking the file's count of 1"
        ),
        "the refusal must name a mode an operator can actually type, \
         not the bare flag `shep start --reset` now refuses on its \
         own, and it must name what following the advice costs: {reply:?}"
    );
    assert_eq!(
        reply[0]
            .app
            .as_ref()
            .expect("an applied app is recorded")
            .config()
            .instances,
        2,
        "the recorded count must be the one really running"
    );

    let reset_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&reset_dir, &[app_with("web", |app| app.instances = 2)]);
    let reply = apply_config(&mut actor, vec![file()], ResetDepth::Policy).await;
    assert_eq!(actor.ids_of_name("web"), vec![0], "--reset must take it");
    assert_eq!(reply[0].applied, vec!["instances".to_string()]);
}

/// All eight [`EXTRAS_FIELDS`] are read when a worker is armed, so a spec
/// write alone leaves the old value enforced for the life of that worker.
/// The observable is the memory ceiling, unchanged across these apps: a
/// re-arm arms every instance, so any arming recorded is proof of one.
#[tokio::test(start_paused = true)]
async fn every_extras_field_triggers_a_rearm() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().display().to_string();
    let ceiling = MemSize::from_bytes(100 << 20);
    // The "before" values every case edits away from.
    let base = |cwd: &str| {
        let cwd = cwd.to_string();
        move |app: &mut AppConfig| {
            app.max_memory = Some(ceiling);
            app.cwd = Some(cwd.clone());
            app.watch = true;
            app.ignore_watch = vec!["target/**".to_string()];
            app.watch_delay = Some(UpDuration::from_millis(1000));
            app.watch_options = vec!["src/**".to_string()];
            app.cron_restart = Some("0 * * * *".to_string());
            app.cron_timezone = Some("UTC".to_string());
            // An interval no paused-clock case advances to, so the probe
            // never runs and this stays about arming.
            app.liveness_probe = Some(ProbeConfig {
                failure_threshold: 3,
                interval: UpDuration::from_millis(600_000),
                timeout: UpDuration::from_millis(1000),
                ..probe_config(ProbeKind::Tcp, "127.0.0.1:1")
            });
        }
    };
    // One edit per entry in `EXTRAS_FIELDS`, named by the field it moves,
    // so a field dropped from that list goes red under its own name.
    type Edit = (&'static str, fn(&mut AppConfig));
    let edits: Vec<Edit> = vec![
        ("max_memory", |app| {
            app.max_memory = Some(MemSize::from_bytes(512 << 20));
        }),
        ("watch", |app| app.watch = false),
        ("ignore_watch", |app| {
            app.ignore_watch = vec!["dist/**".to_string()];
        }),
        ("watch_delay", |app| {
            app.watch_delay = Some(UpDuration::from_millis(2500));
        }),
        ("watch_options", |app| {
            app.watch_options = vec!["lib/**".to_string()];
        }),
        ("cron_restart", |app| {
            app.cron_restart = Some("*/5 * * * *".to_string());
        }),
        ("cron_timezone", |app| {
            app.cron_timezone = Some("Europe/Berlin".to_string());
        }),
        ("liveness_probe", |app| app.liveness_probe = None),
    ];

    for (field, edit) in edits {
        let dir = tempfile::tempdir().unwrap();
        let (mut actor, enforcer) = actor_over(&dir, &[app_with("web", base(&cwd))]);
        let mut file = AppConfig::minimal("web", "./srv");
        base(&cwd)(&mut file);
        edit(&mut file);
        let reply = apply_config(
            &mut actor,
            vec![declared_app(
                file,
                &[
                    "name",
                    "script",
                    "cwd",
                    "max_memory",
                    "watch",
                    "ignore_watch",
                    "watch_delay",
                    "watch_options",
                    "cron_restart",
                    "cron_timezone",
                    "liveness_probe",
                ],
            )],
            ResetDepth::Policy,
        )
        .await;
        assert!(
            reply[0].applied.contains(&field.to_string()),
            "{field} did not apply at all: {reply:?}"
        );
        assert!(
            !enforcer.arms().is_empty(),
            "changing {field} left the armed worker on the old value"
        );
    }
}

/// A name whose instances are all momentarily non-`Online`, each inside a
/// crash-restart backoff, has nothing to arm, and an early return there
/// would skip the teardown too. `disarm_extras` leaves a `WaitingRestart`
/// sheep armed, so it stays a group member and the group is never torn
/// down. Arms first, which makes this a test of the teardown;
/// `a_load_does_not_arm_a_stopped_instance` arms nothing, so its assertion
/// holds either way.
#[tokio::test(start_paused = true)]
async fn a_load_tears_down_a_group_whose_instances_are_all_down() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })],
    );
    actor.arm_extras(0);
    assert_eq!(
        actor.registry.group_members("web"),
        Some(vec![0]),
        "the fixture must really be armed, or this pins nothing"
    );

    // Mid-backoff: no pid, not online, still registered and still a group
    // member.
    let waiting = actor.sheep.get_mut(&0).expect("the fixture registers one");
    waiting.entry.status = ProcStatus::WaitingRestart;
    waiting.entry.pid = None;

    let mut file = AppConfig::minimal("web", "./srv");
    file.cron_restart = Some("*/5 * * * *".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "cron_restart"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(reply[0].applied, vec!["cron_restart".to_string()]);
    assert_eq!(
        actor.registry.group_members("web"),
        None,
        "a worker built from the replaced config survived a load that reported the \
         field as applied"
    );
}

/// The merge normalizes against the count the intended config carries, and
/// the flock can be running a different one, where the same config
/// refuses. The earlier parked config is left alone, still the one a
/// respawn picks up, so the report must not claim this load's fields are
/// in it.
#[tokio::test(start_paused = true)]
async fn a_change_that_cannot_be_parked_is_not_reported_as_parked() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // An earlier load parked a one-instance config. Two are running.
    let earlier = app_with("web", |app| {
        app.instances = 1;
        app.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    });
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }

    // One explicit log path, no `{{instance}}` and no `merge_logs`: legal
    // for the one instance the parked config declares, refused for the
    // two really running.
    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/tmp/web.log".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "out_file"])],
        ResetDepth::None,
    )
    .await;

    assert!(
        !reply[0].pending.contains(&"out_file".to_string()),
        "a field that went nowhere must not be reported as coming: {reply:?}"
    );
    assert!(
        reply[0]
            .refused
            .as_deref()
            .is_some_and(|why| why.contains("out_file")),
        "and the operator must be told which field it was: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("the earlier load's parked config survives")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the earlier load's parked config must not be cleared"
    );
    assert!(
        reply[0].app.is_none(),
        "there is no config that holds at the count really running, so nothing may be \
         recorded: {reply:?}"
    );
}

/// A plain load holds `instances` out of the merge, so the sibling case
/// above does not cover the depth every `shep start` uses. Two running
/// instances plus one shared explicit `out_file` is the reachable shape.
#[tokio::test(start_paused = true)]
async fn a_plain_load_whose_merge_cannot_normalize_refuses_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/tmp/web.log".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "out_file"])],
        ResetDepth::None,
    )
    .await;

    assert!(reply[0].refused.is_some(), "{reply:?}");
    assert!(reply[0].applied.is_empty() && reply[0].pending.is_empty());
    assert!(reply[0].app.is_none());
    assert!(actor.sheep[&0].entry.spec.config().out_file.is_none());
    assert!(actor.sheep[&0].entry.pending.is_none());
    assert_eq!(actor.ids_of_name("web").len(), 2);
}

/// `parked_wanted` is set by an earlier parked config as well as by this
/// load's own `NeedsRespawn` drift, so a load whose only drift is a Live
/// field can fail the rebuild with no field of its own to name. The stale
/// parked config then puts the old value back at the next respawn.
/// Reachable whenever the instance count moved between two loads.
#[tokio::test(start_paused = true)]
async fn a_parked_config_that_cannot_be_rebuilt_is_reported_even_with_no_field_to_name() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // Parked when the app ran one instance: a shared explicit log path is
    // legal for one and refused for the two running now.
    let earlier = app_with("web", |app| {
        app.instances = 1;
        app.max_restarts = 10;
        app.out_file = Some("/tmp/web.log".to_string());
    });
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 99;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(actor.sheep[&0].entry.spec.config().max_restarts, 99);
    assert!(
        reply[0]
            .refused
            .as_deref()
            .is_some_and(|why| why.contains("could not be rebuilt")),
        "a respawn is going to put max_restarts back to 10 and nobody was told: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("the earlier parked config is still there")
            .config()
            .max_restarts,
        10,
        "and that is what makes the refusal true"
    );
}

/// The store's `declared` set is what `ResetDepth::None` skips over, so a
/// refused key entering it makes the refusal's own advice useless: the
/// retry meets silence rather than the same refusal.
#[tokio::test(start_paused = true)]
async fn a_refused_key_is_not_established_so_the_same_file_still_tries() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    let earlier = app_with("web", |app| app.instances = 1);
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.out_file = Some("/tmp/web.log".to_string());
        declared_app(file, &["name", "script", "out_file"])
    };

    let names_it = |why: &str| why.contains("out_file");
    let first = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert!(
        first[0].refused.as_deref().is_some_and(names_it),
        "{first:?}"
    );

    let second = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert!(
        second[0].refused.as_deref().is_some_and(names_it),
        "a retry of the same file must meet the same refusal, not silence: {second:?}"
    );
    assert!(
        !shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .expect("the load recorded what it established")
            .declared
            .contains("out_file"),
        "a key that went nowhere was established by nobody"
    );
}

/// `ResetDepth::None` skips a key somebody has established, so a load that
/// records nothing leaves every key permanently re-writable. Three loads,
/// because two cannot tell the difference: the first establishes the key,
/// the second drops it from the file, and the third re-adds it with a
/// different value, which only a record written by the first can refuse.
#[tokio::test(start_paused = true)]
async fn a_key_a_load_took_is_established_against_the_next_load() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let with_budget = |budget: u32| {
        let mut file = AppConfig::minimal("web", "./srv");
        file.max_restarts = budget;
        declared_app(file, &["name", "script", "max_restarts"])
    };

    let first = apply_config(&mut actor, vec![with_budget(99)], ResetDepth::None).await;
    assert_eq!(first[0].applied, vec!["max_restarts".to_string()]);
    assert_eq!(actor.sheep[&0].entry.spec.config().max_restarts, 99);

    // The key leaves the file. Nothing happens, and nothing may forget
    // that it was established.
    let dropped = apply_config(
        &mut actor,
        vec![declared_app(
            AppConfig::minimal("web", "./srv"),
            &["name", "script"],
        )],
        ResetDepth::None,
    )
    .await;
    assert!(dropped[0].applied.is_empty(), "{dropped:?}");

    // And comes back with a different value. The app has run on 99 since
    // the first load, so a plain load must not take it.
    let third = apply_config(&mut actor, vec![with_budget(5)], ResetDepth::None).await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().max_restarts,
        99,
        "a file overwrote a key an earlier load had established"
    );
    assert!(third[0].applied.is_empty(), "{third:?}");
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .expect("a load records what it established")
            .declared
            .contains("max_restarts"),
        "and the record is what makes that true"
    );
}

/// A key the file declares gives up its override during the merge, so a
/// load that then fails to park has to hand it back. `env` is the one
/// field that reaches this under the default depth: top-level keys hold
/// their override by being established and never spend anything, while
/// `env` merges one key at a time and spends the whole override table for
/// any key the file declares.
#[tokio::test(start_paused = true)]
async fn a_refused_env_change_gives_the_operators_override_back() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // Parked when the app ran one instance, with a shared explicit log
    // path: legal for one, refused for the two running now.
    let earlier = app_with("web", |app| {
        app.instances = 1;
        app.out_file = Some("/tmp/web.log".to_string());
    });
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("env", serde_json::json!({ "OPERATOR": "1" }))],
        ),
    )
    .unwrap();

    // Two env keys: the one the operator already holds, which spends the
    // override table, and one nobody has, which makes `env` drift.
    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([
        ("OPERATOR".to_string(), "2".to_string()),
        ("MODE".to_string(), "blue".to_string()),
    ]);
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        reply[0]
            .refused
            .as_deref()
            .is_some_and(|why| why.contains("env")),
        "the fixture must really refuse the env change: {reply:?}"
    );

    let record = shep_core::overrides::get(&actor.paths.overrides, "web")
        .unwrap()
        .expect("a load records what it established");
    assert_eq!(
        record
            .fields
            .get("env")
            .and_then(|env| env.get("OPERATOR"))
            .and_then(serde_json::Value::as_str),
        Some("1"),
        "a load that changed nothing spent the operator's override"
    );
    assert!(
        record.declared_env.is_empty(),
        "and it established env keys that never landed: {:?}",
        record.declared_env
    );
}

// --- Promotion: a parked config reaching its replacement process ---

// Actor-tier: a promotion moves `spec`, `pending` and `credentials`, and no
// reply reports those three together.

/// `spawn_replacement` carries `restarts`, `dog` and `last_exit` off the
/// drainee on the same grounds: the replacement is the same instance
/// continuing, not a new one.
#[tokio::test(start_paused = true)]
async fn reload_carries_the_overridden_cache_to_its_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    actor.sheep.get_mut(&0).unwrap().entry.overridden = vec!["max_restarts".to_string()];

    actor.advance_reload("web", VecDeque::from([0]));

    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id].entry.overridden,
        vec!["max_restarts".to_string()],
        "the replacement must carry the drainee's own overridden cache, not start blank"
    );
}

/// Without this the pending slot is written and never read, so an operator
/// sees a pending field forever with no way to apply it.
#[tokio::test(start_paused = true)]
async fn reload_promotes_pending_config() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id]
            .entry
            .spec
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the replacement must come up on the config the load parked"
    );
    assert!(
        actor.sheep[&new_id].entry.pending.is_none(),
        "and it is owed nothing further, having been built from what was owed"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the drainee keeps its copy until it is deregistered: this swap can still be \
         abandoned, and the child it would go back to serving has not got the change"
    );
    assert_ne!(
        actor.sheep[&new_id].entry.pid,
        Some(APPLY_FIRST_PID),
        "a promotion is only reachable through a process that actually replaced the old one"
    );
}

/// Both verbs replace the child, so both are chances to apply what is owed.
#[tokio::test(start_paused = true)]
async fn restart_promotes_pending_config() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    // The door `shep restart` takes on a running sheep: `begin_manual`
    // claims the next exit and `handle_exited` respawns. The other door is
    // `apply_immediate`'s `Restart` arm; both end in `respawn`.
    let (reply, _answer) = oneshot::channel();
    actor.begin_manual(
        ProcessSelector::Name("web".to_string()),
        ManualKind::Restart,
        CommandOrigin::Operator,
        ReplyKind::Info(reply),
    );
    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );

    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.spec.config().env.get("MODE").map(String::as_str),
        Some("blue"),
        "the restarted child must come up on the config the load parked"
    );
    assert!(
        entry.pending.is_none(),
        "a promoted config is owed no longer, so the slot must be empty"
    );
    assert_ne!(
        entry.pid,
        Some(APPLY_FIRST_PID),
        "a promotion is only reachable through a process that actually replaced the old one"
    );
}

/// Without this refresh, `to_info` keeps naming a respawned child's old
/// log path forever: `out_file`/`err_file` are `ApplyGroup::NeedsRespawn`,
/// so a restart is the one moment they take effect, and every reader
/// built on `to_info` (`shep describe`, the muster roll) inherits it.
#[tokio::test(start_paused = true)]
async fn restart_refreshes_the_reported_log_paths_from_the_new_spec() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/var/log/moved-out.log".to_string());
    file.err_file = Some("/var/log/moved-err.log".to_string());
    apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "out_file", "err_file"],
        )],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    let (reply, _answer) = oneshot::channel();
    actor.begin_manual(
        ProcessSelector::Name("web".to_string()),
        ManualKind::Restart,
        CommandOrigin::Operator,
        ReplyKind::Info(reply),
    );
    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );

    let after = to_info(&actor.sheep[&0].entry, &actor.smits);
    assert_eq!(
        after.out_file.as_deref(),
        Some("/var/log/moved-out.log"),
        "the restarted child writes to the moved path; the listing must say so"
    );
    assert_eq!(
        after.err_file.as_deref(),
        Some("/var/log/moved-err.log"),
        "and the same for stderr"
    );
}

/// The mirror case: a load parks a moved `out_file`/`err_file`, but the
/// child has not respawned yet and is still appending to the old path.
/// Reporting the parked path early would be this same bug pointed the
/// other way, naming a file nothing writes to yet.
#[tokio::test(start_paused = true)]
async fn a_parked_log_path_change_does_not_reach_the_listing_before_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let logs = actor.paths.logs.clone();

    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/var/log/moved-out.log".to_string());
    file.err_file = Some("/var/log/moved-err.log".to_string());
    apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "out_file", "err_file"],
        )],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    let still_reported = to_info(&actor.sheep[&0].entry, &actor.smits);
    assert_eq!(
        still_reported.out_file.as_deref(),
        logs.join("web-0-out.log").to_str(),
        "the child is still writing to the old path until it respawns"
    );
    assert_eq!(
        still_reported.err_file.as_deref(),
        logs.join("web-0-err.log").to_str(),
        "and the same for stderr"
    );
}

/// A pending field an operator cannot see is a silent divergence.
#[tokio::test(start_paused = true)]
async fn to_info_reports_the_pending_fields_names_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    assert!(
        entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.pending, Some(vec!["env".to_string()]));
}

/// The daemon's one production construction site converts `MemSize` to
/// raw bytes for the wire. The ceiling is chosen off a round megabyte
/// boundary so a unit mix-up (bytes vs. KiB vs. MiB) could not pass by
/// coincidence.
#[tokio::test(start_paused = true)]
async fn to_info_carries_a_sheep_s_configured_memory_ceiling_in_bytes() {
    const CEILING_BYTES: u64 = 43_000_001;
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(CEILING_BYTES));
        })],
    );

    let entry = &actor.sheep[&0].entry;
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.max_memory, Some(CEILING_BYTES));
}

/// The rules reach a client through the listing and nothing else, so a
/// row that drops them leaves the client reading lines the app already
/// explained. Two rules, since order is the contract and one proves no
/// order.
#[tokio::test(start_paused = true)]
async fn to_info_carries_a_sheep_s_declared_level_rules_in_order() {
    let rules = vec![
        LevelRule {
            pattern: r"\[ERROR\]".to_string(),
            level: LineLevel::Error,
        },
        LevelRule {
            pattern: r"\[WARN\]".to_string(),
            level: LineLevel::Warn,
        },
    ];
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| app.level_rules = rules.clone())],
    );

    let entry = &actor.sheep[&0].entry;
    assert_eq!(to_info(entry, &actor.smits).level_rules, rules);
}

/// A dog's `AppConfig::minimal` sets no ceiling, so its `ProcessInfo`
/// must report `None` rather than inheriting a stray value.
#[tokio::test(start_paused = true)]
async fn to_info_reports_none_for_a_dog_with_no_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(&dir, &[dog_app("watcher")]);

    let entry = &actor.sheep[&0].entry;
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.max_memory, None);
}

/// A scale-up calls `overridden_for` once per new instance, so a cache miss
/// costs one locked file read per slot. The store is seeded with a
/// different answer from the sibling's cache, so the sibling winning is the
/// assertion.
#[tokio::test(start_paused = true)]
async fn overridden_for_prefers_a_live_sibling_over_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    actor.sheep.get_mut(&0).unwrap().entry.overridden = vec!["cwd".to_string()];
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("max_restarts", serde_json::json!(9))],
        ),
    )
    .unwrap();

    assert_eq!(actor.overridden_for("web"), vec!["cwd".to_string()]);
}

/// A muster restore and a handover installation both install one sheep at a
/// time, before there is a sibling to ask.
#[tokio::test(start_paused = true)]
async fn overridden_for_reads_the_store_when_no_sibling_exists() {
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(&dir, &[]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("max_restarts", serde_json::json!(9))],
        ),
    )
    .unwrap();

    assert_eq!(
        actor.overridden_for("web"),
        vec!["max_restarts".to_string()]
    );
    assert_eq!(
        actor.overridden_for("nobody-has-heard-of-this-app"),
        Vec::<String>::new(),
        "an unreadable-or-empty answer for a name the store has never seen"
    );
}

/// An override with nothing to show it is a silent divergence, the same
/// class as an unreported `pending`.
#[tokio::test(start_paused = true)]
async fn to_info_reports_the_overridden_field_names_the_store_holds() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |app| app.max_restarts = 7)]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("max_restarts", serde_json::json!(7))],
        ),
    )
    .unwrap();

    let file = AppConfig::minimal("web", "./srv");
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.overridden,
        vec!["max_restarts".to_string()],
        "the cache must mirror what this load wrote back to the override store"
    );
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.overridden, Some(vec!["max_restarts".to_string()]));
}

/// `AppOverrides::fields` is a `serde_json::Map` that can hold anything, so
/// the guarantee is that `Actor::apply_one` and `Actor::overridden_for`
/// extract `.keys()` and never a value. Asserted at the producer, over a
/// store seeded with a secret-shaped value the way `env` arrives there.
#[tokio::test(start_paused = true)]
async fn to_info_never_carries_an_override_value() {
    const SENTINEL: &str = "postgres://sentinel-value-that-must-never-appear";

    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.env = BTreeMap::from([("DATABASE_URL".to_string(), SENTINEL.to_string())]);
        })],
    );
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("env", serde_json::json!({ "DATABASE_URL": SENTINEL }))],
        ),
    )
    .unwrap();

    let file = AppConfig::minimal("web", "./srv");
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    let info = to_info(entry, &actor.smits);
    assert_eq!(
        info.overridden,
        Some(vec!["env".to_string()]),
        "the name must still reach the operator"
    );
    let json = serde_json::to_string(&info).unwrap();
    assert!(
        !json.contains(SENTINEL),
        "an override value reached the wire: {json}"
    );
}

/// `credentials` is resolved once so a restart does not change a running
/// app's identity by accident; an operator editing `user` is the one case
/// that must re-resolve.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn promoting_a_user_change_re_resolves_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.user = Some(own_user_name());
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "user"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    let wanted = Credentials {
        uid: nix::unistd::geteuid().as_raw(),
        gid: None,
    };
    assert_eq!(
        actor.runner.spawned_as(0),
        Some(wanted),
        "the replacement must carry the identity the promoted `user` resolves to; `None` \
         here is the fixture's stale resolution, which is the change being ignored"
    );
    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id].entry.credentials,
        SpawnIdentity::Resolved(Some(wanted)),
        "and the replacement records it, so the restart after this one reuses it"
    );
    assert_eq!(
        actor.sheep[&0].entry.credentials,
        SpawnIdentity::Resolved(None),
        "while the drainee's own identity is untouched: it is still serving under it, and \
         an abandoned swap must not leave it recorded as never looked up"
    );
}

/// Re-resolving on every promotion would mean a passwd lookup per config
/// change, and would defeat the once-only rule.
#[tokio::test(start_paused = true)]
async fn promoting_an_unrelated_change_keeps_the_resolved_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.user = Some(NO_SUCH_USER.to_string())
        })],
    );
    // An unresolvable name makes reuse observable: this value cannot be
    // re-derived, so a spawn carrying it is a spawn that reused it.
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers one instance")
        .entry
        .credentials = SpawnIdentity::Resolved(Some(settled));

    let mut file = AppConfig::minimal("web", "./srv");
    file.args = vec!["--port=8080".to_string()];
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "args"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    assert_eq!(
        actor.runner.spawn_count(),
        1,
        "the replacement must have been spawned at all: an identity re-resolved here could \
         only refuse, and a refusal would abandon the reload"
    );
    assert_eq!(
        actor.runner.spawned_as(0),
        Some(settled),
        "an `args` change is not an identity change, so the replacement runs as whoever the \
         instance was already running as"
    );
    assert_eq!(
        actor.sheep[&0].entry.credentials,
        SpawnIdentity::Resolved(Some(settled)),
        "and the stored resolution is untouched, so no passwd lookup was spent"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the drainee keeps what it was owed; the replacement is what carries it"
    );
    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id].entry.spec.config().args,
        vec!["--port=8080".to_string()],
        "the promotion itself must still have happened"
    );
}

/// `apply_one` derives one spec from `ids_of_name`'s first id, always
/// instance 0, and writes it onto every sibling. A promotion that diffed
/// `pending` against `spec` would find the `user` change instance 1 has not
/// applied already sitting on instance 1's spec. Three loads, not two:
/// by the third, instance 1's spec is already flattened, so a load that
/// recomputed the flag would clear the first load's decision.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_sibling_that_has_not_promoted_yet_still_re_resolves_after_later_loads() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 3)]);
    // The identity the three instances already run under, and one no lookup
    // could produce, so a spawn carrying it is a spawn that reused it.
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    for id in [0, 1, 2] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers three")
            .entry
            .credentials = SpawnIdentity::Resolved(Some(settled));
    }

    let user_change = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.instances = 3;
        file.user = Some(own_user_name());
        vec![declared_app(file, &["name", "script", "instances", "user"])]
    };
    apply_config(&mut actor, user_change(), ResetDepth::None).await;

    // Instance 0 alone: the shape every automatic restart takes.
    actor.respawn(0, true);

    // The same file twice. Each reads its base config off instance 0, which
    // has now promoted, and writes it over instances 1 and 2.
    apply_config(&mut actor, user_change(), ResetDepth::None).await;
    apply_config(&mut actor, user_change(), ResetDepth::None).await;

    actor.respawn(1, true);

    let wanted = Credentials {
        uid: nix::unistd::geteuid().as_raw(),
        gid: None,
    };
    assert_eq!(
        actor.runner.spawned_as(1),
        Some(wanted),
        "instance 1 has still never applied the `user` change, so its promotion must \
         re-resolve; the settled 4242 here is the change being silently dropped"
    );
    assert_eq!(
        actor.sheep[&1].entry.spec.config().user,
        Some(own_user_name()),
        "and its own spec must record what it came up on"
    );
}

/// The drainee goes back to the child it already had, which was never
/// spawned with the parked config, so an entry claiming it with an empty
/// pending slot leaves the next load seeing no drift while the child runs
/// superseded code.
#[tokio::test(start_paused = true)]
async fn an_abandoned_reload_leaves_the_parked_config_where_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    // A live control sender says this instance's task is still there to
    // go back to; the fixture leaves it `None`.
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    actor.sheep.get_mut(&0).expect("the fixture's sheep").ctl = Some(ctl_tx);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;

    actor.advance_reload("web", VecDeque::from([0]));
    actor.handle_reload_deadline("web", actor.reloads["web"].deadline);

    assert!(actor.reloads.is_empty(), "the swap must really be off");
    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.status,
        ProcStatus::Online,
        "the drainee is serving again, so it is the child spawned before the load"
    );
    assert!(
        entry.spec.config().env.is_empty(),
        "and its spec must still describe what that child was spawned from"
    );
    assert_eq!(
        entry
            .pending
            .as_ref()
            .expect("the config is still owed: no child ever came up on it")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue")
    );
}

/// `SpawnIdentity::Unresolved` makes a later spawn resolve from scratch, so
/// for a `user` that has stopped resolving it is a running app whose next
/// restart is refused over the identity it already runs under.
#[tokio::test(start_paused = true)]
async fn an_abandoned_reload_leaves_the_drainees_identity_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers one instance")
        .entry
        .credentials = SpawnIdentity::Resolved(Some(settled));

    // A `user` that cannot resolve, so the reload is abandoned at the one
    // point that runs before anything else in `spawn_replacement`.
    let mut file = AppConfig::minimal("web", "./srv");
    file.user = Some(NO_SUCH_USER.to_string());
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "user"])],
        ResetDepth::None,
    )
    .await;

    actor.advance_reload("web", VecDeque::from([0]));

    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "the fixture must really refuse the replacement, or this case proves nothing"
    );
    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.credentials,
        SpawnIdentity::Resolved(Some(settled)),
        "the drainee is still serving under this identity, so the abandoned swap must not \
         record it as never looked up"
    );
    assert!(
        entry.pending.is_some(),
        "and it is still owed the config that swap was going to bring"
    );
}

/// `readiness_probe` is `NextSpawn` and lands on the stored spec at once,
/// while `wait_ready` is `NeedsRespawn` and parks, so an app moving from
/// channel readiness to an HTTP probe holds both. `wait_ready` wins in
/// `ReadinessSource::of`, so an ordering read from the stored spec says
/// overlap while the replacement comes up probe-gated: two instances on one
/// address, with a probe the drainee answers.
#[tokio::test(start_paused = true)]
async fn a_reload_orders_itself_by_the_config_its_replacement_will_carry() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |app| app.wait_ready = true)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.wait_ready = false;
    file.readiness_probe = Some(probe_config(ProbeKind::Tcp, "127.0.0.1:9"));
    apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "wait_ready", "readiness_probe"],
        )],
        ResetDepth::None,
    )
    .await;
    let entry = &actor.sheep[&0].entry;
    assert!(
        entry.spec.config().wait_ready && entry.spec.config().readiness_probe.is_some(),
        "the fixture must really hold both at once, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    let job = &actor.reloads["web"];
    assert_eq!(
        job.mode,
        ReloadMode::Serial,
        "the replacement is probe-gated, and a probe cannot say which of two overlapping \
         instances answered it"
    );
    assert_eq!(
        job.swap.phase,
        ReloadPhase::DrainFirst,
        "so the drain runs first and nothing is spawned yet"
    );
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "an overlap here would put a second instance on the drainee's address"
    );
}

/// New instances are spawned from the config the old ones are running, read
/// off instance 0, and `spawn_fresh` registers no pending slot, so a
/// `shep stock` during a parking window would leave them on superseded
/// config with nothing saying a restart is due.
#[tokio::test(start_paused = true)]
async fn a_scale_up_carries_the_parked_config_onto_the_instances_it_creates() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "instances", "env"])],
        ResetDepth::None,
    )
    .await;

    // The standalone verb, not the count inside a load: `apply_one` parks
    // onto every slot after its own scale.
    let (reply, mut answer) = oneshot::channel();
    actor.handle_scale("web", 4, reply);
    answer
        .try_recv()
        .expect("handle_scale answers before it returns")
        .expect("the fixture has scripts enough to scale to four");

    for id in [2, 3] {
        assert_eq!(
            actor.sheep[&id]
                .entry
                .pending
                .as_ref()
                .unwrap_or_else(|| panic!("instance {id} must be owed the parked config"))
                .config()
                .env
                .get("MODE")
                .map(String::as_str),
            Some("blue"),
            "an instance created during a parking window is owed the same config as its \
             siblings"
        );
        assert!(
            actor.sheep[&id].entry.spec.config().env.is_empty(),
            "and its own spec still describes what it was actually spawned from"
        );
    }
}

/// A parked config copied verbatim leaves every slot holding
/// `pending.instances = 2` against a spec of 4, so `drifted_fields` reports
/// `instances` pending forever and the reload that promotes writes the
/// count back down.
#[tokio::test(start_paused = true)]
async fn a_scale_updates_the_count_inside_the_config_it_carries() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "instances", "env"])],
        ResetDepth::None,
    )
    .await;

    let (reply, mut answer) = oneshot::channel();
    actor.handle_scale("web", 4, reply);
    answer
        .try_recv()
        .expect("handle_scale answers before it returns")
        .expect("the fixture has scripts enough to scale to four");

    for id in 0..4 {
        let entry = &actor.sheep[&id].entry;
        assert_eq!(
            entry
                .pending
                .as_ref()
                .unwrap_or_else(|| panic!("instance {id} must be owed the parked config"))
                .config()
                .instances,
            4,
            "the count a scale achieved, not the one an earlier load parked"
        );
        assert!(
            !to_info(entry, &actor.smits)
                .pending
                .unwrap_or_default()
                .contains(&"instances".to_string()),
            "a reload owes this instance nothing about the count"
        );
    }
}

/// The `Live` fields ride across on the carried `AppConfig`; everything
/// parked would vanish, and the next load would compare against a spec that
/// already matched. Asserted through a promotion, since a config that
/// arrives without its flag promotes on the identity the flag exists to
/// replace, and only a spawn shows that.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_parked_config_and_its_reset_decision_survive_a_handover() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[]);

    // The predecessor's entry: registered, not running, owed a `user`
    // change, and settled on an identity no lookup could produce.
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    let entry = ProcessEntry {
        id: 7,
        spec: app_with("web", |_| {}),
        pending: Some(app_with("web", |app| app.user = Some(own_user_name()))),
        pending_reidentifies: true,
        overridden: Vec::new(),
        instance: 0,
        status: ProcStatus::Stopped,
        pid: None,
        restarts: 0,
        started_at: None,
        budget: RestartBudget::default(),
        reload: ReloadState::None,
        credentials: SpawnIdentity::Resolved(Some(settled)),
        out_file: PathBuf::new(),
        err_file: PathBuf::new(),
        dog: None,
        last_exit: None,
    };
    let carried =
        CarriedSheep::from_entry(&entry, 0, CarriedFds::none(), false, None, false, None);

    // Through serde, the boundary a handover crosses: an accessor reading
    // the source entry proves nothing about the blob.
    let crossed: CarriedSheep = serde_json::from_value(serde_json::to_value(&carried).unwrap())
        .expect("this daemon reads what it writes");

    actor
        .install_adopted(without_handles(crossed), &Arc::new(AdoptedReaper::new()))
        .expect("a registered-and-stopped sheep installs with nothing to adopt");

    assert!(
        actor.sheep[&7].entry.pending.is_some(),
        "the successor must still owe this sheep the change its predecessor parked"
    );

    actor.respawn(7, true);

    assert_eq!(
        actor.runner.spawned_as(0),
        Some(Credentials {
            uid: nix::unistd::geteuid().as_raw(),
            gid: None,
        }),
        "and promoting it must re-resolve: the settled 4242 here is the reset decision \
         lost in the blob, which is the identity change silently dropped"
    );
    assert_eq!(
        actor.sheep[&7].entry.spec.config().user,
        Some(own_user_name()),
        "and the promoted config is what the successor now records"
    );
}

/// A secret nobody has set is a person's to fix, so a [`BatchPolicy::PerApp`]
/// batch leaves the sheep `Errored` at once rather than spawning it. A
/// restart ladder in front of it would only postpone the same report by
/// sixteen turns.
///
/// `PerApp` because that is the policy under which such an app is still
/// registered: a boot restore, or a dog.
/// `a_batch_with_one_unresolvable_secret_registers_none_of_it` is the
/// other half.
#[tokio::test(start_paused = true)]
async fn a_key_nobody_has_set_errors_the_sheep_without_a_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:ABSENT}}".to_string());

    let err = actor
        .do_start(
            vec![normalize(app).unwrap()],
            None,
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )
        .unwrap_err();

    assert!(matches!(err, SupervisorError::SpawnFailed(_)), "{err:?}");
    let rendered = err.to_string();
    assert!(
        rendered.contains("ABSENT"),
        "names the reference: {rendered}"
    );
    let entry = &actor.sheep.values().next().expect("one row").entry;
    assert_eq!(entry.status, ProcStatus::Errored);
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may reach the runner"
    );
}

/// A store this build cannot read refuses every reference with the same
/// words a key nobody set does, which sends an operator to `shep secret
/// set` for a file that is corrupt or newer than this build. The empty
/// view it falls back to has to say so on its way past.
#[test]
fn an_unreadable_store_warns_before_it_falls_back_to_an_empty_view() {
    let dir = tempfile::tempdir().unwrap();
    let actor = actor_with_an_empty_flock(&dir, vec![]);
    std::fs::write(&actor.paths.secrets, "{ not json").unwrap();
    let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();

    let logs = capture_logs(|| {
        let view = actor.secret_view(&app);
        let reference = shep_core::secrets::SecretRef {
            namespace: None,
            key: "K",
        };
        assert!(
            matches!(
                view.resolve(&reference),
                shep_core::secrets::Resolution::MissingKey
            ),
            "the fallback is an empty view, not a failed spawn"
        );
    });

    assert!(logs.contains("WARN"), "loud enough to read: {logs}");
    assert!(logs.contains("secrets.json"), "names the file: {logs}");
}

/// A namespace no provider dog has pushed to clears itself, so the sheep
/// waits on the ordinary ladder instead of erroring. Collapsing this into
/// the case above would strand an app whose provider is merely late.
#[tokio::test(start_paused = true)]
async fn a_namespace_no_dog_has_pushed_to_leaves_the_sheep_waiting() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:vault/K}}".to_string());

    let started = handle
        .start(vec![normalize(app).unwrap()])
        .await
        .expect("a provider that has not reported yet is not a failed start");

    assert_eq!(started[0].status, ProcStatus::WaitingRestart);
    assert_eq!(handle.list().await[0].status, ProcStatus::WaitingRestart);
    assert_eq!(runner.spawn_count(), 0, "nothing may reach the runner");
}

/// A push carries one `(namespace, environment)` pair, so a provider
/// that has done `production` and not yet `staging` has said nothing
/// about staging. Keying the refusal on the namespace alone `Errored`s
/// a staging sheep permanently the moment the first push lands, which is
/// the ordinary shape for a provider dog polling one environment at a
/// time, and the cache makes it survive a reboot.
#[tokio::test(start_paused = true)]
async fn a_namespace_pushed_for_another_environment_leaves_the_sheep_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    actor
        .provider_secrets
        .put(
            "vercel",
            "production",
            BTreeMap::from([("API_KEY".to_string(), "sk_live".to_string())]),
            false,
        )
        .expect("no cache is written with persist off");
    let mut app = AppConfig::minimal("web", "./srv");
    app.environment = Some("staging".to_string());
    app.env
        .insert("K".to_string(), "{{secret:vercel/API_KEY}}".to_string());

    actor
        .do_start(
            vec![normalize(app).unwrap()],
            None,
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )
        .expect("a provider that has not pushed staging yet is not a failed start");

    let entry = &actor.sheep.values().next().expect("one row").entry;
    assert_eq!(entry.status, ProcStatus::WaitingRestart);
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may reach the runner"
    );
}

/// The other half, which must stay permanent: the pair has been pushed
/// and the key is not in it, so the provider genuinely does not have it
/// and sixteen retries would report the same thing sixteen turns later.
#[tokio::test(start_paused = true)]
async fn a_key_absent_from_a_pushed_pair_errors_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    actor
        .provider_secrets
        .put(
            "vercel",
            "production",
            BTreeMap::from([("OTHER".to_string(), "1".to_string())]),
            false,
        )
        .expect("no cache is written with persist off");
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("K".to_string(), "{{secret:vercel/API_KEY}}".to_string());

    let err = actor
        .do_start(
            vec![normalize(app).unwrap()],
            None,
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )
        .unwrap_err();

    assert!(matches!(err, SupervisorError::SpawnFailed(_)), "{err:?}");
    let entry = &actor.sheep.values().next().expect("one row").entry;
    assert_eq!(entry.status, ProcStatus::Errored);
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may reach the runner"
    );
}

/// The ladder is bounded by the same budget a crash loop spends, so a
/// provider that never arrives ends as an error rather than retrying for
/// the daemon's life.
#[tokio::test(start_paused = true)]
async fn a_namespace_that_never_arrives_errors_once_the_budget_runs_out() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:vault/K}}".to_string());
    // Two turns, and a fixed wait so the clock below knows what to skip.
    app.max_restarts = 2;
    app.restart_delay = Some(UpDuration::from_millis(100));

    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert_eq!(handle.list().await[0].status, ProcStatus::WaitingRestart);

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        handle.list().await[0].status,
        ProcStatus::Errored,
        "the second refusal exhausts the budget"
    );
}

/// The store is read from disk at the spawn, so a value set before the
/// start reaches the child without a daemon restart.
#[tokio::test(start_paused = true)]
async fn a_value_in_the_store_lets_the_sheep_start() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    shep_core::secrets::set(&paths.secrets, "DB_PASSWORD", "production", "hunter2").unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), paths, events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:DB_PASSWORD}}".to_string());

    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
    assert_eq!(runner.spawn_count(), 1);
}

/// A sheep's own `environment` picks which slot it reads, and there is no
/// fallback to another named one: a `staging` value must not answer for a
/// `production` sheep.
#[tokio::test(start_paused = true)]
async fn a_sheeps_environment_decides_which_value_it_reads() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    shep_core::secrets::set(&paths.secrets, "K", "staging", "v").unwrap();
    let handle = spawn_supervisor(runner, paths, events);
    let templated = |name: &str, environment: Option<&str>| {
        let mut app = AppConfig::minimal(name, "./srv");
        app.environment = environment.map(str::to_string);
        app.env.insert("K".to_string(), "{{secret:K}}".to_string());
        normalize(app).unwrap()
    };

    handle
        .start(vec![templated("staged", Some("staging"))])
        .await
        .expect("the staging slot holds a value");
    let err = handle
        .start(vec![templated("live", None)])
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("production"),
        "the host default is what the second one asked for: {err}"
    );
}

/// `environment` is a `NeedsRespawn` field and a promotion rewrites one
/// slot at a time, so two instances of a name can hold different
/// environments at once. Each prober resolves against its own slot.
#[test]
fn a_rearm_resolves_each_instance_against_its_own_environment() {
    let dir = tempfile::tempdir().unwrap();
    let actor = actor_with_an_empty_flock(&dir, vec![]);
    shep_core::secrets::set(&actor.paths.secrets, "K", "production", "live").unwrap();
    shep_core::secrets::set(&actor.paths.secrets, "K", "staging", "rehearsal").unwrap();
    // `armed_entry` assembles against an empty view, which a templated
    // app cannot resolve, so the reference goes on afterwards.
    let entry_of = |id: u32, environment: &str| {
        let mut entry = armed_entry(id, id, 4300 + id, app_with("web", |_| {}), &actor.paths);
        entry.spec = app_with("web", |app| {
            app.environment = Some(environment.to_string());
            app.env.insert("K".to_string(), "{{secret:K}}".to_string());
        });
        entry
    };
    let promoted = entry_of(0, "staging");
    let waiting = entry_of(1, "production");

    let specs = actor.rearm_specs(&[&promoted, &waiting]);

    assert_eq!(
        specs[&0].env.get("K").map(String::as_str),
        Some("rehearsal"),
        "the promoted instance reads its own staging slot: {:?}",
        specs[&0]
    );
    assert_eq!(
        specs[&1].env.get("K").map(String::as_str),
        Some("live"),
        "the instance still on the old config keeps production: {:?}",
        specs[&1]
    );
}
/// [`BatchPolicy::AllOrNothing`] promises to register none of a batch it
/// cannot start whole, and a reference nobody has set is as knowable
/// before the batch as a missing binary is. Refusing it only at the spawn
/// leaves every app ahead of it in the file running.
#[tokio::test(start_paused = true)]
async fn a_batch_with_one_unresolvable_secret_registers_none_of_it() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    let sound = normalize(AppConfig::minimal("first", "./srv")).unwrap();
    let mut broken = AppConfig::minimal("second", "./srv");
    broken
        .env
        .insert("PW".to_string(), "{{secret:TYPO}}".to_string());

    let err = handle
        .start(vec![sound, normalize(broken).unwrap()])
        .await
        .unwrap_err();

    assert!(
        handle.list().await.is_empty(),
        "the app ahead of the refusal must not survive the batch"
    );
    assert_eq!(runner.spawn_count(), 0, "nothing may reach the runner");
    assert!(matches!(err, SupervisorError::CannotStart(_)), "{err:?}");
    assert!(
        err.to_string().contains("TYPO"),
        "names the reference: {err}"
    );
}

/// A started `web` and a registered dog, for the batch tests below. Two
/// scripts because the dog is a spawn of its own.
async fn env_batch_harness() -> Harness {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    start_app(&h, AppConfig::minimal("web", "./srv")).await;
    h.ctx
        .supervisor
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .unwrap();
    h
}

/// The stored env of `name`, or a panic naming what was there instead.
fn stored_env(h: &Harness, name: &str) -> serde_json::Map<String, serde_json::Value> {
    let record = shep_core::overrides::get(&h.ctx.paths.overrides, name)
        .unwrap()
        .expect("an override record");
    record.fields["env"]
        .as_object()
        .expect("a flat env object")
        .clone()
}

#[tokio::test(start_paused = true)]
async fn a_batch_writes_every_key_under_one_lock() {
    let h = env_batch_harness().await;
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A", "B"]);
    assert!(batch.collisions.is_empty());
    let env = stored_env(&h, "web");
    assert_eq!(env["A"], "1");
    assert_eq!(env["B"], "2");
}

#[tokio::test(start_paused = true)]
async fn an_identical_value_is_unchanged_rather_than_a_collision() {
    let h = env_batch_harness().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    h.ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries.clone(), false, false)
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries, false, false)
        .await
        .unwrap()
        .expect("web exists");
    assert!(batch.set.is_empty());
    assert_eq!(batch.unchanged, ["A"]);
    assert!(batch.collisions.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_collision_without_force_writes_nothing_at_all() {
    let h = env_batch_harness().await;
    h.ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "2".to_string()),
                ("B".to_string(), "9".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.collisions, ["A"]);
    assert!(batch.set.is_empty());
    assert!(batch.app.is_none());
    let env = stored_env(&h, "web");
    assert_eq!(env["A"], "1", "the colliding key kept its value");
    assert!(
        !env.contains_key("B"),
        "the clean key was not written either"
    );
}

#[tokio::test(start_paused = true)]
async fn force_overwrites_and_reports_the_collision_in_both_lists() {
    let h = env_batch_harness().await;
    h.ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "2".to_string())]),
            true,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A"]);
    assert_eq!(batch.collisions, ["A"]);
    assert_eq!(stored_env(&h, "web")["A"], "2");
}

#[tokio::test(start_paused = true)]
async fn a_dry_run_answers_and_writes_nothing() {
    let h = env_batch_harness().await;
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            true,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A"]);
    assert!(batch.app.is_none());
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "a dry run left a store behind"
    );
}

/// A preview that does not match the outcome is worse than no preview:
/// `normalize` is this door's only validation, so a dry run that skipped
/// it would report `SHEP_NAME` as `set` and then fail on the real send,
/// after the caller had acted on the preview.
#[tokio::test(start_paused = true)]
async fn a_dry_run_refuses_what_the_real_send_would_refuse() {
    let h = env_batch_harness().await;
    let err = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("SHEP_NAME".to_string(), "nope".to_string())]),
            false,
            true,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::InvalidEnv(_)), "{err:?}");
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "a refused dry run left a store behind"
    );
}

/// A refused batch changes nothing, so validating the merged config is
/// moot and the collision report is the whole answer.
#[tokio::test(start_paused = true)]
async fn a_refused_collision_reports_rather_than_validating() {
    let h = env_batch_harness().await;
    h.ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "2".to_string()),
                ("SHEP_NAME".to_string(), "nope".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.collisions, ["A"]);
    assert!(batch.set.is_empty());
    assert!(batch.app.is_none());
}

/// The contract says `app` is `Some` only when something was written.
/// A batch every key of which is already held writes nothing, so
/// `rpc.rs` must not record a no-op and rewrite the muster roll for it.
#[tokio::test(start_paused = true)]
async fn a_batch_that_changes_nothing_parks_nothing() {
    let h = env_batch_harness().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    h.ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries.clone(), false, false)
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries, false, false)
        .await
        .unwrap()
        .expect("web exists");
    assert!(batch.set.is_empty());
    assert_eq!(batch.unchanged, ["A"]);
    assert!(batch.app.is_none(), "nothing was written to record");
}

#[tokio::test(start_paused = true)]
async fn a_batch_refuses_a_dog_and_an_unknown_name() {
    let h = env_batch_harness().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    assert!(
        h.ctx
            .supervisor
            .set_sheep_env_batch("absent".to_string(), entries.clone(), false, false)
            .await
            .unwrap()
            .is_none()
    );
    let err = h
        .ctx
        .supervisor
        .set_sheep_env_batch("bark".to_string(), entries, false, false)
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::IsADog(_)));
}
