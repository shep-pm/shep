use super::*;
use crate::fake::ScriptedSweep;
use crate::testing::capture_logs;

const GRACE: Duration = Duration::from_secs(5);

fn snapshot_of(pids: &[u32]) -> LambSnapshot {
    LambSnapshot::new(pids.iter().copied(), 1_700_000_000)
}

#[tokio::test(start_paused = true)]
async fn an_empty_snapshot_reads_nothing_and_signals_nothing() {
    let sweep = ScriptedSweep::new().with_survivors(vec![vec![7]]);

    let report = sweep_lambs(&sweep, &snapshot_of(&[]), GRACE).await;

    assert_eq!(report, SweepReport::default());
    assert!(
        sweep.looks().is_empty(),
        "an empty snapshot must not be read"
    );
    assert!(sweep.signals().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_with_no_survivors_signals_nothing_and_does_not_wait() {
    let sweep = ScriptedSweep::new();
    let start = Instant::now();

    let report = sweep_lambs(&sweep, &snapshot_of(&[7, 8]), GRACE).await;

    assert_eq!(report, SweepReport::default());
    assert!(sweep.signals().is_empty());
    assert_eq!(
        Instant::now(),
        start,
        "nothing to signal means nothing to wait for"
    );
}

#[tokio::test(start_paused = true)]
async fn lambs_that_exit_on_term_get_no_kill_and_the_sweep_returns_at_the_first_look() {
    let sweep = ScriptedSweep::new().with_survivors(vec![vec![7, 8], vec![]]);
    let start = Instant::now();

    let report = sweep_lambs(&sweep, &snapshot_of(&[7, 8]), GRACE).await;

    assert_eq!(
        sweep.signals(),
        vec![(7, LambSignal::Term), (8, LambSignal::Term)]
    );
    assert_eq!(report.termed, vec![7, 8]);
    assert!(report.killed.is_empty());
    assert_eq!(Instant::now() - start, SWEEP_POLL_INTERVAL);
}

#[tokio::test(start_paused = true)]
async fn a_lamb_that_ignores_term_is_killed_once_the_grace_runs_out() {
    // Two survive the first look; 8 exits on its TERM and 7 never does.
    let sweep = ScriptedSweep::new().with_survivors(vec![vec![7, 8], vec![7]]);
    let start = Instant::now();

    let report = sweep_lambs(&sweep, &snapshot_of(&[7, 8]), GRACE).await;

    assert_eq!(
        Instant::now() - start,
        GRACE,
        "KILL waits out the whole grace"
    );
    assert_eq!(
        sweep.signals(),
        vec![
            (7, LambSignal::Term),
            (8, LambSignal::Term),
            (7, LambSignal::Kill)
        ]
    );
    assert_eq!(report.killed, vec![7]);
    let looks = sweep.looks();
    assert_eq!(
        looks.len(),
        1 + (GRACE.as_millis() / SWEEP_POLL_INTERVAL.as_millis()) as usize,
        "one look before TERM, then one per poll interval"
    );
    assert!(looks.iter().all(|look| *look == snapshot_of(&[7, 8])));
}

#[tokio::test(start_paused = true)]
async fn a_zero_grace_looks_once_more_then_kills() {
    let sweep = ScriptedSweep::new().with_survivors(vec![vec![7]]);
    let start = Instant::now();

    let report = sweep_lambs(&sweep, &snapshot_of(&[7]), Duration::ZERO).await;

    assert_eq!(Instant::now(), start);
    assert_eq!(report.killed, vec![7]);
    assert_eq!(sweep.looks().len(), 2);
}

// A sync test over its own paused runtime: `capture_logs` only sees records
// written on this thread.
#[test]
fn a_refused_signal_is_logged_and_the_sweep_still_kills_the_rest() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let sweep = ScriptedSweep::new()
        .with_survivors(vec![vec![7, 8], vec![8]])
        .refusing(7);

    let mut report = SweepReport::default();
    let logs = capture_logs(|| {
        report = runtime.block_on(sweep_lambs(&sweep, &snapshot_of(&[7, 8]), GRACE));
    });

    assert_eq!(
        sweep.signals(),
        vec![
            (7, LambSignal::Term),
            (8, LambSignal::Term),
            (8, LambSignal::Kill)
        ]
    );
    assert_eq!(report.killed, vec![8]);
    assert!(
        logs.contains("lamb signal delivery failed") && logs.contains("pid=7"),
        "{logs}"
    );
    assert!(
        logs.contains("INFO") && logs.contains("swept lambs that outlived their sheep"),
        "{logs}"
    );
    assert!(
        logs.contains("SIGKILL") && logs.contains("lambs=[8]"),
        "{logs}"
    );
}

// A lamb born since the last tick is seen only by the fresh walk. Dated at
// the tick's second, it would fail the start-time check and escape.
#[test]
fn a_merge_unions_the_pids_and_dates_each_by_the_latest_look_that_saw_it() {
    let tick = LambSnapshot::new([7, 8], 100);
    let fresh = LambSnapshot::new([8, 9], 200);

    let merged = tick.clone().merge(fresh.clone());
    assert_eq!(
        merged.pids().collect::<Vec<_>>(),
        vec![7, 8, 9],
        "every pid either look saw"
    );
    assert_eq!(merged.seen_at(7), Some(100));
    assert_eq!(merged.seen_at(8), Some(200));
    assert_eq!(merged.seen_at(9), Some(200));
    assert_eq!(merged.seen_at(10), None);
    assert_eq!(fresh.merge(tick), merged, "the merge is symmetric");
}

#[test]
fn a_snapshot_keeps_each_pid_once() {
    let snapshot = LambSnapshot::new([9, 7, 9, 7], 5);
    assert_eq!(snapshot.pids().collect::<Vec<_>>(), vec![7, 9]);
    assert_eq!(snapshot.seen_at(7), Some(5));
}

#[test]
fn the_scripted_sweep_answers_with_its_canned_snapshots_behind_dyn() {
    let fresh = LambSnapshot::new([7], 200);
    let last = LambSnapshot::new([8], 100);
    let scripted = ScriptedSweep::new()
        .with_fresh(1, fresh.clone())
        .with_last(1, last.clone());
    let sweep: &dyn LambSweep = &scripted;

    assert_eq!(sweep.snapshot(1), fresh);
    assert_eq!(sweep.last_snapshot(1), Some(last));
    assert!(sweep.snapshot(2).is_empty());
    assert_eq!(sweep.last_snapshot(2), None);
}
