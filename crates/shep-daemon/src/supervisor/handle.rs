//! The caller's door into the actor.
//!
//! [`SupervisorHandle`] is the only way to reach a running supervisor: each
//! method packs a [`Command`], sends it down the mailbox, and awaits the
//! reply on a fresh oneshot. Cloning a handle is cheap and every clone talks
//! to the same actor.

use super::*;

/// Handle to a running supervisor actor.
///
/// Cloning shares the same actor; every clone's commands are serialized
/// through its single mailbox.
#[derive(Debug, Clone)]
pub struct SupervisorHandle {
    pub(super) tx: mpsc::Sender<Msg>,
}

impl SupervisorHandle {
    /// Registers + spawns each app's instances.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::CannotStart`]: at least one app provably could
    ///   not run, so nothing in this batch was registered. Carries one
    ///   `"<name>: <reason>"` per refused app, never one per failed check.
    /// - [`SupervisorError::SpawnFailed`]: the first instance that failed to
    ///   spawn (already-registered instances persist regardless).
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub async fn start(&self, apps: Vec<ResolvedApp>) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.start_staged(apps, BTreeSet::new(), BatchPolicy::AllOrNothing)
            .await
    }

    /// [`Self::start`], holding every app in `gate` at `Starting` until its
    /// readiness deadline, so a later stage can wait on it.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::CannotStart`]: only reachable under
    ///   [`BatchPolicy::AllOrNothing`] (`Self::start`'s policy); nothing in
    ///   this batch was registered. Carries one `"<name>: <reason>"` per
    ///   refused app, never one per failed check. A staged start sends one
    ///   batch per stage, so an earlier stage of it can be running;
    ///   `boot_order::start_in_stages` is what says so.
    /// - [`SupervisorError::SpawnFailed`]: an instance that failed to spawn.
    ///   Under `AllOrNothing` this is the first such failure, and every
    ///   already-registered app in the batch persists regardless. Under
    ///   [`BatchPolicy::PerApp`] (the muster restore's policy) every app
    ///   was attempted and this carries one `"<name>: <reason>"` entry per
    ///   app that could not start, joined by `"; "`; each such app is
    ///   registered `Errored` and visible.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn start_staged(
        &self,
        apps: Vec<ResolvedApp>,
        gate: BTreeSet<String>,
        policy: BatchPolicy,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Start {
                apps,
                policy,
                gate,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Registers each app as a flock member without starting it.
    ///
    /// Idempotent by name: an app already known is returned as it stands, so
    /// restoring a roll over a live flock disturbs nothing.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn register_at_rest(
        &self,
        apps: Vec<ResolvedApp>,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::RegisterAtRest { apps, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Names the fields in which each app differs from the flock's own copy of
    /// the sheep of the same name.
    ///
    /// Reads the flock and changes nothing. [`Self::start`] on a name the
    /// flock already has adds instances rather than reconciling config.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn config_drift(
        &self,
        apps: Vec<ResolvedApp>,
    ) -> Result<Vec<SheepDrift>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::ConfigDrift { apps, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Merges each app a Flockfile declared into the sheep of the same name.
    ///
    /// Additive by default: a load appends what nobody has established and
    /// leaves everything else alone unless `reset` says otherwise. Nothing is
    /// registered, nothing is pruned, and nothing running is killed; a field
    /// the running child holds parks in [`ProcessEntry::pending`] for its next
    /// spawn. See [`Actor::handle_apply_config`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone. Every per-app
    ///   refusal rides inside the reply instead, in [`Applied::refused`].
    pub(crate) async fn apply_config(
        &self,
        apps: Vec<DeclaredApp>,
        reset: ResetDepth,
    ) -> Result<Vec<Applied>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::ApplyConfig { apps, reset, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// One sheep's effective config, for a pane about to edit it, or `None`
    /// when no sheep has that name.
    ///
    /// Read-only. `env` comes back emptied with its keys listed beside it,
    /// which [`SheepConfigView::new`] is what enforces. See
    /// [`Actor::handle_sheep_config`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone. An unknown
    ///   name is `Ok(None)` rather than an error, so the caller can word the
    ///   refusal itself.
    pub(crate) async fn sheep_config(
        &self,
        name: String,
    ) -> Result<Option<SheepConfigView>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::SheepConfig { name, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Sets `key` on `name`'s env, or removes it with `None`, recorded as an
    /// operator override. Answers `Ok(None)` when no sheep has that name.
    ///
    /// The `Some` carries the config now parked for that sheep's next spawn.
    /// `rpc.rs` hands it to [`crate::snapshot::FlockRegistry::record`], the
    /// way the `Scale` and `ApplyConfig` arms hand it theirs: the muster roll
    /// is written from the registry and nothing on the restore path reads the
    /// override store, so an edit that skipped this survives a
    /// `shep daemon reload` (the handover blob carries `pending`) and is lost
    /// by a cold restart.
    ///
    /// The running child holds the env it was spawned from, so the change
    /// parks for that sheep's next spawn rather than reaching it now, and
    /// `shep reload`/`shep restart` promote it. See
    /// [`Actor::handle_set_sheep_env`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's, and a dog's
    ///   config is not an operator's to edit through a pane.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::Overrides`] - the override store could not be
    ///   read or written, so nothing was recorded and nothing parked.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_sheep_env(
        &self,
        name: String,
        key: String,
        value: Option<EnvValue>,
    ) -> Result<Option<ResolvedApp>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::SetSheepEnv {
                name,
                key,
                value,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Sets several env keys on one sheep as operator overrides, in one
    /// write.
    ///
    /// [`Actor::handle_set_sheep_env_batch`] states what `force` and
    /// `dry_run` do and when the batch is refused whole. The `Some` carries
    /// [`EnvBatch`], whose `app` is the config now parked for that sheep's
    /// next spawn and is `None` whenever nothing was written; `rpc.rs` hands
    /// it to the registry for [`Self::set_sheep_env`]'s reason.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's, and a dog's
    ///   config is not an operator's to edit through a pane.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::Overrides`] - the override store could not be
    ///   read or written, so nothing was recorded and nothing parked.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_sheep_env_batch(
        &self,
        name: String,
        entries: BTreeMap<String, String>,
        force: bool,
        dry_run: bool,
    ) -> Result<Option<EnvBatch>, SupervisorError> {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (key, EnvValue::from(value)))
            .collect();
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::SetSheepEnvBatch {
                name,
                entries,
                force,
                dry_run,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Sets one config field on one sheep as an operator override.
    ///
    /// [`Actor::handle_set_sheep_field`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's, and a dog's
    ///   config is not an operator's to edit through a pane.
    /// - [`SupervisorError::InvalidField`] - the key, the value, or the
    ///   resulting config is one this build will not take. Nothing was
    ///   written.
    /// - [`SupervisorError::Overrides`] - the override store could not be
    ///   read or written, so nothing was recorded and nothing parked.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_sheep_field(
        &self,
        name: String,
        key: String,
        value: serde_json::Value,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::SetSheepField {
                name,
                key,
                value,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Registers and starts one dog, marked as coming from `source`.
    ///
    /// Idempotent by name: a dog already registered under `app`'s name is
    /// reported as it stands rather than started twice.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: shutdown has begun, or the
    ///   actor is gone.
    /// - [`SupervisorError::SpawnFailed`]: the binary could not be spawned.
    pub async fn start_dog(
        &self,
        app: ResolvedApp,
        source: DogSource,
    ) -> Result<ProcessInfo, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::StartDog {
                app: Box::new(app),
                source,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Stops every sheep matching `selector`.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn stop(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Stop { selector, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Restarts every sheep matching `selector`, resetting its restart budget.
    ///
    /// Declares `CommandOrigin::Operator`: nothing may take the sheep off it
    /// mid-kill-ladder, and the events it emits carry `manually: true`. A
    /// restart the daemon raised itself goes through
    /// [`Self::restart_automatic`] or [`Self::extra_restart`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn restart(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.restart_with_origin(selector, CommandOrigin::Operator)
            .await
    }

    /// Restarts every sheep matching `selector` on the daemon's own initiative,
    /// a cron occurrence or a change under a watched tree, resetting its
    /// restart budget exactly as [`Self::restart`] does.
    ///
    /// Declares `CommandOrigin::Automatic`: an operator's `stop` or `delete`
    /// landing mid-kill-ladder takes the sheep off it, and the events it emits
    /// carry `manually: false`.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn restart_automatic(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.restart_with_origin(selector, CommandOrigin::Automatic)
            .await
    }

    /// The body both restart methods share; they differ only in the origin
    /// they declare.
    async fn restart_with_origin(
        &self,
        selector: ProcessSelector,
        origin: CommandOrigin,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Restart {
                selector,
                origin,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Restarts `id` on behalf of a memory breach or a liveness failure, if the
    /// process that produced the report is still the process running now.
    ///
    /// Silently does nothing when the report is stale, and there is no reply.
    /// Resets the restart budget and goes in as `CommandOrigin::Automatic`, so
    /// it is displaceable by an operator's command.
    pub(crate) async fn extra_restart(
        &self,
        id: u32,
        pid: u32,
        epoch: Option<u64>,
        observed: Option<MemSize>,
    ) {
        let _ = self
            .tx
            .send(Msg::Command(Command::ExtraRestart {
                id,
                pid,
                epoch,
                observed,
            }))
            .await;
    }

    /// Replaces every sheep matching `selector` with a fresh instance of the
    /// same app, one instance at a time. An overlap, not zero downtime: the
    /// old listener's backlog is lost unless the app drains inside
    /// `graceful_timeout`. Answers on acceptance, and re-reads no config.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::ReloadInFlight`]: an app the selector reached is
    ///   mid-reload; carries its name.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone, or a shutdown
    ///   forbids the spawn a reload needs.
    pub(crate) async fn reload(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Reload { selector, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Stops + deregisters every sheep matching `selector`.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn delete(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<u32>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Delete { selector, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Sets `name`'s instance count. A partial scale-up answers `Ok` with
    /// [`Scaled::shortfall`] set and the achieved count on [`Scaled::app`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: no app of that name is registered.
    /// - [`SupervisorError::InvalidScale`]: a count of `0`, or a dog.
    /// - [`SupervisorError::CannotStart`]: a scale-up whose app has a
    ///   `user`/`group` that will not resolve. Nothing spawned or removed, and
    ///   only the growing arm resolves anything.
    /// - [`SupervisorError::ReloadInFlight`]: the app is mid-reload.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn scale(&self, name: &str, count: u32) -> Result<Scaled, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Scale {
                name: name.to_string(),
                count,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Attaches `smit` to the sheep called `sheep`, scoped to `conn`, or
    /// clears this connection's own mark with `None`.
    ///
    /// A clear from a connection that did not paint the mark is a no-op that
    /// still answers `Ok`; see [`Command::SetSmit`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: no sheep of that name is registered.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn set_smit(
        &self,
        conn: ConnId,
        sheep: &str,
        smit: Option<Smit>,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::SetSmit {
                conn,
                sheep: sheep.to_string(),
                smit,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Forgets every smit `conn` painted.
    ///
    /// A stopped engine has dropped every smit it held, so this answers even
    /// then; the caller is a connection tail with no use for a failure.
    pub(crate) async fn forget_smits(&self, conn: ConnId) {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Msg::Command(Command::ForgetSmits { conn, reply }))
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    /// Reopens the log files of every sheep matching `selector`, and of every
    /// other sheep writing to one of their paths, for an external rotator.
    /// When this returns no live pump still holds a renamed inode, which is
    /// what a logrotate `postrotate` stanza needs. The reply names only the
    /// sheep the selector reached.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::ReopenFailed`]: at least one pump could not open a
    ///   log path again; the old handles are closed either way.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn reopen(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Reopen { selector, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Describes the whole flock for a daemon handover: one
    /// [`OwnedCandidate`] per registered sheep for the fitness gate, and the
    /// [`Handover`] blob the successor reads.
    ///
    /// `fds` are the daemon's own listener and pidfile descriptors, which
    /// `boot` opened and the actor has never seen. Answers once every live
    /// pump has flushed and reported its descriptors; a registered sheep that
    /// is not running has no pump and no descriptors, which is not a refusal.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    #[cfg(unix)]
    pub(crate) async fn handover_snapshot(
        &self,
        fds: DaemonFds,
    ) -> Result<Snapshot, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::HandoverSnapshot { fds, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Whether this shepherd's flock could be handed to a successor in
    /// place, or the reason it could not.
    ///
    /// Read-only: the trigger for a handover is a signal, and this is the
    /// question a client asks before sending one.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    #[cfg(unix)]
    pub(crate) async fn handover_fitness(&self) -> Result<Fitness, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::HandoverFitness { reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Empties the log files of every sheep matching `selector`: flushes every
    /// pump writing to one of the paths those sheep were registered with, then
    /// truncates those paths. The operation addresses paths, so a matched
    /// sheep that is not running has no pump and its files are truncated all
    /// the same.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::FlushFailed`]: at least one matched file could not
    ///   be flushed or truncated. Every other matched path was emptied.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn flush(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Flush { selector, reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Sends `action` over the shepherd channel of every sheep matching
    /// `selector`, answering with one row per match, by name then by id.
    ///
    /// Answers on completion, bounded per sheep by its own
    /// `AppConfig::action_timeout`; the waits run alongside each other, so a
    /// flock costs the longest of them. A sheep that cannot be reached is
    /// refused in its own row, and `action` and `params` reach it verbatim.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn trigger(
        &self,
        selector: ProcessSelector,
        action: String,
        params: Option<String>,
    ) -> Result<Vec<ActionReply>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Trigger {
                selector,
                action,
                params,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Delivers `sig` to the own process of every sheep matching `selector`,
    /// never its process group, and answers with one row per match, by name
    /// then by id (only partial parity with
    /// `shep_core::protocol::sort_flock`; see `spawn_trigger_task`).
    ///
    /// A `kill(2)` either returns or does not, so this answers as soon as
    /// every matched sheep's delivery has settled.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn signal(
        &self,
        selector: ProcessSelector,
        sig: OperatorSignal,
    ) -> Result<Vec<SignalReply>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Signal {
                selector,
                sig,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Writes `line` to every matched sheep's stdin, and answers with one row
    /// per match, by name then by id (only partial parity with
    /// `shep_core::protocol::sort_flock`; see `spawn_trigger_task`).
    ///
    /// A pipe write blocks until the app reads, so the reply is bounded per
    /// sheep at [`STDIN_WRITE_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn send_line(
        &self,
        selector: ProcessSelector,
        line: String,
    ) -> Result<Vec<LineReply>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::SendLine {
                selector,
                line,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }

    /// Full flock listing, name-grouped (see [`Actor::snapshot_all`]).
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn list_checked(&self) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::List { reply }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)
    }

    /// Full flock listing, by name, then instance slot, then id
    /// (`shep_core::protocol::sort_flock`, which `snapshot_all` calls).
    ///
    /// For callers that need not tell "actor gone" from "empty flock".
    ///
    /// # Panics
    ///
    /// Panics if the actor has shut down.
    #[must_use]
    pub async fn list(&self) -> Vec<ProcessInfo> {
        self.list_checked()
            .await
            .expect("supervisor actor is no longer running")
    }

    /// Graceful engine shutdown: kill ladder on every online sheep, then
    /// stop the actor. A no-op if the actor is already gone.
    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Msg::Command(Command::Shutdown { reply }))
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }
}
