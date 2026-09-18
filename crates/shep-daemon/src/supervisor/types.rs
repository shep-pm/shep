//! Small value types the actor passes around.
//!
//! None of these carry behaviour beyond a constructor or an accessor. They
//! name things the actor needs a word for: which connection asked, what a
//! scale call settled on, which fields a write touched, and what a
//! registration is waiting for.

use super::*;

/// Distinguishes one client connection from another.
///
/// Minted per accepted connection and never reused within a daemon's life.
/// The only thing scoped by it is smits. Here rather than in `server`, which
/// is `#[cfg(unix)]` and so unnameable from a Windows build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ConnId(u64);

impl ConnId {
    /// Mints the next id. Monotonic, and wide enough that a daemon cannot
    /// reach the wrap.
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, atomic::Ordering::Relaxed))
    }
}

/// Every sheep name that currently carries a smit, and which connection
/// painted it.
///
/// Keyed by name, not by instance id, so every instance of a named sheep
/// reads the same mark.
pub(super) type Smits = HashMap<String, (ConnId, String)>;

/// Whether a [`Command::Start`] is all-or-nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchPolicy {
    /// Refuse the whole batch if any app in it provably cannot run, and
    /// register none of it.
    ///
    /// For `shep start` against a Flockfile: a partial registration leaves a
    /// flock matching neither the file nor its previous state.
    AllOrNothing,
    /// Register and spawn each app on its own merits, so one app that cannot
    /// run costs only itself.
    ///
    /// For a muster roll restore at boot, and for a dog. Every failure leaves
    /// an `Errored` row, and one from an unresolvable `user`/`group` carries
    /// [`SpawnIdentity::Unresolved`], so the app never runs as the daemon.
    PerApp,
}

/// Whether a registration call created the row it hands back, or found one
/// already there.
///
/// [`ProcessEventKind::Errored`] says a sheep transitioned, so the emit keys
/// on this rather than on the row's status, which is alike either way.
#[derive(Debug)]
pub(super) enum Registration {
    /// This call registered the row, so any event owed for it is owed now.
    Fresh(ProcessInfo),
    /// An app of this name was already registered; the row is untouched.
    AlreadyKnown(ProcessInfo),
}

impl Registration {
    /// The row, whichever variant it is.
    pub(super) fn into_info(self) -> ProcessInfo {
        match self {
            Self::Fresh(info) | Self::AlreadyKnown(info) => info,
        }
    }
}

/// What a [`Command::SetSheepField`] did.
///
/// Two facts, and only the second is a judgement: the config the caller
/// should record on the muster roll, and whether the running child has the
/// value yet.
///
/// `Debug` is derived (IR-41): [`ResolvedApp`] wraps an [`AppConfig`], whose
/// own manual `Debug` redacts `env`, and the rest is a bool and an
/// operator-facing string with no value from a live flock in it.
#[derive(Debug, Clone)]
pub(crate) struct FieldSet {
    /// The app as it now stands, for `rpc.rs` to hand the registry: the
    /// parked config when the field parked, the stored spec's when it
    /// reached. `Command::SetSheepEnv` answers with the same thing and for
    /// the same reason: the muster roll is written from the registry.
    pub(crate) app: ResolvedApp,
    /// `true` when the running child does not have the value yet.
    pub(crate) pending: bool,
    /// The `warning` [`shep_core::protocol::Response::SheepFieldSet`]
    /// answers with, computed here where the filesystem is in reach.
    pub(crate) warning: Option<String>,
}

/// What a [`Command::SetSheepEnvBatch`] did.
///
/// `Debug` is derived: every field is a key name, and [`ResolvedApp`] wraps
/// an [`AppConfig`], whose own manual `Debug` redacts `env`.
#[derive(Debug, Clone)]
pub(crate) struct EnvBatch {
    /// The parked config, for `rpc.rs` to hand the registry. `None` when
    /// nothing was written, which is a dry run or an unforced collision.
    pub(crate) app: Option<ResolvedApp>,
    /// Keys written.
    pub(crate) set: Vec<String>,
    /// Keys that already held this value.
    pub(crate) unchanged: Vec<String>,
    /// Keys that held a different value.
    pub(crate) collisions: Vec<String>,
}

/// The environment a shepherd resolves secrets in when nothing says
/// otherwise, matching `DaemonSection`'s own default.
pub(crate) const DEFAULT_ENVIRONMENT: &str = "production";

/// Fire-and-forget control message to one sheep task (see `run_sheep`).
///
/// No acknowledgement: a sheep task's own `Msg::Exited` is the only completion
/// signal the actor waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheepCtl {
    /// Run the kill ladder and report the resulting exit.
    Kill {
        /// How long the ladder's polite rung gets before it escalates to
        /// `SIGKILL`. On the message rather than read off the app inside
        /// [`kill_process`]: only the sender knows which of `kill_timeout`
        /// and `graceful_timeout` it is asking for.
        grace: Duration,
    },
}

/// What a completed [`Command::Scale`] produced.
///
/// [`Actor::handle_scale`]'s caller replies with `instances` and re-records
/// `app` in the muster roll. A scale that fell short is still a `Scaled`:
/// recording is unconditional, and only the operator's exit code turns on
/// whether the request was fully satisfied.
#[derive(Debug)]
pub(crate) struct Scaled {
    /// The app's surviving instances, by name, then instance slot, then id
    /// (`shep_core::protocol::sort_flock`). On a partial scale-up this is what
    /// came up, never the count asked for.
    pub(crate) instances: Vec<ProcessInfo>,
    /// The app's config as it now stands, with the achieved `instances`
    /// count.
    pub(crate) app: ResolvedApp,
    /// The count the operator asked for. Equal to [`Self::achieved`] unless
    /// [`Self::shortfall`] is `Some`.
    pub(crate) requested: u32,
    /// `Some(message)` when a scale-up ran out part-way: the spawn failure
    /// that stopped it, in the runner's own words. `None` on every path that
    /// reached the requested count.
    pub(crate) shortfall: Option<String>,
}

impl Scaled {
    /// How many instances the app is left running.
    ///
    /// Read off `instances` rather than the stored config so the two cannot
    /// disagree: they are written from the same survivor list.
    pub(crate) fn achieved(&self) -> u32 {
        u32::try_from(self.instances.len()).unwrap_or(u32::MAX)
    }
}

/// What one app's [`Command::ApplyConfig`] did.
///
/// One of these per app the file named, whether or not the app was found and
/// whether or not anything about it changed.
#[derive(Debug)]
pub(crate) struct Applied {
    /// The app's name, exactly as the file spells it.
    pub(crate) name: String,
    /// Fields now in force, in field-name order. [`ApplyGroup::Live`] only:
    /// the daemon reads the new value at its next decision.
    pub(crate) applied: Vec<String>,
    /// Fields the app picks up at its next spawn, in field-name order.
    ///
    /// Not [`ProcessEntry::pending`], which shares its name and is a whole
    /// parked `ResolvedApp`. Both [`ApplyGroup::NextSpawn`] and
    /// [`ApplyGroup::NeedsRespawn`] land here: the running process keeps the
    /// old value either way, and what differs is where the new one waits, on
    /// the stored spec or in [`ProcessEntry::pending`].
    pub(crate) pending: Vec<String>,
    /// Why some or all of this app's change did not land, in the daemon's own
    /// words, or `None` when the whole of it did.
    ///
    /// Every refusal raised before [`Actor::apply_one`] routes the instance
    /// count leaves the app untouched. Two empty lists are not that promise,
    /// though: a later refusal produces the same shape, and only the message
    /// tells them apart.
    pub(crate) refused: Option<String>,
    /// The merged, normalized app. `rpc.rs` hands this to
    /// `FlockRegistry::record`, so a reboot comes up on the applied config.
    /// The full merge, `NeedsRespawn` fields included, since a reboot spawns
    /// every process afresh.
    ///
    /// `None` for an app whose merge never produced one: the refusals above
    /// that touch nothing, plus a merge that does not normalize at the
    /// instance count really running.
    pub(crate) app: Option<ResolvedApp>,
}

/// One command's aggregation state, replied to once `remaining` is empty.
#[derive(Debug)]
pub(super) struct PendingReply {
    /// Ids not yet observed terminal.
    pub(super) remaining: HashSet<u32>,
    /// Terminal snapshots collected so far, in arrival order.
    pub(super) results: Vec<ProcessInfo>,
    /// Where the answer goes once `remaining` is empty.
    pub(super) reply: ReplyKind,
}
