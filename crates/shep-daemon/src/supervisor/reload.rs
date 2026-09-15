//! State for a reload in flight.
//!
//! A reload replaces a sheep's process without the operator losing the old
//! one first. `ReloadJob` tracks that across several actor turns: which
//! instances are left, which swap is uncommitted, and whether the mode
//! allows an overlap or has to drain serially.

use super::*;

/// Which of an app's two ladder caps a stop runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LadderCap {
    /// `kill_timeout`: an operator's `stop`, `restart` or `delete`, the
    /// daemon's own automatic restarts, and the engine-wide shutdown.
    Stop,
    /// `graceful_timeout`: a reload's drain, the one stop that asks the
    /// instance to finish the work in hand.
    Drain,
}

impl LadderCap {
    /// This cap's value for `app`.
    pub(super) fn of(self, app: &AppConfig) -> Duration {
        match self {
            Self::Stop => app.kill_timeout,
            Self::Drain => app.graceful_timeout,
        }
        .as_duration()
    }
}

/// One app's in-flight reload.
///
/// Keyed by app name in [`Actor::reloads`]. An entry existing there means the
/// app is mid-reload, which is what makes a second reload of the same app
/// refusable ([`SupervisorError::ReloadInFlight`]).
#[derive(Debug)]
pub(super) struct ReloadJob {
    /// Instances not yet taken, in slot order. Popped one at a time, so the
    /// app is only ever one instance short of its configured count.
    pub(super) queue: VecDeque<u32>,
    /// Whether this reload overlaps its two instances or replaces them one
    /// after the other. Decided once, when the job is created: a job that
    /// changed mode half way would drain an instance it had already replaced.
    pub(super) mode: ReloadMode,
    /// The pair mid-swap right now. Exactly one per job.
    pub(super) swap: ReloadSwap,
    /// Which of this job's watchdogs is the live one.
    ///
    /// A job arms more than one over its life, and only the newest may end it,
    /// so each arming takes a fresh stamp off [`Actor::next_deadline`] and
    /// [`Actor::handle_reload_deadline`] drops any message not carrying it.
    pub(super) deadline: u64,
}

/// One app's in-flight reload, as it crosses a handover.
///
/// A [`ReloadJob`] minus [`ReloadJob::deadline`], a stamp on a timer that died
/// with the predecessor's image. The app name is on the row, so the blob holds
/// an array in a stable order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Both sites that build one of these are `cfg(unix)` while the type travels on
// `Handover`, which is not, so a Windows target constructs none. An `expect`,
// so a Windows handover has to delete this line rather than inherit it.
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "no target without a handover ever builds one")
)]
pub(crate) struct CarriedReload {
    /// The app whose reload this is: [`Actor::reloads`]' own key.
    pub(crate) app: String,
    /// Instances not yet taken, in slot order.
    pub(crate) queue: Vec<u32>,
    /// Whether this job overlaps its two instances or replaces them one
    /// after the other.
    pub(crate) mode: ReloadMode,
    /// The pair mid-swap right now.
    pub(crate) swap: ReloadSwap,
}

/// Which of two orderings a reload runs, decided from the app's config.
///
/// A `readiness_probe` asks an address, and an address cannot say which
/// process answered it, so overlapped instances can answer for each other.
/// The app picks by whether it can share a port.
///
/// Serialized because a handover carries the job it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReloadMode {
    /// `SpawnNew → AwaitReady → DrainOld → ReapOld`. Both instances run at
    /// once, so the app is never short one.
    ///
    /// Taken by an app with no probe, by one using `wait_ready`, and by one
    /// whose `reuse_port` says the app sets `SO_REUSEPORT` itself. The last
    /// keeps a residue of the problem above, which
    /// [`Actor::post_drain_probe`] closes.
    Overlap,
    /// `DrainOld → ReapOld → SpawnNew → AwaitReady`. The instance being
    /// replaced goes first, and its replacement is spawned into the empty
    /// slot.
    ///
    /// The default for a probed app. It costs a gap, the drain plus the
    /// replacement's start, and buys a probe only the replacement can answer.
    /// It spares an app without `SO_REUSEPORT` the `EADDRINUSE` an overlapping
    /// reload would take. A single-instance app loses its name-group watch and
    /// cron worker for the width of the gap.
    Serial,
}

impl ReloadMode {
    /// The mode `config` asks for, given the readiness source already derived
    /// from it.
    ///
    /// `source` rather than the config's own two readiness fields, whose
    /// precedence is [`ReadinessSource::of`]'s to state. Only a `Probe` is
    /// answerable by the wrong instance, so only a `Probe` serialises.
    pub(super) fn of(config: &AppConfig, source: &ReadinessSource) -> Self {
        match source {
            ReadinessSource::Probe(..) if !config.reuse_port => Self::Serial,
            _ => Self::Overlap,
        }
    }
}

/// The drainee/replacement pair a reload is working on right now.
///
/// Serialized because a handover carries the job it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ReloadSwap {
    /// The instance being replaced. Carries `ProcStatus::Stopping` and
    /// [`ReloadState::Drainee`] from the moment the swap starts.
    pub(crate) old_id: u32,
    /// Its replacement, in the same instance slot under a new id. Carries
    /// [`ReloadState::Replacement`] until the swap finishes, and is `None`
    /// exactly while the phase is [`ReloadPhase::DrainFirst`].
    pub(crate) new_id: Option<u32>,
    /// How far along this pair is; see [`ReloadPhase`].
    pub(crate) phase: ReloadPhase,
}

/// Where a [`ReloadSwap`] is in the spec's per-instance state machine.
///
/// `SpawnNew` and `ReapOld` are instants rather than intervals, so they get no
/// variant: what a handler asks is whether the old instance is still there to
/// go back to.
///
/// Serialized because a handover carries the swap it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReloadPhase {
    /// [`ReloadMode::Serial`] only: the instance being replaced is on its kill
    /// ladder and nothing has been spawned yet, so `swap.new_id` is `None`.
    ///
    /// Committed: a phase is abandonable only while there is an instance to go
    /// back to, and this one has already asked that instance to go, so
    /// [`Actor::uncommitted_swap_of`] answers `None` here.
    DrainFirst,
    /// The replacement is registered and starting.
    ///
    /// Under [`ReloadMode::Overlap`] nothing has been killed and the reload is
    /// still abandonable. Under [`ReloadMode::Serial`] the drainee is already
    /// gone, and [`Actor::reap_drainee`] moves the phase to `DrainOld` before
    /// returning.
    AwaitReady,
    /// Committed: the replacement went online and the drainee's ladder is
    /// running, or the drainee is already gone. No old instance to return to.
    DrainOld,
    /// [`ReloadMode::Overlap`] only, and only for a probed app: the drainee is
    /// reaped and the replacement is asked alone whether it can serve. See
    /// [`Actor::post_drain_probe`].
    Verify,
}
