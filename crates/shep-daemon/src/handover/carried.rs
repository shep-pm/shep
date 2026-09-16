//! One sheep's carried state: what the successor needs to keep a single
//! sheep alive across the exec.
//!
//! [`CarriedSheep`] is one row of the blob, holding both what the sheep is
//! and what it is currently doing. [`CarriedFds`] is the six descriptor
//! numbers its output, input and shepherd channel travel on, and [`SheepFd`]
//! says which of the six a number is, since the adoption path refuses a pipe
//! and a log handle by opposite checks.

use std::os::fd::RawFd;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use shep_core::config::AppConfig;
use shep_core::protocol::{DogSource, ExitInfo};
use shep_core::status::ProcStatus;

use crate::entry::{ProcessEntry, ReloadState};
use crate::privilege::SpawnIdentity;
use crate::supervisor::PendingManual;

/// One sheep, as the successor will find it.
///
/// [`Self::app`] is what the sheep is, carried whole so the successor can
/// respawn this exact instance without asking the muster roll. Every other
/// field is what this instance is currently doing.
///
/// An absent `Option` key loads as `None`, so a blob an older image wrote
/// still loads and [`VERSION`](super::VERSION) stays unmoved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedSheep {
    /// The supervisor's entry id, which callers already hold and selectors
    /// already name.
    pub(super) id: u32,
    /// The sheep's name, which is how an operator names it back.
    pub(super) name: String,
    /// The instance slot within its app.
    pub(super) instance: u32,
    /// The running process, or `None` for an instance that is registered and
    /// not running.
    pub(super) pid: Option<u32>,
    /// Respawns performed so far, which the restart budget counts against.
    pub(super) restarts: u32,
    /// The supervisor slot's respawn epoch, so a timer armed before the exec
    /// is still recognised as stale afterwards.
    pub(super) epoch: u64,
    /// The instance's lifecycle status.
    pub(super) status: ProcStatus,
    /// How this instance most recently stopped existing, if it has.
    pub(super) last_exit: Option<ExitInfo>,
    /// The identity this instance's next spawn runs under.
    ///
    /// Carried resolved, never re-derived: the value is pinned at the first
    /// spawn, so a later change to the passwd database cannot move a running
    /// app's identity underneath it.
    pub(super) credentials: SpawnIdentity,
    /// The descriptor numbers this instance's output travels on.
    pub(super) fds: CarriedFds,
    /// Whether an operator's `delete` targeted this instance before the
    /// exec. An absent key means "no".
    pub(super) pending_delete: Option<bool>,
    /// The manual command that owned this instance's next exit before the
    /// exec, and who asked for it. `None` is already this field's own word
    /// for "no command owns this exit", so an absent key needs no second
    /// state.
    pub(super) manual: Option<PendingManual>,
    /// Which half of a reload's swap this instance is, if either. An absent
    /// key means [`ReloadState::None`].
    ///
    /// It routes this instance's next exit: without it a drainee's exit goes
    /// to `decide_on_exit`, and an `autorestart` app respawns the old code
    /// into the replacement's slot.
    pub(super) reload: Option<ReloadState>,
    /// Whether a reload's readiness verification has already failed against
    /// this instance. An absent key reads as `false`.
    ///
    /// It keeps a failed reload's leftovers reachable: an abandoned instance
    /// is left `Starting`, and `reload_eligible` reads this flag beside the
    /// status so a rollback can still reach it.
    pub(super) ready_failed: Option<bool>,
    /// When this instance's owed respawn falls due, in wall-clock terms, or
    /// `None` for an instance that is not owed one at all.
    ///
    /// Wall-clock and not monotonic: a [`tokio::time::Instant`] has no epoch
    /// across the `execve`, while an absolute moment lets the successor re-arm
    /// for what is left. A clock that moves under it is clamped by
    /// [`adopted_restart_delay`](crate::backoff::adopted_restart_delay). An
    /// absent key re-arms the whole delay, and only a `WaitingRestart` row
    /// carries one.
    pub(super) restart_due: Option<SystemTime>,
    /// Where this instance's binary came from, for an instance that is a
    /// dog, or `None` for an ordinary sheep.
    ///
    /// An absent key means "not a dog". The whole [`DogSource`] and not a
    /// boolean, because `shep dogs` reports where each dog's binary came from.
    ///
    /// Losing the marker is invisible to a pid check: `Actor::matching_ids`,
    /// `dogs::spawn_dog_watch` and `rpc::dog_staleness` all read it.
    pub(super) dog: Option<DogSource>,
    /// The config a load parked for this instance's next spawn, or `None`
    /// for an instance nothing is owed.
    ///
    /// An `AppConfig` and not a `ResolvedApp` for the reason [`Self::app`]
    /// gives. Losing it is silent config erasure: the parked change vanishes
    /// and the next load compares against a spec that already matched.
    pub(super) pending: Option<AppConfig>,
    /// Whether promoting [`Self::pending`] must re-resolve
    /// [`Self::credentials`].
    ///
    /// `None` reads as `false`, and covers both an absent key and a sheep with
    /// nothing parked.
    ///
    /// Carried with [`Self::pending`] and never without it: the load that
    /// parks the config decides this, and a later diff cannot recompute it.
    pub(super) pending_reidentifies: Option<bool>,
    /// The resolved config this instance runs under, environment included.
    ///
    /// The `AppConfig` beneath [`ProcessEntry::spec`]'s `ResolvedApp`, not the
    /// `ResolvedApp` itself: that type is a proof token minted only by
    /// `normalize`, so the successor rebuilds it by normalizing again. A
    /// successor whose `normalize` has tightened refuses a config its
    /// predecessor accepted, with no stop arm left.
    pub(super) app: AppConfig,
}

impl CarriedSheep {
    /// Describe `entry` for the successor.
    ///
    /// The arguments beyond `entry` do not live on it. `restart_due` is the
    /// one not carried verbatim: it is gated on the entry's status.
    #[must_use]
    pub fn from_entry(
        entry: &ProcessEntry,
        epoch: u64,
        fds: CarriedFds,
        pending_delete: bool,
        manual: Option<PendingManual>,
        ready_failed: bool,
        restart_due: Option<SystemTime>,
    ) -> Self {
        Self {
            id: entry.id,
            name: entry.spec.config().name.clone(),
            instance: entry.instance,
            pid: entry.pid,
            restarts: entry.restarts,
            epoch,
            status: entry.status,
            last_exit: entry.last_exit,
            credentials: entry.credentials,
            fds,
            pending_delete: Some(pending_delete),
            manual,
            reload: Some(entry.reload),
            dog: entry.dog.clone(),
            // Written as a pair with the flag below: a config without its
            // flag promotes on the wrong identity.
            pending: entry.pending.as_ref().map(|parked| parked.config().clone()),
            // Same `Option` as the line above, so the two keys are absent
            // together.
            pending_reidentifies: entry.pending.as_ref().map(|_| entry.pending_reidentifies),
            ready_failed: Some(ready_failed),
            // The slot's own field is written on the one transition into
            // `WaitingRestart` and never cleared, so gating here stops an
            // expired moment riding out on an `Online` row.
            restart_due: (entry.status == ProcStatus::WaitingRestart)
                .then_some(restart_due)
                .flatten(),
            app: entry.spec.config().clone(),
        }
    }

    /// The descriptor numbers this instance's output travels on.
    #[must_use]
    #[allow(dead_code, reason = "read by this crate's own tests")]
    pub const fn fds(&self) -> CarriedFds {
        self.fds
    }

    /// Whether an operator's `delete` targeted this instance before the
    /// exec, or `None`, which means "no".
    #[must_use]
    pub const fn pending_delete(&self) -> Option<bool> {
        self.pending_delete
    }

    /// The manual command that owned this instance's next exit before the
    /// exec, or `None` for an instance no command was waiting on.
    #[must_use]
    pub const fn manual(&self) -> Option<PendingManual> {
        self.manual
    }

    /// Which half of a reload's swap this instance is, or `None`, which
    /// means [`ReloadState::None`].
    #[must_use]
    pub const fn reload(&self) -> Option<ReloadState> {
        self.reload
    }

    /// Where this instance's binary came from if it is a dog, or `None` for
    /// an ordinary sheep.
    ///
    /// Borrowed rather than cloned: [`DogSource::Adopted`] owns a path.
    #[must_use]
    pub const fn dog(&self) -> Option<&DogSource> {
        self.dog.as_ref()
    }

    /// The config a load parked for this instance's next spawn, or `None`
    /// for an instance nothing is owed.
    #[must_use]
    pub const fn pending(&self) -> Option<&AppConfig> {
        self.pending.as_ref()
    }

    /// Whether promoting [`Self::pending`] must re-resolve the identity, or
    /// `None`, which reads as `false`.
    #[must_use]
    pub const fn pending_reidentifies(&self) -> Option<bool> {
        self.pending_reidentifies
    }

    /// Whether a reload's readiness verification has already failed against
    /// this instance, or `None`, which reads as `false`.
    #[must_use]
    pub const fn ready_failed(&self) -> Option<bool> {
        self.ready_failed
    }

    /// When this instance's owed respawn falls due, or `None` for one that is
    /// not owed a respawn.
    #[must_use]
    pub const fn restart_due(&self) -> Option<SystemTime> {
        self.restart_due
    }

    /// The supervisor slot's respawn epoch at the moment of the handover.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The entry id this instance keeps across the handover.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// The name an operator reaches this instance by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The instance slot within its app.
    #[must_use]
    pub const fn instance(&self) -> u32 {
        self.instance
    }

    /// The pid this instance is running under, or `None` for one that is
    /// registered and not running.
    ///
    /// An instance with no pid has [`CarriedFds::none`] and nothing to adopt.
    #[must_use]
    pub const fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Respawns performed so far, which the restart budget counts against.
    #[must_use]
    pub const fn restarts(&self) -> u32 {
        self.restarts
    }

    /// The instance's lifecycle status.
    #[must_use]
    pub const fn status(&self) -> ProcStatus {
        self.status
    }

    /// How this instance most recently stopped existing, if it has.
    #[must_use]
    pub const fn last_exit(&self) -> Option<ExitInfo> {
        self.last_exit
    }

    /// The identity this instance's next spawn runs under, resolved once by
    /// the predecessor.
    #[must_use]
    pub const fn credentials(&self) -> SpawnIdentity {
        self.credentials
    }

    /// The config this instance runs under, as its predecessor normalized it.
    ///
    /// Not a [`ResolvedApp`](shep_core::config::ResolvedApp): see the field's
    /// own doc.
    #[must_use]
    pub const fn app(&self) -> &AppConfig {
        &self.app
    }
}

/// The descriptor numbers one sheep's output travels on, the one its input
/// travels back through, and the one carrying both directions of its
/// shepherd channel.
///
/// `None` on the four output fields means the instance is registered and not
/// running; losing a sheep's stdout read end blocks the child on `write()`
/// once the 64KiB pipe buffer fills. [`Self::stdin`] and [`Self::channel`] are
/// present only for a running sheep whose app asked for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedFds {
    /// The read end of the sheep's stdout pipe.
    pub out_pipe: Option<RawFd>,
    /// The read end of the sheep's stderr pipe.
    pub err_pipe: Option<RawFd>,
    /// The appending handle on the sheep's stdout log file.
    pub out_log: Option<RawFd>,
    /// The appending handle on the sheep's stderr log file.
    pub err_log: Option<RawFd>,
    /// The write end of the sheep's stdin pipe, which `shep whisper` writes
    /// a line into.
    ///
    /// `None` for a sheep whose app did not set `stdin = true`, which has
    /// `/dev/null` on fd 0, and for a sheep that is not running. An absent
    /// field loads as `None`, so [`VERSION`](super::VERSION) is unmoved.
    pub stdin: Option<RawFd>,
    /// The daemon's end of the sheep's shepherd-channel socketpair, whose
    /// other end is the child's fd 3. One number for both directions:
    /// `spawn_channel_pumps` splits it into two tasks over one open file
    /// description.
    ///
    /// `None` for a sheep whose app set none of `channel`, `wait_ready` or
    /// `shutdown_with_message`, for a sheep that is not running, and for one
    /// whose child has closed its fd 3. An absent field loads as `None`.
    pub channel: Option<RawFd>,
}

/// Which of a sheep's six descriptors a number is. A stdout pipe and a stdin
/// pipe are both pipes and are refused by opposite checks, so the slot has to
/// travel with the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SheepFd {
    /// The read end of the sheep's stdout pipe ([`CarriedFds::out_pipe`]).
    OutPipe,
    /// The read end of the sheep's stderr pipe ([`CarriedFds::err_pipe`]).
    ErrPipe,
    /// The appending handle on its stdout log ([`CarriedFds::out_log`]).
    OutLog,
    /// The appending handle on its stderr log ([`CarriedFds::err_log`]).
    ErrLog,
    /// The write end of its stdin pipe ([`CarriedFds::stdin`]).
    Stdin,
    /// The daemon's end of its shepherd channel ([`CarriedFds::channel`]).
    Channel,
}

impl SheepFd {
    /// What this slot is called in a refusal. Must match the wording the
    /// adoption functions use.
    pub(crate) const fn describe(self) -> &'static str {
        match self {
            Self::OutPipe => "stdout pipe",
            Self::ErrPipe => "stderr pipe",
            Self::OutLog => "stdout log",
            Self::ErrLog => "stderr log",
            Self::Stdin => "stdin pipe",
            Self::Channel => "shepherd channel",
        }
    }
}

impl CarriedFds {
    /// The six numbers in a fixed order: stdout's pipe, stderr's pipe,
    /// stdout's log, stderr's log, stdin's pipe, the shepherd channel.
    #[must_use]
    pub const fn all(&self) -> [Option<RawFd>; 6] {
        [
            self.out_pipe,
            self.err_pipe,
            self.out_log,
            self.err_log,
            self.stdin,
            self.channel,
        ]
    }

    /// [`Self::all`], with each number labelled by which of the six it is.
    ///
    /// [`adopt`](super::adopt::adopt) and [`dry_run`](super::adopt::dry_run)
    /// must agree on the kinds: a number rehearsed as the wrong kind is a
    /// rehearsal that passes and a boot that still fails.
    pub(crate) const fn all_kinded(&self) -> [(Option<RawFd>, SheepFd); 6] {
        [
            (self.out_pipe, SheepFd::OutPipe),
            (self.err_pipe, SheepFd::ErrPipe),
            (self.out_log, SheepFd::OutLog),
            (self.err_log, SheepFd::ErrLog),
            (self.stdin, SheepFd::Stdin),
            (self.channel, SheepFd::Channel),
        ]
    }

    /// The no-descriptors case: a sheep that is registered and not running.
    /// [`fitness`](fn@super::fitness) does not refuse it.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            out_pipe: None,
            err_pipe: None,
            out_log: None,
            err_log: None,
            stdin: None,
            channel: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CarriedFds, CarriedSheep, SheepFd};
    use crate::entry::ReloadState;
    use crate::handover::fixtures::{carried, entry_fixture, fds_at, plain};
    use crate::handover::{Counters, DaemonFds, Fitness, Handover, fitness};
    use crate::supervisor::{CommandOrigin, ManualKind, PendingManual};

    /// [`CarriedSheep::instance`] is carried per row rather than re-derived
    /// from a count: without that, two live pids write into each other's log
    /// files under each other's `SHEP_INSTANCE`.
    #[test]
    fn each_instance_carries_its_own_slot_and_descriptors() {
        let mut zero = entry_fixture(|app| app.instances = 2);
        zero.instance = 0;
        let mut one = entry_fixture(|app| app.instances = 2);
        one.id = 2;
        one.instance = 1;
        one.pid = Some(101);

        let blob = Handover::new(
            vec![
                CarriedSheep::from_entry(&zero, 7, fds_at(11), false, None, false, None),
                CarriedSheep::from_entry(&one, 8, fds_at(21), false, None, false, None),
            ],
            DaemonFds {
                listener: 3,
                pidfile: 4,
            },
            Counters {
                next_id: 9,
                next_deadline: 5,
                next_action_stamp: 2,
            },
            Vec::new(),
        );
        let back: Handover = serde_json::from_str(&serde_json::to_string(&blob).unwrap()).unwrap();

        let carried = back.sheep();
        assert_eq!(carried.len(), 2, "one row per instance, never per app");
        // Bound by slot rather than by position, so a reordered blob still
        // has to put each pid with its own slot.
        let slot_zero = carried
            .iter()
            .find(|sheep| sheep.instance() == 0)
            .expect("slot 0 must be carried");
        let slot_one = carried
            .iter()
            .find(|sheep| sheep.instance() == 1)
            .expect("slot 1 must be carried");
        assert_eq!(slot_zero.id(), 1);
        assert_eq!(slot_zero.pid(), Some(100));
        assert_eq!(slot_zero.epoch(), 7);
        assert_eq!(slot_zero.fds(), fds_at(11));
        assert_eq!(slot_one.id(), 2);
        assert_eq!(slot_one.pid(), Some(101));
        assert_eq!(slot_one.epoch(), 8);
        assert_eq!(slot_one.fds(), fds_at(21));
        assert_eq!(
            slot_zero.name(),
            slot_one.name(),
            "both slots are the same app, which is what makes the slot the \
             only thing telling them apart"
        );
    }

    /// Both halves of the swap, and the drainee's linked id with them.
    #[test]
    fn a_swap_in_flight_no_longer_refuses_and_reaches_the_blob() {
        let mut drainee = entry_fixture(|_| {});
        drainee.reload = ReloadState::Drainee { new_id: Some(9) };
        let mut replacement = entry_fixture(|_| {});
        replacement.id = 9;
        replacement.reload = ReloadState::Replacement;
        assert_eq!(
            fitness(&[plain(&drainee), plain(&replacement)]),
            Fitness::Carryable
        );

        assert_eq!(
            carried(&drainee).reload(),
            Some(ReloadState::Drainee { new_id: Some(9) }),
            "the id linking the two halves must survive the blob, not just the role"
        );
        assert_eq!(
            carried(&replacement).reload(),
            Some(ReloadState::Replacement)
        );
    }

    /// `Candidate` has nothing to say about a pending command, so this
    /// asserts through the blob rather than through the gate.
    #[test]
    fn a_pending_manual_command_no_longer_refuses_and_reaches_the_blob() {
        let e = entry_fixture(|_| {});
        assert_eq!(fitness(&[plain(&e)]), Fitness::Carryable);

        let marked = CarriedSheep::from_entry(
            &e,
            7,
            fds_at(11),
            false,
            Some(PendingManual {
                kind: ManualKind::Delete,
                origin: CommandOrigin::Automatic,
            }),
            false,
            None,
        );
        assert_eq!(
            marked.manual(),
            Some(PendingManual {
                kind: ManualKind::Delete,
                origin: CommandOrigin::Automatic,
            }),
            "both halves of the marker must survive the blob, not just that one exists"
        );
    }

    /// Six distinct numbers, so the equality below is about the pairing
    /// rather than the length.
    #[test]
    fn every_carried_number_is_kinded_in_the_same_order() {
        let fds = CarriedFds {
            out_pipe: Some(10),
            err_pipe: Some(11),
            out_log: Some(12),
            err_log: Some(13),
            stdin: Some(14),
            channel: Some(15),
        };

        assert_eq!(
            fds.all_kinded().map(|(fd, _)| fd),
            fds.all(),
            "the kinded walk and the `FD_CLOEXEC` walk must see the same numbers"
        );

        let slots: std::collections::HashSet<SheepFd> =
            fds.all_kinded().iter().map(|(_, slot)| *slot).collect();
        assert_eq!(slots.len(), 6, "each slot must appear exactly once");
    }
}
