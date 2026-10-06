//! Tests for the lamb sweep at the end of a sheep task.
//!
//! A stop sweeps a fresh walk merged with the last tick, a natural exit
//! sweeps the tick alone, and `Msg::Exited` waits for both. Driven through
//! `run_sheep` with a scripted sweep, then through the actor to prove the
//! extras' sweep reaches the task.

use super::*;
use crate::fake::{FIRST_SCRIPTED_PID, ScriptedSweep};
use crate::proc_table::ProcInstant;
use crate::sweep::{LambSignal, LambSnapshot, LambSweep};
use crate::testing::harness_with_extras;

/// The pid the runner hands the one sheep each case spawns.
const ROOT: u32 = FIRST_SCRIPTED_PID;

/// Far past anything a case waits for: a bound, not a schedule.
const NO_LONGER_THAN: Duration = Duration::from_secs(60);

fn at(raw: u64) -> ProcInstant {
    ProcInstant::from_raw(raw)
}

/// One `run_sheep` task and the ends a case drives it through.
struct Running {
    ctl: mpsc::Sender<SheepCtl>,
    actor_rx: mpsc::Receiver<Msg>,
    runner: ScriptedRunner,
    // Held so neither mailbox closes under the task.
    _signals: mpsc::Sender<SignalRequest>,
    _events: tokio::sync::broadcast::Receiver<SharedEvent>,
}

fn sweep_spec() -> SpawnSpec {
    SpawnSpec {
        name: "web".to_string(),
        program: "./srv".to_string(),
        args: Vec::new(),
        cwd: None,
        env: std::collections::BTreeMap::new(),
        out_file: std::path::PathBuf::from("out.log"),
        err_file: std::path::PathBuf::from("err.log"),
        log_timestamps: true,
        // A channel, so `shutdown_with_message` has somewhere to send.
        channel: true,
        stdin: false,
        credentials: None,
    }
}

fn run_one(script: ProcScript, app: AppConfig, sweep: Option<Arc<ScriptedSweep>>) -> Running {
    let runner = ScriptedRunner::new(vec![script]);
    let (proc, io) = runner.spawn(&sweep_spec()).unwrap();
    let (events, events_rx) = crate::bus::test_bus(64);
    let (ctl, ctl_rx) = mpsc::channel(8);
    let (signals, signal_rx) = mpsc::channel(8);
    let (actor_tx, actor_rx) = mpsc::channel(8);
    let sweep = sweep.map(|sweep| sweep as Arc<dyn LambSweep>);
    tokio::spawn(run_sheep(
        7,
        proc,
        io,
        normalize(app).unwrap(),
        ctl_rx,
        signal_rx,
        events,
        actor_tx,
        sweep,
    ));
    Running {
        ctl,
        actor_rx,
        runner,
        _signals: signals,
        _events: events_rx,
    }
}

/// The task's `Msg::Exited`, or `None` if it does not arrive within `within`.
async fn exited_within(rx: &mut mpsc::Receiver<Msg>, within: Duration) -> Option<ExitOutcome> {
    tokio::time::timeout(within, async {
        loop {
            match rx.recv().await {
                Some(Msg::Exited { outcome, .. }) => return outcome,
                Some(_) => {}
                None => panic!("the sheep task ended without reporting an exit"),
            }
        }
    })
    .await
    .ok()
}

fn kill_timeout() -> Duration {
    AppConfig::minimal("web", "./srv")
        .kill_timeout
        .as_duration()
}

#[tokio::test(start_paused = true)]
async fn a_stop_sweeps_the_fresh_walk_merged_with_the_last_tick() {
    let fresh = LambSnapshot::new([7, 8], at(200));
    let last = LambSnapshot::new([8, 9], at(100));
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_fresh(ROOT, fresh.clone())
            .with_last(ROOT, last.clone())
            .with_survivors(vec![vec![7, 9], vec![]]),
    );
    let mut sheep = run_one(
        ProcScript::never_exits(),
        AppConfig::minimal("web", "./srv"),
        Some(Arc::clone(&sweep)),
    );

    sheep
        .ctl
        .send(SheepCtl::Kill {
            grace: kill_timeout(),
        })
        .await
        .unwrap();
    let outcome = exited_within(&mut sheep.actor_rx, NO_LONGER_THAN)
        .await
        .expect("a stopped sheep reports its exit");

    assert_eq!(outcome.signal, Some(15));
    assert_eq!(
        sweep.leader_looks(),
        vec![LambSnapshot::new([ROOT], at(200))],
        "the leader was checked, found gone, and the lambs swept"
    );
    assert_eq!(sweep.looks().first(), Some(&fresh.merge(last)));
    assert_eq!(
        sweep.signals(),
        vec![(7, LambSignal::Term), (9, LambSignal::Term)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_stop_by_message_still_sends_term_to_the_lambs() {
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_fresh(ROOT, LambSnapshot::new([7], at(200)))
            .with_survivors(vec![vec![7], vec![]]),
    );
    let mut app = AppConfig::minimal("web", "./srv");
    app.channel = true;
    app.shutdown_with_message = true;
    let mut sheep = run_one(ProcScript::never_exits(), app, Some(Arc::clone(&sweep)));

    sheep
        .ctl
        .send(SheepCtl::Kill {
            grace: kill_timeout(),
        })
        .await
        .unwrap();
    exited_within(&mut sheep.actor_rx, NO_LONGER_THAN)
        .await
        .expect("a stopped sheep reports its exit");

    assert!(
        sheep.runner.signals(0).is_empty(),
        "sanity: the leader got the message, not a signal"
    );
    assert_eq!(sweep.signals(), vec![(7, LambSignal::Term)]);
}

#[tokio::test(start_paused = true)]
async fn a_natural_exit_sweeps_the_last_ticks_snapshot() {
    let last = LambSnapshot::new([8], at(100));
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_fresh(ROOT, LambSnapshot::new([5], at(200)))
            .with_last(ROOT, last.clone())
            .with_survivors(vec![vec![8], vec![]]),
    );
    let mut sheep = run_one(
        ProcScript::stable_then_exit(1_000, 1),
        AppConfig::minimal("web", "./srv"),
        Some(Arc::clone(&sweep)),
    );

    let outcome = exited_within(&mut sheep.actor_rx, NO_LONGER_THAN)
        .await
        .expect("a crashed sheep reports its exit");

    assert_eq!(outcome.code, Some(1));
    assert_eq!(
        sweep.leader_looks(),
        vec![LambSnapshot::new([ROOT], at(100))]
    );
    let looks = sweep.looks();
    assert!(!looks.is_empty(), "the exit must be swept");
    assert!(
        looks.iter().all(|look| *look == last),
        "only the tick's snapshot, never a fresh walk: {looks:?}"
    );
    assert_eq!(sweep.signals(), vec![(8, LambSignal::Term)]);
}

#[tokio::test(start_paused = true)]
async fn the_exit_is_reported_only_after_a_lamb_that_ignores_term_is_killed() {
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_fresh(ROOT, LambSnapshot::new([7], at(200)))
            .with_survivors(vec![vec![7]]),
    );
    let mut sheep = run_one(
        ProcScript::never_exits(),
        AppConfig::minimal("web", "./srv"),
        Some(Arc::clone(&sweep)),
    );
    let start = tokio::time::Instant::now();

    sheep
        .ctl
        .send(SheepCtl::Kill {
            grace: kill_timeout(),
        })
        .await
        .unwrap();
    let early = exited_within(
        &mut sheep.actor_rx,
        kill_timeout() - Duration::from_millis(1),
    )
    .await;

    assert!(early.is_none(), "the exit was reported mid-sweep");
    assert_eq!(sweep.signals(), vec![(7, LambSignal::Term)]);
    exited_within(&mut sheep.actor_rx, NO_LONGER_THAN)
        .await
        .expect("the exit is reported once the sweep ends");
    assert_eq!(
        start.elapsed(),
        kill_timeout() + crate::sweep::KILL_SETTLE_POLL,
        "the sweep's whole grace, then one look that sees the KILL landed"
    );
    assert_eq!(
        sweep.signals(),
        vec![(7, LambSignal::Term), (7, LambSignal::Kill)]
    );
}

// A wait can return while the leader still runs; its lambs are then a live
// sheep's. A sync test over its own paused runtime: `capture_logs` only sees
// records written on this thread.
#[test]
fn a_stop_leaves_the_lambs_alone_while_the_leader_still_runs() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    // 7 seen only by the tick at 100, 8 by the fresh walk at 200.
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_fresh(ROOT, LambSnapshot::new([8], at(200)))
            .with_last(ROOT, LambSnapshot::new([7], at(100)))
            .with_survivors(vec![vec![7, 8]])
            .with_running_leader(ROOT),
    );

    let mut outcome = None;
    let logs = crate::testing::capture_logs(|| {
        runtime.block_on(async {
            let mut sheep = run_one(
                ProcScript::never_exits(),
                AppConfig::minimal("web", "./srv"),
                Some(Arc::clone(&sweep)),
            );
            sheep
                .ctl
                .send(SheepCtl::Kill {
                    grace: kill_timeout(),
                })
                .await
                .unwrap();
            outcome = exited_within(&mut sheep.actor_rx, NO_LONGER_THAN).await;
        });
    });

    assert!(outcome.is_some(), "the exit is still reported");
    assert!(sweep.signals().is_empty(), "{:?}", sweep.signals());
    assert!(sweep.looks().is_empty(), "no lamb was even looked at");
    assert_eq!(
        sweep.leader_looks(),
        vec![LambSnapshot::new([ROOT], at(200))],
        "the leader is dated by the latest look, not the first pid's"
    );
    assert!(
        logs.contains("WARN") && logs.contains("still runs"),
        "{logs}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_natural_exit_leaves_the_lambs_alone_while_the_leader_still_runs() {
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_last(ROOT, LambSnapshot::new([8], at(100)))
            .with_survivors(vec![vec![8]])
            .with_running_leader(ROOT),
    );
    let mut sheep = run_one(
        ProcScript::stable_then_exit(1_000, 1),
        AppConfig::minimal("web", "./srv"),
        Some(Arc::clone(&sweep)),
    );

    exited_within(&mut sheep.actor_rx, NO_LONGER_THAN)
        .await
        .expect("the exit is still reported");

    assert!(sweep.signals().is_empty(), "{:?}", sweep.signals());
    assert_eq!(
        sweep.leader_looks(),
        vec![LambSnapshot::new([ROOT], at(100))]
    );
}

#[tokio::test(start_paused = true)]
async fn an_empty_snapshot_skips_the_leader_check() {
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_last(ROOT, LambSnapshot::default())
            .with_running_leader(ROOT),
    );
    let mut sheep = run_one(
        ProcScript::stable_then_exit(1_000, 1),
        AppConfig::minimal("web", "./srv"),
        Some(Arc::clone(&sweep)),
    );

    exited_within(&mut sheep.actor_rx, NO_LONGER_THAN)
        .await
        .expect("a crashed sheep reports its exit");

    assert!(sweep.leader_looks().is_empty());
    assert!(sweep.looks().is_empty());
}

#[tokio::test(start_paused = true)]
async fn without_a_sweep_a_stop_reports_its_exit_at_once() {
    let mut sheep = run_one(
        ProcScript::never_exits(),
        AppConfig::minimal("web", "./srv"),
        None,
    );
    let start = tokio::time::Instant::now();

    sheep
        .ctl
        .send(SheepCtl::Kill {
            grace: kill_timeout(),
        })
        .await
        .unwrap();
    let outcome = exited_within(&mut sheep.actor_rx, NO_LONGER_THAN)
        .await
        .expect("a stopped sheep reports its exit");

    assert_eq!(outcome.signal, Some(15));
    assert_eq!(start.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn the_extras_sweep_reaches_a_started_sheep_and_its_respawn() {
    let sweep = Arc::new(
        ScriptedSweep::new()
            .with_last(ROOT, LambSnapshot::new([7], at(100)))
            .with_fresh(ROOT + 1, LambSnapshot::new([8], at(200)))
            .with_survivors(vec![vec![7], vec![], vec![8], vec![]]),
    );
    let scripts = vec![
        ProcScript::stable_then_exit(1_000, 1),
        ProcScript::never_exits(),
    ];
    let h = harness_with_extras(scripts, |reports| Extras {
        clock: Arc::new(SystemClock),
        enforcer: Arc::new(RecordingEnforcer::default()),
        lamb_sweep: Arc::clone(&sweep) as Arc<dyn LambSweep>,
        max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
        reports,
        stats: idle_stats(),
    });
    let supervisor = &h.ctx.supervisor;
    supervisor
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let respawned = tokio::time::timeout(NO_LONGER_THAN, async {
        while supervisor.list().await[0].pid != Some(ROOT + 1) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(respawned.is_ok(), "the crash must respawn the sheep");
    supervisor
        .stop(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();

    assert_eq!(
        sweep.signals(),
        vec![(7, LambSignal::Term), (8, LambSignal::Term)],
        "the crash swept the first pid's tick, the stop the respawn's walk"
    );
}

#[cfg(unix)]
mod slow;
