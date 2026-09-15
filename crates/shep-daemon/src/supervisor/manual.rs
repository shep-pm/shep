//! What the actor is still waiting to answer.
//!
//! A stop, restart or action does not finish in the turn that starts it: the
//! sheep has to exit, or the child has to reply. These types hold the
//! half-finished command until it can be resolved, and record who asked, so
//! the answer reaches the right caller and a later exit is read correctly.

use super::*;

/// Which manual command is pending against a sheep, cleared the moment its
/// `Msg::Exited` is processed.
///
/// Serialized because a handover carries it, inside [`PendingManual`].
/// `snake_case` on the wire to match [`ProcStatus`]'s spelling: the blob is a
/// JSON file an operator may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManualKind {
    /// A `Stop` command targeted this sheep.
    Stop,
    /// A `Restart` command targeted this sheep.
    Restart,
    /// A `Delete` command targeted this sheep.
    Delete,
}

/// Who asked for a pending manual command. Decides which of two racing
/// commands owns a sheep's next exit ([`Actor::claim_manual`]) and the
/// `manually` flag on the bus events the command emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandOrigin {
    /// A person asked for it: a `Stop`, `Restart` or `Delete` off the control
    /// socket, or the daemon-wide `Shutdown`.
    Operator,
    /// The daemon raised it itself: a memory breach or a liveness failure
    /// ([`SupervisorHandle::extra_restart`]), or a cron occurrence or
    /// watched-file change ([`SupervisorHandle::restart_automatic`]).
    ///
    /// Nobody is owed the answer, so an operator's `stop` may take the sheep
    /// off one mid-ladder rather than be converted into it.
    Automatic,
}

/// The manual command that owns a sheep's next exit, and who asked for it.
///
/// Crosses a handover whole, on [`CarriedSheep::manual`]. Without the marker
/// a successor would hand an `autorestart` app its ordinary respawn, so a
/// `shep stop` would come back as a running sheep. [`Self::origin`] crosses
/// unchanged too, answering who caused this exit.
///
/// [`CarriedSheep::manual`]: crate::handover::CarriedSheep::manual
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingManual {
    /// What that exit will be turned into.
    pub(crate) kind: ManualKind,
    /// Who asked. `kind` decides what the command does; `origin` survives into
    /// `handle_exited` only for the `manually` flag on the events that exit
    /// produces.
    pub(crate) origin: CommandOrigin,
}

/// One action the daemon has put on a sheep's shepherd channel and has not
/// finished waiting on.
#[derive(Debug)]
pub(super) struct PendingAction {
    /// Which wait this is, for the whole life of the daemon.
    ///
    /// A counter of its own: the sheep's id and [`SheepSlot::epoch`] name the
    /// instance, not the action.
    pub(super) stamp: u64,
    /// The action name this is waiting for a reply to; [`ActionWaits::answer`]
    /// falls back to it when the app echoes no `stamp`.
    pub(super) action: String,
    /// Wakes the waiting task with the app's reply body.
    ///
    /// Taken by the reply that answers it, so a second reply to one action
    /// finds nothing to hand a body to. The entry stays until the task reports
    /// what it made of the body, which keeps `reply` reachable.
    pub(super) waiter: Option<oneshot::Sender<String>>,
    /// Where this wait's outcome goes once it has one: the row-building half
    /// of [`Actor::begin_action`], which turns it into one [`ActionReply`].
    ///
    /// A bare outcome and not a `Result`: everything that could fail is
    /// decided before a wait is armed.
    pub(super) reply: oneshot::Sender<ActionOutcome>,
}

/// One reply a sheep's app still owes a wait that has already ended.
///
/// The `stamp` separates a late reply from a prompt one naming the same
/// action. An app that does not echo one is matched by `action` and by order.
#[derive(Debug)]
pub(super) struct AbandonedReply {
    /// The wait that ended without this reply.
    pub(super) stamp: u64,
    /// Its action name: the fallback key for an app that does not echo.
    pub(super) action: String,
}

/// What one sheep still owes on its shepherd channel: the action waits armed
/// against it, and the replies its app can still send that no wait wants.
///
/// An app may answer an action after its wait has given up. One that echoes
/// the dispatch stamp settles exactly its own debt; for one that does not,
/// order is the only signal, so the next unstamped reply naming that action
/// pays the debt rather than the live wait.
#[derive(Debug, Default)]
pub(super) struct ActionWaits {
    /// Waits still expecting a message about them, oldest first.
    pub(super) live: Vec<PendingAction>,
    /// One entry per reply the app still owes a wait that has already ended,
    /// oldest first, capped at [`MAX_ABANDONED_ACTION_REPLIES`].
    pub(super) abandoned: VecDeque<AbandonedReply>,
}

impl ActionWaits {
    /// Records a wait the caller has already armed a task for.
    pub(super) fn arm(&mut self, pending: PendingAction) {
        self.live.push(pending);
    }

    /// Routes one reply to `action`, stamped with `stamp` if the app echoed
    /// the dispatch's `id`, to the waiter it belongs to.
    ///
    /// A stamped reply goes to the live wait carrying that stamp, failing that
    /// settles that stamp's own debt. An unstamped one settles the oldest debt
    /// of that name first, and reaches a live wait only once the debt is
    /// clear. `None` is ordinary, not an error.
    pub(super) fn answer(&mut self, action: &str, stamp: Option<u64>) -> Option<oneshot::Sender<String>> {
        if let Some(stamp) = stamp {
            if let Some(pending) = self
                .live
                .iter_mut()
                .find(|pending| pending.stamp == stamp && pending.waiter.is_some())
            {
                return pending.waiter.take();
            }
            if let Some(owed) = self.abandoned.iter().position(|debt| debt.stamp == stamp) {
                self.abandoned.remove(owed);
            }
            return None;
        }
        if let Some(owed) = self.abandoned.iter().position(|debt| debt.action == action) {
            self.abandoned.remove(owed);
            return None;
        }
        self.live
            .iter_mut()
            .find(|pending| pending.action == action && pending.waiter.is_some())
            .and_then(|pending| pending.waiter.take())
    }

    /// Ends the wait `stamp` names, recording the reply it never got if it
    /// never got one; hands back where its outcome goes.
    ///
    /// `None` for a stamp no live wait carries: [`Self::abandon_all`] answered
    /// it already.
    pub(super) fn resolve(&mut self, stamp: u64) -> Option<oneshot::Sender<ActionOutcome>> {
        let at = self
            .live
            .iter()
            .position(|pending| pending.stamp == stamp)?;
        let pending = self.live.remove(at);
        // A waiter still sitting here ended without its reply. The debt stops
        // that reply being read as an answer to something else.
        if pending.waiter.is_some() {
            self.abandoned.push_back(AbandonedReply {
                stamp: pending.stamp,
                action: pending.action,
            });
            if self.abandoned.len() > MAX_ABANDONED_ACTION_REPLIES {
                self.abandoned.pop_front();
            }
        }
        Some(pending.reply)
    }

    /// Answers every live wait [`ActionOutcome::NoChannel`] and forgets every
    /// debt, which is what a sheep's process ending does to both halves.
    ///
    /// The debts go with the process that owed them. The live waits are
    /// answered rather than dropped, a dropped `reply` reaching its caller as
    /// the engine having gone away.
    pub(super) fn abandon_all(&mut self) {
        for pending in self.live.drain(..) {
            let _ = pending.reply.send(ActionOutcome::NoChannel);
        }
        self.abandoned.clear();
    }
}
