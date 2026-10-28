//! One registered instance, as the actor holds it.
//!
//! A slot is the actor's whole record of a sheep: its lifecycle entry, the
//! senders reaching its task, and whatever is in flight against it. The
//! actor keeps one per registered id and never awaits process IO while
//! holding it.

use super::*;

/// One registered instance: lifecycle state plus its live senders.
#[derive(Debug)]
pub(super) struct SheepSlot {
    /// Lifecycle state (spec, status, restart budget, ...).
    pub(super) entry: ProcessEntry,
    /// Sender for this sheep's control mailbox; `None` when not running.
    pub(super) ctl: Option<mpsc::Sender<SheepCtl>>,
    /// A clone of the [`ProcIo::log_ctl`] the most recent successful spawn
    /// handed out, which is how a `Reopen` or a `Flush` reaches this sheep's
    /// log pump. `None` only for a slot whose spawn never succeeded at all.
    ///
    /// Never cleared, unlike `ctl`: a send fails the moment the pump ends, so
    /// clearing this too would be a second copy of that fact. It costs the
    /// pump no extra life.
    pub(super) log_ctl: Option<mpsc::Sender<LogCtl>>,
    /// A clone of the [`ProcIo::to_child`] the most recent successful spawn
    /// handed out: the daemon's writing end of this sheep's shepherd channel,
    /// and the actor's only way to reach a live child directly. `None`
    /// whenever no process is running under this id.
    ///
    /// Not carried as a `ctl` message: [`Actor::claim_manual`] ignores a `Full`
    /// on that mailbox on the strength of [`SheepCtl::Kill`] being its sole occupant.
    pub(super) to_child: Option<mpsc::Sender<ShepherdMessage>>,
    /// Sender for this sheep's signal mailbox, separate from [`Self::ctl`].
    /// `None` whenever no process is running under this id.
    ///
    /// A mailbox of its own rather than a [`SheepCtl`] variant: a burst
    /// sharing [`Self::ctl`]'s bounded slots would make
    /// [`Actor::claim_manual`] drop a stop. Cleared with [`Self::to_child`].
    pub(super) signals: Option<mpsc::Sender<SignalRequest>>,
    /// A clone of the [`ProcIo::to_stdin`] the most recent successful spawn
    /// handed out: the daemon's writing end of this sheep's stdin pipe. `None`
    /// whenever no process is running under this id; present but closed when
    /// the running one never asked for a pipe (`AppConfig::stdin == false`).
    ///
    /// No `.await` on this sender may appear on the actor loop: an app that has
    /// stopped reading fd 0 fills its pipe and blocks the writer task.
    pub(super) to_stdin: Option<mpsc::Sender<StdinWrite>>,
    /// Which manual command (if any) is waiting on this sheep's next exit,
    /// and who asked for it. Claimed through [`Actor::claim_manual`].
    pub(super) manual: Option<PendingManual>,
    /// Set whenever a `Delete` targets this id, even if an earlier command
    /// already owns `manual`. `manual` records who owns the next Kill; this
    /// records intent that must survive that race, so a Delete can never be
    /// downgraded to a Stop or a Restart. `handle_exited` checks it on the
    /// manual-Restart early return as well as on the `CleanStop` branch.
    pub(super) pending_delete: bool,
    /// Bumped on every successful respawn. A `RestartDue` timer carries the
    /// epoch it was scheduled under, and `handle_restart_due` drops one whose
    /// epoch has moved on.
    pub(super) epoch: u64,
    /// The readiness task's signal sender for the current epoch.
    /// `Msg::Ready`'s handler takes it to wake the task.
    ///
    /// `None` means either that no readiness task was ever armed, or that a
    /// channel `Ready` already took the sender. A wait that resolved another
    /// way leaves its sender here, and a late `Msg::Ready` drops silently.
    pub(super) ready_tx: Option<oneshot::Sender<()>>,
    /// The action waits armed against this sheep and the replies its app still
    /// owes ones that have ended; see [`ActionWaits`].
    ///
    /// Cleared with [`Self::to_child`]: a wait armed against an exited process
    /// waits for a reply nobody will write.
    pub(super) actions: ActionWaits,
    /// This instance's readiness wait ended without a signal, and a reload
    /// left it standing anyway: up, registered, and known not to be serving.
    ///
    /// Read by [`Actor::advance_reload`]'s replaceable test, so the instance a
    /// failed reload left behind is still within reach of the next one. Also
    /// read by [`Actor::handle_extra_restart`]'s status guard: an abandoned
    /// reload's replacement is armed the same extras an `Online` sheep is.
    pub(super) ready_failed: bool,
    /// When this slot's owed respawn falls due, in wall-clock terms.
    ///
    /// The same fact as [`Actor::schedule_restart`]'s monotonic timer, in the
    /// only clock that survives an `execve`: a handover carries this and re-arms
    /// a fresh timer from it. See
    /// [`backoff::adopted_restart_delay`](crate::backoff::adopted_restart_delay).
    pub(super) restart_due: Option<SystemTime>,
}

impl SheepSlot {
    /// A registered sheep with nothing attached: no mailboxes, no marker, no
    /// timer, at epoch zero.
    ///
    /// The base every literal in this module builds from, so a field added
    /// here is written once and a caller overwrites only what it owns. Not a
    /// `Default`: `entry` has no blank value.
    pub(super) fn new(entry: ProcessEntry) -> Self {
        Self {
            entry,
            ctl: None,
            log_ctl: None,
            to_child: None,
            signals: None,
            to_stdin: None,
            manual: None,
            pending_delete: false,
            epoch: 0,
            ready_tx: None,
            actions: ActionWaits::default(),
            ready_failed: false,
            restart_due: None,
        }
    }

    /// Lands the slot at `status` with nothing left attached to a process
    /// that is no longer running.
    ///
    /// [`Self::log_ctl`] is the one handle left in place, for the reason on
    /// that field.
    pub(super) fn clear_lifecycle(&mut self, status: ProcStatus) {
        self.entry.status = status;
        self.entry.pid = None;
        self.entry.started_at = None;
        // Cleared, because nothing exited here: leaving the previous
        // process's code would show a sheep that once crashed with 1 as
        // still crashing with 1 while it is failing to start at all.
        self.entry.last_exit = None;
        self.ctl = None;
        // `signals` and `to_stdin` go wherever `to_child` goes, for the
        // reason on each field.
        self.to_child = None;
        self.signals = None;
        self.to_stdin = None;
        self.ready_tx = None;
    }

    /// This sheep's shepherd-channel sender while something is still there to
    /// receive on it, and `None` when nothing is.
    ///
    /// Read off the channel rather than `AppConfig::channel`, so there is no
    /// second copy of the fact. `is_closed` catches an app configured without
    /// a channel.
    pub(super) fn open_channel(&self) -> Option<&mpsc::Sender<ShepherdMessage>> {
        self.to_child
            .as_ref()
            .filter(|to_child| !to_child.is_closed())
    }

    /// This sheep's stdin sender while something is still there to receive on
    /// it, and `None` when nothing is.
    ///
    /// Read off the channel rather than `AppConfig::stdin`, as
    /// [`Self::open_channel`] is. `is_closed` catches an app that never asked
    /// for a pipe, whose receiver the runner dropped at spawn.
    pub(super) fn open_stdin(&self) -> Option<&mpsc::Sender<StdinWrite>> {
        self.to_stdin
            .as_ref()
            .filter(|to_stdin| !to_stdin.is_closed())
    }
}
