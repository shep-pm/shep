//! The lamb sweep: ending what a sheep left running once the sheep is gone
//!
//! A stop signals the sheep's process group, which misses a lamb that left
//! it, and a natural exit signals nothing at all. [`sweep_lambs`] closes both
//! by signalling the pids a [`LambSnapshot`] saw, one by one, after the
//! leader is reaped. [`LambSweep`] is the seam; [`StatsSweep`] reads the real
//! process table.
//!
//! ## Pid reuse
//!
//! A snapshot stores each pid with the second it was seen, not start times.
//! A pid is a survivor only if it is alive, not a zombie, and started no
//! later than that second. A pid recycled within that same second still
//! passes: a one-second residual, accepted.
//!
//! ## Windows
//!
//! A no-op: the job object behind `kill_tree` already reaches every process
//! a sheep spawned, so [`StatsSweep`] finds no survivors there.

use core::fmt;
use core::time::Duration;
use std::collections::BTreeMap;

use tokio::time::Instant;

mod os;

#[cfg_attr(not(test), expect(unused_imports))]
pub(crate) use os::StatsSweep;

/// How often [`sweep_lambs`] re-reads its survivors during the grace.
///
/// Each look is one targeted refresh of a few pids, well under a
/// millisecond, so ten a second costs nothing and returns a sweep within
/// 100 ms of the last lamb exiting.
const SWEEP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The pids a sweep may signal, each with the wall-clock second it was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LambSnapshot {
    seen_at: BTreeMap<u32, u64>,
}

impl LambSnapshot {
    /// `pids` as seen at `taken_at_secs`, seconds since the Unix epoch, read
    /// before the table was walked.
    pub(crate) fn new(pids: impl IntoIterator<Item = u32>, taken_at_secs: u64) -> Self {
        Self {
            seen_at: pids.into_iter().map(|pid| (pid, taken_at_secs)).collect(),
        }
    }

    /// The pids this snapshot saw, in pid order.
    pub(crate) fn pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.seen_at.keys().copied()
    }

    /// The second `pid` was seen, or `None` if this snapshot never saw it.
    pub(crate) fn seen_at(&self, pid: u32) -> Option<u64> {
        self.seen_at.get(&pid).copied()
    }

    /// Whether this snapshot saw no lambs at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.seen_at.is_empty()
    }

    /// Both snapshots' pids, each dated by the later look that saw it.
    ///
    /// Either look proves the identity of a process that started no later
    /// than it, so the later one refuses the fewest real lambs.
    #[must_use]
    pub(crate) fn merge(mut self, other: Self) -> Self {
        for (pid, seen) in other.seen_at {
            let at = self.seen_at.entry(pid).or_insert(seen);
            *at = (*at).max(seen);
        }
        self
    }
}

/// Which of the two signals a sweep sends a lamb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LambSignal {
    /// `SIGTERM`: the polite first rung.
    Term,
    /// `SIGKILL`: for a lamb still alive when the grace runs out.
    Kill,
}

/// Why a sweep's signal never reached a lamb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignalError {
    /// The pid is `0`, `1`, this daemon's own, or too large for the OS to
    /// name: a `kill` there would hit a group, init or the shepherd.
    NotALamb(u32),
    /// The OS refused the `kill`, with its reason: the pid exited, or
    /// belongs to another user.
    Refused(String),
    /// This platform has no per-pid signal delivery for a sweep to use.
    #[cfg(not(unix))]
    Unsupported,
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotALamb(pid) => write!(f, "pid {pid} is never a lamb to signal"),
            Self::Refused(reason) => write!(f, "the OS refused the signal: {reason}"),
            #[cfg(not(unix))]
            Self::Unsupported => f.write_str("this platform cannot signal a single lamb"),
        }
    }
}

impl core::error::Error for SignalError {}

/// The process-table half of a lamb sweep.
///
/// Synchronous and dyn-compatible, like [`MemorySampler`]: each call is a
/// bounded syscall walk.
///
/// [`MemorySampler`]: crate::limits::sample::MemorySampler
pub(crate) trait LambSweep: Send + Sync {
    /// A fresh walk: every ppid descendant of `root_pid`, excluding it.
    fn snapshot(&self, root_pid: u32) -> LambSnapshot;

    /// What the last periodic tick recorded for `root_pid`, if anything.
    fn last_snapshot(&self, root_pid: u32) -> Option<LambSnapshot>;

    /// The pids in `snapshot` that are still the processes it saw.
    ///
    /// Alive, not a zombie, and started no later than the snapshot's second.
    /// An unknown start time, a pid `<= 1` and this daemon's own pid are
    /// never survivors.
    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32>;

    /// Sends `signal` to `pid` alone, never to a group.
    ///
    /// # Errors
    ///
    /// [`SignalError`] when the signal was not delivered.
    fn signal(&self, pid: u32, signal: LambSignal) -> Result<(), SignalError>;
}

/// What one [`sweep_lambs`] call signalled, in pid order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    /// Every survivor sent `SIGTERM`, delivered or not.
    pub termed: Vec<u32>,
    /// Every survivor still alive at the end of the grace, sent `SIGKILL`.
    pub killed: Vec<u32>,
}

/// Ends every survivor of `snapshot`: `SIGTERM`, up to `grace` to exit, then
/// `SIGKILL` for what is left.
///
/// Returns as soon as no survivor remains. An empty snapshot reads nothing.
/// A failed delivery is logged and never stops the sweep.
pub(crate) async fn sweep_lambs(
    sweep: &dyn LambSweep,
    snapshot: &LambSnapshot,
    grace: Duration,
) -> SweepReport {
    if snapshot.is_empty() {
        return SweepReport::default();
    }
    let termed = sweep.survivors(snapshot);
    if termed.is_empty() {
        return SweepReport::default();
    }
    deliver(sweep, &termed, LambSignal::Term);

    let deadline = Instant::now() + grace;
    let killed = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining.min(SWEEP_POLL_INTERVAL)).await;
        let left = sweep.survivors(snapshot);
        if left.is_empty() || Instant::now() >= deadline {
            break left;
        }
    };

    tracing::info!(lambs = ?termed, "swept lambs that outlived their sheep");
    if !killed.is_empty() {
        tracing::warn!(lambs = ?killed, "lambs ignored SIGTERM for the whole grace; sending SIGKILL");
        deliver(sweep, &killed, LambSignal::Kill);
    }
    SweepReport { termed, killed }
}

/// Sends `signal` to each of `pids`, logging every refusal.
fn deliver(sweep: &dyn LambSweep, pids: &[u32], signal: LambSignal) {
    for &pid in pids {
        if let Err(error) = sweep.signal(pid, signal) {
            tracing::warn!(pid, ?signal, %error, "lamb signal delivery failed");
        }
    }
}

#[cfg(test)]
mod tests;
