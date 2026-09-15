//! Tests for the lifecycle extras: the registry, the reporter, the actor
//! tier, `rearm_name`, and the real-time `slow` tier, in that order below.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

use super::*;
use crate::bus::SharedEvent;
use crate::cron::DEFAULT_MAX_CRON_SLEEP;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::limits::PollingEnforcer;
use crate::limits::sample::ProcessRss;
use crate::probes::ProbeFailure;
use crate::supervisor::spawn_supervisor;
use crate::testing::{
    ArmCall, Harness, RecordingEnforcer, ScriptedProber, ScriptedSampler, TestClock, app_with,
    armed_entry, capture_logs, harness, harness_with_extras, idle_stats, probe_config,
    test_paths, touch,
};
use crate::watch::real_time;
use shep_core::config::{ProbeConfig, ProbeKind};
use shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo};
use shep_core::selector::ProcessSelector;
use shep_core::status::ProcStatus;
use shep_core::values::{MemSize, UpDuration};

/// Generous bound on how long a test may wait on the paused tokio clock.
/// Costs no real time: the runtime auto-advances here only if nothing else
/// becomes ready first.
const EVENT_WAIT: Duration = Duration::from_secs(120);

/// Spans a whole hourly cron occurrence and then some, so a negative
/// assertion crosses the occurrence and makes its claim in one call.
const PAST_THE_NEXT_OCCURRENCE: Duration = Duration::from_secs(3_700);

/// How long a real-clock test waits for a liveness report that should
/// arrive. Generous enough that a loaded runner cannot flake it.
const LIVENESS_DEADLINE: Duration = Duration::from_secs(10);

/// The shortest interval `spawn_liveness_task` honours. A literal, because
/// `probes`' own floor is private to that module.
const PROBE_INTERVAL: UpDuration = UpDuration::from_millis(1_000);

fn dt(s: &str) -> DateTime<Utc> {
    s.parse().expect("valid RFC3339 timestamp")
}

/// The registry-tier fixture: paused clock, recording enforcer, and both
/// report receivers held by the test rather than by a reporter.
struct Rig {
    extras: Extras,
    enforcer: Arc<RecordingEnforcer>,
    clock: Arc<TestClock>,
    liveness: mpsc::Receiver<LivenessReport>,
    _breaches: mpsc::Receiver<LimitBreach>,
}

fn rig(max_cron_sleep: Duration) -> Rig {
    let clock = Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z")));
    let enforcer = Arc::new(RecordingEnforcer::default());
    let (breach_tx, breaches) = mpsc::channel(8);
    let (live_tx, liveness) = mpsc::channel(8);
    Rig {
        extras: Extras {
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            enforcer: Arc::clone(&enforcer) as Arc<dyn LimitEnforcer>,
            max_cron_sleep,
            reports: ExtrasReports {
                breaches: breach_tx,
                liveness: live_tx,
            },
            stats: idle_stats(),
        },
        enforcer,
        clock,
        liveness,
        _breaches: breaches,
    }
}

/// One supervisor engine over a scripted runner with enough `never_exits`
/// procs that no negative assertion passes because the script ran out: an
/// exhausted script makes the supervisor emit `Errored`.
fn spawn_test_fixture() -> (
    SupervisorHandle,
    broadcast::Receiver<SharedEvent>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let (events, rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 12]);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    (handle, rx, dir)
}

/// A prober that never fails: the neutral value for a case that arms no
/// liveness probe but must still hand `arm` one.
fn idle_prober() -> Arc<dyn Prober> {
    Arc::new(ScriptedProber::new(vec![]))
}

/// A prober that fails every probe, so a case asserting silence against one
/// is asserting the loop is gone.
fn failing_prober() -> Arc<dyn Prober> {
    Arc::new(ScriptedProber::new(vec![Err(ProbeFailure::Timeout)]))
}

/// Yielding rather than advancing: an `advance` would resolve other timers.
async fn settle_finished(task: &JoinHandle<()>) {
    for _ in 0..100 {
        if task.is_finished() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the task never finished");
}

async fn expect_restart(
    rx: &mut broadcast::Receiver<SharedEvent>,
    name: &str,
    window: Duration,
) -> ProcessInfo {
    expect_restart_event(rx, name, window).await.0
}

/// [`expect_restart`], plus the `manually` flag the bus put on that
/// restart.
async fn expect_restart_event(
    rx: &mut broadcast::Receiver<SharedEvent>,
    name: &str,
    window: Duration,
) -> (ProcessInfo, bool) {
    let restart = async {
        loop {
            match rx.recv().await.map(|event| event.to_event()) {
                Ok(BusEvent::Process {
                    event: ProcessEventKind::Restart,
                    info,
                    manually,
                    ..
                }) if info.name == name => return (info, manually),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(err) => panic!("event stream closed before a restart of {name}: {err}"),
            }
        }
    };
    match tokio::time::timeout(window, restart).await {
        Ok(observed) => observed,
        Err(_) => panic!("timed out waiting for a restart of {name}"),
    }
}

/// A bounded `timeout` + `recv`, never a bare `try_recv`: the window carries
/// the paused clock past the occurrence the abort was supposed to stop.
async fn assert_no_restart_within(
    rx: &mut broadcast::Receiver<SharedEvent>,
    name: &str,
    window: Duration,
) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv())
            .await
            .map(|received| received.map(|event| event.to_event()))
        {
            Err(_) => return, // window elapsed with nothing matching
            Ok(Ok(BusEvent::Process {
                event: ProcessEventKind::Restart,
                info,
                ..
            })) if info.name == name => {
                panic!(
                    "unexpected restart of {name} observed (restarts={})",
                    info.restarts
                );
            }
            Ok(Ok(_)) => continue,
            // A negative assertion cannot skip events: a dropped one may be
            // the `Restart` this forbids. `expect_restart` may skip them,
            // since a lag costs it only a timeout.
            Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                panic!(
                    "event stream lagged by {skipped} while checking for no restart of \
                     {name}: a skipped event may have been the restart this forbids"
                )
            }
            Ok(Err(err)) => {
                panic!("event channel closed while checking for no restart of {name}: {err}")
            }
        }
    }
}

async fn expect_liveness(
    rx: &mut mpsc::Receiver<LivenessReport>,
    window: Duration,
) -> LivenessReport {
    match tokio::time::timeout(window, rx.recv()).await {
        Ok(Some(failure)) => failure,
        Ok(None) => panic!("the liveness channel closed before a failure arrived"),
        Err(_) => panic!("timed out waiting for a liveness failure"),
    }
}

async fn assert_no_liveness_within(rx: &mut mpsc::Receiver<LivenessReport>, window: Duration) {
    match tokio::time::timeout(window, rx.recv()).await {
        Err(_) => {} // window elapsed with nothing arriving
        Ok(Some(failure)) => panic!("unexpected liveness failure observed: {failure:?}"),
        Ok(None) => panic!("the liveness channel disconnected while checking for silence"),
    }
}

/// Crosses one hourly occurrence in steps far finer than either sleep cap,
/// so the worker's own cadence decides how often it wakes.
async fn cross_one_hour() {
    for _ in 0..120 {
        tokio::time::advance(Duration::from_secs(30)).await;
    }
}

/// `arm` keeps a live cron or watch task, which is right for a reload's
/// overlap and wrong for a config change: those tasks read their
/// group-scoped config when they are built.
#[tokio::test(start_paused = true)]
async fn rearm_name_replaces_a_live_group_task() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    let entry = armed_entry(0, 0, 1000, app.clone(), &paths);
    registry.arm(&entry, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let before_cron = registry.groups["web"].cron.as_ref().unwrap().abort_handle();
    let before_watch = registry.groups["web"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();

    registry.rearm_name("web", &[&entry], |_| idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let after_cron = registry.groups["web"].cron.as_ref().unwrap().abort_handle();
    let after_watch = registry.groups["web"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();
    assert_ne!(
        before_cron.id(),
        after_cron.id(),
        "the cron worker survived a rearm"
    );
    assert_ne!(
        before_watch.id(),
        after_watch.id(),
        "the watch task survived a rearm"
    );
}

/// An app left with no watcher at all is worse than one left with a stale
/// watcher.
#[tokio::test(start_paused = true)]
async fn rearm_name_leaves_the_group_armed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    let entry = armed_entry(0, 0, 1000, app.clone(), &paths);
    registry.arm(&entry, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    registry.rearm_name("web", &[&entry], |_| idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let group = &registry.groups["web"];
    assert!(
        group.cron.as_ref().is_some_and(|cron| !cron.is_finished()),
        "the group must still have a live cron worker after a rearm"
    );
    assert!(
        group
            .watch
            .as_ref()
            .is_some_and(|watch| !watch.is_finished()),
        "the group must still have a live watch task after a rearm"
    );
}

#[tokio::test(start_paused = true)]
async fn rearm_name_leaves_another_apps_group_alone() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let web_root = tempfile::tempdir().unwrap();
    let worker_root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let web = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(web_root.path().display().to_string());
    });
    let worker = app_with("worker", |app| {
        app.watch = true;
        app.cwd = Some(worker_root.path().display().to_string());
    });
    handle
        .start(vec![web.clone(), worker.clone()])
        .await
        .unwrap();

    let web_entry = armed_entry(0, 0, 1000, web.clone(), &paths);
    let worker_entry = armed_entry(1, 0, 1001, worker.clone(), &paths);
    registry.arm(&web_entry, idle_prober(), &rig.extras, &handle);
    registry.arm(&worker_entry, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let worker_watch_before = registry.groups["worker"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();

    registry.rearm_name(
        "web",
        &[&web_entry],
        |_| idle_prober(),
        &rig.extras,
        &handle,
    );
    tokio::task::yield_now().await;

    let worker_watch_after = registry.groups["worker"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();
    assert_eq!(
        worker_watch_before.id(),
        worker_watch_after.id(),
        "rearming \"web\" must not touch \"worker\"'s group"
    );
}

/// One group shared across a name's instances is only transitively protected
/// by `arm`'s own idempotency test, since `rearm_name` builds no task
/// itself.
#[tokio::test(start_paused = true)]
async fn rearm_name_rebuilds_a_multi_instance_group_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.instances = 2;
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    let entry_a = armed_entry(0, 0, 1000, app.clone(), &paths);
    let entry_b = armed_entry(1, 1, 1001, app.clone(), &paths);
    registry.arm(&entry_a, idle_prober(), &rig.extras, &handle);
    registry.arm(&entry_b, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let before_cron = registry.groups["web"].cron.as_ref().unwrap().abort_handle();
    let before_watch = registry.groups["web"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();

    // The prober closure is the seam that pins one prober per entry:
    // `assemble` bakes `SHEP_INSTANCE` into the environment a prober runs
    // with, so a shared one would probe every instance as one. The call
    // count and order are what is observable here.
    let probed = std::sync::Mutex::new(Vec::new());
    registry.rearm_name(
        "web",
        &[&entry_a, &entry_b],
        |entry| {
            probed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(entry.id);
            idle_prober()
        },
        &rig.extras,
        &handle,
    );
    tokio::task::yield_now().await;

    assert_eq!(
        probed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        [0, 1],
        "each instance must get its own prober, in id order"
    );

    let group = &registry.groups["web"];
    assert_eq!(
        group.members,
        HashSet::from([0, 1]),
        "both instances must still be members after a rearm, not just the last one arm'd"
    );
    assert!(
        group.cron.is_some(),
        "the group must hold exactly one cron task, not zero"
    );
    assert!(
        group.watch.is_some(),
        "the group must hold exactly one watch task, not zero"
    );
    let after_cron = group.cron.as_ref().unwrap().abort_handle();
    let after_watch = group.watch.as_ref().unwrap().abort_handle();
    assert_ne!(
        before_cron.id(),
        after_cron.id(),
        "the cron worker must be rebuilt by the rearm, not left over"
    );
    assert_ne!(
        before_watch.id(),
        after_watch.id(),
        "the watch task must be rebuilt by the rearm, not left over"
    );
}

/// Tests that wait on real filesystem events or real elapsed time. The
/// inner loop skips them with `--skip ::slow::`; the full suite runs them.
mod slow {
    use super::*;

    // The overlap a reload runs on: the replacement arms before the
    // drainee's exit disarms the old id, so `disarm` finds a member still
    // standing. Task identity tells a surviving group from a rebuilt one, and
    // the two `AbortHandle`s are held unfired so tokio cannot reuse the id.
    #[tokio::test(start_paused = true)]
    async fn a_replacement_arming_before_the_drainee_disarms_keeps_the_groups_own_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let root = tempfile::tempdir().unwrap();
        let (handle, _rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        // Both per-name extras, because the overlap has to hold for both.
        let app = app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
        });
        handle.start(vec![app.clone()]).await.unwrap();

        // The drainee: id 0, holding instance slot 0.
        registry.arm(
            &armed_entry(0, 0, 1000, app.clone(), &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );
        // Lets the cron worker reach its first poll, so the reading below
        // is settled rather than racing it.
        tokio::task::yield_now().await;

        let group = &registry.groups["web"];
        let cron = group
            .cron
            .as_ref()
            .expect("fixture check: the cron worker must have armed")
            .abort_handle();
        let watch = group
            .watch
            .as_ref()
            .expect("fixture check: the watch must have armed")
            .abort_handle();
        let reads_before = rig.clock.reads();

        // The overlap, in the order a swap performs it: the replacement
        // takes a new id in the drainee's slot and goes `Online` first.
        registry.arm(
            &armed_entry(1, 0, 2000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );
        registry.disarm(0, "web");
        // Lets a rebuilt worker reach its own first poll, so an unchanged
        // count below means there is none rather than that it had not run.
        tokio::task::yield_now().await;

        let group = &registry.groups["web"];
        assert_eq!(
            group.members,
            HashSet::from([1]),
            "fixture check: the drainee must really have left a group the \
         replacement had already joined — this reads the same under either \
         ordering, which is why it cannot be the claim that matters"
        );
        assert_eq!(
            group.cron.as_ref().map(JoinHandle::id),
            Some(cron.id()),
            "the group must still hold the cron worker it was armed with, not \
         an identical one put back in its place"
        );
        assert_eq!(
            group.watch.as_ref().map(JoinHandle::id),
            Some(watch.id()),
            "and the watch it was armed with, whose rebuild means re-registering \
         the OS watcher"
        );
        assert_eq!(
            rig.clock.reads(),
            reads_before,
            "a surviving cron worker performs no startup work; a rebuilt one \
         reads the clock again to derive its next occurrence"
        );
    }

    // A name group with zero online instances, armed and disarmed before it
    // ever fires. An app whose first spawn is stopped straight away passes
    // through this shape, and a worker leaked there restarts a flock nobody
    // is running.
    #[tokio::test(start_paused = true)]
    async fn a_group_disarmed_before_its_first_occurrence_leaves_no_worker_behind() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let root = tempfile::tempdir().unwrap();
        let (handle, mut rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        // Both per-name extras and one per-pid extra, so a single disarm
        // has to reach all three.
        let app = app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
            app.max_memory = Some(MemSize::from_bytes(1024));
        });
        handle.start(vec![app.clone()]).await.unwrap();

        registry.arm(
            &armed_entry(0, 0, 1000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );
        let group = &registry.groups["web"];
        assert_eq!(group.members, HashSet::from([0]));
        assert!(group.cron.is_some(), "the cron worker must have armed");
        assert!(group.watch.is_some(), "the watch must have armed");
        assert_eq!(rig.enforcer.arms().len(), 1);

        registry.disarm(0, "web");

        assert!(
            registry.groups.is_empty(),
            "a group whose only member left before its first occurrence must go with it"
        );
        assert!(
            registry.instances.is_empty(),
            "the same disarm must take the instance's own extras too"
        );
        assert_eq!(rig.enforcer.disarms(), vec![0]);
        assert_no_restart_within(&mut rx, "web", PAST_THE_NEXT_OCCURRENCE).await;
    }

    // The watch twin of the case above, and separate because the gate is two
    // independent conditions. The ending is forced with `abort` rather than
    // by killing the `WatchSource` the loop really returns on, which dies
    // with an OS thread no test reaches; both leave a finished handle.
    #[tokio::test]
    async fn a_watch_that_ended_on_its_own_is_rebuilt_on_the_next_arm() {
        let home = tempfile::tempdir().unwrap();
        let paths = test_paths(&home);
        let root = tempfile::tempdir().unwrap();
        let (handle, mut rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        let app = app_with("web", |app| {
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
            app.watch_delay = Some(UpDuration::from_millis(
                real_time::TEST_DELAY.as_millis() as u64
            ));
        });
        handle.start(vec![app.clone()]).await.unwrap();

        registry.arm(
            &armed_entry(0, 0, 1000, app.clone(), &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );
        let armed = registry.groups["web"]
            .watch
            .as_ref()
            .expect("the first arm registers a watcher");
        armed.abort();
        settle_finished(armed).await;

        registry.arm(
            &armed_entry(0, 0, 2000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );

        touch(root.path(), "trigger.txt").unwrap();
        let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
        assert_eq!(info.restarts, 1);
    }

    // An unresolved root fires never: on macOS a tempdir under `/var/...` is
    // delivered as `/private/var/...` and every `strip_prefix` fails.
    #[tokio::test]
    async fn a_watched_app_restarts_on_a_save_and_goes_quiet_once_disarmed() {
        let home = tempfile::tempdir().unwrap();
        let paths = test_paths(&home);
        let root = tempfile::tempdir().unwrap();
        let (handle, mut rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        let app = app_with("web", |app| {
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
            // From `watch::real_time`, the owner of this subsystem's
            // real-time constants.
            app.watch_delay = Some(UpDuration::from_millis(
                real_time::TEST_DELAY.as_millis() as u64
            ));
        });
        handle.start(vec![app.clone()]).await.unwrap();
        registry.arm(
            &armed_entry(0, 0, 1000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );

        touch(root.path(), "trigger.txt").unwrap();
        let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
        assert_eq!(info.restarts, 1);

        registry.disarm(0, "web");
        touch(root.path(), "after-disarm.txt").unwrap();
        assert_no_restart_within(&mut rx, "web", real_time::NO_EVENT_WINDOW).await;
    }

    // `DEFAULT_WATCH_DELAY` is 500ms, so any longer fallback leaves the save
    // below with no restart inside the deadline.
    #[tokio::test]
    async fn a_watched_app_naming_no_delay_still_restarts_on_a_save() {
        let home = tempfile::tempdir().unwrap();
        let paths = test_paths(&home);
        let root = tempfile::tempdir().unwrap();
        let (handle, mut rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        let app = app_with("web", |app| {
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
            // No `watch_delay`: this case exists for the default.
        });
        handle.start(vec![app.clone()]).await.unwrap();
        registry.arm(
            &armed_entry(0, 0, 1000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );

        touch(root.path(), "trigger.txt").unwrap();
        let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
        assert_eq!(info.restarts, 1);
    }

    // The watch's own door into the claim the cron case makes, and separate
    // because the two subsystems pick their `SupervisorHandle` method
    // independently: through `restart`, an autosave is reported as a deploy.
    #[tokio::test]
    async fn a_watch_restart_is_not_reported_as_a_user_action() {
        let home = tempfile::tempdir().unwrap();
        let paths = test_paths(&home);
        let root = tempfile::tempdir().unwrap();
        let (handle, mut rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        let app = app_with("web", |app| {
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
            app.watch_delay = Some(UpDuration::from_millis(
                real_time::TEST_DELAY.as_millis() as u64
            ));
        });
        handle.start(vec![app.clone()]).await.unwrap();
        registry.arm(
            &armed_entry(0, 0, 1000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );

        touch(root.path(), "trigger.txt").unwrap();

        let (info, manually) =
            expect_restart_event(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
        assert_eq!(info.restarts, 1);
        assert!(
            !manually,
            "a file changing under a watched tree is not a user action"
        );
    }

    // A filter built from empty slices discards every ignore rule the user
    // wrote.
    #[tokio::test]
    async fn a_watched_app_ignores_the_paths_its_ignore_watch_names() {
        let home = tempfile::tempdir().unwrap();
        let paths = test_paths(&home);
        let root = tempfile::tempdir().unwrap();
        let (handle, mut rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        let app = app_with("web", |app| {
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
            app.ignore_watch = vec!["ignored.txt".to_string()];
            app.watch_delay = Some(UpDuration::from_millis(
                real_time::TEST_DELAY.as_millis() as u64
            ));
        });
        handle.start(vec![app.clone()]).await.unwrap();
        registry.arm(
            &armed_entry(0, 0, 1000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );

        touch(root.path(), "ignored.txt").unwrap();
        assert_no_restart_within(&mut rx, "web", real_time::NO_EVENT_WINDOW).await;

        touch(root.path(), "trigger.txt").unwrap();
        let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
        assert_eq!(info.restarts, 1);
    }

    // The default globs plus `ignore_watch` alone let an app naming an
    // explicit `out_file`/`err_file` under its own `cwd` restart on its own
    // log writes forever: the default log glob covers only a directory named
    // `logs`, and an automatic restart resets the budget.
    #[tokio::test]
    async fn a_watched_app_ignores_its_own_log_writes() {
        let home = tempfile::tempdir().unwrap();
        let paths = test_paths(&home);
        let root = tempfile::tempdir().unwrap();
        let (handle, mut rx, _fixture) = spawn_test_fixture();
        let rig = rig(DEFAULT_MAX_CRON_SLEEP);
        let mut registry = ExtrasRegistry::default();
        let app = app_with("web", |app| {
            app.watch = true;
            app.cwd = Some(root.path().display().to_string());
            // Absolute, under the watched tree, and named nothing like
            // `logs`: a shep write really does land inside the tree.
            app.out_file = Some(root.path().join("app-out.txt").display().to_string());
            app.err_file = Some(root.path().join("app-err.txt").display().to_string());
            app.watch_delay = Some(UpDuration::from_millis(
                real_time::TEST_DELAY.as_millis() as u64
            ));
        });
        handle.start(vec![app.clone()]).await.unwrap();
        registry.arm(
            &armed_entry(0, 0, 1000, app, &paths),
            idle_prober(),
            &rig.extras,
            &handle,
        );

        touch(root.path(), "app-out.txt").unwrap();
        touch(root.path(), "app-err.txt").unwrap();
        assert_no_restart_within(&mut rx, "web", real_time::NO_EVENT_WINDOW).await;

        touch(root.path(), "trigger.txt").unwrap();
        let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
        assert_eq!(info.restarts, 1);
    }

    // `probe_exec` runs `env_clear().envs(&self.env)` and `SHEP_INSTANCE` is
    // written by `assemble` alone, so a prober built from `config.env`, or
    // built once and shared, expands it to nothing and both instances report.
    // A file, not a port: `test -f` needs no listener and cannot race.
    #[cfg(unix)]
    #[tokio::test]
    async fn each_instances_liveness_probe_runs_with_its_own_assembled_env() {
        let markers = tempfile::tempdir().unwrap();
        // Instance 0's marker exists; instance 1's never will.
        std::fs::write(markers.path().join("live-0"), b"").unwrap();

        let mut h = harness(vec![ProcScript::never_exits(); 4]);
        h.ctx
            .supervisor
            .start(vec![app_with("web", |app| {
                app.instances = 2;
                app.liveness_probe = Some(ProbeConfig {
                    failure_threshold: 1,
                    interval: PROBE_INTERVAL,
                    timeout: UpDuration::from_millis(5_000),
                    ..probe_config(
                        ProbeKind::Exec,
                        &format!(
                            r#"test -f "{}/live-$SHEP_INSTANCE""#,
                            markers.path().display()
                        ),
                    )
                });
            })])
            .await
            .unwrap();

        let listing = h.ctx.supervisor.list().await;
        // `ProcessInfo` carries no instance number, but the assembler's log
        // path does, from the same `assemble` call, so this pins which
        // instance id 1 is rather than assuming the allocation order.
        assert!(
            listing[1]
                .out_file
                .as_ref()
                .is_some_and(|path| path.ends_with("web-1-out.log")),
            "id 1 must be instance 1: {:?}",
            listing[1].out_file
        );
        let instance_one_pid = listing[1].pid.expect("a live sheep has a pid");

        let failure = expect_liveness(&mut h.liveness, LIVENESS_DEADLINE).await;
        assert_eq!(
            failure,
            LivenessReport {
                id: 1,
                pid: instance_one_pid,
                epoch: 1,
            },
            "only the instance whose own marker is missing may report"
        );
        // Both instances report under the bugs above and which arrives
        // first is a race, so the window catching the other is not optional.
        assert_no_liveness_within(&mut h.liveness, PROBE_INTERVAL.as_duration() * 3).await;
    }
}

mod registry;

mod reporter;

mod actor;
