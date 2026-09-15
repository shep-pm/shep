//! Driving a reload forward.
//!
//! A reload walks one instance at a time: spawn the replacement, wait for it
//! to prove itself, drain the old one, commit the swap. `advance_reload` is
//! the step function that moves it along, and each of the others is one
//! stage of that walk.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Accepts a reload: answers the caller at once, then starts one swap per
    /// matched app.
    ///
    /// The answer is an acceptance, not a result. One instance costs
    /// `listen_timeout` + `graceful_timeout` against `crate::rpc`'s 60s ceiling
    /// on a request budget, so the reply is the matched sheep as they stood
    /// when the reload was accepted; the swaps report themselves on the bus.
    ///
    /// Only an `Online` instance is replaced, and everything else the selector
    /// matched is a no-op success: a `Starting` sheep is excluded too, since a
    /// second live process in its slot buys an overlap nobody can use yet.
    /// Nothing here re-reads configuration.
    pub(super) fn handle_reload(
        &mut self,
        selector: &ProcessSelector,
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    ) {
        let matched = self.matching_ids(selector);

        if matched.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }

        // Refused whole, before anything is spawned: a partly-accepted
        // selector is worse than a refused one. The rule holds per supervisor
        // call rather than per request now, since `rpc::reload_in_stages`
        // walks a multi-sheep selector one name at a time and so reaches this
        // once per app. That walk carries every refusal back on
        // `Response::Reloading`, so a fold reloaded around a busy app is
        // still named at the operator rather than being a missing row.
        let in_flight = matched.iter().find_map(|id| {
            let slot = self
                .sheep
                .get(id)
                .expect("handle_reload: `matched` holds ids read off this map a moment ago");
            let name = &slot.entry.spec.config().name;
            self.reloads.contains_key(name).then(|| name.clone())
        });
        if let Some(name) = in_flight {
            let _ = reply.send(Err(SupervisorError::ReloadInFlight(name)));
            return;
        }

        let mut accepted: Vec<ProcessInfo> = matched
            .iter()
            .map(|id| {
                let slot = self
                    .sheep
                    .get(id)
                    .expect("handle_reload: `matched` holds ids read off this map a moment ago");
                let mut info = to_info(&slot.entry, &self.smits);
                // Only where a swap is actually coming: the queue below is
                // built on the same predicate, so a row carrying a number
                // is a row this reload will try to replace. A number beside
                // an instance nothing will touch would have a reader
                // waiting out a deadline for a swap that was never queued.
                info.reload_deadline_ms = reload_eligible(slot).then(|| {
                    u64::try_from(swap_budget(slot.entry.spec.config()).as_millis())
                        .unwrap_or(u64::MAX)
                });
                info
            })
            .collect();
        // The table `shep reload` prints, so it takes the order every
        // operator-facing listing takes, not `matching_ids`' id order.
        sort_flock(&mut accepted);

        // Grouped by app, since a reload runs one instance of an app at a time,
        // and ordered by instance slot, since that is the order an operator
        // reads a clustered app in.
        let mut queues: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
        for id in matched {
            let slot = self
                .sheep
                .get(&id)
                .expect("handle_reload: `matched` holds ids read off this map a moment ago");
            if !reload_eligible(slot) {
                continue;
            }
            let entry = &slot.entry;
            queues
                .entry(entry.spec.config().name.clone())
                .or_default()
                .push((entry.instance, id));
        }

        let _ = reply.send(Ok(accepted));

        for (name, mut instances) in queues {
            instances.sort_unstable();
            let queue = instances.into_iter().map(|(_, id)| id).collect();
            self.advance_reload(&name, queue);
        }
    }

    /// Starts the next swap of `name`'s reload, or ends the reload when
    /// `queue` runs out.
    ///
    /// The one door into `SpawnNew`, so "one instance at a time" and, through
    /// [`Self::arm_reload_deadline`], "every swap is bounded" are properties of
    /// this function rather than rules spread over its callers.
    ///
    /// An instance that stopped being `Online`, or whose next exit something
    /// already claimed, is skipped: a swap against a claimed exit is doomed,
    /// landing inside `AwaitReady` carrying the marker. A replacement that
    /// cannot be spawned ends the reload, leaving the old instances running.
    /// [`ReloadMode::Serial`] reaches `SpawnNew` from [`Self::reap_drainee`].
    pub(super) fn advance_reload(&mut self, name: &str, mut queue: VecDeque<u32>) {
        // Defence in depth: a shutdown clears every job before anything can
        // reach here. A replacement spawned now would be a child outside the
        // shutdown aggregation.
        if self.shutting_down {
            self.reloads.remove(name);
            return;
        }
        while let Some(old_id) = queue.pop_front() {
            let slot = self.sheep.get(&old_id);
            let replaceable =
                slot.is_some_and(|slot| reload_eligible(slot) && slot.manual.is_none());
            if !replaceable {
                // A whole reload can end here having replaced nothing, long
                // after its caller was told `Ok`, so this is its only account.
                tracing::debug!(
                    name,
                    old_id,
                    status = ?slot.map(|slot| slot.entry.status),
                    "reload skipped an instance: it is gone, no longer online, or its next \
                     exit is already claimed"
                );
                continue;
            }
            if self.reload_mode_of(old_id) == ReloadMode::Serial {
                self.begin_serial_drain(name, old_id, queue);
                return;
            }
            match self.spawn_replacement(old_id, ReloadMode::Overlap) {
                Ok(new_id) => {
                    self.reloads.insert(
                        name.to_string(),
                        ReloadJob {
                            queue,
                            mode: ReloadMode::Overlap,
                            deadline: 0,
                            swap: ReloadSwap {
                                old_id,
                                new_id: Some(new_id),
                                phase: ReloadPhase::AwaitReady,
                            },
                        },
                    );
                    self.arm_reload_deadline(name, new_id);
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        name,
                        old_id,
                        error,
                        "reload abandoned: the replacement could not be spawned"
                    );
                    self.reloads.remove(name);
                    // The one end a reload reaches without `abort_reload`: no
                    // replacement was ever registered, and `spawn_replacement`
                    // has already put the drainee back to `Online`.
                    if let Some(slot) = self.sheep.get(&old_id) {
                        let info = to_info(&slot.entry, &self.smits);
                        self.emit(ProcessEventKind::ReloadAbandoned, info, true);
                    }
                    return;
                }
            }
        }
        self.reloads.remove(name);
    }

    /// The reload ordering the instance `old_id` occupies asks for.
    ///
    /// Read off [`Self::intended_spec`], the config the replacement will be
    /// spawned from, so the mode and the spawn cannot decide from two different
    /// files. Not off the stored spec: `readiness_probe` lands there at once
    /// while `wait_ready` parks, so a migrating app holds both, `wait_ready`
    /// wins in [`ReadinessSource::of`], and the mode would overlap against a
    /// probe the drainee can answer.
    pub(super) fn reload_mode_of(&self, old_id: u32) -> ReloadMode {
        let config = self
            .intended_spec(old_id)
            .expect("reload_mode_of: the instance was found replaceable a moment ago")
            .config();
        let source = ReadinessSource::of(config)
            .expect("ResolvedApp already passed ProbeTarget::parse in normalize");
        ReloadMode::of(config, &source)
    }

    /// `DrainOld`, run first: a serial reload asks the instance it is about to
    /// replace to go, and spawns nothing until it has.
    ///
    /// The mirror of the overlap arm of [`Self::advance_reload`], with the
    /// spawn moved to [`Self::reap_drainee`]. The drainee's `Reload` is emitted
    /// here, since a reload silent until its drain was over would look like one
    /// that never started. The watchdog is armed against the drainee's entry,
    /// there being no replacement yet: without one, an instance wedged past its
    /// own `SIGKILL` leaves a job nothing can remove.
    pub(super) fn begin_serial_drain(&mut self, name: &str, old_id: u32, queue: VecDeque<u32>) {
        let drainee = self
            .sheep
            .get_mut(&old_id)
            .expect("begin_serial_drain: the instance was found replaceable a moment ago");
        drainee.entry.status = ProcStatus::Stopping;
        // `None`: there is no replacement until this instance's exit is
        // handled. The marker routes that exit to `reap_drainee` rather than to
        // `decide_on_exit`, which would respawn the old code into the slot.
        drainee.entry.reload = ReloadState::Drainee { new_id: None };
        let info = to_info(&drainee.entry, &self.smits);

        self.reloads.insert(
            name.to_string(),
            ReloadJob {
                queue,
                mode: ReloadMode::Serial,
                deadline: 0,
                swap: ReloadSwap {
                    old_id,
                    new_id: None,
                    phase: ReloadPhase::DrainFirst,
                },
            },
        );
        self.arm_reload_deadline(name, old_id);
        self.emit(ProcessEventKind::Reload, info, true);
        self.claim_manual(
            old_id,
            PendingManual {
                kind: ManualKind::Stop,
                origin: CommandOrigin::Operator,
            },
            LadderCap::Drain,
        );
    }

    /// `SpawnNew`: spawns a replacement in `old_id`'s instance slot under a
    /// new id, and returns that id.
    ///
    /// Same slot, so `SHEP_INSTANCE`, templated env and log paths follow the
    /// drainee; new id, since two live processes per id breaks the property
    /// test; readiness gated even for `Heuristic`, or `DrainOld` would kill the
    /// drainee at once. `restarts` carries over, the budget does not.
    ///
    /// Marks the drainee `Stopping` before spawning, or `handle_extra_restart`
    /// would restart a drainee mid-`AwaitReady`. Under [`ReloadMode::Serial`]
    /// the drainee is already dead, so the `Reload` event and restore-on-failure
    /// are skipped, and this must run before `deregister_on_exit`.
    pub(super) fn spawn_replacement(&mut self, old_id: u32, mode: ReloadMode) -> Result<u32, String> {
        // `Credentials` is `Copy`; reused, and re-resolved only when the config
        // being promoted is what changed `user` or `group`. A drainee is
        // running, so the seam finds a resolved identity and touches nothing.
        let credentials = self
            .intended_credentials(old_id)
            .map_err(|err| err.to_string())?;
        // The promotion, and it is a read: the replacement is built from the
        // config the drainee was owed, and the drainee keeps its own copy until
        // it is deregistered, which is what makes an abandoned reload harmless.
        let app = self
            .intended_spec(old_id)
            .expect("spawn_replacement: the instance was found replaceable a moment ago")
            .clone();
        let drainee = &self.sheep[&old_id].entry;
        let instance = drainee.instance;
        let restarts = drainee.restarts;
        // Carried across the swap: nothing here could re-derive it, and a dog
        // has to stay a dog across a reload.
        let dog = drainee.dog.clone();
        // Carried across the swap: a reload is not an exit, so the answer to
        // "why did this instance last stop" is still the drainee's, and `None`
        // would read as "this instance has never exited".
        let last_exit = drainee.last_exit;
        // Carried across the swap: a reload is not a config load, and without
        // this a reload would blank the cache `shep describe` reads.
        let overridden = drainee.overridden.clone();

        let new_id = self.next_id;
        self.next_id += 1;

        // Before the drainee is marked `Stopping` below, and refusing here
        // abandons the reload rather than reaching for a restart ladder: the
        // drainee is still up and still serving, which beats either status
        // `refuse_spawn` could leave behind.
        let spec = assemble(
            &app,
            instance,
            &self.paths,
            credentials,
            &self.secret_view(&app),
        )
        .map_err(|err| err.to_string())?;
        let out_file = spec.out_file.clone();
        let err_file = spec.err_file.clone();
        let source = ReadinessSource::of(app.config())
            .expect("ResolvedApp already passed ProbeTarget::parse in normalize");

        let drainee = self
            .sheep
            .get_mut(&old_id)
            .expect("spawn_replacement: the drainee was read a moment ago");
        drainee.entry.status = ProcStatus::Stopping;
        drainee.entry.reload = ReloadState::Drainee {
            new_id: Some(new_id),
        };

        match self.runner.spawn(&spec) {
            Ok((proc, io)) => {
                let pid = proc.pid();
                let entry = ProcessEntry {
                    id: new_id,
                    spec: app.clone(),
                    pending: None,
                    pending_reidentifies: false,
                    overridden,
                    instance,
                    status: ProcStatus::Starting,
                    pid: Some(pid),
                    restarts,
                    started_at: Some(tokio::time::Instant::now()),
                    budget: RestartBudget::default(),
                    reload: ReloadState::Replacement,
                    credentials: SpawnIdentity::Resolved(credentials),
                    out_file,
                    err_file,
                    dog,
                    last_exit,
                };
                let info = to_info(&entry, &self.smits);
                let log_ctl = io.log_ctl.clone();
                let to_child = io.to_child.clone();
                let to_stdin = io.to_stdin.clone();
                let handles = spawn_sheep_task::<R::Proc>(
                    new_id,
                    proc,
                    io,
                    app.clone(),
                    self.events.clone(),
                    self.tx.clone(),
                );
                let ready_tx = spawn_readiness_task(
                    new_id,
                    0,
                    // A reload is an operator's doing, so the `Online` this
                    // wait defers reports itself as one.
                    true,
                    source,
                    app.config().listen_timeout.as_duration(),
                    spec_prober(&spec),
                    self.tx.clone(),
                );
                self.sheep.insert(
                    new_id,
                    SheepSlot {
                        ctl: Some(handles.ctl),
                        log_ctl: Some(log_ctl),
                        to_child: Some(to_child),
                        signals: Some(handles.signals),
                        to_stdin: Some(to_stdin),
                        ready_tx: Some(ready_tx),
                        ..SheepSlot::new(entry)
                    },
                );
                // The instance being replaced announces itself before its
                // replacement's `Start`: a reload's reply is an acceptance, so
                // a subscriber's whole account of the swap arrives here.
                if mode == ReloadMode::Overlap {
                    let drainee = to_info(&self.sheep[&old_id].entry, &self.smits);
                    self.emit(ProcessEventKind::Reload, drainee, true);
                }
                self.emit(ProcessEventKind::Start, info, true);
                Ok(new_id)
            }
            Err(error) => {
                // Nothing is registered for a replacement that never existed:
                // the drainee still owns this instance slot, and an `Errored`
                // row beside it would double every name-keyed verb. The id is
                // spent all the same, since ids are never reused.
                if mode == ReloadMode::Overlap {
                    let drainee = self
                        .sheep
                        .get_mut(&old_id)
                        .expect("spawn_replacement: the drainee was marked a moment ago");
                    drainee.entry.status = restored_status(drainee);
                    drainee.entry.reload = ReloadState::None;
                }
                Err(error.to_string())
            }
        }
    }

    /// `AwaitReady` resolved for a replacement: commit the swap, or abandon
    /// the reload.
    ///
    /// A reload is the one caller for which a readiness deadline elapsing is a
    /// failure: a replacement that cannot answer has not proved it can take
    /// over, so it is never marked `Online` on a `TimedOut`.
    ///
    /// Keyed on the [`Readiness`] verdict and never on the deadline, which is
    /// what makes it correct for all three sources: `await_ready`'s `Heuristic`
    /// arm reports `Ready`, since for a heuristic the elapse is the signal.
    pub(super) fn reload_ready_result(&mut self, new_id: u32, manually: bool, readiness: Readiness) {
        let Some(name) = self.reload_of(new_id) else {
            // Defensive: nothing leaves a `Replacement` marker behind without a
            // job naming it. Take the ordinary transition rather than strand
            // the sheep at `Starting`.
            tracing::warn!(
                id = new_id,
                "a replacement resolved with no reload to belong to"
            );
            self.clear_reload(new_id);
            let info = self.set_status(new_id, ProcStatus::Online);
            self.went_online(new_id, info, manually);
            return;
        };

        if readiness == Readiness::TimedOut {
            // Abandoning protects the instance that can still serve, so the
            // full abandonment, killing the replacement and putting the
            // drainee back, is available only while there is one.
            let old_id = self.reloads[&name].swap.old_id;
            if self.sheep.contains_key(&old_id) {
                self.abort_reload(&name, "the replacement was not ready inside listen_timeout");
                return;
            }
            // With nothing to fall back to, the reload ends here. The
            // replacement is left running and `Starting`, never `Online`: a
            // deploy tool reads `Online` as "the new release is serving". That
            // costs it the extras `went_online` arms.
            tracing::warn!(
                name,
                new_id,
                "reload abandoned: the replacement was not ready inside listen_timeout, with \
                 the instance it replaced already gone; it is left running and not online"
            );
            self.reloads.remove(&name);
            self.clear_reload(new_id);
            if let Some(slot) = self.sheep.get_mut(&new_id) {
                slot.ready_failed = true;
                let info = to_info(&slot.entry, &self.smits);
                self.emit(ProcessEventKind::ReloadAbandoned, info, true);
            }
            return;
        }

        let info = self.set_status(new_id, ProcStatus::Online);
        self.went_online(new_id, info, manually);
        self.begin_drain(&name);
    }

    /// `DrainOld`: the replacement is serving, so ask the instance it
    /// replaced to go.
    ///
    /// The ladder runs under `graceful_timeout` rather than `kill_timeout` (see
    /// [`LadderCap`]): this is the stop that expects the instance to stop
    /// accepting, finish what it already has, and exit.
    ///
    /// Marks the swap committed first, so a later abandonment leaves the
    /// replacement where it is instead of undoing a kill already in flight.
    pub(super) fn begin_drain(&mut self, name: &str) {
        let Some(job) = self.reloads.get_mut(name) else {
            return;
        };
        job.swap.phase = ReloadPhase::DrainOld;
        let old_id = job.swap.old_id;

        if !self.sheep.contains_key(&old_id) {
            // The drainee went on its own while the replacement was still
            // starting, so `ReapOld` already happened and there is nothing
            // left to drain.
            self.finish_swap(name);
            return;
        }
        self.claim_manual(
            old_id,
            PendingManual {
                kind: ManualKind::Stop,
                origin: CommandOrigin::Operator,
            },
            LadderCap::Drain,
        );
    }

    /// One instance replaced: the replacement stops being half of a pair, and
    /// the reload moves on to the next instance (or ends).
    ///
    /// The `Reloaded` this announces is the one event that says a swap
    /// succeeded, and it goes out before the next swap begins so a clustered
    /// app's reload reads in order. It is owed only to a replacement that is
    /// serving: one that went down inside the drain window keeps its row, and
    /// the event would name a process that is not there. Anything else ends
    /// the reload as an abandonment, the queue with it being the rest of a
    /// clustered app left on the old code. An overlapping reload of a probed
    /// app defers the success and asks again with the drainee gone, since its
    /// `Online` may rest on a probe the drainee answered.
    pub(super) fn finish_swap(&mut self, name: &str) {
        let Some(job) = self.reloads.remove(name) else {
            return;
        };
        let new_id = job
            .swap
            .new_id
            .expect("finish_swap: a swap with no replacement never reaches the drain");

        let serving = self
            .sheep
            .get(&new_id)
            .is_some_and(|slot| slot.entry.status == ProcStatus::Online);
        if serving && let Some(source) = self.post_drain_probe(new_id, job.mode) {
            // The job goes back rather than ending, and the replacement keeps
            // its `Replacement` marker: the swap is not over, so a second
            // reload is still refused and a replacement that dies inside the
            // window still reaches `handle_exited`'s reload arm.
            self.reloads.insert(
                name.to_string(),
                ReloadJob {
                    swap: ReloadSwap {
                        phase: ReloadPhase::Verify,
                        ..job.swap
                    },
                    ..job
                },
            );
            // Armed again: the running watchdog was sized for `AwaitReady` plus
            // the drain, and the probe below can take another `listen_timeout`
            // on top of both.
            self.arm_reload_deadline(name, new_id);
            self.spawn_verify_task(name, new_id, source);
            return;
        }
        self.clear_reload(new_id);

        if serving {
            let info = to_info(&self.sheep[&new_id].entry, &self.smits);
            self.emit(ProcessEventKind::Reloaded, info, true);
            self.advance_reload(name, job.queue);
            return;
        }

        tracing::warn!(
            name,
            new_id,
            "reload abandoned: the replacement was no longer serving when the instance it \
             replaced went"
        );
        if let Some(slot) = self.sheep.get(&new_id) {
            let info = to_info(&slot.entry, &self.smits);
            self.emit(ProcessEventKind::ReloadAbandoned, info, true);
        }
    }

    /// The readiness source a drained swap still has to prove, if it has one.
    ///
    /// `Some` only for an overlapping reload of an app whose readiness comes
    /// from a probe: the two instances shared a `SO_REUSEPORT` group, so the
    /// probe that put the replacement `Online` may have been answered by the
    /// instance now reaped.
    ///
    /// `None` everywhere else. A serial reload already asked with the slot
    /// empty; `Channel` readiness is the replacement's own; `Heuristic` has
    /// nothing to re-run.
    pub(super) fn post_drain_probe(&self, new_id: u32, mode: ReloadMode) -> Option<ReadinessSource> {
        if mode != ReloadMode::Overlap {
            return None;
        }
        let config = self.sheep.get(&new_id)?.entry.spec.config();
        let source = ReadinessSource::of(config)
            .expect("ResolvedApp already passed ProbeTarget::parse in normalize");
        matches!(source, ReadinessSource::Probe(..)).then_some(source)
    }

    /// Asks the replacement, alone this time, whether it can serve, and reports
    /// back as [`Msg::ReloadVerified`].
    ///
    /// The wait is bounded by the app's own `listen_timeout`, which extends how
    /// long a reload of a `reuse_port` app can take: a deploy tool sizing its
    /// patience off `listen_timeout + graceful_timeout` has to know.
    ///
    /// A task rather than a call, since a probe is I/O and the actor loop never
    /// awaits. The reply comes back as a message.
    pub(super) fn spawn_verify_task(&self, name: &str, new_id: u32, source: ReadinessSource) {
        let slot = self
            .sheep
            .get(&new_id)
            .expect("spawn_verify_task: the replacement was read a moment ago");
        let deadline = slot.entry.spec.config().listen_timeout.as_duration();
        // Rebuilt for the prober's sake alone, as `arm_extras` does: `spec`,
        // `instance` and `credentials` never change after registration, so
        // this is the instance's own spawn spec, bar a reference the store
        // has stopped answering.
        let credentials = match slot.entry.credentials {
            SpawnIdentity::Resolved(creds) => creds,
            SpawnIdentity::Unresolved => None,
        };
        let described = describe(
            &slot.entry.spec,
            slot.entry.instance,
            &self.paths,
            credentials,
            &self.secret_view(&slot.entry.spec),
        );
        let prober = spec_prober(&described);

        // `await_ready` wants a channel receiver and its `Probe` arm never
        // reads one, which is the only arm reachable here. Dropping the sender
        // signals nothing; nothing on the other end is listening.
        let (_ready_tx, ready_rx) = oneshot::channel();
        let tx = self.tx.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            let readiness = await_ready(&source, deadline, ready_rx, prober).await;
            let _ = tx
                .send(Msg::ReloadVerified {
                    name,
                    new_id,
                    readiness,
                })
                .await;
        });
    }
}
