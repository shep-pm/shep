//! The lamb sweep: ending what a process left running once it is gone
//!
//! A stop signals a process group, which misses a lamb that left it, and a
//! natural exit signals nothing at all. [`Sweeper`] closes both: it signals
//! the pids a [`LambSnapshot`] saw, and what those start while it waits.
//! [`LambSweep`] is the seam over a process table, and `ProcessTable` the
//! real one behind it, under the `sysinfo` feature, which carries the worked
//! example.

use core::fmt;
use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::instant::ProcInstant;

#[cfg(feature = "sysinfo")]
mod table;

#[cfg(feature = "sysinfo")]
pub use table::ProcessTable;

/// How often a sweep re-reads its survivors during the grace.
///
/// One look is a targeted refresh of the pids it names plus one walk from
/// them, so ten a second costs little and a sweep returns within 100 ms of
/// the last lamb exiting.
pub const SWEEP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a sweep waits for a `SIGKILL`ed lamb to stop running.
///
/// A killed process is gone within milliseconds unless it is stuck in
/// uninterruptible sleep, which no signal ends; this only bounds that case.
pub const KILL_SETTLE: Duration = Duration::from_secs(1);

/// How often a sweep looks while waiting out [`KILL_SETTLE`].
pub const KILL_SETTLE_POLL: Duration = Duration::from_millis(10);

/// The pids a sweep may signal, each with the instant it was seen.
///
/// A pid is a survivor only if it is alive, not a zombie, and started no
/// later than the instant it was seen, which on Linux is the kernel's tick:
/// about 10 ms at the usual 100 Hz.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LambSnapshot {
    seen_at: BTreeMap<u32, ProcInstant>,
}

impl LambSnapshot {
    /// `pids` as seen at `taken_at`, read before the table was walked.
    #[must_use]
    pub fn new(pids: impl IntoIterator<Item = u32>, taken_at: ProcInstant) -> Self {
        Self {
            seen_at: pids.into_iter().map(|pid| (pid, taken_at)).collect(),
        }
    }

    /// The pids this snapshot saw, in pid order.
    pub fn pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.seen_at.keys().copied()
    }

    /// The instant `pid` was seen, or `None` if this snapshot never saw it.
    #[must_use]
    pub fn seen_at(&self, pid: u32) -> Option<ProcInstant> {
        self.seen_at.get(&pid).copied()
    }

    /// The latest instant any of its pids was seen, `None` when empty.
    #[must_use]
    pub fn latest(&self) -> Option<ProcInstant> {
        self.seen_at.values().copied().max()
    }

    /// Whether this snapshot saw no lambs at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen_at.is_empty()
    }

    /// Both snapshots' pids, each dated by the later look that saw it.
    ///
    /// Either look proves the identity of a process that started no later
    /// than it, so the later one refuses the fewest real lambs.
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        for (pid, seen) in other.seen_at {
            let at = self.seen_at.entry(pid).or_insert(seen);
            *at = (*at).max(seen);
        }
        self
    }
}

/// Which of the two signals a sweep sends a lamb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LambSignal {
    /// `SIGTERM`: the polite first rung.
    Term,
    /// `SIGKILL`: for a lamb still alive when the grace runs out.
    Kill,
}

/// Why a sweep's signal never reached a lamb.
///
/// `#[non_exhaustive]` because this is a published library's error enum: a
/// caller's exhaustive match would break on a variant a later platform adds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignalError {
    /// The pid is `0`, `1`, this process's own, or too large for the OS to
    /// name: a `kill` there would hit a group, init or the sweeper itself.
    NotALamb(u32),
    /// The OS refused the `kill`, with its reason: the pid exited, or
    /// belongs to another user.
    Refused(String),
    /// This platform has no per-pid signal delivery for a sweep to use.
    Unsupported,
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotALamb(pid) => write!(f, "pid {pid} is never a lamb to signal"),
            Self::Refused(reason) => write!(f, "the OS refused the signal: {reason}"),
            Self::Unsupported => f.write_str("this platform cannot signal a single lamb"),
        }
    }
}

impl core::error::Error for SignalError {}

/// The process-table half of a lamb sweep.
///
/// Synchronous and dyn-compatible: each call is a bounded syscall walk, so a
/// caller may drive [`Sweeper`] from a plain thread or from an async one.
pub trait LambSweep: Send + Sync {
    /// A fresh walk: every ppid descendant of `root_pid`, excluding it.
    fn snapshot(&self, root_pid: u32) -> LambSnapshot;

    /// The pids in `snapshot` that are still the processes it saw.
    ///
    /// Alive, not a zombie, and started no later than the snapshot saw it. An
    /// unknown start time, a pid `<= 1` and this process's own pid are never
    /// survivors.
    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32>;

    /// A fresh walk: every current ppid descendant of `roots`, the roots
    /// excluded, dated by this walk's instant.
    ///
    /// Called with survivors only. A survivor's children still point at it,
    /// so a walk from it finds what it started after the snapshot, and a
    /// survivor already passed the ancestry check its snapshot did. The
    /// caller re-checks the survivors after, in case one exited mid-walk.
    fn descendants(&self, roots: &[u32]) -> LambSnapshot;

    /// Sends `signal` to `pid` alone, never to a group.
    ///
    /// # Errors
    ///
    /// [`SignalError`] when the signal was not delivered.
    fn signal(&self, pid: u32, signal: LambSignal) -> Result<(), SignalError>;

    /// What the last periodic tick recorded for `root_pid`, if anything.
    ///
    /// # Default implementation
    ///
    /// `None`, so a table with no stats tick fakes nothing. A driver with a
    /// periodic tick overrides this.
    fn last_snapshot(&self, _root_pid: u32) -> Option<LambSnapshot> {
        None
    }
}

/// What one sweep signalled, in pid order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Every lamb sent `SIGTERM`, delivered or not.
    pub termed: Vec<u32>,
    /// Every lamb still alive at the end of the grace, sent `SIGKILL`.
    pub killed: Vec<u32>,
    /// Every signal that never landed, each with its pid, its signal and the
    /// reason the table gave.
    pub refused: Vec<(u32, LambSignal, SignalError)>,
    /// Every lamb still running when [`KILL_SETTLE`] ran out, left alone.
    pub stuck: Vec<u32>,
}

/// What a [`Sweeper`] asks of its caller next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Sleep this long, then call [`Sweeper::step`] again with `now` advanced
    /// by the same duration.
    Wait(Duration),
    /// The sweep is over, with what it ended.
    Done(SweepReport),
}

/// Which rung of a sweep a [`Sweeper`] is on.
#[derive(Debug)]
enum Phase {
    /// Signalling survivors until the grace runs out.
    Signalling,
    /// Waiting out [`KILL_SETTLE`] for the lambs that were `SIGKILL`ed.
    Settling { started_at: Instant },
    /// Over. Every later [`Sweeper::step`] answers without reading a table.
    Done,
}

/// One lamb sweep, driven a look at a time by its caller.
///
/// The caller owns the clock and the sleeping: each [`Sweeper::step`] reads
/// the table once, signals what it must, and answers with the [`Step`] to
/// take next. [`Sweeper::run_blocking`] is the caller that sleeps on a plain
/// thread; a caller on an async runtime sleeps with its own timer, which is
/// how a test drives the sweep on a paused clock.
#[derive(Debug)]
pub struct Sweeper {
    snapshot: LambSnapshot,
    grace: Duration,
    phase: Phase,
    /// The first `step`'s `now`, which starts the grace.
    started_at: Option<Instant>,
    termed: BTreeSet<u32>,
    killed: Vec<u32>,
    refused: Vec<(u32, LambSignal, SignalError)>,
}

impl Sweeper {
    /// A sweep of `snapshot`'s lambs, giving them `grace` to exit on
    /// `SIGTERM` before killing what is left.
    ///
    /// A caller with both a fresh walk and the last tick's snapshot merges
    /// them before this call, as [`LambSnapshot::merge`] does, so a lamb
    /// orphaned since the tick is still swept. A [`Sweeper`] never calls
    /// [`LambSweep::last_snapshot`] itself.
    #[must_use]
    pub fn new(snapshot: LambSnapshot, grace: Duration) -> Self {
        Self {
            snapshot,
            grace,
            phase: Phase::Signalling,
            started_at: None,
            termed: BTreeSet::new(),
            killed: Vec::new(),
            refused: Vec::new(),
        }
    }

    /// Reads `table` once and takes one rung of the sweep.
    ///
    /// Pass a `now` advanced by exactly the [`Step::Wait`] durations already
    /// slept: the sweep times its grace and its settle from the `now` it is
    /// given. A failed delivery lands in [`SweepReport::refused`] and never
    /// stops the sweep. After a [`Step::Done`], every later call answers
    /// `Done` with an empty report and reads no table.
    #[must_use]
    pub fn step(&mut self, table: &dyn LambSweep, now: Instant) -> Step {
        match self.phase {
            Phase::Signalling => self.look(table, now),
            Phase::Settling { started_at } => self.settle(table, started_at, now),
            Phase::Done => Step::Done(SweepReport::default()),
        }
    }

    /// Runs the sweep to the end on this thread, sleeping with
    /// [`std::thread::sleep`] for each [`Step::Wait`].
    ///
    /// Blocks for up to `grace`, and then for up to [`KILL_SETTLE`] more.
    #[must_use]
    pub fn run_blocking(mut self, table: &dyn LambSweep) -> SweepReport {
        loop {
            match self.step(table, Instant::now()) {
                Step::Wait(sleep) => std::thread::sleep(sleep),
                Step::Done(report) => return report,
            }
        }
    }

    /// One look at the survivors: re-check them, walk from them, and `SIGTERM`
    /// whatever this look is the first to see.
    fn look(&mut self, table: &dyn LambSweep, now: Instant) -> Step {
        if self.snapshot.is_empty() {
            self.phase = Phase::Done;
            return Step::Done(SweepReport::default());
        }
        let started_at = *self.started_at.get_or_insert(now);
        let roots = table.survivors(&self.snapshot);
        if roots.is_empty() {
            return self.finish(table, roots, now);
        }
        let alive = self.walk_from(table, roots);
        let first_seen: Vec<u32> = alive
            .iter()
            .copied()
            .filter(|&pid| self.termed.insert(pid))
            .collect();
        self.deliver(table, &first_seen, LambSignal::Term);
        let left = self
            .grace
            .saturating_sub(now.saturating_duration_since(started_at));
        if left.is_zero() {
            return self.finish(table, alive, now);
        }
        Step::Wait(left.min(SWEEP_POLL_INTERVAL))
    }

    /// `roots`, the survivors of the snapshot, plus what a walk from them
    /// found, which is merged into it.
    ///
    /// The walk has no ancestry check, so its finds count only if every root
    /// survives a re-read after it: a root alive at both reads was alive
    /// throughout, and a live pid is never reused. Otherwise the finds are
    /// dropped, only the roots that passed are kept, and the next look walks.
    fn walk_from(&mut self, table: &dyn LambSweep, roots: Vec<u32>) -> Vec<u32> {
        let born = table.descendants(&roots);
        if born.is_empty() {
            return roots;
        }
        let still = table.survivors(&self.snapshot);
        if !roots.iter().all(|root| still.contains(root)) {
            return roots
                .into_iter()
                .filter(|root| still.contains(root))
                .collect();
        }
        let mut alive = roots;
        alive.extend(born.pids());
        alive.sort_unstable();
        alive.dedup();
        self.snapshot = std::mem::take(&mut self.snapshot).merge(born);
        alive
    }

    /// The grace is spent: kill what is left, or report and stop.
    fn finish(&mut self, table: &dyn LambSweep, killed: Vec<u32>, now: Instant) -> Step {
        if self.termed.is_empty() {
            self.phase = Phase::Done;
            return Step::Done(SweepReport::default());
        }
        if killed.is_empty() {
            self.phase = Phase::Done;
            return Step::Done(self.report(Vec::new()));
        }
        self.deliver(table, &killed, LambSignal::Kill);
        self.killed = killed;
        self.phase = Phase::Settling { started_at: now };
        Step::Wait(KILL_SETTLE_POLL)
    }

    /// One look of the settle: re-read the killed lambs until none are left
    /// or [`KILL_SETTLE`] has passed.
    fn settle(&mut self, table: &dyn LambSweep, started_at: Instant, now: Instant) -> Step {
        let left = table.survivors(&self.snapshot);
        let remaining = KILL_SETTLE.saturating_sub(now.saturating_duration_since(started_at));
        if left.is_empty() || remaining.is_zero() {
            self.phase = Phase::Done;
            return Step::Done(self.report(left));
        }
        Step::Wait(remaining.min(KILL_SETTLE_POLL))
    }

    /// Sends `signal` to each of `pids`, recording every refusal.
    fn deliver(&mut self, table: &dyn LambSweep, pids: &[u32], signal: LambSignal) {
        for &pid in pids {
            if let Err(error) = table.signal(pid, signal) {
                self.refused.push((pid, signal, error));
            }
        }
    }

    fn report(&self, stuck: Vec<u32>) -> SweepReport {
        SweepReport {
            termed: self.termed.iter().copied().collect(),
            killed: self.killed.clone(),
            refused: self.refused.clone(),
            stuck,
        }
    }
}

#[cfg(test)]
mod tests;
