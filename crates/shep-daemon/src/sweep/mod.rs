//! The lamb sweep: ending what a sheep left running once the sheep is gone
//!
//! [`shep_tree::sweep::Sweeper`] owns the sweep. [`sweep_lambs`] drives it
//! with Tokio's clock and logs its report. [`StatsSweep`] supplies the
//! process-table reads and the last stats tick's snapshot.

use core::time::Duration;

use shep_tree::sweep::{Step, Sweeper};
use tokio::time::Instant;

mod os;

pub(crate) use os::StatsSweep;
pub(crate) use shep_tree::{LambSignal, LambSnapshot, LambSweep, SignalError, SweepReport};

#[cfg(test)]
pub(crate) use shep_tree::sweep::KILL_SETTLE_POLL;

/// Ends `snapshot`'s survivors with `grace` to exit before `SIGKILL`, then logs the report.
pub(crate) async fn sweep_lambs(
    sweep: &dyn LambSweep,
    snapshot: &LambSnapshot,
    grace: Duration,
) -> SweepReport {
    let mut sweeper = Sweeper::new(snapshot.clone(), grace);
    loop {
        let looked_at = Instant::now();
        match sweeper.step(sweep, looked_at.into_std()) {
            // The sweeper timed this wait from `looked_at`, so the look's own
            // blocking time comes out of the sleep and the grace ends on time.
            Step::Wait(duration) => {
                tokio::time::sleep(duration.saturating_sub(looked_at.elapsed())).await;
            }
            Step::Done(report) => {
                for (pid, signal, error) in &report.refused {
                    tracing::warn!(pid, ?signal, %error, "lamb signal delivery failed");
                }
                if !report.termed.is_empty() {
                    tracing::info!(lambs = ?report.termed, "swept lambs that outlived their sheep");
                }
                if !report.killed.is_empty() {
                    tracing::warn!(lambs = ?report.killed, "lambs ignored SIGTERM for the whole grace; sending SIGKILL");
                }
                if !report.stuck.is_empty() {
                    tracing::warn!(lambs = ?report.stuck, "lambs still running after SIGKILL; leaving them");
                }
                return report;
            }
        }
    }
}

#[cfg(test)]
mod tests;
