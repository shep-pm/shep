//! [`LambSweep`] over the machine's real process table
//!
//! [`ProcessTable`] walks the whole table for a fresh [`LambSnapshot`], and
//! refreshes only the pids a [`LambSnapshot`] names when it re-checks its
//! survivors, so the expensive walk happens once per look and not once per
//! pid.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use super::{LambSignal, LambSnapshot, LambSweep, SignalError};
use crate::instant::ProcInstant;

/// The [`LambSweep`] that reads the machine's process table.
///
/// [`Self::snapshot`] answers only for a `root_pid` that descends from this
/// process, so a sweep never signals a tree it did not start.
///
/// # Example
///
/// ```no_run
/// use std::process::Command;
/// use std::time::Duration;
///
/// use shep_tree::sweep::{LambSweep, ProcessTable, Sweeper};
/// use shep_tree::{Signal, Tree};
///
/// let (tree, mut child) = Tree::spawn(Command::new("sleep").arg("600")).unwrap();
/// let table = ProcessTable::new();
/// // Read the lambs while their root is still alive to walk from.
/// let snapshot = table.snapshot(child.id());
/// tree.stop(&mut child, Signal::Term, Duration::from_secs(5)).unwrap();
/// let report = Sweeper::new(snapshot, Duration::from_secs(5)).run_blocking(&table);
/// println!("termed {} lambs, killed {}", report.termed.len(), report.killed.len());
/// ```
#[derive(Debug)]
pub struct ProcessTable {
    // `std::sync::Mutex`, not a runtime's: the critical section is one
    // synchronous syscall walk, never held across an await. `LambSweep` takes
    // `&self`, so a retained table needs interior mutability.
    system: Mutex<System>,
}

impl ProcessTable {
    /// A table holding no process rows yet.
    ///
    /// Construction does no walk: the first call performs the first refresh,
    /// so building one at startup cannot block.
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: Mutex::new(System::new()),
        }
    }

    /// One full-table walk, indexed by parent pid.
    ///
    /// A retained [`System`]: `ProcessesToUpdate::All` with dead rows removed
    /// re-reads every parent link, so a reparented lamb is never read from a
    /// stale row, and the walk does not reallocate its table per look.
    fn children_of(&self) -> HashMap<u32, Vec<u32>> {
        let mut system = self.system.lock().unwrap_or_else(PoisonError::into_inner);
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, walk_refresh_kind());
        let mut children: HashMap<u32, Vec<u32>> = HashMap::with_capacity(system.processes().len());
        for process in system.processes().values() {
            if let Some(parent) = process.parent() {
                children
                    .entry(parent.as_u32())
                    .or_default()
                    .push(process.pid().as_u32());
            }
        }
        children
    }
}

// clippy's `new_without_default` fires under `-D warnings` for a `new()`
// that takes no arguments; this impl silences it.
impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

/// The refresh kind a walk asks sysinfo for.
///
/// Nothing: a walk reads only pid and parent. `.without_tasks()`, because
/// `nothing()` defaults `tasks` to on and on Linux that lists every thread as
/// a row no sweep can tell from a real child.
fn walk_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().without_tasks()
}

/// Every pid below `roots` in `children`, the roots themselves excluded.
///
/// Seeded with `roots`, so a parent-link cycle terminates rather than
/// recursing forever.
fn below(children: &HashMap<u32, Vec<u32>>, roots: &[u32]) -> HashSet<u32> {
    let mut seen: HashSet<u32> = roots.iter().copied().collect();
    let mut stack: Vec<u32> = roots.to_vec();
    while let Some(pid) = stack.pop() {
        for &child in children.get(&pid).into_iter().flatten() {
            if seen.insert(child) {
                stack.push(child);
            }
        }
    }
    for root in roots {
        seen.remove(root);
    }
    seen
}

impl LambSweep for ProcessTable {
    fn snapshot(&self, root_pid: u32) -> LambSnapshot {
        let taken_at = ProcInstant::now();
        let children = self.children_of();
        if !below(&children, &[std::process::id()]).contains(&root_pid) {
            return LambSnapshot::default();
        }
        LambSnapshot::new(below(&children, &[root_pid]), taken_at)
    }

    fn descendants(&self, roots: &[u32]) -> LambSnapshot {
        let taken_at = ProcInstant::now();
        LambSnapshot::new(below(&self.children_of(), roots), taken_at)
    }

    #[cfg(unix)]
    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32> {
        let pids: Vec<u32> = snapshot.pids().collect();
        unix::survivors_in(
            snapshot,
            &crate::instant::read_pids(&pids),
            std::process::id(),
            ProcInstant::earliest_start(),
        )
    }

    // The job object's `kill_tree` already ended every lamb.
    #[cfg(not(unix))]
    fn survivors(&self, _snapshot: &LambSnapshot) -> Vec<u32> {
        Vec::new()
    }

    #[cfg(unix)]
    fn signal(&self, pid: u32, signal: LambSignal) -> Result<(), SignalError> {
        unix::signal_one(pid, signal, std::process::id())
    }

    // Unreachable in practice: `survivors` never names a pid here.
    #[cfg(not(unix))]
    fn signal(&self, _pid: u32, _signal: LambSignal) -> Result<(), SignalError> {
        Err(SignalError::Unsupported)
    }
}

#[cfg(unix)]
mod unix {
    use std::collections::HashMap;

    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    use super::{LambSignal, LambSnapshot, SignalError};
    use crate::instant::{PidReading, ProcInstant};

    /// The positive pid `kill` may name, or why `pid` is never a lamb.
    ///
    /// Never `0` (the caller's group), `1` (init) or `own` (the sweeper).
    pub(super) fn lamb_pid(pid: u32, own: u32) -> Result<Pid, SignalError> {
        match i32::try_from(pid) {
            Ok(raw) if pid > 1 && pid != own => Ok(Pid::from_raw(raw)),
            _ => Err(SignalError::NotALamb(pid)),
        }
    }

    /// The pids in `snapshot` that `readings` show are still the processes it
    /// saw.
    ///
    /// A start before `earliest` is not a real start: an epoch clock read
    /// before boot would pass every pid.
    pub(super) fn survivors_in(
        snapshot: &LambSnapshot,
        readings: &HashMap<u32, PidReading>,
        own: u32,
        earliest: ProcInstant,
    ) -> Vec<u32> {
        snapshot
            .pids()
            .filter(|&pid| lamb_pid(pid, own).is_ok())
            .filter(|pid| {
                readings.get(pid).is_some_and(|reading| {
                    !reading.zombie
                        && reading
                            .started
                            .zip(snapshot.seen_at(*pid))
                            .is_some_and(|(started, seen)| earliest <= started && started <= seen)
                })
            })
            .collect()
    }

    /// `kill(pid, signal)` with a positive pid, refusing anything not a lamb.
    pub(super) fn signal_one(pid: u32, signal: LambSignal, own: u32) -> Result<(), SignalError> {
        let pid = lamb_pid(pid, own)?;
        let signal = match signal {
            LambSignal::Term => Signal::SIGTERM,
            LambSignal::Kill => Signal::SIGKILL,
        };
        kill(pid, signal).map_err(|errno| SignalError::Refused(errno.to_string()))
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;

    #[test]
    fn a_table_off_unix_names_no_survivor_and_refuses_every_signal() {
        let snapshot = LambSnapshot::new([10], ProcInstant::now());
        let table = ProcessTable::new();

        assert!(table.survivors(&snapshot).is_empty());
        assert_eq!(
            table.signal(10, LambSignal::Term),
            Err(SignalError::Unsupported)
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::process::{Command, Stdio};

    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    use super::unix::{lamb_pid, survivors_in};
    use super::*;
    use crate::instant::{BOOT_SLACK_SECS, PidReading, ProcInstant, read_pids};

    const OWN: u32 = 4_242;
    const TAKEN_AT: u64 = 1_700_000_000;

    fn at(raw: u64) -> ProcInstant {
        ProcInstant::from_raw(raw)
    }

    fn live(started: u64) -> PidReading {
        PidReading {
            started_secs: None,
            started: Some(at(started)),
            zombie: false,
        }
    }

    /// A `sh` child of this test process, in a group of its own, killed and
    /// reaped on drop.
    ///
    /// Its own group is what makes the drop enough: a lamb `sh` backgrounded
    /// outlives `sh`, and killing the group ends it instead of leaving it.
    struct Child {
        child: std::process::Child,
        pid: u32,
    }

    impl Child {
        /// A child running `script`, which keeps it alive.
        fn running(script: &str) -> Self {
            Self::wrapped(Self::spawn(script, false))
        }

        /// A child backgrounding `lamb`, returned once `lamb` is running.
        ///
        /// `sh` prints `ready` only after it has forked `lamb`, so reading
        /// that line is what makes a walk that expects `lamb` deterministic.
        fn with_lamb(lamb: &str) -> Self {
            let mut child = Self::spawn(&format!("{lamb} & echo ready; wait"), true);
            let stdout = child.stdout.take().expect("spawned with stdout piped");
            let line = BufReader::new(stdout)
                .lines()
                .next()
                .expect("the child prints one line")
                .expect("the pipe stays open");
            assert_eq!(line, "ready", "the readiness handshake");
            Self::wrapped(child)
        }

        fn spawn(script: &str, piped: bool) -> std::process::Child {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .stdin(Stdio::null())
                .stdout(if piped {
                    Stdio::piped()
                } else {
                    Stdio::inherit()
                })
                .process_group(0)
                .spawn()
                .expect("/bin/sh is present on every platform this module compiles for")
        }

        fn wrapped(child: std::process::Child) -> Self {
            let pid = child.id();
            Self { child, pid }
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            let group = Pid::from_raw(-i32::try_from(self.pid).expect("a pid `kill` can name"));
            let _ = kill(group, Signal::SIGKILL);
            let _ = self.child.wait();
        }
    }

    #[test]
    fn a_survivor_is_alive_unreaped_and_no_younger_than_the_snapshot() {
        let snapshot = LambSnapshot::new([10, 11, 12, 13, 14, 15], at(TAKEN_AT));
        let readings = HashMap::from([
            (10, live(TAKEN_AT - 60)),
            (11, live(TAKEN_AT)),
            (12, live(TAKEN_AT + 1)),
            (
                13,
                PidReading {
                    zombie: true,
                    ..live(TAKEN_AT - 60)
                },
            ),
            (
                14,
                PidReading {
                    started: None,
                    ..live(TAKEN_AT - 60)
                },
            ),
        ]);

        // 12 started after the snapshot, 13 is a zombie, 14 has no start
        // time and 15 is gone.
        assert_eq!(survivors_in(&snapshot, &readings, OWN, at(0)), vec![10, 11]);
    }

    // The sweep reads the clock-native start, not the epoch second the OS
    // reports, which on Linux is two seconds coarse.
    #[test]
    fn a_survivor_is_judged_by_its_clock_start_not_its_epoch_second() {
        let snapshot = LambSnapshot::new([10], at(TAKEN_AT));
        let reused = PidReading {
            started_secs: Some(TAKEN_AT - 60),
            ..live(TAKEN_AT + 1)
        };
        assert!(survivors_in(&snapshot, &HashMap::from([(10, reused)]), OWN, at(0)).is_empty());
    }

    #[test]
    fn each_pid_is_checked_against_the_look_that_saw_it() {
        let tick = LambSnapshot::new([10, 11], at(TAKEN_AT));
        let fresh = LambSnapshot::new([11, 12], at(TAKEN_AT + 10));
        let readings = HashMap::from([
            (10, live(TAKEN_AT + 5)),
            (11, live(TAKEN_AT + 5)),
            (12, live(TAKEN_AT + 5)),
        ]);

        // 10 was seen only by the tick, before it started: a recycled pid.
        assert_eq!(
            survivors_in(&tick.merge(fresh), &readings, OWN, at(0)),
            vec![11, 12]
        );
    }

    #[test]
    fn init_pid_zero_and_the_sweeping_process_itself_are_never_survivors() {
        let snapshot = LambSnapshot::new([0, 1, OWN, 10], at(TAKEN_AT));
        let readings = HashMap::from([(0, live(0)), (1, live(0)), (OWN, live(0)), (10, live(0))]);
        assert_eq!(survivors_in(&snapshot, &readings, OWN, at(0)), vec![10]);
    }

    #[test]
    fn a_start_time_from_before_the_machine_booted_is_not_a_survivor() {
        let booted = TAKEN_AT - 3_600;
        let snapshot = LambSnapshot::new([10, 11, 12], at(TAKEN_AT));
        let readings = HashMap::from([
            (10, live(booted)),
            (11, live(booted - BOOT_SLACK_SECS - 1)),
            // An uptime-sized epoch reading, as a Linux host with no
            // `btime` line produces.
            (12, live(90)),
        ]);
        assert_eq!(
            survivors_in(&snapshot, &readings, OWN, ProcInstant::after_boot(booted)),
            vec![10]
        );
    }

    // Tests the guard without `kill`: a broken guard here must not
    // signal this test's own process group.
    #[test]
    fn a_pid_that_is_never_a_lamb_is_refused() {
        for pid in [0, 1, OWN, u32::MAX] {
            assert_eq!(lamb_pid(pid, OWN), Err(SignalError::NotALamb(pid)));
        }
        assert_eq!(lamb_pid(10, OWN), Ok(Pid::from_raw(10)));
    }

    #[test]
    fn a_walk_names_the_child_and_the_generation_below_it() {
        let child = Child::with_lamb("sleep 30");
        let child_pid = child.pid;
        let table = ProcessTable::new();

        let below_us = table.descendants(&[std::process::id()]);
        assert!(
            below_us.pids().any(|pid| pid == child_pid),
            "our own child is below us: {:?}",
            below_us.pids().collect::<Vec<_>>()
        );

        let below_child = table.snapshot(child_pid);
        assert!(
            !below_child.is_empty(),
            "the ancestry guard passes a real child, so its backgrounded `sleep` is swept"
        );
        assert!(
            !below_child.pids().any(|pid| pid == child_pid),
            "a root is never its own descendant"
        );
    }

    #[test]
    fn a_walk_from_a_root_that_is_not_ours_is_empty() {
        let table = ProcessTable::new();
        assert!(table.snapshot(1).is_empty(), "init is no lamb of ours");
        assert!(
            table.snapshot(std::process::id()).is_empty(),
            "this process is not below itself"
        );
    }

    // Stamped at the child's own start it passes, one unit earlier it
    // does not: the stamp and the start are read on one clock.
    #[test]
    fn the_real_table_refuses_init_our_own_pid_and_a_child_younger_than_the_snapshot() {
        let child = Child::running("sleep 30");
        let child_pid = child.pid;
        let started = read_pids(&[child_pid])
            .get(&child_pid)
            .and_then(|reading| reading.started)
            .expect("a running child has a start");
        let pids = [1, std::process::id(), child_pid];
        let table = ProcessTable::new();

        let at_start = table.survivors(&LambSnapshot::new(pids, started));
        let now = table.survivors(&LambSnapshot::new(pids, ProcInstant::now()));
        let just_before = table.survivors(&LambSnapshot::new(
            pids,
            ProcInstant::from_raw(started.raw().saturating_sub(1)),
        ));

        assert_eq!(at_start, vec![child_pid], "only the child is a lamb");
        assert_eq!(now, vec![child_pid], "a walk now saw the child");
        assert!(
            just_before.is_empty(),
            "a child that started after the snapshot is not the process it saw"
        );
    }

    #[test]
    fn a_reaped_child_is_no_longer_a_survivor() {
        let mut child = Child::running("sleep 30");
        let child_pid = child.pid;
        let snapshot = LambSnapshot::new([child_pid], ProcInstant::now());
        let _ = child.child.kill();
        let _ = child.child.wait();

        assert!(ProcessTable::new().survivors(&snapshot).is_empty());
    }

    #[test]
    fn term_reaches_the_one_pid_it_names() {
        let mut child = Child::running("sleep 30");

        let delivered = ProcessTable::new().signal(child.pid, LambSignal::Term);
        if delivered.is_err() {
            let _ = child.child.kill();
        }
        let status = child.child.wait().expect("the child is ours to reap");

        assert_eq!(delivered, Ok(()));
        assert_eq!(status.signal(), Some(Signal::SIGTERM as i32));
    }
}
