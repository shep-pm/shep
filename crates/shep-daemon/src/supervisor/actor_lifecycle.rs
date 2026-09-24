//! Restarting a sheep, and arming the command that asked for it.
//!
//! `respawn` is the path back to running after any exit, whether the brain
//! decided it or an operator did. The `begin_manual` half records that a
//! stop or restart is in flight against a set of ids, so the exits it causes
//! are read as that command finishing rather than as fresh crashes.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// The config `id`'s next child will be spawned from: what a load parked
    /// for it, or its stored spec when nothing is parked.
    ///
    /// The one reader of [`ProcessEntry::pending`] that does not consume it. A
    /// reload decides readiness source, ordering, identity and the assembled
    /// config off this one accessor: deciding one of those from `spec` and
    /// another from `pending` puts two probe-gated instances on one address.
    pub(super) fn intended_spec(&self, id: u32) -> Option<&ResolvedApp> {
        let slot = self.sheep.get(&id)?;
        Some(slot.entry.pending.as_ref().unwrap_or(&slot.entry.spec))
    }

    /// The identity `id`'s next child runs under, resolving it if the config
    /// it is about to be spawned from asks for a different one.
    ///
    /// Reads and never writes, so a caller that may not go through with the
    /// spawn does not leave the entry claiming an identity it never used.
    /// [`Self::credentials_for_spawn`] is the writing twin.
    ///
    /// # Errors
    ///
    /// - Whatever [`privilege::resolve`] refused the intended config's
    ///   `user`/`group`.
    pub(super) fn intended_credentials(
        &self,
        id: u32,
    ) -> Result<Option<Credentials>, PrivilegeError> {
        // `expect` rather than an `Ok(None)` fallback: `None` here means "the
        // app asked for nobody", so a missing slot would spawn as the
        // shepherd.
        let slot = self
            .sheep
            .get(&id)
            .expect("intended_credentials: the instance was found replaceable a moment ago");
        match slot.entry.credentials {
            SpawnIdentity::Resolved(credentials) if !slot.entry.pending_reidentifies => {
                Ok(credentials)
            }
            SpawnIdentity::Resolved(_) | SpawnIdentity::Unresolved => privilege::resolve(
                self.intended_spec(id)
                    .expect("intended_credentials: the slot was read a moment ago")
                    .config(),
            ),
        }
    }

    /// Moves a sheep's pending config onto its stored spec, if it has any.
    ///
    /// Called where a child is about to be replaced under the same entry, the
    /// only moment an [`ApplyGroup::NeedsRespawn`] field can take effect
    /// without a new id. A reload's replacement is a different entry, so it
    /// reads through [`Self::intended_spec`] and leaves the drainee's parked
    /// copy alone. Neither path re-reads a file.
    pub(super) fn promote_pending(&mut self, id: u32) {
        let Some(slot) = self.sheep.get_mut(&id) else {
            return;
        };
        let Some(pending) = slot.entry.pending.take() else {
            return;
        };

        // The one exception to `ProcessEntry::credentials` being resolved once:
        // an operator who edited `user` or `group` asked for exactly that. The
        // decision is read here, never made here, since the spec may have been
        // rewritten by a later load from a sibling that already promoted.
        if core::mem::take(&mut slot.entry.pending_reidentifies) {
            slot.entry.credentials = SpawnIdentity::Unresolved;
        }
        slot.entry.spec = pending;
    }

    /// Respawns an already-registered id in place: reassembles from its stored
    /// spec + instance, bumps `restarts` and resets timing on success, or marks
    /// the entry `Errored` on failure.
    ///
    /// `manually` is narrower than "forced": `true` only for an operator's
    /// `Restart`, `false` for the crash loop and every restart the daemon
    /// raised itself. Callers pass `origin == CommandOrigin::Operator`.
    pub(super) fn respawn(&mut self, id: u32, manually: bool) -> ProcessInfo {
        // Before the identity below is read, since a promoted `user` or `group`
        // is what clears it. Here rather than in the `Restart` arms: every door
        // a replacement child comes through passes through this one.
        self.promote_pending(id);
        // Reused as-is once resolved: a restart must never re-touch the passwd
        // database, nor silently change identity under a running app. An entry
        // never resolved resolves here, since coming up as the shepherd would
        // be an unreported privilege downgrade.
        let credentials = match self.credentials_for_spawn(id) {
            Ok(credentials) => credentials,
            Err(err) => return self.respawn_failed(id, manually, &err),
        };
        let slot = self.sheep.get(&id).expect("respawn: unknown id");
        let app = slot.entry.spec.clone();
        let instance = slot.entry.instance;
        // Computed ahead of the mutable borrow below.
        let next_epoch = slot.epoch + 1;
        let spec = match assemble(
            &app,
            instance,
            &self.paths,
            credentials,
            &self.secret_view(&app),
        ) {
            Ok(spec) => spec,
            Err(err) => return self.refuse_spawn(id, manually, &err),
        };
        let source = ReadinessSource::of(app.config())
            .expect("ResolvedApp already passed ProbeTarget::parse in normalize");
        let gated = !matches!(source, ReadinessSource::Heuristic);
        // Read before the move below: a gated app's readiness task wants the
        // same timeout the spawn just used, and `app` is what hands it over.
        let listen_timeout = app.config().listen_timeout.as_duration();

        match self.runner.spawn(&spec) {
            Ok((proc, io)) => {
                let pid = proc.pid();
                let log_ctl = io.log_ctl.clone();
                let to_child = io.to_child.clone();
                let to_stdin = io.to_stdin.clone();
                let handles = spawn_sheep_task::<R::Proc>(
                    id,
                    proc,
                    io,
                    // Moved, not cloned: the entry's spec was cloned once
                    // above, and a crash loop paid a second full config copy
                    // here on every respawn.
                    app,
                    self.events.clone(),
                    self.tx.clone(),
                );
                let ready_tx = if gated {
                    Some(spawn_readiness_task(
                        id,
                        next_epoch,
                        // Carried, not defaulted: a gated app must report who
                        // caused the respawn as the ungated arm below does.
                        manually,
                        source,
                        listen_timeout,
                        spec_prober(&spec),
                        self.tx.clone(),
                    ))
                } else {
                    None
                };
                let slot = self
                    .sheep
                    .get_mut(&id)
                    .expect("respawn: entry vanished mid-respawn");
                slot.entry.status = if gated {
                    ProcStatus::Starting
                } else {
                    ProcStatus::Online
                };
                slot.entry.pid = Some(pid);
                slot.entry.started_at = Some(tokio::time::Instant::now());
                slot.entry.restarts += 1;
                // `out_file`/`err_file` are `ApplyGroup::NeedsRespawn`: this
                // respawn is when they take effect. `to_info` reads these
                // fields, not `spec`, so a moved path reaches an operator
                // only once it moves here too.
                slot.entry.out_file = spec.out_file;
                slot.entry.err_file = spec.err_file;
                // A different process under the same id, so an earlier reload's
                // verdict about the last one does not apply to it.
                slot.ready_failed = false;
                slot.ctl = Some(handles.ctl);
                slot.log_ctl = Some(log_ctl);
                slot.to_child = Some(to_child);
                slot.signals = Some(handles.signals);
                slot.to_stdin = Some(to_stdin);
                // A new process under this id makes any RestartDue timer or
                // readiness task scheduled earlier stale. An old readiness task
                // finds its sender gone, rides out its deadline, and its
                // `ReadyResult` is dropped by `handle_ready_result`'s epoch check.
                slot.epoch += 1;
                debug_assert_eq!(slot.epoch, next_epoch);
                slot.ready_tx = ready_tx;
                let info = to_info(&slot.entry, &self.smits);
                self.emit(ProcessEventKind::Restart, info.clone(), manually);
                // A gated app goes `Online` later, from `handle_ready_result`.
                if !gated {
                    self.went_online(id, info.clone(), manually);
                }
                info
            }
            Err(error) => self.respawn_failed(id, manually, &error),
        }
    }

    /// Lands `id` in the terminal state a respawn that could not start
    /// reaches: `Errored`, every handle cleared, its lifecycle extras
    /// disarmed.
    ///
    /// `reason` is logged here rather than by the caller. The `Errored` event
    /// carries no reason and the deferred aggregation reply has no per-id error
    /// slot, so this log line is the only place an operator learns why a
    /// restart produced no process.
    pub(super) fn respawn_failed(
        &mut self,
        id: u32,
        manually: bool,
        reason: &dyn fmt::Display,
    ) -> ProcessInfo {
        tracing::warn!(id, %reason, "no process was started");
        let slot = self
            .sheep
            .get_mut(&id)
            .expect("respawn: entry vanished mid-respawn");
        slot.entry.status = ProcStatus::Errored;
        slot.entry.pid = None;
        slot.entry.started_at = None;
        // Cleared, because nothing exited here: leaving the previous process's
        // code would show a sheep that once crashed with 1 as still crashing
        // with 1 while it is failing to start at all.
        slot.entry.last_exit = None;
        slot.ctl = None;
        // Already `None` on every route into a respawn, written anyway so that
        // "these two go together" is visible at each site.
        slot.to_child = None;
        slot.signals = None;
        slot.to_stdin = None;
        slot.ready_tx = None;
        let info = to_info(&slot.entry, &self.smits);
        self.emit(ProcessEventKind::Errored, info.clone(), manually);
        // The same terminal status `Decision::Errored` reaches, and it needs
        // the same disarm: otherwise the name-group's cron worker and watch
        // stay live, and the enforcer stays armed against a dead pid.
        self.disarm_extras(id, &info.name);
        info
    }

    /// A spawn that never happened because a `{{secret:...}}` would not
    /// resolve, routed to the status its refusal deserves.
    ///
    /// The two shapes must not collapse into one. A namespace no provider
    /// dog has pushed to yet clears itself without anybody doing anything,
    /// so the sheep waits on the same budget a crash loop spends and errors
    /// when that runs out. A key nobody has set waits on a person instead,
    /// and a ladder in front of it would only postpone the report by
    /// sixteen turns.
    ///
    /// The slot must already exist: `spawn_fresh` registers before it calls
    /// this, so the sheep is visible whichever way the refusal goes.
    pub(super) fn refuse_spawn(
        &mut self,
        id: u32,
        manually: bool,
        err: &AssembleError,
    ) -> ProcessInfo {
        if !err.is_retriable() {
            return self.respawn_failed(id, manually, err);
        }
        let slot = self
            .sheep
            .get_mut(&id)
            .expect("refuse_spawn: the slot was registered a moment ago");
        slot.entry.budget.note_failed_start();
        // Read before the entry is written to, so the config borrow ends.
        let (max_restarts, delay) = {
            let config = slot.entry.spec.config();
            (
                config.max_restarts,
                restart_delay(config, slot.entry.budget.unstable_count()),
            )
        };
        if slot.entry.budget.exhausted(max_restarts) {
            return self.respawn_failed(id, manually, err);
        }
        tracing::warn!(id, %err, "spawn refused; waiting to try again");
        let epoch = slot.epoch;
        slot.entry.status = ProcStatus::WaitingRestart;
        slot.entry.pid = None;
        slot.entry.started_at = None;
        // Nothing exited here, so the previous process's code would read as
        // a crash this sheep is still repeating; `respawn_failed` clears it
        // for the same reason.
        slot.entry.last_exit = None;
        slot.ctl = None;
        slot.to_child = None;
        slot.signals = None;
        slot.to_stdin = None;
        slot.ready_tx = None;
        // The wall-clock half of the timer below, which is what a handover
        // successor re-arms from.
        slot.restart_due = SystemTime::now()
            .checked_add(delay.unwrap_or(Duration::ZERO))
            .filter(|due| due.duration_since(SystemTime::UNIX_EPOCH).is_ok());
        let info = to_info(&slot.entry, &self.smits);
        // The event every `WaitingRestart` transition is published as, so a
        // subscriber reads the status rather than inferring it from the kind.
        self.emit(ProcessEventKind::Exit, info.clone(), manually);
        self.schedule_restart(id, epoch, delay);
        info
    }

    /// Offers `manual` the `manual` marker on a running sheep, starting its
    /// kill ladder if nothing else already has.
    ///
    /// The first manual command to reach a running sheep owns its marker and
    /// its one live `Kill`. A later command racing the same in-flight kill
    /// rides the same eventual `Msg::Exited`, so a `stop()` caller is never
    /// handed back an `Online` `ProcessInfo`. One carve-out: an operator's
    /// command takes the marker off an in-flight automatic restart, which has
    /// nobody waiting behind it.
    ///
    /// `cap` decides how long the ladder this may start waits before `SIGKILL`,
    /// and is read only on the arm that sends a `Kill`.
    pub(super) fn claim_manual(&mut self, id: u32, manual: PendingManual, cap: LadderCap) {
        let Some(slot) = self.sheep.get_mut(&id) else {
            return;
        };
        match slot.manual.map(|in_flight| in_flight.origin) {
            // Nothing has claimed this sheep's next exit yet, so this command
            // owns it and starts the one kill ladder that produces it.
            None => {
                slot.manual = Some(manual);
                // `try_send`, never `.await`: the sheep task stops draining its
                // ctl mailbox once the ladder starts, so a blocking send could
                // park the actor for `kill_timeout`. `Full` means a `Kill` is
                // already queued; `Closed` means the sheep already exited.
                if let Some(ctl) = &slot.ctl {
                    let grace = cap.of(slot.entry.spec.config());
                    let _ = ctl.try_send(SheepCtl::Kill { grace });
                }
            }
            // The carve-out: take the marker, and leave the ladder the
            // automatic restart already started running.
            Some(CommandOrigin::Automatic) if manual.origin == CommandOrigin::Operator => {
                slot.manual = Some(manual);
            }
            // Already claimed by an operator, or by an automatic restart this
            // command cannot displace: ride that one's outcome. Both variants
            // named so a third origin has to be ruled on here.
            Some(CommandOrigin::Operator | CommandOrigin::Automatic) => {}
        }
    }

    /// Every registered id `selector` names, in id order.
    ///
    /// The one place selection happens. A dog is included only for a selector
    /// that named it ([`ProcessSelector::is_exact`]), so `stop all`, `reload
    /// all`, `delete all` and a `/regex/` sweep pass every dog by while `shep
    /// restart bark` still reaches one.
    pub(super) fn matching_ids(&self, selector: &ProcessSelector) -> Vec<u32> {
        let exact = selector.is_exact();
        let mut ids: Vec<u32> = self
            .sheep
            .iter()
            .filter(|(_, slot)| exact || slot.entry.dog.is_none())
            .filter_map(|(id, slot)| {
                let config = slot.entry.spec.config();
                selector
                    .matches(
                        &config.name,
                        *id,
                        config.fold.as_deref(),
                        Some(slot.entry.instance),
                    )
                    .then_some(*id)
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Resolves `selector`, then either defers to each matched sheep's next
    /// exit (if running) or applies the command immediately (if not).
    ///
    /// Immediate results are collected up front and folded into the same
    /// `PendingReply` as the deferred ids, so a mixed selector's reply carries
    /// every match and fires once the last running one goes terminal.
    pub(super) fn begin_manual(
        &mut self,
        selector: ProcessSelector,
        kind: ManualKind,
        origin: CommandOrigin,
        reply: ReplyKind,
    ) {
        let matched = self.matching_ids(&selector);

        if matched.is_empty() {
            send_reply(reply, Err(SupervisorError::NotFound));
            return;
        }

        self.begin_manual_ids(matched, kind, origin, reply);
    }

    /// [`Self::begin_manual`]'s per-id aggregation, taking the matched ids
    /// directly rather than resolving a [`ProcessSelector`] into them.
    ///
    /// The seam [`Self::handle_scale`]'s scale-down needs: the ids it is
    /// deregistering are the highest instance slots of one already-resolved
    /// app, not a fresh selector match.
    pub(super) fn begin_manual_ids(
        &mut self,
        matched: Vec<u32>,
        kind: ManualKind,
        origin: CommandOrigin,
        reply: ReplyKind,
    ) {
        let mut remaining = HashSet::new();
        let mut results = Vec::new();

        for id in matched {
            // An automatic restart is held off both halves of a swap that has
            // not committed: killing either half turns the deploy into the hard
            // restart the overlap exists to avoid. This drops the restart, it
            // does not defer it, and it stops at the commit.
            let held_off_by_a_swap =
                origin == CommandOrigin::Automatic && self.in_an_uncommitted_swap(id);
            if held_off_by_a_swap {
                // A dropped save for a watched app has nothing else an operator
                // can read.
                tracing::debug!(
                    id,
                    ?kind,
                    "automatic command dropped: this sheep is half of a swap that has not \
                     committed"
                );
                continue;
            }
            let is_running = self.sheep.get(&id).is_some_and(|slot| slot.ctl.is_some());
            if is_running {
                // Whoever ends up owning the marker, this id joins `remaining`
                // and this command is answered off the same `Msg::Exited`.
                self.claim_manual(id, PendingManual { kind, origin }, LadderCap::Stop);
                if kind == ManualKind::Delete {
                    // Whichever command's `manual` marker won, this id must
                    // still be deregistered once it goes terminal.
                    if let Some(slot) = self.sheep.get_mut(&id) {
                        slot.pending_delete = true;
                    }
                }
                remaining.insert(id);
            } else if let Some(info) = self.apply_immediate(id, kind, origin) {
                results.push(info);
            }
        }

        if remaining.is_empty() {
            sort_flock(&mut results);
            send_reply(reply, Ok(results));
            return;
        }

        self.pending.push(PendingReply {
            remaining,
            results,
            reply,
        });
    }

    /// Applies a manual command synchronously to a matched sheep that has no
    /// live task right now (already `Stopped`/`Errored`/`WaitingRestart`).
    ///
    /// `handle_exited`'s branches cannot cover these: a sheep waiting out its
    /// restart backoff still holds every extra its last `Online` armed, and its
    /// exit already happened, so stopping or deleting it here is the moment it
    /// goes terminal. Without it, `shep stop web` during a backoff leaves the
    /// group's watcher and cron worker armed.
    ///
    /// `origin` feeds the `manually` flag on the events below: a cron
    /// occurrence landing on a mid-backoff name restarts it from here.
    pub(super) fn apply_immediate(
        &mut self,
        id: u32,
        kind: ManualKind,
        origin: CommandOrigin,
    ) -> Option<ProcessInfo> {
        let manually = origin == CommandOrigin::Operator;
        match kind {
            ManualKind::Stop => {
                let slot = self.sheep.get_mut(&id)?;
                match slot.entry.status {
                    // `WaitingRestart`: cancels the pending restart, since
                    // `handle_restart_due` only respawns an id still in
                    // `WaitingRestart` on a still-current epoch. `Errored`:
                    // `stop` lands it in `Stopped` rather than a no-op.
                    ProcStatus::WaitingRestart | ProcStatus::Errored => {
                        slot.entry.status = ProcStatus::Stopped;
                        let info = to_info(&slot.entry, &self.smits);
                        self.emit(ProcessEventKind::Stop, info.clone(), manually);
                        self.disarm_extras(id, &info.name);
                        Some(info)
                    }
                    _ => Some(to_info(&slot.entry, &self.smits)),
                }
            }
            ManualKind::Delete => {
                let slot = self.sheep.remove(&id)?;
                let info = to_info(&slot.entry, &self.smits);
                self.emit(ProcessEventKind::Delete, info.clone(), manually);
                self.disarm_extras(id, &info.name);
                Some(info)
            }
            ManualKind::Restart => {
                self.sheep.get_mut(&id)?.entry.budget.reset();
                Some(self.respawn(id, manually))
            }
        }
    }

    /// Every registered id of `name`, in instance order.
    ///
    /// Not [`Self::matching_ids`] over a name selector: this is a lookup of one
    /// app's own slots, where a selector's folds, wildcards and slot suffixes
    /// would be a second way to answer the same question.
    pub(super) fn ids_of_name(&self, name: &str) -> Vec<u32> {
        let mut slots: Vec<(u32, u32)> = self
            .sheep
            .iter()
            .filter(|(_, slot)| slot.entry.spec.config().name == name)
            .map(|(id, slot)| (slot.entry.instance, *id))
            .collect();
        slots.sort_unstable();
        slots.into_iter().map(|(_, id)| id).collect()
    }
}
