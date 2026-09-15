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
    armed_entry, capture_logs, harness, harness_with_extras, idle_stats, probe_config, test_paths,
    touch,
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

mod registry;

mod reporter;

mod actor;

mod rearm;

mod slow;
