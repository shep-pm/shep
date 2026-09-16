//! Everything the actor can be asked to do.
//!
//! [`Command`] is the operator-facing vocabulary a [`SupervisorHandle`] sends;
//! `Msg` wraps it alongside the internal events the actor raises for itself,
//! and is what the mailbox actually carries. `ReplyKind` names the channel a
//! finished command answers on.

use super::*;

/// Commands the supervisor actor accepts (wrapped in [`Msg::Command`]).
#[derive(Debug)]
pub(crate) enum Command {
    /// Registers + spawns each app's instances.
    Start {
        /// Already-validated app specs to expand into instances.
        apps: Vec<ResolvedApp>,
        /// Whether one app that provably cannot run refuses the whole batch.
        policy: BatchPolicy,
        /// Names in `apps` that a later boot stage depends on.
        ///
        /// Each one is armed with [`ReadinessSource::Heuristic`] rather than
        /// reported `Online` at spawn, so a stage waiting on it waits for
        /// its `listen_timeout` rather than for `fork` returning. Empty for
        /// every caller but the boot-order driver.
        gate: BTreeSet<String>,
        /// Answers with every spawned instance, or the first spawn failure.
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Registers each app as a flock member without spawning anything.
    ///
    /// For restoring a muster roll: a sheep saved while stopped returns
    /// stopped and restartable.
    RegisterAtRest {
        /// Already-validated app specs to register, one entry each.
        apps: Vec<ResolvedApp>,
        /// Answers with every entry now registered, in the order given.
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Reports which of `apps` name a sheep registered under a different
    /// config.
    ///
    /// Read-only, so it is answered during a shutdown rather than refused.
    ConfigDrift {
        /// Already-validated app specs to compare against the flock.
        apps: Vec<ResolvedApp>,
        /// Answers with one entry per app that is registered and different.
        reply: oneshot::Sender<Result<Vec<SheepDrift>, SupervisorError>>,
    },
    /// Registers + spawns one dog, marked with where it came from.
    ///
    /// Separate from [`Self::Start`] only for the marker and for being
    /// idempotent by name. A dog is supervised exactly as a sheep is.
    StartDog {
        /// The dog's already-validated app spec, built by the daemon rather
        /// than read from a Flockfile.
        ///
        /// Boxed: unboxed it would size every [`Msg`] the actor receives.
        app: Box<ResolvedApp>,
        /// Where this dog came from, written onto its entry.
        source: DogSource,
        /// Answers with the dog's instance, started or already registered.
        reply: oneshot::Sender<Result<ProcessInfo, SupervisorError>>,
    },
    /// Stops every sheep matching `selector` (stays registered).
    Stop {
        /// Which sheep.
        selector: ProcessSelector,
        /// Answers once every matched sheep is terminal.
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Restarts every sheep matching `selector`.
    Restart {
        /// Which sheep.
        selector: ProcessSelector,
        /// Who asked. Governs whether this restart can be displaced
        /// mid-kill-ladder (see `Actor::claim_manual`) and the `manually` flag
        /// on the bus events it produces, never what the restart does.
        origin: CommandOrigin,
        /// Answers once every matched sheep is back online (or errored).
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Restarts one sheep on behalf of a memory breach or a liveness failure,
    /// if the process that produced the report is still the one running now.
    ///
    /// The only command with no `reply`: dropping a stale report is the
    /// intended outcome, not an error its reporter could act on.
    ExtraRestart {
        /// The sheep's id.
        id: u32,
        /// The pid the report was raised against, used as a generation token.
        pid: u32,
        /// The liveness epoch the reporting probe was armed under, or `None`
        /// for a memory breach, which has nothing that can go stale.
        epoch: Option<u64>,
        /// The tree size a memory breach was observed at, or `None` for a
        /// liveness failure, which observes nothing.
        ///
        /// A breach is computed against the ceiling armed at that moment and
        /// delivered later, so `Actor::handle_extra_restart` re-asks against
        /// the ceiling in force now.
        observed: Option<MemSize>,
    },
    /// Replaces every matched sheep with a fresh instance, one instance of
    /// each app at a time.
    Reload {
        /// Which sheep.
        selector: ProcessSelector,
        /// Answers the moment the reload is accepted, not when it finishes.
        /// See [`Actor::handle_reload`].
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Stops + deregisters every sheep matching `selector`.
    Delete {
        /// Which sheep.
        selector: ProcessSelector,
        /// Answers with the deleted ids once every matched sheep is terminal.
        reply: oneshot::Sender<Result<Vec<u32>, SupervisorError>>,
    },
    /// Sets one app's instance count. See [`Actor::handle_scale`].
    Scale {
        /// The app's name, exactly as its config spells it. Not a selector:
        /// see [`shep_core::protocol::Request::Scale`].
        name: String,
        /// How many instances the app has when this returns.
        count: u32,
        /// Answers with the app's surviving instances and its new config.
        reply: oneshot::Sender<Result<Scaled, SupervisorError>>,
    },
    /// Merges a Flockfile's apps into the running flock. See
    /// [`Actor::handle_apply_config`].
    ApplyConfig {
        /// One entry per app the document declared, carrying the keys it
        /// literally wrote as well as the values behind them.
        apps: Vec<DeclaredApp>,
        /// How much of the load overwrites what an operator has set since.
        reset: ResetDepth,
        /// Answers with one report per app given, in the order given.
        reply: oneshot::Sender<Result<Vec<Applied>, SupervisorError>>,
    },
    /// Reads one sheep's effective config for a pane. See
    /// [`Actor::handle_sheep_config`].
    SheepConfig {
        /// The sheep's name, exactly as its config spells it. Not a
        /// selector, for [`Self::Scale`]'s reason.
        name: String,
        /// Answers with the view, or `None` when no sheep has that name.
        reply: oneshot::Sender<Result<Option<SheepConfigView>, SupervisorError>>,
    },
    /// Sets or removes one env key on one sheep, as an operator override.
    /// See [`Actor::handle_set_sheep_env`].
    SetSheepEnv {
        /// The sheep's name, not a selector, for [`Self::Scale`]'s reason.
        name: String,
        /// The env key.
        key: String,
        /// The value, or `None` to remove the key.
        ///
        /// [`EnvValue`], not a bare `String`: this enum derives `Debug` and
        /// an env value is the most secret-dense thing that reaches it
        /// (IR-41). The wire type carries the same protection for the same
        /// reason.
        value: Option<EnvValue>,
        /// Answers the config now parked for the sheep's next spawn, which
        /// `rpc.rs` hands to the registry, or `None` when no sheep has that
        /// name. An error only when the request is refused or the override
        /// store itself could not be read or written.
        reply: oneshot::Sender<Result<Option<ResolvedApp>, SupervisorError>>,
    },
    /// Several env keys on one sheep, applied as one write. See
    /// [`Actor::handle_set_sheep_env_batch`].
    SetSheepEnvBatch {
        /// The sheep's name, not a selector, for [`Self::Scale`]'s reason.
        name: String,
        /// The keys and their values.
        ///
        /// [`EnvValue`], not bare strings, for [`Self::SetSheepEnv`]'s
        /// reason: this enum derives `Debug` and a map of env values is the
        /// most secret-dense thing that reaches it (IR-41).
        entries: BTreeMap<String, EnvValue>,
        /// Overwrite colliding keys instead of refusing the batch.
        force: bool,
        /// Compute the three lists and write nothing.
        dry_run: bool,
        /// Answers what was written and what collided, or `None` when no
        /// sheep has that name. An error only when the request is refused
        /// or the override store could not be read or written.
        reply: oneshot::Sender<Result<Option<EnvBatch>, SupervisorError>>,
    },
    /// See [`Actor::handle_set_sheep_field`].
    SetSheepField {
        /// The sheep's name, not a selector, for [`Self::Scale`]'s reason.
        name: String,
        /// The `AppConfig` field.
        key: String,
        /// The new value, in the shape that field serializes as.
        value: serde_json::Value,
        /// Answers what the write did: the config now on the stored spec or
        /// parked, and whether the running child still lacks it. `None`
        /// when no sheep has that name. An error only when the request is
        /// refused or the override store itself could not be read or
        /// written.
        reply: oneshot::Sender<Result<Option<FieldSet>, SupervisorError>>,
    },
    /// Attaches a marker to one sheep by name, or clears it.
    ///
    /// Last writer wins: `Some` overwrites whatever is there, including
    /// another connection's mark. A `None` takes effect only when the stored
    /// [`ConnId`] matches.
    SetSmit {
        /// The connection painting it: the scope the mark lives in.
        conn: ConnId,
        /// The sheep's name, exactly as its config spells it. Not a selector,
        /// for [`Self::Scale`]'s reason.
        sheep: String,
        /// The marker, or `None` to clear this connection's own.
        smit: Option<Smit>,
        /// Answers with the named sheep's instances, or
        /// [`SupervisorError::NotFound`] when no sheep holds that name.
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Forgets every smit `conn` painted, leaving every other connection's
    /// alone.
    ///
    /// Sent from the server layer's per-connection tail, and that is the whole
    /// of a smit's cleanup: every way a mark can end also ends a socket.
    ForgetSmits {
        /// The connection that has ended.
        conn: ConnId,
        /// Answers once the marks are gone rather than merely queued.
        reply: oneshot::Sender<()>,
    },
    /// Full flock listing, name-grouped (see [`Actor::snapshot_all`]).
    List {
        /// Answers with the current snapshot.
        reply: oneshot::Sender<Vec<ProcessInfo>>,
    },
    /// Reopens the log files of every sheep matching `selector`.
    Reopen {
        /// Which sheep.
        selector: ProcessSelector,
        /// Answers once every matched pump has acknowledged, off a task of
        /// its own (see [`Actor::handle_reopen`]).
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Empties the log files of every sheep matching `selector`: flushes
    /// every pump writing to one of those paths, then truncates them.
    Flush {
        /// Which sheep.
        selector: ProcessSelector,
        /// Answers once every path has been truncated, off a task of its own
        /// (see [`Actor::handle_flush`]).
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    },
    /// Describes the whole flock for a daemon handover: what the fitness gate
    /// needs, and the blob the successor reads.
    #[cfg(unix)]
    HandoverSnapshot {
        /// The daemon's own two descriptors, which only `boot` knows.
        fds: DaemonFds,
        /// Answers once every live pump has flushed and reported, off a task
        /// of its own (see [`Actor::handle_handover_snapshot`]).
        reply: oneshot::Sender<Result<Snapshot, SupervisorError>>,
    },
    /// Answers whether this flock could be handed to a successor in place.
    ///
    /// The same gate [`Command::HandoverSnapshot`]'s caller applies, without
    /// the descriptor round trip: a question must not flush a pump.
    #[cfg(unix)]
    HandoverFitness {
        /// Answers on the actor loop: the gate awaits nothing.
        reply: oneshot::Sender<Result<Fitness, SupervisorError>>,
    },
    /// Puts one named action on the shepherd channel of every sheep matching
    /// `selector` and answers with what each app said back, or with why
    /// nothing came.
    ///
    /// One row per matched sheep: an answer is one reply body from one
    /// process.
    Trigger {
        /// Which sheep.
        selector: ProcessSelector,
        /// The action name, passed to the app verbatim.
        action: String,
        /// Argument text for the action, passed to the app verbatim.
        params: Option<String>,
        /// Answers once every matched sheep has answered, timed out or been
        /// refused, off a task of its own (see [`Actor::begin_action`]).
        reply: oneshot::Sender<Result<Vec<ActionReply>, SupervisorError>>,
    },
    /// Delivers one signal to the own process of every sheep matching
    /// `selector`, never its process group (see [`Actor::begin_signal`]).
    Signal {
        /// Which sheep.
        selector: ProcessSelector,
        /// The signal to deliver.
        sig: OperatorSignal,
        /// Answers once every matched sheep has been signalled or found not
        /// running, off a task of its own (see [`Actor::begin_signal`]).
        reply: oneshot::Sender<Result<Vec<SignalReply>, SupervisorError>>,
    },
    /// Writes one line to every matched sheep's stdin.
    SendLine {
        /// Which sheep.
        selector: ProcessSelector,
        /// The line, without its terminator: the writer appends exactly one
        /// `\n`.
        line: String,
        /// Answers once every matched write has settled or timed out, off a
        /// task of its own (see [`Actor::begin_send_line`]).
        reply: oneshot::Sender<Result<Vec<LineReply>, SupervisorError>>,
    },
    /// Graceful engine shutdown: kill ladder on every online sheep, then stop.
    Shutdown {
        /// Answers once every online sheep is terminal.
        reply: oneshot::Sender<()>,
    },
}

/// The actor's mailbox message: [`Command`]s plus events the actor generates
/// for itself (sheep-task exits, restart timers, readiness signals).
#[derive(Debug)]
pub(crate) enum Msg {
    /// A caller-issued command.
    Command(Command),
    /// A sheep task's proc resolved: natural exit or a completed kill.
    Exited {
        /// The sheep's id.
        id: u32,
        /// How it ended.
        outcome: ExitOutcome,
    },
    /// A scheduled restart's backoff has elapsed.
    RestartDue {
        /// The sheep's id.
        id: u32,
        /// The sheep's `SheepSlot::epoch` at scheduling time. A timer whose
        /// epoch has moved on was left behind by a respawn, and is dropped.
        epoch: u64,
    },
    /// The sheep's shepherd channel reported readiness.
    ///
    /// Forwarded to `SheepSlot::ready_tx` if a task is waiting, dropped
    /// silently otherwise: an app may report ready whenever it likes.
    Ready {
        /// The sheep's id.
        id: u32,
    },
    /// One swap of a reload ran out of time.
    ///
    /// The only way out of a [`ReloadJob`] the actor raises for itself: every
    /// other one waits on a task it cannot make report.
    ReloadDeadline {
        /// The app whose reload this was armed for.
        name: String,
        /// Which arming this is, off [`Actor::next_deadline`]. A message not
        /// carrying the job's current [`ReloadJob::deadline`] is dropped.
        stamp: u64,
    },
    /// A replacement answered its own probe, or failed to, with the instance
    /// it replaced already gone.
    ///
    /// The result of the second readiness wait [`Actor::post_drain_probe`]
    /// asks for, the first one being answerable by the wrong process (see
    /// [`ReloadMode`]).
    ReloadVerified {
        /// The app whose reload this is the last step of.
        name: String,
        /// The replacement that was re-probed; a stale result is dropped.
        new_id: u32,
        /// Whether the replacement answered inside the deadline.
        readiness: Readiness,
    },
    /// The sheep's shepherd channel carried a reply to an action.
    ///
    /// Routed to the waiting action task if one is waiting, dropped silently
    /// otherwise. Without a `stamp` the only correlation is the action name;
    /// [`ActionWaits::answer`] decides which wait the reply belongs to.
    ActionReply {
        /// The sheep's id.
        id: u32,
        /// The action the app is answering.
        action: String,
        /// The reply body, exactly as the app sent it.
        body: String,
        /// The dispatch stamp the app echoed, if it echoed one.
        stamp: Option<u64>,
    },
    /// An action wait resolved.
    ActionResult {
        /// The sheep's id.
        id: u32,
        /// Which wait on that sheep this is the answer to; see
        /// [`PendingAction::stamp`].
        stamp: u64,
        /// The app's reply, or why none arrived.
        outcome: ActionOutcome,
    },
    /// A readiness wait resolved.
    ReadyResult {
        /// The sheep's id.
        id: u32,
        /// The slot's epoch when the wait began; a stale result is dropped.
        epoch: u64,
        /// The `manually` flag this spawn's `Online` would have carried had
        /// it not been gated. Rides with `epoch` rather than on the slot, so
        /// the two cannot drift apart.
        manually: bool,
        /// Whether the signal arrived or the deadline elapsed.
        readiness: Readiness,
    },
}

/// Where a deferred `Stop`/`Restart`/`Delete`/`Shutdown` reply eventually
/// goes: the three commands differ only in their reply's payload shape.
#[derive(Debug)]
pub(super) enum ReplyKind {
    /// `Stop`/`Restart`: reply with the matched sheep's terminal snapshots.
    Info(oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>),
    /// `Delete`: reply with the matched (and now deregistered) ids.
    Ids(oneshot::Sender<Result<Vec<u32>, SupervisorError>>),
    /// `Shutdown`: reply once every online sheep is terminal.
    Shutdown(oneshot::Sender<()>),
}
