//! [`LambSweep`] over the real process table

use std::sync::Arc;

use super::{LambSignal, LambSnapshot, LambSweep, SignalError};
use crate::limits::stats::StatsState;

/// The real [`LambSweep`]: walks through [`StatsState`]'s sampler and reads
/// the snapshot its poll tick recorded.
#[derive(Debug)]
pub(crate) struct StatsSweep {
    stats: Arc<StatsState>,
}

impl StatsSweep {
    /// A sweep over `stats`, the same state the poll tick records into.
    pub(crate) fn new(stats: Arc<StatsState>) -> Self {
        Self { stats }
    }
}

impl LambSweep for StatsSweep {
    fn snapshot(&self, root_pid: u32) -> LambSnapshot {
        self.stats.lamb_snapshot_now(root_pid, std::process::id())
    }

    fn last_snapshot(&self, root_pid: u32) -> Option<LambSnapshot> {
        self.stats.last_lamb_snapshot(root_pid)
    }

    fn descendants(&self, roots: &[u32]) -> LambSnapshot {
        self.stats.lamb_walk_from(roots)
    }

    #[cfg(unix)]
    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32> {
        let pids: Vec<u32> = snapshot.pids().collect();
        unix::survivors_in(
            snapshot,
            &crate::proc_table::read_pids(&pids),
            std::process::id(),
            crate::proc_table::ProcInstant::earliest_start(),
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
    use crate::proc_table::{PidReading, ProcInstant};

    /// The positive pid `kill` may name, or why `pid` is never a lamb.
    ///
    /// Never `0` (the caller's group), `1` (init) or `own` (the shepherd).
    fn lamb_pid(pid: u32, own: u32) -> Result<Pid, SignalError> {
        match i32::try_from(pid) {
            Ok(raw) if pid > 1 && pid != own => Ok(Pid::from_raw(raw)),
            _ => Err(SignalError::NotALamb(pid)),
        }
    }

    /// The pids in `snapshot` that `readings` show are still the processes
    /// it saw.
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

    #[cfg(test)]
    mod tests {
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::Command;

        use super::*;
        use crate::sweep::{LambSweep as _, StatsSweep};

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

        // The sweep reads the clock-native start, not the epoch second a
        // handover reads, which on Linux is two seconds coarse.
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
        fn init_pid_zero_and_the_shepherd_itself_are_never_survivors() {
            let snapshot = LambSnapshot::new([0, 1, OWN, 10], at(TAKEN_AT));
            let readings =
                HashMap::from([(0, live(0)), (1, live(0)), (OWN, live(0)), (10, live(0))]);
            assert_eq!(survivors_in(&snapshot, &readings, OWN, at(0)), vec![10]);
        }

        #[test]
        fn a_start_time_from_before_the_machine_booted_is_not_a_survivor() {
            let booted = TAKEN_AT - 3_600;
            let snapshot = LambSnapshot::new([10, 11, 12], at(TAKEN_AT));
            let readings = HashMap::from([
                (10, live(booted)),
                (11, live(booted - crate::proc_table::BOOT_SLACK_SECS - 1)),
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

        /// A child of this test process, killed and reaped on drop.
        struct Child(std::process::Child);

        impl Child {
            fn sleeping() -> Self {
                let child = Command::new("/bin/sh")
                    .arg("-c")
                    .arg("sleep 30")
                    .spawn()
                    .expect("/bin/sh is present on every platform this module compiles for");
                Self(child)
            }
        }

        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn real_sweep() -> StatsSweep {
            StatsSweep::new(crate::testing::idle_stats())
        }

        // Stamped at the child's own start it passes, one unit earlier it
        // does not: the stamp and the start are read on one clock.
        #[test]
        fn the_real_table_refuses_init_our_own_pid_and_a_child_younger_than_the_snapshot() {
            let child = Child::sleeping();
            let child_pid = child.0.id();
            let started = crate::proc_table::read_pids(&[child_pid])
                .get(&child_pid)
                .and_then(|reading| reading.started)
                .expect("a running child has a start");
            let pids = [1, std::process::id(), child_pid];
            let sweep = real_sweep();

            let at_start = sweep.survivors(&LambSnapshot::new(pids, started));
            let now = sweep.survivors(&LambSnapshot::new(pids, ProcInstant::now()));
            let just_before = sweep.survivors(&LambSnapshot::new(
                pids,
                at(started.raw().saturating_sub(1)),
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
            let mut child = Child::sleeping();
            let child_pid = child.0.id();
            let snapshot = LambSnapshot::new([child_pid], ProcInstant::now());
            let _ = child.0.kill();
            let _ = child.0.wait();

            assert!(real_sweep().survivors(&snapshot).is_empty());
        }

        #[test]
        fn term_reaches_the_one_pid_it_names() {
            let mut child = Child::sleeping();

            let delivered = real_sweep().signal(child.0.id(), LambSignal::Term);
            if delivered.is_err() {
                let _ = child.0.kill();
            }
            let status = child.0.wait().expect("the child is ours to reap");

            assert_eq!(delivered, Ok(()));
            assert_eq!(status.signal(), Some(Signal::SIGTERM as i32));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::sample::TreeIndex;
    use crate::proc_table::ProcInstant;
    use crate::testing::{ScriptedSampler, rss};

    #[test]
    fn the_real_sweep_walks_through_stats_and_reads_the_ticks_snapshot() {
        let table = vec![
            rss(100, Some(std::process::id()), 0),
            rss(101, Some(100), 0),
        ];
        let stats = Arc::new(StatsState::new(Arc::new(ScriptedSampler::new(vec![
            table.clone(),
        ]))));
        stats.watch(1, 100);
        let sweep: &dyn LambSweep = &StatsSweep::new(Arc::clone(&stats));

        assert_eq!(sweep.snapshot(100).pids().collect::<Vec<_>>(), vec![101]);
        assert_eq!(sweep.last_snapshot(100), None, "no tick has run");

        let taken_at = ProcInstant::from_raw(5);
        stats.record_lamb_snapshots(&TreeIndex::build(&table), taken_at, std::process::id());
        assert_eq!(
            sweep.last_snapshot(100),
            Some(LambSnapshot::new([101], taken_at))
        );
    }
}
