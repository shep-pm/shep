use crate::channel::{ChildMessage, ShepherdMessage};
use crate::runner::{ExitOutcome, LogLine, RunnerError, RunningProcess, StopSignal};
use core::fmt;
use shep_core::signals::OperatorSignal;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::{Instant, sleep_until};

/// The IO endpoints [`ScriptedRunner::io_handles`](crate::fake::ScriptedRunner::io_handles) hands back for a spawn:
/// the test-side counterparts to the [`ProcIo`](crate::runner::ProcIo) the same spawn returned.
#[derive(Debug)]
pub struct FakeIo {
    /// Injects stdout/stderr lines into the spawned [`ProcIo::logs`](crate::runner::ProcIo::logs)
    pub logs_tx: mpsc::Sender<LogLine>,
    /// Injects child→daemon messages into the spawned [`ProcIo::from_child`](crate::runner::ProcIo::from_child)
    pub from_child_tx: mpsc::Sender<ChildMessage>,
    /// Observes every message the daemon sends on the spawned [`ProcIo::to_child`](crate::runner::ProcIo::to_child)
    pub to_child_rx: mpsc::Receiver<ShepherdMessage>,
}

/// Shared, thread-safe state for one scripted proc: lives in an `Arc` so
/// [`FakeProc`]'s clones (used to drive `wait()` and `signal()`/`kill_tree()`
/// from separate tasks in tests) and the `to_child` relay task all observe
/// the same signal/kill events.
pub(super) struct ProcState {
    /// Spawn-relative exit instant, computed once at spawn (cancel-safety: a
    /// dropped-and-recreated `wait()` future must never restart this clock).
    pub(super) exit_deadline: Instant,
    /// Outcome reported when `exit_deadline` is reached naturally
    pub(super) outcome: ExitOutcome,
    /// Whether a signal/shutdown event resolves the wait early
    pub(super) obeys_signal: bool,
    /// Whether a `kill_tree()` event resolves the wait; see
    /// [`ProcScript::obeys_kill`](crate::fake::ProcScript::obeys_kill)
    pub(super) obeys_kill: bool,
    /// Notified on `signal()` or a `Shutdown` message; permit buffers if
    /// nobody is awaiting yet, so events firing before OR during a `wait()`
    /// both resolve it.
    pub(super) signal_notify: Notify,
    /// Raw signal number recorded by the most recent explicit `signal()`
    /// call. A `Shutdown` message does not set this (see `record_shutdown`),
    /// so `wait()`'s fallback naturally reports `StopSignal::Term` for it.
    pub(super) pending_signal: Mutex<Option<i32>>,
    /// Every raw signal number an explicit `signal()` call has recorded, in
    /// call order, read back via [`ScriptedRunner::signals`](crate::fake::ScriptedRunner::signals). A `Shutdown`
    /// message does not append here, so tests can assert "no `signal()` call
    /// happened" even though the wait still resolved.
    pub(super) signals: Mutex<Vec<i32>>,
    /// Every `signal_process` call, in call order. Separate from `signals`,
    /// which records group deliveries; see `ScriptedRunner::process_signals`.
    pub(super) process_signals: Mutex<Vec<OperatorSignal>>,
    /// Notified on `kill_tree()`; same before-or-during buffering as above
    pub(super) kill_notify: Notify,
    /// `kill_tree()` call count, read back via [`ScriptedRunner::kill_counts`](crate::fake::ScriptedRunner::kill_counts)
    pub(super) kill_count: AtomicU32,
    /// Latches the first resolved outcome so a repeated `wait()` re-reports
    /// it instead of racing the notify/sleep branches again: matches
    /// `tokio::process::Child::wait`'s documented repeat-call behavior.
    pub(super) resolved: Mutex<Option<ExitOutcome>>,
    /// Flipped to `true` when `wait()` latches an outcome, and watched by
    /// this proc's log-control task so that task ends with the proc, unless
    /// [`ProcScript::lamb_holds_the_pipe`](crate::fake::ProcScript::lamb_holds_the_pipe) says the streams outlive it.
    ///
    /// Without it the fake would answer a reopen aimed at a proc that exited
    /// long ago, while the real runner's pump (and with it the receiving end
    /// of [`ProcIo::log_ctl`](crate::runner::ProcIo::log_ctl)) is gone once the child's streams reach EOF. A
    /// caller's "the pump is already gone" branch would then be unreachable
    /// from this tier.
    pub(super) exited: watch::Sender<bool>,
}

impl fmt::Debug for ProcState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcState")
            .field("exit_deadline", &self.exit_deadline)
            .field("outcome", &self.outcome)
            .field("obeys_signal", &self.obeys_signal)
            .field("kill_count", &self.kill_count.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

/// A single scripted live child produced by [`ScriptedRunner::spawn`](crate::runner::ProcessRunner::spawn)
///
/// `Clone`s share the same underlying state (private `ProcState`, held in an
/// `Arc`), which lets one handle drive `wait()` on a spawned task while
/// another delivers `signal()`/`kill_tree()` concurrently, the pattern the
/// daemon's kill ladder uses against the real runner too. A control event
/// (signal, shutdown, kill) resolves exactly one waiting `wait()` call, not
/// every clone independently: `tokio::sync::Notify::notify_one` semantics.
#[derive(Debug, Clone)]
pub struct FakeProc {
    pub(super) pid: u32,
    pub(super) state: Arc<ProcState>,
}

impl ProcState {
    /// Records an explicit `signal()` call: appends to the `signals` ledger,
    /// arms `pending_signal` with the raw number `wait()` should report, and
    /// wakes a pending (or buffers for a future) wait.
    fn record_signal(&self, raw: i32) {
        self.signals.lock().unwrap().push(raw);
        *self.pending_signal.lock().unwrap() = Some(raw);
        self.signal_notify.notify_one();
    }

    /// A `Shutdown` message resolves an obeys_signal wait exactly like
    /// `signal()` would (falling back to `StopSignal::Term` since
    /// `pending_signal` is left untouched), but is not itself an explicit
    /// `signal()` call: it never appears in `signals`.
    pub(super) fn record_shutdown(&self) {
        self.signal_notify.notify_one();
    }

    fn record_kill(&self) {
        self.kill_count.fetch_add(1, Ordering::SeqCst);
        self.kill_notify.notify_one();
    }

    fn record_process_signal(&self, sig: OperatorSignal) {
        self.process_signals.lock().unwrap().push(sig);
    }
}

impl RunningProcess for FakeProc {
    fn pid(&self) -> u32 {
        self.pid
    }

    async fn wait(&mut self) -> ExitOutcome {
        if let Some(outcome) = *self.state.resolved.lock().unwrap() {
            return outcome;
        }

        // Every branch resolves the wait: an event this wait doesn't obey
        // isn't a candidate branch (the `if` guard), not a fallthrough. With
        // both guards off, only `exit_deadline` remains, which for
        // `never_reports_its_exit` is `NEVER_MS` away.
        let outcome = tokio::select! {
            () = sleep_until(self.state.exit_deadline) => self.state.outcome,
            () = self.state.signal_notify.notified(), if self.state.obeys_signal => {
                let raw = self.state.pending_signal.lock().unwrap().take();
                ExitOutcome {
                    code: None,
                    signal: Some(raw.unwrap_or_else(|| StopSignal::Term.as_raw())),
                }
            }
            () = self.state.kill_notify.notified(), if self.state.obeys_kill => {
                ExitOutcome { code: None, signal: Some(StopSignal::Kill.as_raw()) }
            }
        };
        *self.state.resolved.lock().unwrap() = Some(outcome);
        // Ends this proc's log-control task; see `ProcState::exited`.
        // `send_replace` rather than `send` because the task may already be
        // gone, and a proc having exited is not news the fake can fail on.
        self.state.exited.send_replace(true);
        outcome
    }

    // A scripted proc models exactly one process with no descendants, so
    // `signal`'s group-wide contract and a leader-only delivery are
    // indistinguishable here. Neither is evidence that a real sheep's
    // forked lambs are signalled; `tests/real_runner.rs` proves that.
    fn signal(&mut self, sig: StopSignal) -> Result<(), RunnerError> {
        self.state.record_signal(sig.as_raw());
        Ok(())
    }

    fn kill_tree(&mut self) -> Result<(), RunnerError> {
        self.state.record_kill();
        Ok(())
    }

    // Recorded on its own list, not `record_signal`'s: which one the
    // supervisor called is exactly what a `shep signal` test needs. Does
    // not resolve the wait; only `signal` does.
    fn signal_process(&mut self, sig: OperatorSignal) -> Result<(), RunnerError> {
        self.state.record_process_signal(sig);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::proc_script::ProcScript;
    use super::super::scripted_runner::ScriptedRunner;

    use shep_core::signals::OperatorSignal;

    use tokio::time::Duration;

    use crate::runner::{ExitOutcome, ProcessRunner, RunningProcess};

    use super::super::testing::*;

    /// fails if a signal aimed at one sheep is recorded as a group delivery, or
    /// not recorded at all. `signal` and `signal_process` are two different
    /// contracts against the same OS primitive, and a fake that answered both from
    /// one counter could not tell a reviewer which one the supervisor called.
    #[tokio::test(start_paused = true)]
    async fn a_process_signal_is_recorded_apart_from_a_group_signal() {
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (mut proc, _io) = runner.spawn(&spec()).unwrap();

        proc.signal_process(OperatorSignal::Hup).unwrap();
        proc.signal_process(OperatorSignal::Usr1).unwrap();

        assert_eq!(
            runner.process_signals(0),
            vec![OperatorSignal::Hup, OperatorSignal::Usr1]
        );
        assert!(
            runner.signals(0).is_empty(),
            "a per-process signal must not be counted as a group signal"
        );
    }

    /// `signal_process` must not resolve the scripted proc's `wait()`:
    /// `signal` does that (the stop ladder's polite rung), and a nudge that
    /// ended the sheep would read `Delivered` off a process that had just died.
    #[tokio::test(start_paused = true)]
    async fn a_process_signal_does_not_end_the_scripted_proc() {
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (proc, _io) = runner.spawn(&spec()).unwrap();
        let mut waiter = proc.clone();
        let mut signaller = proc;
        let waiting = tokio::spawn(async move { waiter.wait().await });

        tokio::time::advance(Duration::from_millis(1)).await;
        signaller.signal_process(OperatorSignal::Hup).unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;

        assert!(!waiting.is_finished(), "signal_process resolved the wait");
        waiting.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_safety_deadline_survives_drop_and_reawait() {
        let runner = ScriptedRunner::new(vec![ProcScript::stable_then_exit(5_000, 7)]);
        let (mut proc, _io) = runner.spawn(&spec()).unwrap();

        // Drive `wait()` on a spawned task so we can abort (drop) it
        // mid-flight, the "sheep task owns the proc" shape
        // `RunningProcess::wait` documents.
        let mut first_wait = proc.clone();
        let handle = tokio::spawn(async move { first_wait.wait().await });
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            !handle.is_finished(),
            "wait() must still be pending 1s into a 5s delay"
        );
        handle.abort(); // drop the wait() future without ever resolving it

        // If the deadline were recomputed from this re-await instead of the
        // original spawn-relative one, 4 more seconds would not be enough.
        tokio::time::advance(Duration::from_secs(4)).await;
        let outcome = proc.wait().await;
        assert_eq!(
            outcome,
            ExitOutcome {
                code: Some(7),
                signal: None
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_wait_returns_the_same_cached_outcome() {
        // MINOR-8 regression guard: a second wait() must re-report the latched
        // outcome instead of racing the (already-fired) select! branches again.
        let runner = ScriptedRunner::new(vec![ProcScript::const_exit(3)]);
        let (mut proc, _io) = runner.spawn(&spec()).unwrap();
        let first = proc.wait().await;
        let second = proc.wait().await;
        assert_eq!(first, second);
        assert_eq!(
            first,
            ExitOutcome {
                code: Some(3),
                signal: None
            }
        );
    }
}
