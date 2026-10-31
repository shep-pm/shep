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
use crate::fake::{ProcScript, ScriptedRunner, idle_sweep};
use crate::testing::capture_logs;
use crate::testing::{
    Harness, RecordingEnforcer, ScriptedProber, SharedRunner, app_with, armed_entry, harness,
    idle_stats, plain_entry, probe_config, test_paths,
};
#[cfg(unix)]
use crate::tokio_runner::TokioRunner;
// aliased: `Ordering` in this module means `cmp::Ordering`
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

// --- module tree ---
mod action_fixtures;
mod actions;
#[cfg(unix)]
mod adopt;
#[cfg(unix)]
mod adopt_fixtures;
#[cfg(unix)]
mod adopt_restart;
#[cfg(unix)]
mod adopt_swap;
mod config_fixtures;
mod config_load;
mod config_promote;
mod config_rearm;
mod config_report;
mod config_reset;
mod credentials;
mod disable_in_flight;
mod disabled_respawn;
mod dogs;
mod dogs_at_rest;
mod env_batch;
mod flush;
#[cfg(unix)]
mod handover;
mod interleaving;
mod lamb_sweep;
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
/// The bus receiver is dropped here, as every fixture already dropped its own:
/// a bus with no subscriber still takes every send.
///
/// `next_id` is one past the slots, which the contiguous ids every fixture
/// assigns make right. A case that needs another value writes it over this.
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
        dog_defaults: DogDefaults::default(),
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
        disabled: BTreeSet::new(),
    }
}

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

/// What [`started`] and [`started_apps`] hand back: the handle, the runner
/// behind it, one bus subscriber.
type Started = (
    SupervisorHandle,
    Arc<ScriptedRunner>,
    tokio::sync::broadcast::Receiver<SharedEvent>,
);

/// The apps in `apps`, all started through one `Start`, their runner and a
/// bus subscriber.
///
/// `bus_capacity` is a parameter, not one size for every case: the cases that
/// read a whole run off one receiver, `shutdown.rs`'s `drain_kinds`, stop at
/// the first `Lagged`. The runner is shared rather than moved so a case can
/// read `kill_counts().len()`, the number of spawns that succeeded.
async fn started_apps(
    dir: &tempfile::TempDir,
    apps: Vec<AppConfig>,
    scripts: Vec<ProcScript>,
    bus_capacity: usize,
) -> Started {
    let (events, rx) = crate::bus::test_bus(bus_capacity);
    let runner = Arc::new(ScriptedRunner::new(scripts));
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(dir), events);
    let apps = apps
        .into_iter()
        .map(|app| normalize(app).unwrap())
        .collect::<Vec<_>>();
    handle.start(apps).await.unwrap();
    (handle, runner, rx)
}

/// One started app: [`started_apps`] on one app, at the bus size no
/// single-app case has outgrown.
async fn started(dir: &tempfile::TempDir, app: AppConfig, scripts: Vec<ProcScript>) -> Started {
    started_apps(dir, vec![app], scripts, 256).await
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

/// Hands the fixture's sheep a live `ctl` sender, as a running instance has.
///
/// The receiver comes back so the caller keeps the mailbox open: bind it,
/// never `let _ =` it, or the drain's send lands on a closed channel.
fn attach_ctl(actor: &mut Actor<ScriptedRunner>) -> mpsc::Receiver<SheepCtl> {
    let (ctl_tx, ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    actor.sheep.get_mut(&0).expect("the fixture's sheep").ctl = Some(ctl_tx);
    ctl_rx
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
        (DOG_ID, "bark", Some(DogSource::BuiltIn { grants: None })),
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
        Some(DogSource::BuiltIn { grants: None })
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
// --- a swap in flight, carried across the exec ----------------------

// --- Promotion: a parked config reaching its replacement process ---

// Actor-tier: a promotion moves `spec`, `pending` and `credentials`, and no
// reply reports those three together.
