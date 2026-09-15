//! Assembling what a successor daemon needs to take the flock over.
//!
//! A handover snapshot has to name every live process and the descriptors
//! behind its log pumps, without stopping anything. `spawn_handover_task`
//! does that off the actor loop and answers within `REPORT_DEADLINE`, so one
//! wedged pump cannot hold the whole snapshot open.

use super::*;

/// What a [`Command::HandoverSnapshot`] answers with: one candidate per
/// registered sheep for the fitness gate, and the blob a successor reads.
///
/// Three types because they go three places, and only the blob crosses the
/// exec: the candidates decide whether there is to be an exec at all, and the
/// parked pumps matter only if there is not.
#[cfg(unix)]
pub(super) type Snapshot = (Vec<OwnedCandidate>, Handover, ParkedPumps);

/// The log pumps a snapshot stopped, so a handover that is then abandoned
/// can start them reading again.
///
/// Taking the snapshot parks every pump that answers it, because a report is
/// only true while nothing moves behind it. A pump that missed
/// [`REPORT_DEADLINE`] is not in here: it never parked. The exec normally ends
/// the park by replacing the image; every other way out leaves this daemon
/// running with pumps that have stopped reading.
///
/// The party that parked them hands them back rather than the abort path
/// re-reading the actor's slots: what is owed a resume is what was reported.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct ParkedPumps(Vec<mpsc::Sender<LogCtl>>);

#[cfg(unix)]
impl ParkedPumps {
    /// Lets every pump this snapshot parked read its sheep's streams again.
    ///
    /// A send that fails is a pump that has ended since the report, which is
    /// nothing to repair.
    pub(crate) async fn resume(&self) {
        for pump in &self.0 {
            let _ = pump.send(LogCtl::Resume).await;
        }
    }
}

/// One sheep on its way from the actor to the handover task.
///
/// Everything here is read off a [`SheepSlot`] inside the actor loop, and
/// carried out owned because the assembly runs on a task of its own.
#[cfg(unix)]
#[derive(Debug)]
pub(super) struct HandoverDraft {
    /// The sheep's lifecycle entry, cloned off the slot.
    pub(super) entry: ProcessEntry,
    /// The manual command owning this sheep's next exit, if one does.
    pub(super) manual: Option<PendingManual>,
    /// Whether a `Delete` targets this sheep.
    pub(super) pending_delete: bool,
    /// The slot's respawn epoch, so a timer armed before the exec is still
    /// recognised as stale after it.
    pub(super) epoch: u64,
    /// Whether a reload's readiness verification has already failed against
    /// this sheep. See [`SheepSlot::ready_failed`].
    pub(super) ready_failed: bool,
    /// When this sheep's owed respawn falls due, so the successor re-arms
    /// for what is left of the delay rather than for the whole of it. See
    /// [`SheepSlot::restart_due`].
    pub(super) restart_due: Option<SystemTime>,
    /// This sheep's log pump, or `None` for a slot whose spawn never
    /// succeeded.
    pub(super) log_ctl: Option<mpsc::Sender<LogCtl>>,
    /// Whether this sheep's shepherd channel is still one the daemon can
    /// write to.
    ///
    /// Read here rather than taken from the pump's report: the pump reports
    /// the number it was told at the spawn and cannot learn that the channel
    /// died, after which the number names whatever the kernel has since handed
    /// to the next `open`. [`SheepSlot::open_channel`] is the fact that decides
    /// delivery, and is only reachable from the actor loop.
    pub(super) channel_open: bool,
}

/// Spawns the task that assembles one handover snapshot and answers its
/// caller; must be called from within a Tokio runtime context.
///
/// Every await lives in here, off the actor loop; see
/// [`Actor::handle_reopen`] for the cycle that rules out doing it inline.
///
/// The sheep are visited concurrently because the deadline on the other end is
/// fixed at `shep-cli`'s `admin::KILL_TEARDOWN_WAIT`. Serially, N wedged pumps
/// cost N times [`REPORT_DEADLINE`], and past thirty that outlasts the client,
/// which falls back to a predecessor still serving and exits 0 mid-sweep.
/// `join_all` returns results in input order, so `drafts`' id-sorted order
/// survives into `candidates` and `carried` with no re-sort.
#[cfg(unix)]
pub(super) fn spawn_handover_task(
    drafts: Vec<HandoverDraft>,
    fds: DaemonFds,
    counters: Counters,
    reloads: Vec<CarriedReload>,
    reply: oneshot::Sender<Result<Snapshot, SupervisorError>>,
) {
    tokio::spawn(async move {
        let visited = futures_util::future::join_all(drafts.into_iter().map(|draft| async move {
            // Only a pump that answered parked, so only that one is owed a
            // resume. Only a wedged pump refuses the flock: it is the one case
            // where a live sheep's descriptors are unknown, and the gate below
            // is what stands between that and a successor with no stdout.
            let (mut fds, pump_unresponsive, parked_pump) = match &draft.log_ctl {
                Some(log_ctl) => match report_fds(log_ctl).await {
                    PumpReport::Parked(fds) => (fds, false, Some(log_ctl.clone())),
                    PumpReport::Gone => (CarriedFds::none(), false, None),
                    PumpReport::Unresponsive => (CarriedFds::none(), true, None),
                },
                None => (CarriedFds::none(), false, None),
            };
            // The one number the pump can report and be wrong about; see
            // `HandoverDraft::channel_open`. `UnixStream::from(OwnedFd)` checks
            // nothing, so a reissued number would take a shepherd message into
            // whatever that descriptor is now.
            if !draft.channel_open {
                fds.channel = None;
            }
            let carried = CarriedSheep::from_entry(
                &draft.entry,
                draft.epoch,
                fds,
                draft.pending_delete,
                draft.manual,
                draft.ready_failed,
                draft.restart_due,
            );
            let candidate = OwnedCandidate {
                entry: draft.entry,
                pump_unresponsive,
            };
            (candidate, carried, parked_pump)
        }))
        .await;

        // `join_all` hands `visited` back in `drafts`' id-sorted order, not
        // completion order, so this loop needs no sort of its own.
        let mut candidates = Vec::with_capacity(visited.len());
        let mut carried = Vec::with_capacity(visited.len());
        let mut parked = ParkedPumps::default();
        for (candidate, sheep, parked_pump) in visited {
            candidates.push(candidate);
            carried.push(sheep);
            if let Some(log_ctl) = parked_pump {
                parked.0.push(log_ctl);
            }
        }
        let blob = Handover::new(carried, fds, counters, reloads);
        let _ = reply.send(Ok((candidates, blob, parked)));
    });
}

/// What one log pump answered a snapshot's [`LogCtl::ReportFds`] with.
///
/// A pump that cannot be reached and a pump that is wedged both have no
/// descriptors to give and mean opposite things: the first is a sheep that has
/// stopped, the second a live sheep whose four descriptors this daemon does
/// not know. Folding the second into [`CarriedFds::none`] would carry it,
/// silently dropping its stdout, stderr and both log handles.
///
/// [`CarriedFds::none`]: CarriedFds::none
#[cfg(unix)]
#[derive(Debug)]
pub(super) enum PumpReport {
    /// The pump answered, and has stopped reading its streams until the
    /// exec. It is owed a [`LogCtl::Resume`] if the handover is abandoned.
    Parked(CarriedFds),
    /// There is no pump on the other end any more: the send found a closed
    /// mailbox, or the answer channel dropped unanswered. Either way it is
    /// reading nothing and is owed nothing.
    ///
    /// Not a refusal: a registered sheep that is not running reaches the gate
    /// exactly like this, and there is nothing to carry.
    Gone,
    /// The pump did not answer inside [`REPORT_DEADLINE`].
    ///
    /// It never parked, so it is still reading its sheep's streams and must
    /// not be resumed.
    Unresponsive,
}

/// How long a snapshot waits for one log pump to report its descriptors.
///
/// The work it bounds is a handful of `write(2)`s of at most 8 KiB:
/// microseconds on a healthy filesystem, single-digit milliseconds on a busy
/// one, so two seconds is three orders of magnitude clear of it. Firing early
/// costs the whole flock its handover; firing late costs a few seconds.
///
/// Per pump but paid once: [`spawn_handover_task`] visits every pump
/// concurrently, so a flock of wedged pumps costs one deadline, not N. `shep
/// daemon reload` gives the successor `admin::KILL_TEARDOWN_WAIT` (60s, and
/// 10s until the staged teardown raised it), which a sweep scaling with N
/// would outlast at thirty wedged pumps.
#[cfg(unix)]
pub(super) const REPORT_DEADLINE: Duration = Duration::from_secs(2);

/// Asks one sheep's log pump to write out everything it is holding, report
/// the four descriptors it owns, and stop reading until the exec; waits up to
/// [`REPORT_DEADLINE`] for the answer.
///
/// Not reaching a pump at all is [`PumpReport::Gone`], not an error: a stopped
/// sheep has nothing to carry and nothing to lose, and the fitness gate does
/// not refuse it.
///
/// The deadline covers the send as well as the answer, since a pump that has
/// stopped serving its mailbox fills it and blocks the send instead.
#[cfg(unix)]
pub(super) async fn report_fds(log_ctl: &mpsc::Sender<LogCtl>) -> PumpReport {
    let (done, ack) = oneshot::channel();
    let answer = async {
        if log_ctl.send(LogCtl::ReportFds { done }).await.is_err() {
            return PumpReport::Gone;
        }
        ack.await.map_or(PumpReport::Gone, PumpReport::Parked)
    };
    tokio::time::timeout(REPORT_DEADLINE, answer)
        .await
        .unwrap_or(PumpReport::Unresponsive)
}
