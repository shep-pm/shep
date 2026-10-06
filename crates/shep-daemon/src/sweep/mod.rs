//! The lamb sweep: ending what a sheep left running once the sheep is gone
//!
//! A stop signals the sheep's process group, which misses a lamb that left
//! it, and a natural exit signals nothing at all. [`sweep_lambs`] closes both
//! by signalling the pids a [`LambSnapshot`] saw, and what those start while
//! the sweep waits, one by one, after the leader is reaped. [`LambSweep`] is the seam; [`StatsSweep`] reads the real
//! process table.
//!
//! ## Pid reuse
//!
//! A snapshot stores each pid with the [`ProcInstant`] it was seen, not start
//! times. A pid is a survivor only if it is alive, not a zombie, and started
//! no later than that instant. On Linux the clock is the kernel's tick, so a
//! pid recycled within one tick of the walk still passes: 10 ms at the usual
//! 100 Hz. Elsewhere it is the second, so about a second. A residual,
//! accepted. A start time from before the machine booted is never trusted.
//!
//! ## Windows
//!
//! A no-op: the job object behind `kill_tree` already reaches every process
//! a sheep spawned, so [`StatsSweep`] finds no survivors there.

use core::fmt;
use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};

use tokio::time::Instant;

use crate::proc_table::ProcInstant;

mod os;

pub(crate) use os::StatsSweep;

/// How often [`sweep_lambs`] re-reads its survivors during the grace.
///
/// Each look is one targeted refresh of a few pids, well under a
/// millisecond, so ten a second costs nothing and returns a sweep within
/// 100 ms of the last lamb exiting.
const SWEEP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long [`sweep_lambs`] waits for a `SIGKILL`ed lamb to stop running.
///
/// A killed process is gone within milliseconds unless it is stuck in
/// uninterruptible sleep, which no signal ends; this only bounds that case.
const KILL_SETTLE: Duration = Duration::from_secs(1);

/// How often [`sweep_lambs`] looks while waiting out [`KILL_SETTLE`].
pub(crate) const KILL_SETTLE_POLL: Duration = Duration::from_millis(10);

/// The pids a sweep may signal, each with the instant it was seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LambSnapshot {
    seen_at: BTreeMap<u32, ProcInstant>,
}

impl LambSnapshot {
    /// `pids` as seen at `taken_at`, read before the table was walked.
    pub(crate) fn new(pids: impl IntoIterator<Item = u32>, taken_at: ProcInstant) -> Self {
        Self {
            seen_at: pids.into_iter().map(|pid| (pid, taken_at)).collect(),
        }
    }

    /// The pids this snapshot saw, in pid order.
    pub(crate) fn pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.seen_at.keys().copied()
    }

    /// The instant `pid` was seen, or `None` if this snapshot never saw it.
    #[cfg_attr(all(not(unix), not(test)), expect(dead_code))]
    pub(crate) fn seen_at(&self, pid: u32) -> Option<ProcInstant> {
        self.seen_at.get(&pid).copied()
    }

    /// The latest instant any of its pids was seen, `None` when empty.
    pub(crate) fn latest(&self) -> Option<ProcInstant> {
        self.seen_at.values().copied().max()
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
    // Only the unix `kill` path builds these two, plus the test fake.
    #[cfg_attr(not(unix), expect(dead_code))]
    NotALamb(u32),
    /// The OS refused the `kill`, with its reason: the pid exited, or
    /// belongs to another user.
    #[cfg_attr(all(not(unix), not(test)), expect(dead_code))]
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
    /// Alive, not a zombie, and started no later than the snapshot saw it.
    /// An unknown start time, a pid `<= 1` and this daemon's own pid are
    /// never survivors.
    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32>;

    /// A fresh walk: every current ppid descendant of `roots`, the roots
    /// excluded, dated by this walk's instant.
    ///
    /// Called with survivors only. A survivor's children still point at it,
    /// so a walk from it finds what it started after the snapshot, and a
    /// survivor already passed the shepherd-ancestry check its snapshot did.
    fn descendants(&self, roots: &[u32]) -> LambSnapshot;

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
    /// Every lamb sent `SIGTERM`, delivered or not.
    pub termed: Vec<u32>,
    /// Every lamb still alive at the end of the grace, sent `SIGKILL`.
    pub killed: Vec<u32>,
}

/// Ends every survivor of `snapshot`, and whatever they start meanwhile:
/// `SIGTERM`, up to `grace` to exit, then `SIGKILL` for what is left.
///
/// Each look, every [`SWEEP_POLL_INTERVAL`], walks from the survivors and
/// adds what they started since, so a child is held before its parent exits
/// and orphans it. A lamb gets `SIGTERM` when first seen and `SIGKILL` at
/// the deadline. Returns as soon as no lamb is left, waiting up to
/// [`KILL_SETTLE`] for a `SIGKILL` to land. An empty snapshot reads nothing.
/// A failed delivery is logged and never stops the sweep.
pub(crate) async fn sweep_lambs(
    sweep: &dyn LambSweep,
    snapshot: &LambSnapshot,
    grace: Duration,
) -> SweepReport {
    if snapshot.is_empty() {
        return SweepReport::default();
    }
    let mut snapshot = snapshot.clone();
    let mut termed = BTreeSet::new();
    let deadline = Instant::now() + grace;
    let killed = loop {
        let mut alive = sweep.survivors(&snapshot);
        if alive.is_empty() {
            break alive;
        }
        let born = sweep.descendants(&alive);
        alive.extend(born.pids());
        alive.sort_unstable();
        alive.dedup();
        snapshot = snapshot.merge(born);
        let first_seen: Vec<u32> = alive
            .iter()
            .copied()
            .filter(|&pid| termed.insert(pid))
            .collect();
        deliver(sweep, &first_seen, LambSignal::Term);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break alive;
        }
        tokio::time::sleep(remaining.min(SWEEP_POLL_INTERVAL)).await;
    };
    if termed.is_empty() {
        return SweepReport::default();
    }
    let termed: Vec<u32> = termed.into_iter().collect();

    tracing::info!(lambs = ?termed, "swept lambs that outlived their sheep");
    if !killed.is_empty() {
        tracing::warn!(lambs = ?killed, "lambs ignored SIGTERM for the whole grace; sending SIGKILL");
        deliver(sweep, &killed, LambSignal::Kill);
        let stuck = poll_until_gone(sweep, &snapshot, KILL_SETTLE, KILL_SETTLE_POLL).await;
        if !stuck.is_empty() {
            tracing::warn!(lambs = ?stuck, "lambs still running after SIGKILL; leaving them");
        }
    }
    SweepReport { termed, killed }
}

/// Re-reads `snapshot`'s survivors every `every` until none are left or
/// `bound` has passed, and returns the last reading.
async fn poll_until_gone(
    sweep: &dyn LambSweep,
    snapshot: &LambSnapshot,
    bound: Duration,
    every: Duration,
) -> Vec<u32> {
    let deadline = Instant::now() + bound;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining.min(every)).await;
        let left = sweep.survivors(snapshot);
        if left.is_empty() || Instant::now() >= deadline {
            break left;
        }
    }
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
