//! Getting a process running, or taking one over.
//!
//! `spawn_fresh` assembles a spec, resolves credentials and secrets, and
//! hands the runner a process. `install_adopted` is the other door: a
//! survivor of a previous shepherd arrives already running, and has to be
//! fitted back into a slot without being restarted.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// The credentials `id`'s next spawn must apply, resolved once if this
    /// entry has never had them resolved.
    ///
    /// An entry resolved at its `Start` answers from the stored value, so a
    /// running app's identity cannot change underneath it. A failed resolution
    /// stores nothing, so a later restart asks again.
    ///
    /// # Errors
    /// - Whatever [`privilege::resolve`] refused this app's `user`/`group`
    ///   for, including a non-root daemon asked to change identity.
    pub(super) fn credentials_for_spawn(
        &mut self,
        id: u32,
    ) -> Result<Option<Credentials>, PrivilegeError> {
        let slot = self
            .sheep
            .get(&id)
            .expect("credentials_for_spawn: unknown id");
        match slot.entry.credentials {
            SpawnIdentity::Resolved(credentials) => Ok(credentials),
            SpawnIdentity::Unresolved => {
                let credentials = privilege::resolve(slot.entry.spec.config())?;
                self.sheep
                    .get_mut(&id)
                    .expect("credentials_for_spawn: the entry was read a moment ago")
                    .entry
                    .credentials = SpawnIdentity::Resolved(credentials);
                Ok(credentials)
            }
        }
    }

    /// Registers one app as a member of the flock without spawning anything.
    ///
    /// The flock is a membership list, not a list of live processes: `stop`
    /// leaves a sheep registered and `Stopped`, `delete` ends membership.
    ///
    /// One entry per app rather than one per configured instance, at
    /// `instance: 0`, the slot `start` fills first, so a later `restart` lands
    /// where it would have. Idempotent by name.
    pub(super) fn register_at_rest(&mut self, app: &ResolvedApp) -> ProcessInfo {
        self.register_without_spawning(app, ProcStatus::Stopped, None)
            .into_info()
    }

    /// The secret view one spawn of `app` resolves against.
    ///
    /// The store is read here rather than inside [`assemble`], the same way
    /// `credentials` is resolved by the caller: real I/O belongs to the
    /// caller so the assembler stays a pure function of its arguments. Read
    /// per spawn rather than cached, so a `shep secret set` between two
    /// spawns reaches the second without a daemon restart.
    ///
    /// A store that cannot be read yields an empty view rather than failing
    /// here, and logs why. A sheep that needs nothing from it still spawns,
    /// and one that does gets the ordinary refusal naming its own reference,
    /// which on its own would send an operator to `shep secret set` for a
    /// store that is corrupt or newer than this build.
    ///
    /// The namespaced half comes from [`ProviderSecrets`], in memory, so
    /// this reads the file for the operator's own store and nothing else.
    pub(super) fn secret_view(&self, app: &ResolvedApp) -> SecretView {
        let environment = app
            .config()
            .environment
            .clone()
            .unwrap_or_else(|| self.host_environment.clone());
        let store = shep_core::secrets::all(&self.paths.secrets).unwrap_or_else(|error| {
            tracing::warn!(
                path = %self.paths.secrets.display(),
                %error,
                "the secret store could not be read; every reference will refuse"
            );
            BTreeMap::new()
        });
        SecretView::new(environment, store, self.provider_secrets.snapshot())
    }

    /// The field names an operator has overridden for `name`, for a
    /// [`ProcessEntry`] about to be built from scratch.
    ///
    /// Checks a live sibling first and reads the override store only when none
    /// exists, so a scale-up costs no file access. An unreadable store answers
    /// empty rather than refusing: the worst case is a blank CFG cell.
    pub(super) fn overridden_for(&self, name: &str) -> Vec<String> {
        if let Some(slot) = self
            .sheep
            .values()
            .find(|slot| slot.entry.spec.config().name == name)
        {
            return slot.entry.overridden.clone();
        }
        overrides::get(&self.paths.overrides, name)
            .ok()
            .flatten()
            .map(|record| record.fields.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Registers one app as a flock member in `status`, spawning nothing.
    ///
    /// Registers the status it is handed: [`Self::register_at_rest`] passes
    /// `Stopped`, [`Self::do_start`] passes `Errored` when credential
    /// resolution fails under [`BatchPolicy::PerApp`].
    ///
    /// The entry's identity is [`SpawnIdentity::Unresolved`] whatever the
    /// status, so a later `restart` resolves it through
    /// [`Self::credentials_for_spawn`] instead of reusing a settled `None` and
    /// starting the sheep as the shepherd. Idempotent by name: an app already
    /// known is left as it is, and [`Registration`] says which happened.
    pub(super) fn register_without_spawning(
        &mut self,
        app: &ResolvedApp,
        status: ProcStatus,
        dog: Option<DogSource>,
    ) -> Registration {
        let name = &app.config().name;
        if let Some(slot) = self
            .sheep
            .values()
            .find(|slot| &slot.entry.spec.config().name == name)
        {
            return Registration::AlreadyKnown(to_info(&slot.entry, &self.smits));
        }

        let overridden = self.overridden_for(name);
        let id = self.next_id;
        self.next_id += 1;
        // Assembled for its log paths only: nothing is spawned, but the entry
        // has to name the files a later `restart` will append to. `describe`,
        // so an unresolvable `{{secret:...}}` still registers: `shep add`
        // exists to land a template whose secrets nobody has filled in yet.
        let described = describe(app, 0, &self.paths, None, &self.secret_view(app));
        let entry = ProcessEntry {
            id,
            spec: app.clone(),
            pending: None,
            pending_reidentifies: false,
            overridden,
            instance: 0,
            status,
            pid: None,
            restarts: 0,
            started_at: None,
            budget: RestartBudget::default(),
            reload: ReloadState::None,
            // `respawn` resolves it, so a restored app comes up under its
            // configured `user`.
            credentials: SpawnIdentity::Unresolved,
            out_file: described.out_file,
            err_file: described.err_file,
            dog,
            last_exit: None,
        };
        let info = to_info(&entry, &self.smits);
        self.sheep.insert(id, SheepSlot::new(entry));
        Registration::Fresh(info)
    }

    /// Names the fields in which each app differs from the registered sheep
    /// of the same name, skipping every app that matches and every app the
    /// flock does not have.
    ///
    /// Several instances of one app share one config, so the first slot found
    /// under a name answers for all of them.
    pub(super) fn config_drift(&self, apps: &[ResolvedApp]) -> Vec<SheepDrift> {
        apps.iter()
            .filter_map(|app| {
                let incoming = app.config();
                let stored = self
                    .sheep
                    .values()
                    .find(|slot| slot.entry.spec.config().name == incoming.name)?
                    .entry
                    .spec
                    .config();
                let fields = stored.drifted_fields(incoming);
                (!fields.is_empty()).then(|| SheepDrift::new(&incoming.name, fields))
            })
            .collect()
    }

    /// Registers + spawns one brand-new instance (a fresh id, `restarts: 0`).
    ///
    /// Always inserts a [`SheepSlot`] before returning: on success `Starting`
    /// with a readiness task armed when the app configures `wait_ready` or
    /// `readiness_probe`, `Starting` with a readiness task armed on its
    /// `listen_timeout` fallback when the app's name is in `gate` even though
    /// it configures no signal of its own, and `Online` otherwise. `Errored`
    /// with no task on failure. `dog` lands on the entry either way, so a dog
    /// whose binary cannot be spawned still shows up in the dogs table.
    pub(super) fn spawn_fresh(
        &mut self,
        app: &ResolvedApp,
        instance: u32,
        credentials: Option<Credentials>,
        dog: Option<DogSource>,
        gate: &BTreeSet<String>,
    ) -> Result<ProcessInfo, String> {
        // Read before the spawn: a scale-up's new instance must show the same
        // overrides its siblings do, not a blank cell until the next load.
        let overridden = self.overridden_for(&app.config().name);
        let secrets = self.secret_view(app);
        let id = self.next_id;
        self.next_id += 1;
        let spec = match assemble(app, instance, &self.paths, credentials, &secrets) {
            Ok(spec) => spec,
            Err(err) => {
                // Registered before the refusal is routed: a sheep that never
                // spawned still has to be visible, and `refuse_spawn` reads
                // the slot it lands in. Its log paths come from `describe`,
                // since `assemble` is what just refused. The `Errored` here
                // is never published: `refuse_spawn` decides the status this
                // sheep is first seen in, and emits that one.
                let described = describe(app, instance, &self.paths, credentials, &secrets);
                let entry = ProcessEntry {
                    id,
                    spec: app.clone(),
                    pending: None,
                    pending_reidentifies: false,
                    overridden,
                    instance,
                    status: ProcStatus::Errored,
                    pid: None,
                    restarts: 0,
                    started_at: None,
                    budget: RestartBudget::default(),
                    reload: ReloadState::None,
                    credentials: SpawnIdentity::Resolved(credentials),
                    out_file: described.out_file,
                    err_file: described.err_file,
                    dog,
                    last_exit: None,
                };
                self.sheep.insert(id, SheepSlot::new(entry));
                let info = self.refuse_spawn(id, true, &err);
                // A retriable refusal is not a failed start: the sheep is
                // registered, waiting, and comes up on its own once the
                // namespace does, so the batch above must not tear down for
                // it. Only a key nobody has set is reported as a failure.
                return if err.is_retriable() {
                    Ok(info)
                } else {
                    Err(err.to_string())
                };
            }
        };

        // Cloned off the spec, which is the only place that knows whether the
        // app set an explicit `out_file`/`err_file` or takes the `merge_logs`
        // default. Both arms below register an entry.
        let out_file = spec.out_file.clone();
        let err_file = spec.err_file.clone();

        // A `ResolvedApp` has already been through `ProbeTarget::parse` in
        // `normalize`, so an `Err` here means an app skipped that step.
        let source = ReadinessSource::of(app.config())
            .expect("ResolvedApp already passed ProbeTarget::parse in normalize");
        // An app a later stage waits on is gated even with no signal of its
        // own: the wait then costs its `listen_timeout`, which is the field's
        // documented fallback, rather than costing nothing.
        let gated =
            !matches!(source, ReadinessSource::Heuristic) || gate.contains(&app.config().name);

        match self.runner.spawn(&spec) {
            Ok((proc, io)) => {
                let pid = proc.pid();
                let status = if gated {
                    ProcStatus::Starting
                } else {
                    ProcStatus::Online
                };
                let entry = ProcessEntry {
                    id,
                    spec: app.clone(),
                    pending: None,
                    pending_reidentifies: false,
                    overridden,
                    instance,
                    status,
                    pid: Some(pid),
                    restarts: 0,
                    started_at: Some(tokio::time::Instant::now()),
                    budget: RestartBudget::default(),
                    reload: ReloadState::None,
                    credentials: SpawnIdentity::Resolved(credentials),
                    out_file,
                    err_file,
                    dog,
                    last_exit: None,
                };
                let info = to_info(&entry, &self.smits);
                let log_ctl = io.log_ctl.clone();
                let to_child = io.to_child.clone();
                let to_stdin = io.to_stdin.clone();
                let handles = spawn_sheep_task::<R::Proc>(
                    id,
                    proc,
                    io,
                    app.clone(),
                    self.events.clone(),
                    self.tx.clone(),
                );
                let ready_tx = if gated {
                    Some(spawn_readiness_task(
                        id,
                        0,
                        // A `Start` is always a caller's own doing, matching
                        // the `manually: true` the ungated arm below emits.
                        true,
                        source,
                        app.config().listen_timeout.as_duration(),
                        spec_prober(&spec),
                        self.tx.clone(),
                    ))
                } else {
                    None
                };
                self.sheep.insert(
                    id,
                    SheepSlot {
                        ctl: Some(handles.ctl),
                        log_ctl: Some(log_ctl),
                        to_child: Some(to_child),
                        signals: Some(handles.signals),
                        to_stdin: Some(to_stdin),
                        ready_tx,
                        ..SheepSlot::new(entry)
                    },
                );
                self.emit(ProcessEventKind::Start, info.clone(), true);
                // A gated app goes `Online` later, from `handle_ready_result`.
                // `Start` is the bus's first word on this sheep either way.
                if !gated {
                    self.went_online(id, info.clone(), true);
                }
                Ok(info)
            }
            Err(error) => {
                let entry = ProcessEntry {
                    id,
                    spec: app.clone(),
                    pending: None,
                    pending_reidentifies: false,
                    overridden,
                    instance,
                    status: ProcStatus::Errored,
                    pid: None,
                    restarts: 0,
                    started_at: None,
                    budget: RestartBudget::default(),
                    reload: ReloadState::None,
                    credentials: SpawnIdentity::Resolved(credentials),
                    out_file,
                    err_file,
                    dog,
                    last_exit: None,
                };
                let info = to_info(&entry, &self.smits);
                self.sheep.insert(id, SheepSlot::new(entry));
                self.emit(ProcessEventKind::Errored, info, true);
                // `error` names neither the app nor the path, and the caller
                // adds the name. `spec.program` and `spec.cwd` verbatim: they
                // are what the Flockfile said. A `Doubtful` verdict replaces
                // the clause, being its only channel to an operator.
                let attempted = match self.runner.preflight(&spec) {
                    Preflight::Doubtful(reason) => reason,
                    _ => match &spec.cwd {
                        Some(cwd) => format!("tried `{}` in {}", spec.program, cwd.display()),
                        None => format!("tried `{}`", spec.program),
                    },
                };
                Err(format!("{error}; {attempted}"))
            }
        }
    }

    /// Installs one sheep this image inherited rather than started.
    ///
    /// Nothing here spawns, signals or reopens anything, and nothing is
    /// emitted on the bus: the sheep never transitioned. `started_at` cannot
    /// cross the handover, since a `tokio::time::Instant` means nothing
    /// outside the runtime that read it, so it is re-derived via
    /// [`handover::uptime`](crate::handover::uptime).
    ///
    /// # Errors
    ///
    /// - [`AdoptError::Spec`] if the carried config does not normalize.
    /// - [`AdoptError::Runner`] if the runner refused the inherited handles.
    #[cfg(unix)]
    pub(super) fn install_adopted(
        &mut self,
        sheep: AdoptedSheep,
        reaper: &Arc<AdoptedReaper>,
    ) -> Result<(), AdoptError> {
        let AdoptedSheep {
            carried,
            out_pipe,
            err_pipe,
            out_log,
            err_log,
            stdin_pipe,
            channel,
        } = sheep;
        let app = normalize(carried.app().clone()).map_err(|source| AdoptError::Spec {
            sheep: carried.name().to_string(),
            source,
        })?;
        // Reused as resolved, never re-resolved: the value was pinned at the
        // predecessor's first spawn, so a passwd change cannot move a running
        // app's identity.
        let credentials = match carried.credentials() {
            SpawnIdentity::Resolved(credentials) => credentials,
            SpawnIdentity::Unresolved => None,
        };
        // Assembled for its log paths only: the entry has to name the files
        // this sheep is writing to, and a later rotation reopens them by path.
        // `describe`, so a store that moved under a running flock costs a log
        // path its rendering rather than costing the adoption a live sheep.
        let described = describe(
            &app,
            carried.instance(),
            &self.paths,
            credentials,
            &self.secret_view(&app),
        );
        let id = carried.id();
        let status = carried.status();
        // `None` for a blob written before this daemon carried a swap; see
        // `CarriedSheep::reload`.
        let reload = carried.reload().unwrap_or(ReloadState::None);
        // Read here rather than at either `SheepSlot` literal below: the
        // readiness re-arm between them has to see it, since a `ready_failed`
        // instance is `Starting` by construction and must not get a wait.
        let ready_failed = carried.ready_failed().unwrap_or(false);
        // Restored as a pair with its reset flag: a parked config promoted
        // without it comes up on the identity the flag exists to replace.
        // A parked config that fails to normalize is dropped and warned about
        // rather than refusing the adoption, which would strand a live flock.
        let pending = carried
            .pending()
            .and_then(|parked| match normalize(parked.clone()) {
                Ok(app) => Some(app),
                Err(source) => {
                    tracing::warn!(
                        sheep = carried.name(),
                        %source,
                        "a config this sheep was owed did not survive the handover: this daemon \
                         will not accept it, so the change is gone and the file must be loaded \
                         again"
                    );
                    None
                }
            });
        // `false` when the config was dropped just above, and for a blob
        // written before the flag existed: nothing left to promote.
        let pending_reidentifies =
            pending.is_some() && carried.pending_reidentifies().unwrap_or(false);
        let mut entry = ProcessEntry {
            id,
            spec: app.clone(),
            pending,
            pending_reidentifies,
            instance: carried.instance(),
            status,
            pid: carried.pid(),
            restarts: carried.restarts(),
            // Filled in below for a sheep with a process, and left `None` for
            // one without, which is what a stopped slot carries anyway.
            started_at: None,
            // The count is carried, since losing it would hand a crash-looping
            // app amnesty, but the window it is counted over is wall-clock this
            // image did not observe.
            budget: RestartBudget::default(),
            // Restored: it decides where this instance's next exit goes.
            // `handle_exited` routes a `Drainee` to `reap_drainee` and reads a
            // `Replacement` out of the swap, while `None` takes
            // `decide_on_exit`.
            reload,
            credentials: carried.credentials(),
            out_file: described.out_file.clone(),
            err_file: described.err_file.clone(),
            // Restored: it is the marker that keeps a dog out of the flock.
            // `matching_ids` passes a marked entry over for every selector but
            // an exact one, so dropping it would put the dog in `shep flock`.
            // `None` for a blob written before this daemon carried a dog.
            dog: carried.dog().cloned(),
            last_exit: carried.last_exit(),
            // Not carried by `CarriedSheep`: a handover installs one sheep at a
            // time with no live sibling to ask, so this is one store read.
            overridden: self.overridden_for(&app.config().name),
        };

        let Some(pid) = carried.pid() else {
            // Registered and not running: adopting an absent process would ask
            // the reaper to wait on a pid this image never had.
            self.sheep.insert(
                id,
                SheepSlot {
                    // A marker is only claimed against a sheep with a live
                    // task: there is no ladder here to re-arm and no
                    // `Msg::Exited` coming to clear one.
                    manual: None,
                    // Restored: it needs no task to act on it, and the exit
                    // that consumes it is this slot's next spawn's.
                    pending_delete: carried.pending_delete().unwrap_or(false),
                    epoch: carried.epoch(),
                    // Restored: it needs no task to act on it, and `respawn`
                    // clears it at the spawn that answers it.
                    ready_failed,
                    // Restored verbatim so a second reload during the same
                    // wait does not start the delay over: the re-arm below
                    // computes a fresh timer from this absolute moment.
                    restart_due: carried.restart_due(),
                    ..SheepSlot::new(entry)
                },
            );
            // Nothing but this raises `Msg::RestartDue`, so a carried
            // `WaitingRestart` sheep would sit there for the daemon's life.
            // The timer is re-armed off the carried deadline, so the successor
            // sleeps out what is left of `restart_delay`.
            if status == ProcStatus::WaitingRestart {
                let delay = crate::backoff::adopted_restart_delay(
                    app.config(),
                    carried.restart_due(),
                    SystemTime::now(),
                );
                self.schedule_restart(id, carried.epoch(), delay);
            }
            return Ok(());
        };

        let (proc, io) = self
            .runner
            .adopt(AdoptSpec {
                pid,
                out_file: described.out_file.clone(),
                err_file: described.err_file.clone(),
                out_pipe,
                err_pipe,
                out_log,
                err_log,
                stdin_pipe,
                channel,
                reaper: Arc::clone(reaper),
            })
            .map_err(|source| AdoptError::Runner {
                sheep: carried.name().to_string(),
                source,
            })?;
        // Off the proc rather than off the blob, as both spawn paths take it:
        // the pid this entry reports has to be the one the runner will signal.
        entry.pid = Some(proc.pid());
        // Load-bearing: `handle_exited` reads `started_at` to tell a real exit
        // from a duplicate `Msg::Exited`, and an entry without it would sit
        // `Online` forever after its process died. Derived from the operating
        // system, since a `tokio::time::Instant` cannot cross the exec.
        entry.started_at = Some(crate::handover::uptime::started_at_of(proc.pid()));
        // `manually` reaches only the `Online` event's flag: `false` for a
        // `Starting` sheep this image did not spawn, `true` for a carried
        // `Replacement`, which is an operator's reload.
        let manually = matches!(reload, ReloadState::Replacement);
        // A carried `Starting` sheep has readiness unresolved and nothing else
        // moves it off, so `listen_timeout` starts again. `ready_failed` is the
        // exception: its verdict already stands, and a fresh wait would clear
        // the flag a rollback needs.
        let ready_tx = (status == ProcStatus::Starting && !ready_failed).then(|| {
            let source = ReadinessSource::of(app.config())
                .expect("ResolvedApp already passed ProbeTarget::parse in normalize");
            spawn_readiness_task(
                id,
                carried.epoch(),
                manually,
                source,
                app.config().listen_timeout.as_duration(),
                spec_prober(&described),
                self.tx.clone(),
            )
        });
        let log_ctl = io.log_ctl.clone();
        let to_child = io.to_child.clone();
        let to_stdin = io.to_stdin.clone();
        let handles = spawn_sheep_task::<R::Proc>(
            id,
            proc,
            io,
            app.clone(),
            self.events.clone(),
            self.tx.clone(),
        );
        self.sheep.insert(
            id,
            SheepSlot {
                ctl: Some(handles.ctl),
                log_ctl: Some(log_ctl),
                to_child: Some(to_child),
                signals: Some(handles.signals),
                to_stdin: Some(to_stdin),
                // Re-claimed below rather than written in directly: restoring
                // the marker is only half of what a carried one means.
                manual: None,
                pending_delete: carried.pending_delete().unwrap_or(false),
                epoch: carried.epoch(),
                ready_tx,
                // Restored: `reload_eligible` reads it beside the status, so a
                // rollback reload can replace an instance that never reached
                // `Online`.
                ready_failed,
                // Restored verbatim, though always `None` in practice: a sheep
                // with a pid is not `WaitingRestart`.
                restart_due: carried.restart_due(),
                ..SheepSlot::new(entry)
            },
        );
        // The ladder that would kill a carried `manual` sheep went with the
        // predecessor's image, so it runs again from the polite rung. The cap
        // comes off the role: a carried drainee was asked under
        // `graceful_timeout`, the rest under `kill_timeout`.
        if let Some(manual) = carried.manual() {
            let cap = match reload {
                ReloadState::Drainee { .. } => LadderCap::Drain,
                ReloadState::None | ReloadState::Replacement => LadderCap::Stop,
            };
            self.claim_manual(id, manual, cap);
        }
        // A watch, a schedule or a memory limit this image did not arm is one
        // the app quietly stops having.
        if status == ProcStatus::Online {
            self.arm_extras(id);
        }
        Ok(())
    }

    /// Restores the reload jobs the blob carried, and re-arms every timer
    /// each of them was waiting on.
    ///
    /// Runs after the whole flock is installed, not per sheep: a job names two
    /// entries and reads an app's timings off one of them.
    ///
    /// [`Self::arm_reload_deadline`] is the only thing that removes a
    /// [`ReloadJob`] nothing else can finish, and its stamp comes off the
    /// carried `next_deadline`. A swap in [`ReloadPhase::Verify`] is re-armed
    /// through the same [`Self::post_drain_probe`] the original went through.
    /// A job whose swap names no registered entry is dropped: arming against a
    /// missing entry would panic.
    #[cfg(unix)]
    pub(super) fn install_carried_reloads(&mut self, reloads: Vec<CarriedReload>) {
        for carried in reloads {
            let CarriedReload {
                app,
                queue,
                mode,
                swap,
            } = carried;
            // The replacement first, as it is the half that exists in every
            // phase but `DrainFirst`. This is about which one is registered: a
            // serial reload deregisters its drainee at `ReapOld`.
            let anchor = swap
                .new_id
                .filter(|id| self.sheep.contains_key(id))
                .or_else(|| self.sheep.contains_key(&swap.old_id).then_some(swap.old_id));
            let Some(anchor) = anchor else {
                tracing::warn!(
                    name = app,
                    old_id = swap.old_id,
                    new_id = swap.new_id,
                    "a carried reload named no instance this shepherd was given, so it was \
                     dropped rather than left unfinishable"
                );
                continue;
            };
            self.reloads.insert(
                app.clone(),
                ReloadJob {
                    queue: queue.into_iter().collect(),
                    mode,
                    swap,
                    // Overwritten by the arm below, the one site that stamps a
                    // live watchdog.
                    deadline: 0,
                },
            );
            self.arm_reload_deadline(&app, anchor);
            if swap.phase == ReloadPhase::Verify
                && let Some(new_id) = swap.new_id
                && let Some(source) = self.post_drain_probe(new_id, mode)
            {
                self.spawn_verify_task(&app, new_id, source);
            }
        }
    }
}
