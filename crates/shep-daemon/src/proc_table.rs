//! Targeted reads of the OS process table: a handful of named pids, never a
//! whole walk
//!
//! A handover asks when one adopted sheep started; a lamb sweep asks whether
//! each pid it saw is still the same live process. Both refresh only the pids
//! they name, so the cost scales with the question rather than with the
//! machine's process count. Unix only, like both callers.

use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

/// What one targeted read says about a pid the table still lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PidReading {
    /// The wall-clock second the process started, `None` when the OS will
    /// not say.
    pub started_secs: Option<u64>,
    /// Whether the process has exited and waits only to be reaped.
    pub zombie: bool,
}

/// The refresh kind [`read_pids`] asks sysinfo for.
///
/// Nothing: start time and status are filled whatever this says, so memory
/// or CPU would cost the memory poll's syscalls, and tasks a
/// `/proc/<pid>/task/` walk, for figures nothing here reads.
fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().without_tasks()
}

/// One refresh of only `pids`, keyed by pid.
///
/// A pid absent from the answer is not running. Its own [`System`], since a
/// retained table would answer from a stale row for a pid it has seen
/// before. A reported start of `0` is sysinfo's unfilled value, so it reads
/// as unknown rather than as the epoch.
pub(crate) fn read_pids(pids: &[u32]) -> HashMap<u32, PidReading> {
    let wanted: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&wanted), true, refresh_kind());
    system
        .processes()
        .values()
        .map(|process| {
            let started = process.start_time();
            let reading = PidReading {
                started_secs: (started != 0).then_some(started),
                zombie: matches!(
                    process.status(),
                    ProcessStatus::Zombie | ProcessStatus::Dead
                ),
            };
            (process.pid().as_u32(), reading)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    // Pid 0 is the one number that is never a process this daemon could
    // read, so it cannot collide with a real child.
    #[test]
    fn a_pid_the_table_does_not_know_is_absent() {
        assert!(read_pids(&[0]).is_empty());
    }

    #[test]
    fn the_refresh_kind_asks_for_nothing_the_start_time_does_not_need() {
        let kind = refresh_kind();
        assert!(!kind.memory(), "the start time is not a memory reading");
        assert!(!kind.cpu(), "the start time is not a CPU reading");
        assert!(!kind.tasks(), "nothing here reads a thread");
    }

    #[test]
    fn a_running_child_reads_as_live_with_a_start_time() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("/bin/sh is present on every platform this module compiles for");
        let pid = child.id();

        let readings = read_pids(&[pid]);

        let _ = child.kill();
        let _ = child.wait();
        let reading = readings.get(&pid).expect("a running child is in the table");
        assert!(!reading.zombie, "{reading:?}");
        assert!(reading.started_secs.is_some(), "{reading:?}");
        assert_eq!(readings.len(), 1, "only the named pid is read");
    }
}
