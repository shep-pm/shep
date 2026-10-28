//! Reaping the sheep a successor adopted, one pid at a time and never
//! wildcard.
//!
//! An adopted sheep has no `tokio::process::Child` and none can be built
//! from a bare pid, so this reaper runs alongside tokio's own in the same
//! process. `waitpid(-1, ..)` here would race tokio's reaper for a status
//! tokio needed, losing an exit code and a signal for good. A targeted wait
//! steals nothing: nothing else in this process holds a `Child` for an
//! adopted pid. Each wait arms its own multiplexed `SIGCHLD` stream.
//! `shep-handover` does the waiting; what stays here is the replay.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::OnceCell;

use crate::runner::ExitOutcome;

/// Waits on the pids a successor adopted, targeted and never wildcard.
///
/// One reaper serves a whole adopted flock; [`AdoptedReaper::wait`] takes
/// `&self`, so several waits can be in flight at once, one per sheep. The
/// waiting itself is [`shep_handover::wait_for_child`]'s.
///
/// `Debug` is derived: pids and exit statuses only, no environment value.
#[derive(Debug, Default)]
pub struct AdoptedReaper {
    /// The status of each pid waited on, once taken from the kernel.
    ///
    /// A status can be collected once, so a second wait would meet `ECHILD`
    /// and lose it. Remembering replays the first answer instead, which is
    /// what `crate::runner::RunningProcess::wait` already promises. A cell
    /// rather than a bare outcome, so two waits on one pid share the one
    /// `waitpid` instead of racing for it.
    observed: Mutex<HashMap<u32, Arc<OnceCell<ExitOutcome>>>>,
}

impl AdoptedReaper {
    /// A reaper that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for one adopted pid and reports how it exited.
    ///
    /// A dropped wait loses no status: one already taken is replayed.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::InvalidInput`] if `pid` is zero, which `waitpid`
    ///   reads as a group-wide wait, or does not fit in an `i32`.
    /// - [`io::ErrorKind::NotFound`] if the kernel has no such child and
    ///   this reaper never took its status, so the exit is gone.
    /// - Any other `waitpid` errno, or a failure to arm `SIGCHLD`, as-is.
    pub async fn wait(&self, pid: u32) -> io::Result<ExitOutcome> {
        let cell = {
            let mut observed = self.observed.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(observed.entry(pid).or_default())
        };
        cell.get_or_try_init(|| async {
            let exit = shep_handover::wait_for_child(pid).await?;
            Ok(ExitOutcome {
                code: exit.code,
                signal: exit.signal,
            })
        })
        .await
        .copied()
    }
}

#[cfg(test)]
mod tests {
    // An adopted sheep reaches a successor as a bare pid with no handle, so
    // the reaper collects the status a `Child::wait()` would take first.
    #![expect(
        clippy::zombie_processes,
        reason = "the reaper collects these statuses; a Child::wait would take them first"
    )]

    use core::time::Duration;
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    use super::AdoptedReaper;
    use crate::runner::ExitOutcome;

    /// A child spawned the way an adopted sheep reaches a successor: through
    /// `std::process::Command`, so tokio holds no `Child` for it.
    fn adopted(script: &str) -> std::process::Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn a shell")
    }

    #[tokio::test]
    async fn an_adopted_pid_yields_its_real_exit_code() {
        let child = adopted("exit 7");
        let reaper = AdoptedReaper::new();
        let outcome = tokio::time::timeout(Duration::from_secs(10), reaper.wait(child.id()))
            .await
            .expect("the reaper answered within the budget")
            .expect("the adopted pid was reapable");
        assert_eq!(
            outcome,
            ExitOutcome {
                code: Some(7),
                signal: None
            }
        );
    }

    #[tokio::test]
    async fn an_adopted_pid_killed_by_a_signal_reports_the_signal() {
        let child = adopted("sleep 30");
        let pid = child.id();
        signal::kill(
            Pid::from_raw(i32::try_from(pid).expect("a pid fits in an i32")),
            Signal::SIGKILL,
        )
        .expect("kill the adopted child");
        let reaper = AdoptedReaper::new();
        let outcome = tokio::time::timeout(Duration::from_secs(10), reaper.wait(pid))
            .await
            .expect("the reaper answered within the budget")
            .expect("the adopted pid was reapable");
        assert_eq!(
            outcome,
            ExitOutcome {
                code: None,
                signal: Some(9)
            }
        );
    }

    /// The kernel holds the zombie until somebody waits, so an
    /// implementation that awaits the signal before looking hangs here and
    /// the timeout turns that into a failure.
    #[tokio::test]
    async fn a_pid_that_exited_before_the_first_look_still_yields_its_status() {
        let mut child = adopted("echo bye; exit 5");
        let pid = child.id();
        let mut said = String::new();
        child
            .stdout
            .take()
            .expect("piped stdout")
            .read_to_string(&mut said)
            .expect("read to EOF, which the child's own exit closes");
        assert_eq!(said.trim(), "bye");
        // EOF means the child is in its exit path; this gives the kernel
        // time to finish making it a zombie.
        std::thread::sleep(Duration::from_millis(50));

        let reaper = AdoptedReaper::new();
        let outcome = tokio::time::timeout(Duration::from_secs(10), reaper.wait(pid))
            .await
            .expect("the reaper answered within the budget")
            .expect("the adopted pid was reapable");
        assert_eq!(
            outcome,
            ExitOutcome {
                code: Some(5),
                signal: None
            }
        );
    }

    /// The steal is deterministic rather than raced: tokio calls `waitpid`
    /// for a live `Child` only once its `wait()` is polled, so the
    /// supervised child's status sits pending in the kernel while the
    /// adopted sheep is still running. A `waitpid(-1, ..)` in this position
    /// takes that pending status every time.
    #[tokio::test]
    async fn reaping_an_adopted_pid_does_not_disturb_a_tokio_spawned_child() {
        let reaper = std::sync::Arc::new(AdoptedReaper::new());
        for round in 0..3 {
            // Exits only when its stdin closes, so it is alive across the
            // whole window below.
            let mut carried = adopted("read line; exit 4");
            let carried_pid = carried.id();
            let waiting = {
                let reaper = std::sync::Arc::clone(&reaper);
                tokio::spawn(async move { reaper.wait(carried_pid).await })
            };
            // Long enough to arm the SIGCHLD stream and take a first look
            // before anything else exits.
            tokio::time::sleep(Duration::from_millis(50)).await;

            let mut supervised = tokio::process::Command::new("/bin/sh")
                .args(["-c", "exit 3"])
                .stdout(Stdio::null())
                .spawn()
                .expect("spawn a tokio-supervised child");
            // Nothing polls its `wait()`, so its status is pending when the
            // reaper's wakeup arrives.
            tokio::time::sleep(Duration::from_millis(200)).await;

            drop(carried.stdin.take());
            let carried_exit = tokio::time::timeout(Duration::from_secs(10), waiting)
                .await
                .expect("the reaper answered within the budget")
                .expect("the waiting task did not panic")
                .unwrap_or_else(|error| panic!("round {round}: adopted pid unreapable: {error}"));
            assert_eq!(
                carried_exit,
                ExitOutcome {
                    code: Some(4),
                    signal: None
                },
                "round {round}: the adopted pid reported somebody else's status"
            );

            let supervised_exit = tokio::time::timeout(Duration::from_secs(10), supervised.wait())
                .await
                .expect("tokio answered within the budget")
                .unwrap_or_else(|error| {
                    panic!("round {round}: tokio's own status was stolen: {error}")
                });
            assert_eq!(
                supervised_exit.code(),
                Some(3),
                "round {round}: tokio's child reported somebody else's status"
            );
        }
    }

    #[tokio::test]
    async fn a_second_wait_replays_the_status_the_first_one_saw() {
        let child = adopted("exit 6");
        let reaper = AdoptedReaper::new();
        let first = tokio::time::timeout(Duration::from_secs(10), reaper.wait(child.id()))
            .await
            .expect("the reaper answered within the budget")
            .expect("the adopted pid was reapable");
        let second = tokio::time::timeout(Duration::from_secs(10), reaper.wait(child.id()))
            .await
            .expect("the replay answered within the budget")
            .expect("the replay is not an error");
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn two_waits_on_one_pid_share_the_one_status() {
        let child = adopted("sleep 0.2; exit 8");
        let pid = child.id();
        let reaper = AdoptedReaper::new();
        let (first, second) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(reaper.wait(pid), reaper.wait(pid))
        })
        .await
        .expect("both waits answered within the budget");
        let expected = ExitOutcome {
            code: Some(8),
            signal: None,
        };
        assert_eq!(first.expect("the first wait saw the exit"), expected);
        assert_eq!(second.expect("the second wait saw the same exit"), expected);
    }

    #[tokio::test]
    async fn a_code_and_a_signal_are_never_collapsed_into_one_another() {
        let exited = adopted("exit 3");
        let signalled = adopted("kill -TERM $$; sleep 30");
        let reaper = AdoptedReaper::new();
        let (exited, signalled) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(reaper.wait(exited.id()), reaper.wait(signalled.id()))
        })
        .await
        .expect("both waits answered within the budget");
        assert_eq!(
            exited.expect("the first child was reapable"),
            ExitOutcome {
                code: Some(3),
                signal: None
            }
        );
        assert_eq!(
            signalled.expect("the second child was reapable"),
            ExitOutcome {
                code: None,
                signal: Some(15)
            }
        );
    }

    #[tokio::test]
    async fn pid_zero_is_refused_rather_than_becoming_a_group_wide_wait() {
        let reaper = AdoptedReaper::new();
        let error = reaper.wait(0).await.expect_err("pid 0 is refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
