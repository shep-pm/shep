//! The supervisor actor: owns the flock's lifecycle state machine.
//!
//! [`spawn_supervisor`] starts the actor task and hands back a
//! [`SupervisorHandle`]. Every registered instance ("sheep") gets its own task
//! owning the live `(proc, ProcIo)` pair; the actor holds one lifecycle entry
//! plus two senders per id, so the loop never awaits process I/O. However a
//! sheep ends, it reaches the actor as exactly one `Msg::Exited`.
//!
//! `stop`/`restart`/`delete` and `shutdown` resolve their selector up front,
//! then answer once every matched sheep is terminal.
//!
//! This file holds the `Actor` struct and the mailbox capacities. The impl is
//! split across the `actor_*` modules, one per group of handlers.

use core::cmp::Ordering;
use core::fmt;
use core::sync::atomic::{self, AtomicU64};
use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use shep_core::config::{
    AppConfig, ApplyGroup, DeclaredApp, ResetDepth, ResolvedApp, apply_group, normalize,
    reaches_running,
};
use shep_core::overrides::{self, AppOverrides};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    ActionOutcome, ActionReply, BusEvent, DogSource, EnvValue, ExitInfo, LineOutcome, LineReply,
    ProcessEventKind, ProcessInfo, SheepConfigView, SheepDrift, SignalOutcome, SignalReply, Smit,
    sort_flock,
};
use shep_core::secrets::SecretView;
use shep_core::selector::ProcessSelector;
use shep_core::signals::OperatorSignal;
use shep_core::status::ProcStatus;
use shep_core::values::MemSize;

use crate::assemble::{AssembleError, assemble, describe, instance_slots};
use crate::backoff::restart_delay;
use crate::brain::{Decision, decide_on_exit};
use crate::bus::{Bus, SharedEvent};
use crate::channel::{ChildMessage, ShepherdMessage};
use crate::entry::{ProcessEntry, ReloadState, RestartBudget};
use crate::extras::{Extras, ExtrasRegistry};
#[cfg(unix)]
use crate::handover::adopt::AdoptedSheep;
#[cfg(unix)]
use crate::handover::reap::AdoptedReaper;
#[cfg(unix)]
use crate::handover::{
    Candidate, CarriedFds, CarriedSheep, Counters, DaemonFds, Fitness, Handover, OwnedCandidate,
    fitness,
};
use crate::kill::kill_process;
use crate::privilege::{self, Credentials, PrivilegeError, SpawnIdentity};
use crate::probes::Prober;
use crate::probes::os::OsProber;
use crate::probes::ready::{Readiness, ReadinessSource, await_ready};
#[cfg(unix)]
use crate::runner::AdoptSpec;
use crate::runner::{
    ExitOutcome, FlushError, LogCtl, Preflight, ProcIo, ProcessRunner, ReopenError, RunnerError,
    RunningProcess, SpawnSpec, StdinWrite, check_log_ancestry, cwd_advisory, log_path_advisory,
    open_log_path,
};
use crate::secrets::ProviderSecrets;

// --- module tree ---
mod actor_actions;
mod actor_config;
mod actor_core;
mod actor_exit;
mod actor_lifecycle;
mod actor_pane;
mod actor_reload;
mod actor_reload_done;
mod actor_scale;
mod actor_spawn;
mod builder;
mod command;
mod config_merge;
mod error;
mod handle;
mod handover;
mod logs;
mod manual;
mod reload;
mod sheep;
mod slot;
mod tasks;
mod types;
mod wire;

#[cfg(unix)] pub(crate) use error::AdoptError;
pub use builder::spawn_supervisor;
pub use error::SupervisorError;
pub use handle::SupervisorHandle;
pub(crate) use builder::SupervisorBuilder;
pub(crate) use command::{Command, Msg};
pub(crate) use manual::{CommandOrigin, ManualKind, PendingManual};
pub(crate) use reload::{CarriedReload, ReloadMode, ReloadPhase, ReloadSwap};
pub(crate) use types::{Applied, BatchPolicy, ConnId, DEFAULT_ENVIRONMENT, EnvBatch, FieldSet};
use command::ReplyKind;
use config_merge::{EXTRAS_FIELDS, dog_config_refusal, env_override_map, merge_declared, pending_fields, reached_spec, with_count};
use handover::{HandoverDraft, REPORT_DEADLINE, Snapshot, spawn_handover_task};
use logs::{flush_logs, reopen_logs, spawn_flush_task, spawn_reopen_task, truncate_log};
use manual::{ActionWaits, PendingAction};
use reload::{LadderCap, ReloadJob};
use sheep::{SignalRequest, run_sheep, spawn_sheep_task};
use slot::SheepSlot;
use tasks::{spawn_action_task, spawn_readiness_task, spawn_send_line_task, spawn_signal_task, spawn_trigger_task, spec_prober};
use types::{PendingReply, Registration, Scaled, SheepCtl, Smits};
use wire::{reload_eligible, restored_status, send_reply, swap_budget, to_info};
// --- end module tree ---


/// Capacity of the actor's own mailbox (commands + internal events).
const MAILBOX_CAPACITY: usize = 256;

/// Capacity of one sheep task's control mailbox. At most one live `Kill` is
/// ever in flight, so this stays small.
const SHEEP_CTL_CAPACITY: usize = 4;

/// Capacity of one sheep task's signal mailbox.
///
/// Wider than [`SHEEP_CTL_CAPACITY`]: nothing bounds a burst of `shep signal`
/// calls at one sheep, and [`Actor::begin_signal`] reads a `Full` mailbox as
/// "this sheep's task is busy".
const SIGNAL_CAPACITY: usize = 16;

/// How much longer than its own two timeouts one swap of a reload is given
/// before the actor gives up on it (see [`Actor::arm_reload_deadline`]).
///
/// A swap is already bounded by `listen_timeout` then `graceful_timeout`, so
/// this covers scheduling jitter only. Abandonment never ends a serving
/// instance; what is lost is the rest of the reload.
const RELOAD_DEADLINE_SLACK: Duration = Duration::from_secs(5);

/// How long the shepherd waits for one line to land in a sheep's stdin before
/// reporting [`LineOutcome::NotWritten`].
///
/// A pipe fills at 64 KiB and the write then blocks until the app reads. Two
/// seconds, under the 5s an RPC caller gets when it sends no deadline, so the
/// `not_written` row reaches it. The waits run concurrently, so one `sendline`
/// costs at most this whatever the selector matched.
const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How many replies one sheep may still owe from actions that stopped
/// waiting before the oldest of them is forgotten (see [`ActionWaits`]).
///
/// One entry per trigger whose deadline the app missed, removed when the app
/// answers. A dropped entry costs one late reply going to the wrong wait.
const MAX_ABANDONED_ACTION_REPLIES: usize = 64;

// ---------------------------------------------------------------------
// Public command / handle surface
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// Internal actor state
// ---------------------------------------------------------------------

/// The supervisor actor. Holds every registered sheep's lifecycle state and
/// control handle, never a live `proc`, plus any deferred command replies
/// still waiting on matched sheep to go terminal.
struct Actor<R: ProcessRunner> {
    /// Spawn seam (real OS processes or, in tests, the scripted fake).
    runner: R,
    /// `$SHEP_HOME` layout, for assembling spawn specs.
    paths: ShepPaths,
    /// The environment a sheep that sets no `environment` of its own
    /// resolves its `{{secret:...}}` references in; see [`Self::secret_view`].
    host_environment: String,
    /// What provider dogs have pushed, the namespaced half of every
    /// [`SecretView`] this actor builds. Shared with the connection tasks
    /// that write it; see [`ProviderSecrets`].
    provider_secrets: Arc<ProviderSecrets>,
    /// Bus: process lifecycle events + forwarded logs.
    events: Bus,
    /// Clone handed to sheep tasks and restart timers so they can report
    /// back into this same actor's mailbox.
    tx: mpsc::Sender<Msg>,
    /// Every registered instance, keyed by id.
    sheep: HashMap<u32, SheepSlot>,
    /// Monotonic id counter: ids are never reused.
    next_id: u32,
    /// Stamps the next reload watchdog, so a job's older ones cannot end it.
    /// Never reused; see [`ReloadJob::deadline`].
    next_deadline: u64,
    /// Monotonic stamp counter for action waits; see
    /// [`PendingAction::stamp`].
    next_action_stamp: u64,
    /// Deferred command replies still waiting on matched sheep.
    pending: Vec<PendingReply>,
    /// Set once a `Shutdown` command starts. While `true`, `Start`/`Restart`
    /// are rejected and `RestartDue` respawns nothing: no child may appear
    /// that the shutdown aggregation, fixed when it ran, cannot know to kill.
    shutting_down: bool,
    /// The lifecycle extras' seams and report wiring, or `None` for an engine
    /// built without them (`spawn_supervisor`).
    extras: Option<Extras>,
    /// What is armed right now, per sheep and per name. Stays empty while
    /// `extras` is `None`: there are no seams to arm anything on.
    registry: ExtrasRegistry,
    /// Every sheep name currently carrying a smit; see [`Smits`]. A dog
    /// painting one puts an entry here, and its connection closing removes it.
    smits: Smits,
    /// Every app currently mid-reload, keyed by app name. An entry is what
    /// makes a second reload of the same app refusable, and what tells a
    /// sheep's exit whether it is a swap's business or its own.
    reloads: HashMap<String, ReloadJob>,
}

#[cfg(test)]
mod tests;
