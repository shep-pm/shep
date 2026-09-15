//! What the actor does when something finishes.
//!
//! `handle_exited` is the busiest method in the file: every sheep that ends,
//! for any reason, arrives here exactly once, and this is where a crash is
//! told apart from an operator's stop and from a drain. The others answer
//! the remaining events a sheep or a subsystem raises.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Resolves `selector` and hands every pump writing to a matched sheep's
    /// log paths to a task that reopens their files and then answers the
    /// caller.
    ///
    /// Nothing is awaited here: awaiting a pump inside the actor loop
    /// deadlocks, since the actor stops draining the mailbox its answer must
    /// come through. `&self` keeps the handler free of state changes, which is
    /// what lets it skip the epoch check.
    ///
    /// A matched sheep with no pump is a success. The pumps asked are every
    /// slot writing to a path a matched sheep writes to, matched or not; the
    /// reply stays keyed by the selector.
    pub(super) fn handle_reopen(
        &self,
        selector: &ProcessSelector,
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    ) {
        let mut matched: Vec<ProcessInfo> = Vec::new();
        let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
        for id in self.matching_ids(selector) {
            let slot = self
                .sheep
                .get(&id)
                .expect("`matching_ids` answers with ids read off this map a moment ago");
            paths.insert(slot.entry.out_file.clone());
            paths.insert(slot.entry.err_file.clone());
            matched.push(to_info(&slot.entry, &self.smits));
        }

        if matched.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }

        let mut pumps: Vec<(ProcessInfo, mpsc::Sender<LogCtl>)> = self
            .sheep
            .values()
            .filter(|slot| {
                paths.contains(&slot.entry.out_file) || paths.contains(&slot.entry.err_file)
            })
            .filter_map(|slot| {
                slot.log_ctl
                    .clone()
                    .map(|log_ctl| (to_info(&slot.entry, &self.smits), log_ctl))
            })
            .collect();

        // `HashMap` iteration order is arbitrary and pump failures are
        // reported in collection order, so an unsorted set would word a
        // multi-pump failure differently run to run. Id order is fine here,
        // unlike `matched` below: this list is never rendered.
        pumps.sort_unstable_by_key(|(info, _)| info.id);
        // The table `shep reopen` prints, so it takes the listing order.
        sort_flock(&mut matched);
        spawn_reopen_task(matched, pumps, reply);
    }

    /// Reads everything a handover needs off the actor and hands it to a
    /// task that asks each live pump for its descriptors.
    ///
    /// Synchronous and `&self` for the reason [`Self::handle_reopen`] gives.
    /// The entries are cloned because the assembly runs after the loop, which
    /// is what [`OwnedCandidate`](OwnedCandidate) exists for.
    ///
    /// Every registered sheep is described, with no selector: a handover
    /// carries one process image, so the flock goes whole or not at all.
    #[cfg(unix)]
    pub(super) fn handle_handover_snapshot(
        &self,
        fds: DaemonFds,
        reply: oneshot::Sender<Result<Snapshot, SupervisorError>>,
    ) {
        let mut drafts: Vec<HandoverDraft> = self
            .sheep
            .values()
            .map(|slot| HandoverDraft {
                entry: slot.entry.clone(),
                // Whole, not `is_some()`: the kind decides what the
                // successor's `handle_exited` makes of this exit, the origin
                // what its bus events say caused it.
                manual: slot.manual,
                pending_delete: slot.pending_delete,
                epoch: slot.epoch,
                ready_failed: slot.ready_failed,
                restart_due: slot.restart_due,
                log_ctl: slot.log_ctl.clone(),
                channel_open: slot.open_channel().is_some(),
            })
            .collect();
        // `HashMap` iteration order is arbitrary; id order makes two
        // snapshots of an unchanged flock identical.
        drafts.sort_unstable_by_key(|draft| draft.entry.id);
        // Read in the same synchronous step as the entries above: a job and
        // the two entries it names are one picture, and a swap could otherwise
        // finish between the two halves.
        let mut reloads: Vec<CarriedReload> = self
            .reloads
            .iter()
            .map(|(app, job)| CarriedReload {
                app: app.clone(),
                queue: job.queue.iter().copied().collect(),
                mode: job.mode,
                swap: job.swap,
                // `ReloadJob::deadline` is absent: it stamps a timer that
                // dies with this image, and the successor re-stamps from the
                // carried `next_deadline`.
            })
            .collect();
        // `HashMap` iteration order is arbitrary, for the reason the sheep
        // are sorted above.
        reloads.sort_unstable_by(|left, right| left.app.cmp(&right.app));
        spawn_handover_task(
            drafts,
            fds,
            Counters {
                next_id: self.next_id,
                next_deadline: self.next_deadline,
                next_action_stamp: self.next_action_stamp,
            },
            reloads,
            reply,
        );
    }

    /// Answers whether every sheep this actor holds could be carried across
    /// a handover.
    ///
    /// On the actor loop, unlike [`Self::handle_handover_snapshot`]: this
    /// awaits nothing. Visited in id order, so a flock with two unsupported
    /// sheep names the same one every time.
    #[cfg(unix)]
    pub(super) fn handle_handover_fitness(
        &self,
        reply: oneshot::Sender<Result<Fitness, SupervisorError>>,
    ) {
        let mut slots: Vec<&SheepSlot> = self.sheep.values().collect();
        slots.sort_unstable_by_key(|slot| slot.entry.id);
        let candidates: Vec<Candidate<'_>> = slots
            .iter()
            .map(|slot| Candidate {
                entry: &slot.entry,
                // Always `false`: this gate awaits nothing, so it cannot ask
                // a pump, and a pump wedged now may answer by the time a
                // SIGHUP arrives. `spawn_handover_task` holds the real gate.
                pump_unresponsive: false,
            })
            .collect();
        let _ = reply.send(Ok(fitness(&candidates)));
    }

    /// Resolves `selector` and hands every match to a task that flushes its
    /// log pump, truncates its log files and then answers the caller.
    /// Synchronous and `&self` for the reason [`Self::handle_reopen`] gives.
    ///
    /// The paths are [`ProcessEntry::out_file`]/[`ProcessEntry::err_file`],
    /// never the inode the pump holds: chasing the handle after an external
    /// rotator's rename would empty the archive and leave the live log alone.
    /// A stopped sheep has no pump and is still flushed.
    ///
    /// Every slot writing to one of those paths is flushed, matched or not: an
    /// unflushed sibling's dispatched `write(2)` would land at offset 0 of the
    /// file just reported empty. The reply stays keyed by the selector.
    pub(super) fn handle_flush(
        &self,
        selector: &ProcessSelector,
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    ) {
        let mut matched: Vec<ProcessInfo> = Vec::new();
        let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
        for id in self.matching_ids(selector) {
            let slot = self
                .sheep
                .get(&id)
                .expect("`matching_ids` answers with ids read off this map a moment ago");
            paths.insert(slot.entry.out_file.clone());
            paths.insert(slot.entry.err_file.clone());
            matched.push(to_info(&slot.entry, &self.smits));
        }

        if matched.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }

        let mut pumps: Vec<(u32, mpsc::Sender<LogCtl>)> = self
            .sheep
            .iter()
            .filter(|(_, slot)| {
                paths.contains(&slot.entry.out_file) || paths.contains(&slot.entry.err_file)
            })
            .filter_map(|(id, slot)| slot.log_ctl.clone().map(|log_ctl| (*id, log_ctl)))
            .collect();

        // Sorted for the reason `handle_reopen` sorts: `HashMap` iteration
        // order is arbitrary and failures are reported in collection order.
        pumps.sort_unstable_by_key(|&(id, _)| id);
        // The table `shep empty` prints, so it takes the listing order.
        sort_flock(&mut matched);
        let pumps = pumps.into_iter().map(|(_, log_ctl)| log_ctl).collect();
        spawn_flush_task(matched, pumps, paths, reply);
    }

    /// Kills every currently-online sheep, deferring the reply the same way
    /// `Stop` does; returns `true` (break the actor loop) once there was
    /// nothing to wait on, or once the deferred reply resolves.
    ///
    /// Sets `shutting_down` before computing `online`, so nothing outside that
    /// snapshot can ever need killing later: every downstream check against
    /// the flag rests on no sheep registering or respawning from here on.
    pub(super) fn begin_shutdown(&mut self, reply: oneshot::Sender<()>) -> bool {
        self.shutting_down = true;
        // Every in-flight reload is abandoned: its next step is always a
        // spawn, forbidden from here on. Both halves are `ctl.is_some()`, so
        // both are killed below; the replacement stops registered, and the
        // drainee still carries `ReloadState::Drainee` and is deregistered.
        self.reloads.clear();

        let online: HashSet<u32> = self
            .sheep
            .iter()
            .filter(|(_, slot)| slot.ctl.is_some())
            .map(|(&id, _)| id)
            .collect();

        if online.is_empty() {
            let _ = reply.send(());
            return true;
        }

        for &id in &online {
            // Same marker rule as `begin_manual`: an id already mid-kill
            // under an operator's command keeps its marker; one held by an
            // automatic restart is taken over. It joins `remaining` either way.
            self.claim_manual(
                id,
                PendingManual {
                    kind: ManualKind::Stop,
                    origin: CommandOrigin::Operator,
                },
                // A shutdown is a stop even for a draining instance: the
                // longer cap buys time to finish work.
                LadderCap::Stop,
            );
        }

        self.pending.push(PendingReply {
            remaining: online,
            results: Vec::new(),
            reply: ReplyKind::Shutdown(reply),
        });
        false
    }

    /// Handles one sheep's terminal exit: computes uptime, consults
    /// `decide_on_exit`, applies the resulting transition, and resolves any
    /// deferred reply waiting on this id. Returns `true` iff this exit just
    /// completed a `Shutdown`'s aggregation (the actor loop should break).
    pub(super) fn handle_exited(&mut self, id: u32, outcome: ExitOutcome) -> bool {
        let Some(slot) = self.sheep.get_mut(&id) else {
            tracing::warn!(id, "Msg::Exited for an unregistered id");
            return false;
        };
        slot.ctl = None;
        // Goes with `ctl`: the writer task cannot notice a child that exited
        // quietly, and would hold the daemon's half of the socketpair for as
        // long as the entry lived. See `SheepSlot::to_child`.
        slot.to_child = None;
        // Same: a sheep task parked on `signal_rx.recv()` would hold this
        // sender's receiver open. See `SheepSlot::signals`.
        slot.signals = None;
        // Same: the writer task parks on `recv()`. See `SheepSlot::to_stdin`.
        slot.to_stdin = None;
        // The one place a process under a registered id stops existing, so a
        // wait cannot survive into a second process's life.
        slot.actions.abandon_all();
        slot.entry.pid = None;
        // Cleared for the reason `pid` is: it is a verdict about a process.
        // Left set, `reload_eligible` would let a reload drain a row with no
        // process behind it. See `SheepSlot::ready_failed`.
        slot.ready_failed = false;
        // Set before any branch below decides what this exit becomes, and
        // unconditionally: an operator's own `stop` reaches this line exactly
        // as a crash does. A branch that removes the entry carries the value
        // into the `ProcessInfo` its removal emits.
        slot.entry.last_exit = Some(outcome.into());
        // `kind` decides what this exit becomes; the origin decides only what
        // the bus says caused it, and is read once, on the forced-respawn
        // branch. The Stop and Delete branches stay literal `true`: every site
        // that puts either kind on a marker declares `Operator`.
        let manual = slot.manual.take();
        let kind = manual.map(|pending| pending.kind);
        let pending_delete = std::mem::take(&mut slot.pending_delete);
        // Read off the slot with the borrow already open, because both
        // branches below hand the actor back to itself.
        let reload = slot.entry.reload;
        let started_at = slot.entry.started_at.take();

        // Neither half of a reload takes the ordinary decision path.
        // `decide_on_exit` knows nothing about reloads, so an `autorestart`
        // app's drainee would be respawned into its replacement's slot.
        match reload {
            ReloadState::Drainee { .. } => {
                // The drainee's slot belongs to its replacement, so its
                // registration goes with the process, unless an operator's own
                // command reached it first: `stop` must leave a sheep
                // registered and `Stopped`, not deleted.
                match self.uncommitted_swap_of(id) {
                    Some(name) if kind.is_some() => {
                        self.abort_reload(&name, "an operator's command reached the drainee first");
                        // Falls through as an ordinary entry: `abort_reload`
                        // cleared the marker and put the status back.
                    }
                    _ => return self.reap_drainee(id),
                }
            }
            ReloadState::Replacement => {
                // Still `AwaitReady` means the replacement never proved it
                // could take over, so the reload is abandoned and the drainee
                // kept. Past that the swap is committed and this is an
                // ordinary instance, whose exit is its restart policy's.
                self.clear_reload(id);
                if let Some(name) = self.uncommitted_swap_of(id) {
                    self.abort_reload(&name, "the replacement exited before it was ready");
                    return self.deregister_on_exit(id);
                }
                // A committed swap normally ends on the drainee's exit, but
                // one `reap_drainee` committed has none left, and the marker
                // cleared above cancels the readiness route. So the job ends
                // here or never, and one nothing can end blocks every reload.
                if let Some(name) = self.reload_of(id) {
                    let old_id = self.reloads[&name].swap.old_id;
                    if !self.sheep.contains_key(&old_id) {
                        debug_assert_ne!(
                            self.sheep.get(&id).map(|slot| slot.entry.status),
                            Some(ProcStatus::Online),
                            "a swap committed by the drainee's death cannot have had a live \
                             replacement"
                        );
                        tracing::warn!(
                            name,
                            new_id = id,
                            "reload abandoned: the replacement exited before it was ready, with \
                             the instance it replaced already gone"
                        );
                        self.reloads.remove(&name);
                        if let Some(slot) = self.sheep.get(&id) {
                            let info = to_info(&slot.entry, &self.smits);
                            self.emit(ProcessEventKind::ReloadAbandoned, info, true);
                        }
                    }
                }
            }
            ReloadState::None => {}
        }

        let Some(started_at) = started_at else {
            // Should not happen: a duplicate `Msg::Exited` would violate the
            // one-exit-path invariant. Any pending reply is still resolved
            // with a best-effort snapshot rather than parked forever.
            tracing::warn!(
                id,
                "Msg::Exited for an entry with no started_at (duplicate?)"
            );
            // Unreachable today: `pending_delete` is only set for a sheep
            // with `ctl.is_some()`. Honoured anyway, because the takes above
            // have consumed both markers, so a Delete would otherwise be
            // dropped while its caller was told it succeeded.
            if kind == Some(ManualKind::Delete) || pending_delete {
                let mut removed = self.sheep.remove(&id).expect("checked above");
                removed.entry.status = ProcStatus::Stopped;
                let info = to_info(&removed.entry, &self.smits);
                self.emit(ProcessEventKind::Delete, info.clone(), true);
                self.disarm_extras(id, &info.name);
                return self.resolve_pending(id, info);
            }
            let info = to_info(
                &self.sheep.get(&id).expect("checked above").entry,
                &self.smits,
            );
            return self.resolve_pending(id, info);
        };
        let uptime = tokio::time::Instant::now().saturating_duration_since(started_at);

        // A manual Restart forces a respawn, where `decide_on_exit` would
        // choose CleanStop off `manual_stop`. Not while shutting down, which
        // would orphan a child outside the shutdown snapshot; not under
        // `pending_delete`, which would hand back a live process as deleted.
        if kind == Some(ManualKind::Restart) && !self.shutting_down && !pending_delete {
            // Re-fetched rather than carried down: the reload branches above
            // hand `self` back to itself, ending the earlier borrow.
            self.sheep
                .get_mut(&id)
                .expect("checked above")
                .entry
                .budget
                .reset();
            // The one place the origin is read. A `shep restart` is a user
            // action; cron, watch, memory and liveness restarts are the
            // daemon's own, and a subscriber told otherwise cannot tell a
            // deploy from an app thrashing.
            let manually = matches!(
                manual,
                Some(PendingManual {
                    origin: CommandOrigin::Operator,
                    ..
                })
            );
            let info = self.respawn(id, manually);
            return self.resolve_pending(id, info);
        }

        let decision = {
            let slot = self.sheep.get_mut(&id).expect("checked above");
            decide_on_exit(
                slot.entry.spec.config(),
                &mut slot.entry.budget,
                uptime,
                outcome,
                kind.is_some(),
            )
        };

        let info = match decision {
            Decision::Restart { delay } => {
                let info = self.set_status(id, ProcStatus::WaitingRestart);
                self.emit(ProcessEventKind::Exit, info.clone(), false);
                // Capture the current epoch so this timer can tell, when it
                // fires, whether it is still this id's authoritative one.
                let epoch = self.sheep.get(&id).expect("checked above").epoch;
                // In the one clock that survives an `execve`, stamped here
                // because `install_adopted` re-arms a deadline this produced.
                // `checked_add` avoids a panic on overflow; overflow or a
                // pre-epoch result falls back to the whole delay.
                self.sheep.get_mut(&id).expect("checked above").restart_due = SystemTime::now()
                    .checked_add(delay.unwrap_or(Duration::ZERO))
                    .filter(|due| due.duration_since(SystemTime::UNIX_EPOCH).is_ok());
                self.schedule_restart(id, epoch, delay);
                info
            }
            Decision::Errored => {
                let info = self.set_status(id, ProcStatus::Errored);
                self.emit(ProcessEventKind::Errored, info.clone(), kind.is_some());
                self.disarm_extras(id, &info.name);
                info
            }
            Decision::CleanStop if kind == Some(ManualKind::Delete) || pending_delete => {
                let mut removed = self.sheep.remove(&id).expect("checked above");
                removed.entry.status = ProcStatus::Stopped;
                let info = to_info(&removed.entry, &self.smits);
                self.emit(ProcessEventKind::Delete, info.clone(), true);
                self.disarm_extras(id, &info.name);
                info
            }
            Decision::CleanStop => {
                let info = self.set_status(id, ProcStatus::Stopped);
                self.disarm_extras(id, &info.name);
                self.emit(
                    ProcessEventKind::Stop,
                    info.clone(),
                    kind == Some(ManualKind::Stop),
                );
                info
            }
        };

        self.resolve_pending(id, info)
    }

    /// A memory breach or a liveness failure asked for a restart.
    ///
    /// Guarded on the pid rather than [`SheepSlot`]'s respawn epoch: the pid is
    /// on both reports and is as good a generation token. A liveness report
    /// carries its probe's own epoch too, since `InstanceExtras::disarm` does
    /// not await the aborted task, so a probe already inside `failures.send`
    /// can deliver against the same pid in the same status, and a config apply
    /// must never kill a process. A memory breach re-asks the ceiling instead.
    ///
    /// Delegates to `begin_manual`, not `respawn`, which keeps the kill ladder
    /// and the budget reset; it goes in as [`CommandOrigin::Automatic`], so an
    /// operator's `stop` can take the sheep back off a restart mid-ladder.
    pub(super) fn handle_extra_restart(
        &mut self,
        id: u32,
        pid: u32,
        epoch: Option<u64>,
        observed: Option<MemSize>,
    ) {
        if self.shutting_down {
            tracing::debug!(id, pid, "extra restart dropped: engine is shutting down");
            return;
        }
        let Some(slot) = self.sheep.get(&id) else {
            tracing::debug!(id, pid, "extra restart dropped: no such sheep");
            return;
        };
        if slot.entry.pid != Some(pid) {
            tracing::debug!(
                id,
                pid,
                current = slot.entry.pid,
                "extra restart dropped: the reported pid is no longer this sheep's"
            );
            return;
        }
        if slot.entry.status != ProcStatus::Online {
            tracing::debug!(
                id,
                pid,
                status = %slot.entry.status,
                "extra restart dropped: the sheep is no longer online"
            );
            return;
        }
        // A probe replaced because its config changed leaves the pid and the
        // status untouched, so a failure already in flight passes both guards
        // above. `epoch` is `None` for a memory breach, which has no
        // per-instance task that can go stale.
        if let Some(epoch) = epoch {
            let current = self.registry.liveness_epoch(id);
            if current != epoch {
                tracing::debug!(
                    id,
                    pid,
                    epoch,
                    current,
                    "extra restart dropped: the reporting probe has been replaced"
                );
                return;
            }
        }
        // A breach is computed under `PollingEnforcer`'s lock and sent after
        // it is released, so a ceiling re-armed in between leaves a report in
        // flight against a ceiling nobody enforces. Re-asking rather than
        // comparing ceilings: a lowered one makes an old measurement real.
        if let Some(observed) = observed {
            let ceiling = self
                .sheep
                .get(&id)
                .and_then(|slot| slot.entry.spec.config().max_memory);
            if ceiling.is_none_or(|limit| observed <= limit) {
                tracing::debug!(
                    id,
                    pid,
                    observed = observed.bytes(),
                    "extra restart dropped: the ceiling it breached is no longer in force"
                );
                return;
            }
        }
        // A throwaway reply: the reporter is fire-and-forget by contract.
        let (reply, _dropped) = oneshot::channel();
        self.begin_manual(
            ProcessSelector::Id(id),
            ManualKind::Restart,
            CommandOrigin::Automatic,
            ReplyKind::Info(reply),
        );
    }

    /// A scheduled restart's backoff elapsed.
    ///
    /// Dropped while shutting down, since nothing here would be in the
    /// shutdown's `online` snapshot. Dropped unless the entry is still
    /// `WaitingRestart`, which a manual command may have intercepted and which
    /// also excludes a reload's drainee, and unless the epoch still matches: a
    /// respawn since this timer was scheduled makes it stale even though the
    /// sheep is legitimately `WaitingRestart` again, under a newer backoff.
    pub(super) fn handle_restart_due(&mut self, id: u32, epoch: u64) {
        if self.shutting_down {
            return;
        }
        let Some(slot) = self.sheep.get(&id) else {
            return;
        };
        if slot.entry.status != ProcStatus::WaitingRestart {
            return;
        }
        if slot.epoch != epoch {
            return;
        }
        self.respawn(id, false);
    }

    /// Forwards the shepherd channel's readiness signal to `id`'s waiting
    /// readiness task, if one is waiting. A `Ready` with no live wait is
    /// dropped silently: an app may write `{"kind":"ready"}` twice.
    pub(super) fn handle_ready_signal(&mut self, id: u32) {
        let Some(slot) = self.sheep.get_mut(&id) else {
            return;
        };
        if let Some(tx) = slot.ready_tx.take() {
            let _ = tx.send(());
        }
    }

    /// A readiness wait resolved.
    ///
    /// Dropped while shutting down, or when the slot is gone, its epoch has
    /// moved, or its status has left `Starting`. Without those, a sheep that
    /// exited and respawned while its readiness task was still waiting would
    /// have the old wait mark the new process online.
    ///
    /// Past the guards the wait belongs to one of two callers, which want
    /// opposite things from a deadline that elapsed; see
    /// [`Self::reload_ready_result`], which owns the reload half.
    pub(super) fn handle_ready_result(
        &mut self,
        id: u32,
        epoch: u64,
        manually: bool,
        readiness: Readiness,
    ) {
        if self.shutting_down {
            return;
        }
        let Some(slot) = self.sheep.get(&id) else {
            return;
        };
        if slot.epoch != epoch {
            return;
        }
        if slot.entry.status != ProcStatus::Starting {
            return;
        }
        if matches!(slot.entry.reload, ReloadState::Replacement) {
            self.reload_ready_result(id, manually, readiness);
            return;
        }
        if readiness == Readiness::TimedOut {
            // Online anyway: treating a readiness timeout as a spawn failure
            // would turn a slow-starting app into a restart loop.
            tracing::warn!(id, "readiness deadline elapsed; marking online anyway");
        }
        let info = self.set_status(id, ProcStatus::Online);
        // `manually` comes from the spawn that armed this wait, so gating an
        // app changes only when `Online` fires, never what the event says
        // caused it.
        self.went_online(id, info, manually);
    }
}
