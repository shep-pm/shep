//! The caller's door into the actor.
//!
//! [`SupervisorHandle`] is the only way to reach a running supervisor: each
//! method hands [`SupervisorHandle::call`] a [`Command`] to pack, and that
//! one call is where a command meets the mailbox and its reply. Cloning a
//! handle is cheap and every clone talks to the same actor.

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
    /// Packs the command `build` makes from the reply channel, sends it, and
    /// awaits that reply.
    ///
    /// Every method here is this one with a different [`Command`] variant, so
    /// what a dead actor costs each caller is decided once.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor's mailbox closed
    ///   before the command landed, or the reply channel was dropped before
    ///   it answered. A caller cannot tell the two apart, and nothing here
    ///   can: both mean no answer is coming.
    async fn call<R>(
        &self,
        build: impl FnOnce(oneshot::Sender<R>) -> Command,
    ) -> Result<R, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(build(reply)))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)
    }

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
    /// - [`SupervisorError::CannotStart`]: only reachable under
    ///   [`BatchPolicy::AllOrNothing`]; nothing in this batch was registered.
    ///   Carries one `"<name>: <reason>"` per refused app, never one per
    ///   failed check.
    /// - [`SupervisorError::SpawnFailed`]: an instance that failed to spawn.
    ///   Under [`BatchPolicy::PerApp`] every app was attempted and this carries
    ///   one `"<name>: <reason>"` entry per app that could not start, joined
    ///   by `"; "`; each such app is registered `Errored` and visible.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn start_staged(
        &self,
        apps: Vec<ResolvedApp>,
        gate: BTreeSet<String>,
        policy: BatchPolicy,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.call(|reply| Command::Start {
            apps,
            policy,
            gate,
            reply,
        })
        .await?
    }

    /// Registers each app as a flock member without starting it.
    ///
    /// Idempotent by name: an app already known is returned as it stands, so
    /// restoring a roll over a live flock disturbs nothing.
    ///
    /// `disabled` names the apps of `apps` to mark disabled as they register,
    /// so nothing can spawn them in between. A name not in `apps` is ignored.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn register_at_rest(
        &self,
        apps: Vec<ResolvedApp>,
        disabled: BTreeSet<String>,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.call(|reply| Command::RegisterAtRest {
            apps,
            disabled,
            reply,
        })
        .await?
    }

    /// Registers each dog as a flock member without starting it.
    ///
    /// Idempotent by name, as [`Self::register_at_rest`] is: a dog or sheep
    /// already registered under the name is left as it stands. `disabled`
    /// names the dogs of `dogs` to mark disabled as they register, and a name
    /// registered already keeps whatever mark it has.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn register_dogs_at_rest(
        &self,
        dogs: Vec<(ResolvedApp, DogSource)>,
        disabled: BTreeSet<String>,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.call(|reply| Command::RegisterDogsAtRest {
            dogs,
            disabled,
            reply,
        })
        .await?
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
        self.call(|reply| Command::ConfigDrift { apps, reply })
            .await?
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
        self.call(|reply| Command::ApplyConfig { apps, reset, reply })
            .await?
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
        name: &str,
    ) -> Result<Option<SheepConfigView>, SupervisorError> {
        self.call(|reply| Command::SheepConfig {
            name: name.to_string(),
            reply,
        })
        .await?
    }

    /// Sets `key` on `name`'s env, or removes it with `None`, recorded as an
    /// operator override. Answers `Ok(None)` when no sheep has that name, and
    /// `Some` with the config now parked for that sheep's next spawn.
    ///
    /// `rpc/dispatch.rs` hands that config to
    /// [`crate::snapshot::FlockRegistry::record`]: the muster roll is written
    /// from the registry, so an edit that skipped it is lost by a cold restart.
    ///
    /// # Errors
    /// - [`SupervisorError::IsADog`] - the name is a dog's.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_sheep_env(
        &self,
        name: &str,
        key: &str,
        value: Option<EnvValue>,
    ) -> Result<Option<ResolvedApp>, SupervisorError> {
        self.call(|reply| Command::SetSheepEnv {
            name: name.to_string(),
            key: key.to_string(),
            value,
            reply,
        })
        .await?
    }

    /// Sets several env keys on one sheep as operator overrides, in one write.
    ///
    /// [`Actor::handle_set_sheep_env_batch`] states what `force` and `dry_run`
    /// do and when the batch is refused whole. `EnvBatch::app` is the config
    /// now parked for the next spawn, and is `None` whenever nothing was
    /// written; `rpc/dispatch.rs` records it for [`Self::set_sheep_env`]'s reason.
    ///
    /// # Errors
    /// - [`SupervisorError::IsADog`] - the name is a dog's.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_sheep_env_batch(
        &self,
        name: &str,
        entries: BTreeMap<String, String>,
        force: bool,
        dry_run: bool,
    ) -> Result<Option<EnvBatch>, SupervisorError> {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (key, EnvValue::from(value)))
            .collect();
        self.call(|reply| Command::SetSheepEnvBatch {
            name: name.to_string(),
            entries,
            force,
            dry_run,
            reply,
        })
        .await?
    }

    /// Copies `from`'s env onto `to` as operator overrides, in one write.
    ///
    /// [`Actor::handle_copy_sheep_env`] states what `replace`, `except` and
    /// `dry_run` do, and when [`EnvCopy::app`] is `None`; `rpc/dispatch.rs`
    /// records what it is for [`Self::set_sheep_env`]'s reason.
    ///
    /// # Errors
    /// - [`SupervisorError::NoSuchSheep`] - `from` or `to` is no sheep.
    /// - [`SupervisorError::IsADog`] - `from` or `to` is a dog's.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::EnvChanged`] - `expect` is not the current list.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn copy_sheep_env(
        &self,
        from: &str,
        to: &str,
        replace: bool,
        except: Vec<String>,
        dry_run: bool,
        expect: Option<Vec<shep_core::protocol::EnvCopyChange>>,
    ) -> Result<EnvCopy, SupervisorError> {
        self.call(|reply| Command::CopySheepEnv {
            from: from.to_string(),
            to: to.to_string(),
            replace,
            except,
            dry_run,
            expect,
            reply,
        })
        .await?
    }

    /// Sets one config field on one sheep as an operator override.
    ///
    /// [`Actor::handle_set_sheep_field`].
    ///
    /// # Errors
    /// - [`SupervisorError::IsADog`] - the name is a dog's, and a dog's config
    ///   is not an operator's to edit through a pane.
    /// - [`SupervisorError::InvalidField`] - the key, the value, or the
    ///   resulting config is one this build will not take. Nothing was written.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_sheep_field(
        &self,
        name: &str,
        key: &str,
        value: serde_json::Value,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        self.call(|reply| Command::SetSheepField {
            name: name.to_string(),
            key: key.to_string(),
            value,
            reply,
        })
        .await?
    }

    /// Drops the operator's override of one config field on one sheep.
    ///
    /// [`Actor::handle_reset_sheep_field`].
    ///
    /// # Errors
    ///
    /// As [`Self::set_sheep_field`], and [`SupervisorError::InvalidField`]
    /// also when the operator holds no override of the key.
    pub(crate) async fn reset_sheep_field(
        &self,
        name: &str,
        key: &str,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        self.call(|reply| Command::ResetSheepField {
            name: name.to_string(),
            key: key.to_string(),
            reply,
        })
        .await?
    }

    /// `dog`'s table on every sheep carrying one, by sheep name. See
    /// [`Actor::handle_dog_sheep_settings`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn dog_sheep_settings(
        &self,
        dog: String,
    ) -> Result<BTreeMap<String, DogTable>, SupervisorError> {
        self.call(|reply| Command::DogSheepSettings { dog, reply })
            .await
    }

    /// `name`'s table for `dog` and the environment its references resolve
    /// in. `Ok(None)` when no sheep has that name. See
    /// [`Actor::handle_sheep_dog_read`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn sheep_dog_read(
        &self,
        name: String,
        dog: String,
    ) -> Result<Option<SheepDogRead>, SupervisorError> {
        self.call(|reply| Command::SheepDogRead { name, dog, reply })
            .await
    }

    /// Sets `dog`'s table on `name`, or removes it with `None`, as an operator
    /// override of the whole `dogs` field. `Ok(None)` when no sheep has that
    /// name. See [`Actor::handle_set_sheep_dog_settings`].
    ///
    /// The dispatch arm records [`FieldSet::app`] to the muster roll, for
    /// [`Self::set_sheep_env`]'s reason.
    ///
    /// # Errors
    /// - [`SupervisorError::IsADog`] - the name is a dog's.
    /// - [`SupervisorError::InvalidField`] - `normalize` refuses the result,
    ///   as it does an empty dog name.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_sheep_dog_settings(
        &self,
        name: String,
        dog: String,
        table: Option<DogTable>,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        self.call(|reply| Command::SetSheepDogSettings {
            name,
            dog,
            table,
            reply,
        })
        .await?
    }

    /// `dog`'s default table as stored. See [`Actor::handle_dog_defaults`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn dog_defaults(
        &self,
        dog: String,
    ) -> Result<Option<DogTable>, SupervisorError> {
        self.call(|reply| Command::DogDefaults { dog, reply }).await
    }

    /// Sets `dog`'s default table, or removes it with `None`. See
    /// [`Actor::handle_set_dog_defaults`].
    ///
    /// # Errors
    /// - [`SupervisorError::InvalidField`] - the dog's name is empty.
    /// - [`SupervisorError::DogDefaults`] - the store could not be read or written.
    /// - [`SupervisorError::EngineStopped`] - the actor is gone.
    pub(crate) async fn set_dog_defaults(
        &self,
        dog: String,
        table: Option<DogTable>,
    ) -> Result<(), SupervisorError> {
        self.call(|reply| Command::SetDogDefaults { dog, table, reply })
            .await?
    }

    /// Registers and starts one dog, marked as coming from `source`.
    ///
    /// Idempotent by name: a dog already running under `app`'s name is
    /// reported as it stands rather than started twice. A dog registered at
    /// rest (`Stopped`) is not running, so it is started in its own entry
    /// from `app` and `source`, replacing the ones it was registered with,
    /// and any disabled mark on it is cleared.
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
        self.call(|reply| Command::StartDog {
            app: Box::new(app),
            source,
            reply,
        })
        .await?
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
        self.call(|reply| Command::Stop { selector, reply }).await?
    }

    /// Disables the app of every sheep matching `selector` and stops it.
    ///
    /// The mark belongs to the app, so a selector that matched one instance
    /// disables all of them. Every returned row carries
    /// [`ProcessInfo::disabled`] as `Some(true)`.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn disable(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let mut infos = self
            .call(|reply| Command::Disable { selector, reply })
            .await??;
        for info in &mut infos {
            info.disabled = Some(true);
        }
        Ok(infos)
    }

    /// Stops the dog registered as `name` and marks it disabled, leaving it
    /// registered at rest.
    ///
    /// Answers with the dog's id. A dog that is already stopped is marked and
    /// answered the same way.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: no dog is registered under `name`.
    ///   A sheep holding the name is left alone.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn disable_dog(&self, name: String) -> Result<Vec<u32>, SupervisorError> {
        self.call(|reply| Command::DisableDog { name, reply })
            .await?
    }

    /// Clears the disabled mark on the app of every sheep matching `selector`
    /// and starts it.
    ///
    /// Answers with the instances it started, which is empty when every
    /// matched app was not disabled.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn enable(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.call(|reply| Command::Enable { selector, reply })
            .await?
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
        self.call(|reply| Command::Restart {
            selector,
            origin,
            reply,
        })
        .await?
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
        self.call(|reply| Command::Reload { selector, reply })
            .await?
    }

    /// Stops + deregisters every sheep matching `selector`.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NotFound`]: nothing matched.
    /// - [`SupervisorError::IsADog`]: the selector reached a built-in dog,
    ///   which no verb removes; the message names `shep disable`.
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn delete(
        &self,
        selector: ProcessSelector,
    ) -> Result<Vec<u32>, SupervisorError> {
        self.call(|reply| Command::Delete { selector, reply })
            .await?
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
        self.call(|reply| Command::Scale {
            name: name.to_string(),
            count,
            reply,
        })
        .await?
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
        self.call(|reply| Command::SetSmit {
            conn,
            sheep: sheep.to_string(),
            smit,
            reply,
        })
        .await?
    }

    /// Forgets every smit `conn` painted.
    ///
    /// A stopped engine has dropped every smit it held, so this answers even
    /// then; the caller is a connection tail with no use for a failure.
    pub(crate) async fn forget_smits(&self, conn: ConnId) {
        let _ = self
            .call(|reply| Command::ForgetSmits { conn, reply })
            .await;
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
        self.call(|reply| Command::Reopen { selector, reply })
            .await?
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
        self.call(|reply| Command::HandoverSnapshot { fds, reply })
            .await?
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
        self.call(|reply| Command::HandoverFitness { reply })
            .await?
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
        self.call(|reply| Command::Flush { selector, reply })
            .await?
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
        self.call(|reply| Command::Trigger {
            selector,
            action,
            params,
            reply,
        })
        .await?
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
        self.call(|reply| Command::Signal {
            selector,
            sig,
            reply,
        })
        .await?
    }

    /// Tells sheep `id`, on its channel, that its lamb `pid` was sent `sig`,
    /// and answers whether the message was queued.
    ///
    /// `root` is the pid the lamb was found under. The message is queued only
    /// while the sheep still runs as that process, so a respawn in between
    /// does not hand the replacement news of its predecessor's lamb. Not
    /// queued is not an error: the sheep has no channel, is not reading it,
    /// or runs as another process now.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn tell_lamb_signalled(
        &self,
        id: u32,
        root: u32,
        pid: u32,
        sig: OperatorSignal,
    ) -> Result<bool, SupervisorError> {
        self.call(|reply| Command::LambSignalled {
            id,
            root,
            pid,
            sig,
            reply,
        })
        .await
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
        self.call(|reply| Command::SendLine {
            selector,
            line,
            reply,
        })
        .await?
    }

    /// Full flock listing, name-grouped (see [`Actor::snapshot_all`]).
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    pub(crate) async fn list_checked(&self) -> Result<Vec<ProcessInfo>, SupervisorError> {
        self.call(|reply| Command::List { reply }).await
    }

    /// Each sheep's configured working directory, by id. A sheep without one
    /// is absent.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::EngineStopped`]: the actor is gone.
    #[cfg(feature = "vcs")]
    pub(crate) async fn working_dirs(&self) -> Result<HashMap<u32, PathBuf>, SupervisorError> {
        self.call(|reply| Command::WorkingDirs { reply }).await
    }

    /// Full flock listing, by name, then instance slot, then id
    /// (`shep_core::protocol::sort_flock`, which `snapshot_all` calls).
    ///
    /// For callers that need not tell "actor gone" from "empty flock".
    ///
    /// # Panics
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
        let _ = self.call(|reply| Command::Shutdown { reply }).await;
    }
}
