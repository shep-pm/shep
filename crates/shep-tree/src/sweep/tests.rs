use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use super::*;

const GRACE: Duration = Duration::from_secs(5);

fn at(raw: u64) -> ProcInstant {
    ProcInstant::from_raw(raw)
}

fn snapshot_of(pids: &[u32]) -> LambSnapshot {
    LambSnapshot::new(pids.iter().copied(), at(1_700_000_000))
}

/// A [`LambSweep`] that touches no process: one survivor list per look, one
/// born list per walk, and a ledger of every signal.
///
/// [`LambSweep::survivors`] and [`LambSweep::descendants`] replay their
/// scripts and repeat the last entry once one runs out. A delivered `Kill`
/// removes its pid from every later survivor list, as the kernel would,
/// unless the pid is [`Scripted::unkillable`].
#[derive(Debug, Default)]
struct Scripted {
    survivors: Vec<Vec<u32>>,
    born: Vec<Vec<u32>>,
    refusing: HashSet<u32>,
    unkillable: HashSet<u32>,
    looks: Mutex<Vec<LambSnapshot>>,
    walks: Mutex<Vec<Vec<u32>>>,
    killed: Mutex<HashSet<u32>>,
    signals: Mutex<Vec<(u32, LambSignal)>>,
}

impl Scripted {
    /// One survivor list per [`LambSweep::survivors`] call, in order.
    fn with_survivors(mut self, script: Vec<Vec<u32>>) -> Self {
        self.survivors = script;
        self
    }

    /// One list per [`LambSweep::descendants`] call, in order, of what the
    /// survivors started since.
    fn with_born(mut self, script: Vec<Vec<u32>>) -> Self {
        self.born = script;
        self
    }

    /// Every signal to `pid` fails as the OS refusing it.
    fn refusing(mut self, pid: u32) -> Self {
        self.refusing.insert(pid);
        self
    }

    /// `pid` takes its `Kill` and stays in the survivor lists, like a process
    /// stuck in uninterruptible sleep.
    fn unkillable(mut self, pid: u32) -> Self {
        self.unkillable.insert(pid);
        self
    }

    /// Every signal sent so far, in order, refused ones included.
    fn signals(&self) -> Vec<(u32, LambSignal)> {
        self.signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The snapshot each [`LambSweep::survivors`] call was asked about.
    fn looks(&self) -> Vec<LambSnapshot> {
        self.looks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The roots each [`LambSweep::descendants`] call walked from.
    fn walks(&self) -> Vec<Vec<u32>> {
        self.walks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl LambSweep for Scripted {
    fn snapshot(&self, _root_pid: u32) -> LambSnapshot {
        LambSnapshot::default()
    }

    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32> {
        let mut looks = self.looks.lock().unwrap_or_else(PoisonError::into_inner);
        looks.push(snapshot.clone());
        let Some(last) = self.survivors.len().checked_sub(1) else {
            return Vec::new();
        };
        let killed = self.killed.lock().unwrap_or_else(PoisonError::into_inner);
        self.survivors[(looks.len() - 1).min(last)]
            .iter()
            .copied()
            .filter(|pid| !killed.contains(pid))
            .collect()
    }

    fn descendants(&self, roots: &[u32]) -> LambSnapshot {
        let mut walks = self.walks.lock().unwrap_or_else(PoisonError::into_inner);
        walks.push(roots.to_vec());
        let Some(last) = self.born.len().checked_sub(1) else {
            return LambSnapshot::default();
        };
        LambSnapshot::new(self.born[(walks.len() - 1).min(last)].clone(), at(0))
    }

    fn signal(&self, pid: u32, signal: LambSignal) -> Result<(), SignalError> {
        self.signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((pid, signal));
        if self.refusing.contains(&pid) {
            return Err(SignalError::Refused("scripted refusal".to_string()));
        }
        if signal == LambSignal::Kill && !self.unkillable.contains(&pid) {
            self.killed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(pid);
        }
        Ok(())
    }
}

/// Drives `sweeper` to [`Step::Done`] on a virtual clock, advancing it by
/// exactly each [`Step::Wait`]. Returns the report and the virtual time the
/// sweep took.
///
/// The step count is the bound: a sweep that never finishes is a defect in
/// the step machine, and naming it beats hanging the test binary.
fn drive(sweeper: &mut Sweeper, table: &Scripted) -> (SweepReport, Duration) {
    let start = Instant::now();
    let mut elapsed = Duration::ZERO;
    for _ in 0..1_000 {
        match sweeper.step(table, start + elapsed) {
            Step::Wait(sleep) => elapsed += sleep,
            Step::Done(report) => return (report, elapsed),
        }
    }
    panic!("the sweep never finished, {elapsed:?} of virtual time and no `Done`")
}

#[test]
fn an_empty_snapshot_reads_nothing_and_signals_nothing() {
    let table = Scripted::default().with_survivors(vec![vec![7]]);

    let (report, _) = drive(&mut Sweeper::new(snapshot_of(&[]), GRACE), &table);

    assert_eq!(report, SweepReport::default());
    assert!(
        table.looks().is_empty(),
        "an empty snapshot must not be read"
    );
    assert!(table.signals().is_empty());
}

#[test]
fn a_snapshot_with_no_survivors_signals_nothing_and_does_not_wait() {
    let table = Scripted::default();

    let (report, elapsed) = drive(&mut Sweeper::new(snapshot_of(&[7, 8]), GRACE), &table);

    assert_eq!(report, SweepReport::default());
    assert!(table.signals().is_empty());
    assert_eq!(
        elapsed,
        Duration::ZERO,
        "nothing to signal means nothing to wait for"
    );
}

#[test]
fn lambs_that_exit_on_term_get_no_kill_and_the_sweep_returns_at_the_first_look() {
    let table = Scripted::default().with_survivors(vec![vec![7, 8], vec![]]);

    let (report, elapsed) = drive(&mut Sweeper::new(snapshot_of(&[7, 8]), GRACE), &table);

    assert_eq!(
        table.signals(),
        vec![(7, LambSignal::Term), (8, LambSignal::Term)]
    );
    assert_eq!(report.termed, vec![7, 8]);
    assert!(report.killed.is_empty());
    assert_eq!(elapsed, SWEEP_POLL_INTERVAL);
}

#[test]
fn a_lamb_that_ignores_term_is_killed_once_the_grace_runs_out() {
    // Two survive the first look; 8 exits on its TERM and 7 never does.
    let table = Scripted::default().with_survivors(vec![vec![7, 8], vec![7]]);

    let (report, elapsed) = drive(&mut Sweeper::new(snapshot_of(&[7, 8]), GRACE), &table);

    assert_eq!(
        elapsed,
        GRACE + KILL_SETTLE_POLL,
        "KILL waits out the whole grace, then one look sees it landed"
    );
    assert_eq!(
        table.signals(),
        vec![
            (7, LambSignal::Term),
            (8, LambSignal::Term),
            (7, LambSignal::Kill)
        ]
    );
    assert_eq!(report.killed, vec![7]);
    let looks = table.looks();
    assert_eq!(
        looks.len(),
        2 + (GRACE.as_millis() / SWEEP_POLL_INTERVAL.as_millis()) as usize,
        "one look before TERM, one per poll interval, one after KILL"
    );
    assert!(looks.iter().all(|look| *look == snapshot_of(&[7, 8])));
}

#[test]
fn a_zero_grace_looks_once_more_then_kills() {
    let table = Scripted::default().with_survivors(vec![vec![7]]);

    let (report, elapsed) = drive(&mut Sweeper::new(snapshot_of(&[7]), Duration::ZERO), &table);

    assert_eq!(elapsed, KILL_SETTLE_POLL);
    assert_eq!(report.killed, vec![7]);
    assert_eq!(table.looks().len(), 2, "one look, then one after KILL");
}

// A stop reply must not race the kernel tearing a killed lamb down.
#[test]
fn the_sweep_returns_only_once_a_killed_lamb_has_stopped_running() {
    let table = Scripted::default().with_survivors(vec![vec![7]]);

    drive(&mut Sweeper::new(snapshot_of(&[7]), Duration::ZERO), &table);

    assert!(table.survivors(&snapshot_of(&[7])).is_empty());
    assert_eq!(table.looks().len(), 3, "a look after KILL saw it gone");
}

// The second survivor list is the re-check after the walk: 7 still runs.
#[test]
fn a_child_a_lamb_started_after_the_snapshot_is_termed_with_it() {
    let table = Scripted::default()
        .with_survivors(vec![vec![7], vec![7], vec![]])
        .with_born(vec![vec![9]]);

    let (report, _) = drive(&mut Sweeper::new(snapshot_of(&[7]), GRACE), &table);

    assert_eq!(
        table.walks().first(),
        Some(&vec![7]),
        "walked from the survivor"
    );
    assert_eq!(
        table.looks().get(1),
        Some(&snapshot_of(&[7])),
        "the re-check reads the roots, before the walk's finds are merged"
    );
    assert_eq!(
        table.signals(),
        vec![(7, LambSignal::Term), (9, LambSignal::Term)]
    );
    assert_eq!(report.termed, vec![7, 9]);
}

// 7 exits after the survivor read and its pid is reused before the walk: the
// walk's 9 is a stranger's child.
#[test]
fn a_walk_from_a_root_that_stopped_surviving_signals_none_of_its_finds() {
    let table = Scripted::default()
        .with_survivors(vec![vec![7], vec![], vec![]])
        .with_born(vec![vec![9]]);

    let (report, _) = drive(&mut Sweeper::new(snapshot_of(&[7]), GRACE), &table);

    assert!(
        !table.signals().iter().any(|&(pid, _)| pid == 9),
        "{:?}",
        table.signals()
    );
    assert!(
        !table.signals().iter().any(|&(pid, _)| pid == 7),
        "a root that failed the re-check is not signalled on that look either"
    );
    assert!(
        table.looks().iter().all(|look| look.seen_at(9).is_none()),
        "the dropped find is never merged into a later look"
    );
    assert_eq!(report, SweepReport::default());
}

// One root of two fails the re-check: the look keeps the other, drops the
// walk's finds, and the next look walks again.
#[test]
fn a_failed_re_check_keeps_the_roots_that_passed_and_walks_again() {
    let table = Scripted::default()
        .with_survivors(vec![vec![7, 8], vec![8], vec![8], vec![8], vec![]])
        .with_born(vec![vec![9], vec![10]]);

    let (report, _) = drive(&mut Sweeper::new(snapshot_of(&[7, 8]), GRACE), &table);

    assert_eq!(
        table.signals(),
        vec![(8, LambSignal::Term), (10, LambSignal::Term)]
    );
    assert_eq!(table.walks(), vec![vec![7, 8], vec![8]]);
    assert_eq!(report.termed, vec![8, 10]);
}

// The lamb exits on its TERM, orphaning a child it started mid-grace. The
// look that found the child while its parent lived is what holds it.
#[test]
fn a_child_born_mid_grace_outlives_its_parent_and_is_still_killed() {
    // The third survivor list is the re-check after the walk that found 9.
    let table = Scripted::default()
        .with_survivors(vec![vec![7], vec![7], vec![7], vec![9]])
        .with_born(vec![vec![], vec![9], vec![]]);

    let (report, elapsed) = drive(&mut Sweeper::new(snapshot_of(&[7]), GRACE), &table);

    assert_eq!(
        table.signals(),
        vec![
            (7, LambSignal::Term),
            (9, LambSignal::Term),
            (9, LambSignal::Kill)
        ]
    );
    assert_eq!(report.termed, vec![7, 9]);
    assert_eq!(report.killed, vec![9]);
    assert_eq!(elapsed, GRACE + KILL_SETTLE_POLL);
    assert!(
        table
            .looks()
            .iter()
            .skip(3)
            .all(|look| look.pids().any(|pid| pid == 9)),
        "every look after the find carries the orphan"
    );
}

#[test]
fn a_lamb_that_survives_kill_is_reported_stuck_and_left_after_the_settle_bound() {
    let table = Scripted::default()
        .with_survivors(vec![vec![7]])
        .unkillable(7);

    let (report, elapsed) = drive(&mut Sweeper::new(snapshot_of(&[7]), Duration::ZERO), &table);

    assert_eq!(
        elapsed, KILL_SETTLE,
        "the settle bound is waited out in full"
    );
    assert_eq!(report.killed, vec![7]);
    assert_eq!(
        report.stuck,
        vec![7],
        "the lamb still running is reported, not retried"
    );
}

// A refusal is reported and the sweep goes on: the rest of the tree is still
// termed and killed.
#[test]
fn a_refused_signal_is_reported_and_the_sweep_still_kills_the_rest() {
    let table = Scripted::default()
        .with_survivors(vec![vec![7, 8], vec![8]])
        .refusing(7);

    let (report, elapsed) = drive(&mut Sweeper::new(snapshot_of(&[7, 8]), GRACE), &table);

    assert_eq!(
        table.signals(),
        vec![
            (7, LambSignal::Term),
            (8, LambSignal::Term),
            (8, LambSignal::Kill)
        ]
    );
    assert_eq!(report.termed, vec![7, 8]);
    assert_eq!(report.killed, vec![8]);
    assert_eq!(
        report.refused,
        vec![(
            7,
            LambSignal::Term,
            SignalError::Refused("scripted refusal".to_string())
        )]
    );
    assert!(report.stuck.is_empty());
    assert_eq!(elapsed, GRACE + KILL_SETTLE_POLL);
}

#[test]
fn a_merge_unions_the_pids_and_dates_each_by_the_latest_look_that_saw_it() {
    let tick = LambSnapshot::new([7, 8], at(100));
    let fresh = LambSnapshot::new([8, 9], at(200));

    let merged = tick.clone().merge(fresh.clone());
    assert_eq!(
        merged.pids().collect::<Vec<_>>(),
        vec![7, 8, 9],
        "every pid either look saw"
    );
    assert_eq!(merged.seen_at(7), Some(at(100)));
    assert_eq!(merged.seen_at(8), Some(at(200)));
    assert_eq!(merged.seen_at(9), Some(at(200)));
    assert_eq!(merged.seen_at(10), None);
    assert_eq!(fresh.merge(tick), merged, "the merge is symmetric");
}

#[test]
fn a_snapshots_latest_look_is_the_latest_any_pid_was_seen() {
    let merged = LambSnapshot::new([7, 9], at(100)).merge(LambSnapshot::new([8], at(200)));
    assert_eq!(merged.latest(), Some(at(200)), "not the first pid's, 100");
    assert_eq!(LambSnapshot::default().latest(), None);
}

#[test]
fn a_snapshot_keeps_each_pid_once() {
    let snapshot = LambSnapshot::new([9, 7, 9, 7], at(5));
    assert_eq!(snapshot.pids().collect::<Vec<_>>(), vec![7, 9]);
    assert_eq!(snapshot.seen_at(7), Some(at(5)));
}

// A table with no stats tick answers `last_snapshot` with `None`, so a sweep
// over it fakes nothing.
#[test]
fn a_table_with_no_tick_of_its_own_answers_last_snapshot_with_none() {
    let table = Scripted::default();
    let sweep: &dyn LambSweep = &table;
    assert_eq!(sweep.last_snapshot(7), None);
}

// Real sleeps, because `run_blocking` is the real-clock driver of the same
// step machine: the bound is the one `Wait` a zero grace asks for.
#[test]
fn run_blocking_ends_what_the_step_machine_ends() {
    let scripted = Scripted::default().with_survivors(vec![vec![7]]);
    let (stepped, _) = drive(
        &mut Sweeper::new(snapshot_of(&[7]), Duration::ZERO),
        &scripted,
    );

    let blocking = Scripted::default().with_survivors(vec![vec![7]]);
    let report = Sweeper::new(snapshot_of(&[7]), Duration::ZERO).run_blocking(&blocking);

    assert_eq!(report, stepped);
    assert_eq!(blocking.signals(), scripted.signals());
}
