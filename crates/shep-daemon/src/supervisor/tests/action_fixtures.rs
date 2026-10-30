//! Fixtures for the custom-action and log-control cases: the actor with an open
//! channel, the waits it arms, and the runners that never answer or refuse a
//! pump request.

use super::*;
use tokio::sync::watch;

// --- Custom actions: one action out, one answer back or none ---

/// What an action gets to answer in. Virtual time, and long enough that no
/// scheduling order inside a case reaches it by accident.
const ACTION_TIMEOUT: Duration = Duration::from_secs(20);

/// A window generous enough for any action wait to report home, so a case
/// whose result never arrives fails instead of parking the suite.
pub(super) const ACTION_WINDOW: Duration = Duration::from_secs(120);

/// A bare actor holding one sheep whose shepherd channel is open, plus the
/// mailbox every spawned wait reports to and the child's end of the
/// channel. Driven by hand so a case can put a reply on the channel at an
/// exact point relative to a wait's deadline.
pub(super) fn actor_with_an_open_channel(
    dir: &tempfile::TempDir,
) -> (
    Actor<ScriptedRunner>,
    mpsc::Receiver<Msg>,
    mpsc::Receiver<ShepherdMessage>,
) {
    // No scripts: a spawn that should not have happened fails loudly.
    let (mut actor, mailbox) = actor_with_one_online_sheep(dir, vec![]);
    // Wide enough that no case can fill it, so a blocking `send` is a bug.
    let (to_child, child_rx) = mpsc::channel(16);
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers id 0")
        .to_child = Some(to_child);
    (actor, mailbox, child_rx)
}

/// Puts one action on the fixture's sheep and hands back the receiver its
/// answer will arrive on. Arms the wait directly rather than through
/// `Command::Trigger`.
pub(super) fn trigger_action(
    actor: &mut Actor<ScriptedRunner>,
    action: &str,
) -> oneshot::Receiver<ActionOutcome> {
    let to_child = actor.sheep[&0]
        .to_child
        .clone()
        .expect("the fixture's sheep holds the daemon's end of a channel");
    actor.arm_action(0, to_child, action.to_string(), None, ACTION_TIMEOUT)
}

/// Drives the one message an action wait sends home and applies it,
/// returning what it carried.
pub(super) async fn settle_action(
    actor: &mut Actor<ScriptedRunner>,
    mailbox: &mut mpsc::Receiver<Msg>,
) -> ActionOutcome {
    let msg = tokio::time::timeout(ACTION_WINDOW, mailbox.recv())
        .await
        .expect("an action wait reported nothing within the window")
        .expect("the actor's mailbox closed");
    match msg {
        Msg::ActionResult { id, stamp, outcome } => {
            actor.handle_action_result(id, stamp, outcome.clone());
            outcome
        }
        other => panic!("expected an action result, got {other:?}"),
    }
}

/// Reads the action the daemon put on the child's end of the channel,
/// failing rather than hanging if nothing was sent.
pub(super) async fn sent_action(child_rx: &mut mpsc::Receiver<ShepherdMessage>) -> ShepherdMessage {
    tokio::time::timeout(ACTION_WINDOW, child_rx.recv())
        .await
        .expect("nothing reached the child's end of the channel")
        .expect("the child's end of the channel closed")
}

/// A [`ScriptedRunner`] whose spawns hand out a log-control channel that
/// accepts requests and never answers them.
///
/// Each request is held rather than dropped, so the `oneshot` sender inside
/// stays owed instead of resolving `Err`. The `watch` counts requests that
/// reached a pump.
pub(super) struct SilentPumpRunner {
    inner: ScriptedRunner,
    seen: watch::Sender<u32>,
}

impl SilentPumpRunner {
    pub(super) fn new(scripts: Vec<ProcScript>) -> (Self, watch::Receiver<u32>) {
        let (seen, requests) = watch::channel(0);
        (
            Self {
                inner: ScriptedRunner::new(scripts),
                seen,
            },
            requests,
        )
    }
}

impl fmt::Debug for SilentPumpRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SilentPumpRunner").finish_non_exhaustive()
    }
}

impl ProcessRunner for SilentPumpRunner {
    type Proc = crate::fake::FakeProc;

    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
        let (proc, mut io) = self.inner.spawn(spec)?;
        let (tx, mut rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
        // Replacing the sender drops the fake's own, ending the control
        // task it spawned. This runner exists so nothing answers.
        io.log_ctl = tx;
        let seen = self.seen.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Some(request) = rx.recv().await {
                held.push(request);
                seen.send_modify(|count| *count += 1);
            }
        });
        Ok((proc, io))
    }
}

/// What [`FailingPumpRunner`]'s pump answers every reopen with. One owner
/// for the string: the case below asserts the whole error it ends up in.
pub(super) const PUMP_REFUSAL: &str = "/gone/web-out.log: No such file or directory";

/// The sheep [`FailingPumpRunner`] gives a failing pump to.
pub(super) const REFUSING_SHEEP: &str = "web";

/// A [`ScriptedRunner`] whose spawn of [`REFUSING_SHEEP`] gets a pump that
/// answers every reopen with a failure. Every other sheep keeps the
/// scripted fake's own answering pump. By name rather than by spawn order,
/// so one case can hold a failed reopen and a healthy sheep beside it.
#[derive(Debug)]
pub(super) struct FailingPumpRunner {
    inner: Arc<ScriptedRunner>,
}

impl FailingPumpRunner {
    pub(super) fn new(inner: Arc<ScriptedRunner>) -> Self {
        Self { inner }
    }
}

impl ProcessRunner for FailingPumpRunner {
    type Proc = crate::fake::FakeProc;

    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
        let (proc, mut io) = self.inner.spawn(spec)?;
        if spec.name != REFUSING_SHEEP {
            return Ok((proc, io));
        }
        let (tx, mut rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
        // Replacing the sender drops the fake's own, ending the control
        // task it spawned. This pump answers in its place.
        io.log_ctl = tx;
        tokio::spawn(async move {
            while let Some(ctl) = rx.recv().await {
                // Both variants, so this pump keeps serving whichever
                // arrives.
                match ctl {
                    LogCtl::Reopen { done } => {
                        let _ = done.send(Err(ReopenError {
                            message: PUMP_REFUSAL.to_string(),
                        }));
                    }
                    LogCtl::Flush { done } => {
                        let _ = done.send(Err(FlushError {
                            message: PUMP_REFUSAL.to_string(),
                        }));
                    }
                    // This runner exists for the reopen and flush refusals.
                    #[cfg(unix)]
                    LogCtl::ReportFds { done } => {
                        let _ = done.send(CarriedFds::none());
                    }
                    // Nothing to start reading again: this runner reads no
                    // streams.
                    #[cfg(unix)]
                    LogCtl::Resume => {}
                }
            }
        });
        Ok((proc, io))
    }
}
