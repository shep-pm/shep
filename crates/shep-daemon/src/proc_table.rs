//! Targeted reads of the OS process table: a handful of named pids, never a
//! whole walk
//!
//! A handover asks when one adopted sheep started; a lamb sweep asks whether
//! each pid it saw is still the same live process. Both refresh only the pids
//! they name, so the cost scales with the question rather than with the
//! machine's process count. The reads are unix only, like both callers.
//! [`ProcInstant`], the clock a sweep dates pids on, exists everywhere.

#[cfg(unix)]
use std::collections::HashMap;

#[cfg(unix)]
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

/// A moment on the clock a process start is read in.
///
/// Ticks since boot on Linux, so two moments a tick apart differ. Epoch
/// seconds everywhere else, where the start a process reports is a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProcInstant(u64);

impl ProcInstant {
    /// Now, on the clock a fresh process's start would read.
    ///
    /// On Linux an unreadable clock reads as boot itself, which refuses every
    /// lamb rather than trusting a stranger.
    pub(crate) fn now() -> Self {
        #[cfg(target_os = "linux")]
        let now = linux::ticks_now().unwrap_or(0);
        #[cfg(not(target_os = "linux"))]
        let now = crate::now_ms() / 1000;
        Self(now)
    }

    /// The earliest start a live process can truthfully report.
    ///
    /// Ticks since boot cannot precede boot. Elsewhere this is the boot
    /// second, less `BOOT_SLACK_SECS`.
    #[cfg(unix)]
    pub(crate) fn earliest_start() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self(0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::after_boot((crate::now_ms() / 1000).saturating_sub(System::uptime()))
        }
    }

    /// The earliest start second to trust on a machine booted at
    /// `booted_secs`.
    #[cfg(any(test, all(unix, not(target_os = "linux"))))]
    pub(crate) fn after_boot(booted_secs: u64) -> Self {
        Self(booted_secs.saturating_sub(BOOT_SLACK_SECS))
    }

    /// The instant `raw` units after this clock's zero.
    #[cfg(test)]
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The units since this clock's zero.
    #[cfg(test)]
    pub(crate) const fn raw(self) -> u64 {
        self.0
    }
}

/// How far before the boot second an epoch start may read and still count:
/// both are floored seconds.
#[cfg(any(test, all(unix, not(target_os = "linux"))))]
pub(crate) const BOOT_SLACK_SECS: u64 = 2;

/// What one targeted read says about a pid the table still lists.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PidReading {
    /// The wall-clock second the process started, `None` when the OS will
    /// not say.
    pub started_secs: Option<u64>,
    /// When the process started on the [`ProcInstant`] clock, `None` when
    /// the OS will not say.
    pub started: Option<ProcInstant>,
    /// Whether the process has exited and waits only to be reaped.
    pub zombie: bool,
}

/// The refresh kind [`read_pids`] asks sysinfo for.
///
/// Nothing: start time and status are filled whatever this says, so memory
/// or CPU would cost the memory poll's syscalls, and tasks a
/// `/proc/<pid>/task/` walk, for figures nothing here reads.
#[cfg(unix)]
fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().without_tasks()
}

/// One refresh of only `pids`, keyed by pid.
///
/// A pid absent from the answer is not running. Its own [`System`], since a
/// retained table would answer from a stale row for a pid it has seen
/// before. A reported start of `0` is sysinfo's unfilled value, so it reads
/// as unknown rather than as the epoch.
#[cfg(unix)]
pub(crate) fn read_pids(pids: &[u32]) -> HashMap<u32, PidReading> {
    let wanted: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&wanted), true, refresh_kind());
    system
        .processes()
        .values()
        .map(|process| {
            let pid = process.pid().as_u32();
            let started = process.start_time();
            let started_secs = (started != 0).then_some(started);
            let reading = PidReading {
                started_secs,
                started: start_instant(pid, started_secs),
                zombie: matches!(
                    process.status(),
                    ProcessStatus::Zombie | ProcessStatus::Dead
                ),
            };
            (pid, reading)
        })
        .collect()
}

/// `pid`'s start in clock ticks since boot, read after sysinfo's refresh:
/// a pid reused in between reads as started later, never earlier.
#[cfg(target_os = "linux")]
fn start_instant(pid: u32, _started_secs: Option<u64>) -> Option<ProcInstant> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_start_ticks(&stat).map(ProcInstant)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn start_instant(_pid: u32, started_secs: Option<u64>) -> Option<ProcInstant> {
    started_secs.map(ProcInstant)
}

/// Field 22 of a `/proc/<pid>/stat` line: the start, in ticks since boot.
///
/// Counted from the last `)`, since `comm` (field 2) may hold spaces and
/// parens of its own.
#[cfg(any(test, target_os = "linux"))]
fn parse_start_ticks(stat: &str) -> Option<u64> {
    let (_, after_comm) = stat.rsplit_once(')')?;
    // `after_comm` opens on field 3, so field 22 is its twentieth.
    after_comm.split_ascii_whitespace().nth(19)?.parse().ok()
}

/// `secs` and `nanos` of a clock reading as whole ticks at `hz`, floored as
/// the kernel floors a start.
#[cfg(any(test, target_os = "linux"))]
fn ticks_at(secs: u64, nanos: u64, hz: u64) -> Option<u64> {
    secs.checked_mul(hz)?
        .checked_add(nanos.checked_mul(hz)? / 1_000_000_000)
}

#[cfg(target_os = "linux")]
mod linux {
    use nix::time::{ClockId, clock_gettime};
    use nix::unistd::{SysconfVar, sysconf};

    /// `USER_HZ`, the unit `/proc/<pid>/stat` reports a start in.
    pub(super) fn ticks_per_second() -> Option<u64> {
        let hz = sysconf(SysconfVar::CLK_TCK).ok()??;
        u64::try_from(hz).ok().filter(|&hz| hz > 0)
    }

    /// `CLOCK_BOOTTIME` in ticks: the clock a process start is taken on.
    pub(super) fn ticks_now() -> Option<u64> {
        let hz = ticks_per_second()?;
        let now = clock_gettime(ClockId::CLOCK_BOOTTIME).ok()?;
        super::ticks_at(
            u64::try_from(now.tv_sec()).ok()?,
            u64::try_from(now.tv_nsec()).ok()?,
            hz,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `stat` line whose field 22 is 98765, with `comm` as given.
    fn stat_line(comm: &str) -> String {
        format!(
            "1234 ({comm}) S 1 1234 1234 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 0 98765 \
             5636096 200 18446744073709551615"
        )
    }

    #[test]
    fn the_start_is_field_twenty_two_of_a_stat_line() {
        assert_eq!(parse_start_ticks(&stat_line("sleep")), Some(98765));
    }

    // Split at the first `)`, this `comm` shifts every field by two.
    #[test]
    fn a_comm_holding_parens_and_spaces_does_not_shift_the_fields() {
        assert_eq!(parse_start_ticks(&stat_line("a) (b")), Some(98765));
        assert_eq!(parse_start_ticks(&stat_line("x) 1 2 3 (y")), Some(98765));
    }

    #[test]
    fn a_stat_line_that_is_cut_short_or_garbled_has_no_start() {
        assert_eq!(parse_start_ticks(""), None);
        assert_eq!(parse_start_ticks("1234 sleep S 1 2 3"), None, "no `)`");
        assert_eq!(parse_start_ticks("1234 (sleep) S 1 1234"), None);
        let garbled = stat_line("sleep").replace(" 98765 ", " 98x65 ");
        assert_eq!(parse_start_ticks(&garbled), None);
    }

    // Rounding up would date a walk a tick late, passing a pid reused in
    // the tick after it.
    #[test]
    fn a_clock_reading_floors_to_the_tick() {
        assert_eq!(ticks_at(5, 0, 100), Some(500));
        assert_eq!(ticks_at(5, 9_999_999, 100), Some(500));
        assert_eq!(ticks_at(5, 10_000_000, 100), Some(501));
        assert_eq!(ticks_at(5, 999_999_999, 100), Some(599));
        assert_eq!(ticks_at(u64::MAX, 0, 100), None);
    }

    #[test]
    fn the_earliest_start_after_a_boot_allows_the_floored_seconds_slack() {
        assert_eq!(
            ProcInstant::after_boot(1_000).raw(),
            1_000 - BOOT_SLACK_SECS
        );
        assert_eq!(ProcInstant::after_boot(1).raw(), 0);
    }

    #[cfg(unix)]
    mod unix {
        use std::process::Command;

        use super::*;

        /// A `sleep` child of this test process, killed and reaped on drop.
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
            let child = Child::sleeping();
            let pid = child.0.id();

            let readings = read_pids(&[pid]);

            let reading = readings.get(&pid).expect("a running child is in the table");
            assert!(!reading.zombie, "{reading:?}");
            assert!(reading.started_secs.is_some(), "{reading:?}");
            assert_eq!(readings.len(), 1, "only the named pid is read");
        }

        // Both reads before and after the spawn bound the child's start, so
        // a start on any other clock or unit lands outside them.
        #[test]
        fn a_fresh_childs_start_lies_between_the_clock_before_and_after_it() {
            let before = ProcInstant::now();
            let child = Child::sleeping();
            let readings = read_pids(&[child.0.id()]);
            let after = ProcInstant::now();

            let started = readings
                .get(&child.0.id())
                .and_then(|reading| reading.started)
                .expect("a running child has a start");
            assert!(
                before <= started && started <= after,
                "{before:?} <= {started:?} <= {after:?}"
            );
        }

        #[test]
        fn the_earliest_start_is_no_later_than_a_running_childs() {
            let child = Child::sleeping();
            let started = read_pids(&[child.0.id()])
                .get(&child.0.id())
                .and_then(|reading| reading.started)
                .expect("a running child has a start");
            assert!(ProcInstant::earliest_start() <= started);
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::super::linux::ticks_per_second;
        use super::*;

        // The spec's bound: a start within a second before now proves the
        // stat field and `CLOCK_BOOTTIME` share a base.
        #[test]
        fn a_fresh_childs_start_is_within_a_second_of_now_in_ticks() {
            let hz = ticks_per_second().expect("every Linux has a USER_HZ");
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("sleep 30")
                .spawn()
                .expect("/bin/sh is present on Linux");
            let pid = child.id();
            let started = read_pids(&[pid]).get(&pid).and_then(|r| r.started);
            let now = ProcInstant::now();
            let _ = child.kill();
            let _ = child.wait();

            let started = started.expect("a running child has a start").raw();
            assert!(
                now.raw().saturating_sub(hz) <= started && started <= now.raw(),
                "start {started} ticks against now {now:?} at {hz} Hz"
            );
        }
    }
}
