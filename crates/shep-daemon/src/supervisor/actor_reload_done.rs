//! Finishing, abandoning or timing out a reload.
//!
//! Every reload ends somewhere: the replacement passed and the swap commits,
//! it failed and the old process is kept, or the deadline elapsed and the
//! job is abandoned. These handle those endings, plus the small lookups that
//! ask what a given id is currently caught up in.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// The post-drain probe answered, or did not: finish the swap or abandon
    /// the reload.
    ///
    /// Exact for a single-instance app, where one process is left. Weaker for a
    /// clustered app: surviving old instances are still in the `SO_REUSEPORT`
    /// group and can answer for a bad replacement until the last swap.
    pub(super) fn handle_reload_verified(&mut self, name: &str, new_id: u32, readiness: Readiness) {
        let Some(job) = self.reloads.get(name) else {
            return;
        };
        // Ids are never reused, so a verdict naming anything but this swap's
        // replacement belongs to a swap that has already ended.
        if job.swap.new_id != Some(new_id) || job.swap.phase != ReloadPhase::Verify {
            return;
        }
        let job = self
            .reloads
            .remove(name)
            .expect("handle_reload_verified: the job was read a moment ago");
        self.clear_reload(new_id);

        if readiness == Readiness::Ready {
            self.commit_swap_success(name, new_id, job.queue);
            return;
        }

        // Demoted out of `Online`: that status was written on a probe the
        // now-reaped instance may have answered. The process is not killed,
        // since a replacement that is up but not answering is more than none,
        // and `handle_extra_restart`'s guard admits a `ready_failed` sheep.
        tracing::warn!(
            name,
            new_id,
            "reload abandoned: the replacement did not answer its readiness probe once the \
             instance it replaced was gone, so the probe that put it online was answered by \
             that instance"
        );
        let info = self.set_status(new_id, ProcStatus::Starting);
        if let Some(slot) = self.sheep.get_mut(&new_id) {
            slot.ready_failed = true;
        }
        self.emit(ProcessEventKind::ReloadAbandoned, info, true);
        self.arm_extras(new_id);
    }

    /// Abandons `name`'s reload: the drainee goes back to serving where that
    /// is still available to it, the instances not yet reached are left alone,
    /// and the replacement is killed and deregistered.
    ///
    /// The replacement goes through the kill ladder, since it may have forked
    /// lambs. Its entry is deregistered rather than left `Errored`: the slot
    /// belongs to the drainee.
    pub(super) fn abort_reload(&mut self, name: &str, reason: &str) {
        let Some(job) = self.reloads.remove(name) else {
            return;
        };
        // Guards the restore below and nothing else: a job outliving both of
        // its ids is a different failure, handled in `handle_exited`.
        debug_assert_eq!(
            job.swap.phase,
            ReloadPhase::AwaitReady,
            "abort_reload: a committed swap has no old instance to go back to"
        );
        tracing::warn!(
            name,
            old_id = job.swap.old_id,
            new_id = job.swap.new_id,
            reason,
            "reload abandoned"
        );

        // Read back out of the map rather than emitted inside the block: the
        // event carries the status the restore decides, and that block holds a
        // mutable borrow while it decides it.
        let kept = self.sheep.get_mut(&job.swap.old_id).map(|drainee| {
            drainee.entry.reload = ReloadState::None;
            // Restored only where going back is still available. A drainee
            // whose own exit triggered this has no task left, and one mid-kill
            // ladder holds an operator's marker: `Stopping` is honest for both,
            // and writing over it hands an operator a live pid.
            if drainee.ctl.is_some() && drainee.manual.is_none() {
                drainee.entry.status = restored_status(drainee);
            }
            to_info(&drainee.entry, &self.smits)
        });
        if let Some(info) = kept {
            self.emit(ProcessEventKind::ReloadAbandoned, info, true);
        }

        let Some(new_id) = job.swap.new_id else {
            // `DrainFirst` has no replacement, and the drainee has already been
            // put back by the block above.
            return;
        };
        let Some(replacement) = self.sheep.get_mut(&new_id) else {
            return;
        };
        replacement.entry.reload = ReloadState::None;
        if replacement.ctl.is_none() {
            // Already terminal: this abandonment is its exit being handled,
            // and `handle_exited` deregisters it. A `Kill` to an ended task
            // would claim a marker no exit will ever clear.
            return;
        }
        replacement.pending_delete = true;
        self.claim_manual(
            new_id,
            PendingManual {
                kind: ManualKind::Delete,
                origin: CommandOrigin::Operator,
            },
            // A failed start, not a graceful handover: nothing is being
            // drained, so there is no work in hand to wait on.
            LadderCap::Stop,
        );
    }

    /// Bounds the swap that has just started: after
    /// `listen_timeout + graceful_timeout + `[`RELOAD_DEADLINE_SLACK`], a
    /// `Msg::ReloadDeadline` comes back to end it if nothing else has.
    ///
    /// Every other transition out of a [`ReloadJob`] is driven by a message
    /// from a task the actor cannot make report, and [`kill_process`]'s wait
    /// after `SIGKILL` has no timeout: a wedged instance would wedge the app.
    pub(super) fn arm_reload_deadline(&mut self, name: &str, id: u32) {
        // Loud rather than silent: a swap that failed to arm one is the state
        // this exists to make impossible.
        let app = self
            .sheep
            .get(&id)
            .expect("arm_reload_deadline: the swap's entry was read a moment ago")
            .entry
            .spec
            .config();
        let deadline = swap_budget(app);

        let stamp = self.next_deadline;
        self.next_deadline += 1;
        // Every caller arms with its job already in the map; without one
        // there is nothing for a watchdog to end, so arming would leak a
        // timer that could only be dropped as stale.
        let job = self
            .reloads
            .get_mut(name)
            .expect("arm_reload_deadline: no job to arm for");
        job.deadline = stamp;

        let tx = self.tx.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(deadline).await;
            let _ = tx.send(Msg::ReloadDeadline { name, stamp }).await;
        });
    }

    /// A swap ran out of time: end the reload rather than leave a job nothing
    /// can remove.
    ///
    /// Stale deadlines are dropped on the swap's `new_id`, which ids are never
    /// reused for. Before the commit this is an ordinary abandonment; after it
    /// the job is dropped where it stands. The drainee keeps
    /// [`ReloadState::Drainee`] so a late exit routes to [`Self::reap_drainee`].
    pub(super) fn handle_reload_deadline(&mut self, name: &str, stamp: u64) {
        let Some(job) = self.reloads.get(name) else {
            return;
        };
        if job.deadline != stamp {
            return;
        }
        let old_id = job.swap.old_id;
        match job.swap.phase {
            ReloadPhase::DrainFirst => {
                // A serial reload whose drain never produced an exit, with no
                // replacement to kill. The marker comes off, unlike the
                // `DrainOld` arm: the slot is this instance's own, so a late
                // exit leaves the operator a `Stopped` row to restart.
                tracing::warn!(
                    name,
                    old_id,
                    "reload abandoned: the instance being drained passed the swap's deadline \
                     without exiting, so its replacement was never spawned"
                );
                self.reloads.remove(name);
                self.clear_reload(old_id);
                self.emit_reload_abandoned(old_id);
            }
            ReloadPhase::AwaitReady => {
                self.abort_reload(
                    name,
                    "the swap passed its deadline with no readiness result",
                );
            }
            ReloadPhase::DrainOld | ReloadPhase::Verify => {
                let new_id = job
                    .swap
                    .new_id
                    .expect("a swap past AwaitReady has a replacement");
                tracing::warn!(
                    name,
                    old_id,
                    new_id,
                    drainee_registered = self.sheep.contains_key(&old_id),
                    "reload abandoned: the swap passed its deadline, so the message that would \
                     have ended it is not coming"
                );
                self.reloads.remove(name);
                self.clear_reload(new_id);
                self.emit_reload_abandoned(new_id);
            }
        }
    }

    /// `ReapOld`: the drainee has exited, so its registration goes with it.
    ///
    /// Nothing else would remove it: a drainee is not deleted and does not
    /// respawn, so without this its `SheepSlot` outlives the process. Returns
    /// what [`Self::resolve_pending`] returned, so an operator's `stop`/`delete`
    /// waiting on this exit is still answered. A [`ReloadMode::Serial`]'s
    /// replacement is created here, before the deregistration.
    pub(super) fn reap_drainee(&mut self, old_id: u32) -> bool {
        if let Some(name) = self.serial_drain_of(old_id) {
            // An operator's `delete` can reach the instance a serial reload is
            // draining, and `DrainFirst` is the one phase with no other guard
            // against it. Without this the delete would leave a replacement
            // running under a new id for an app nobody has.
            if self.sheep[&old_id].pending_delete {
                tracing::warn!(
                    name,
                    old_id,
                    "reload abandoned: the instance being drained was deleted, so no \
                     replacement was spawned"
                );
                self.reloads.remove(&name);
            } else {
                self.spawn_serial_replacement(&name, old_id);
            }
        }
        let terminal = self.deregister_on_exit(old_id);
        let Some(name) = self.reload_of(old_id) else {
            return terminal;
        };
        match self.reloads[&name].swap.phase {
            ReloadPhase::DrainOld | ReloadPhase::Verify => self.finish_swap(&name),
            // An overlapping swap whose drainee died before the replacement was
            // ready has nothing left to abandon back to, so it commits here. A
            // serial one is in the same position by construction.
            ReloadPhase::AwaitReady => {
                self.reloads
                    .get_mut(&name)
                    .expect("reap_drainee: the phase was read a moment ago")
                    .swap
                    .phase = ReloadPhase::DrainOld;
            }
            // The serial spawn above failed and ended the job, so `reload_of`
            // found nothing. `DrainFirst` is entered once and left by the spawn
            // a few lines up.
            ReloadPhase::DrainFirst => {
                debug_assert!(false, "reap_drainee: DrainFirst outlived its own spawn");
            }
        }
        terminal
    }

    /// The app whose serial reload is draining `old_id` right now, if one is.
    ///
    /// Kept apart from the answer so the borrow of `self.reloads` ends before
    /// [`Self::reap_drainee`]'s spawn begins.
    pub(super) fn serial_drain_of(&self, old_id: u32) -> Option<String> {
        self.reloads
            .iter()
            .find(|(_, job)| job.swap.phase == ReloadPhase::DrainFirst && job.swap.old_id == old_id)
            .map(|(name, _)| name.clone())
    }

    /// `SpawnNew`, for a serial reload: the instance is drained, so put its
    /// replacement in the slot it has just left.
    ///
    /// The mirror of the `Ok`/`Err` pair in [`Self::advance_reload`]'s overlap
    /// arm. On success the swap moves to `AwaitReady` and arms a watchdog that
    /// makes the drain's own stale. On failure nothing is still serving, so the
    /// abandonment names an instance that is already dead.
    pub(super) fn spawn_serial_replacement(&mut self, name: &str, old_id: u32) {
        match self.spawn_replacement(old_id, ReloadMode::Serial) {
            Ok(new_id) => {
                let job = self
                    .reloads
                    .get_mut(name)
                    .expect("spawn_serial_replacement: the job was read a moment ago");
                job.swap.new_id = Some(new_id);
                job.swap.phase = ReloadPhase::AwaitReady;
                self.arm_reload_deadline(name, new_id);
            }
            Err(error) => {
                tracing::warn!(
                    name,
                    old_id,
                    error,
                    "reload abandoned: the instance was drained and its replacement could not \
                     be spawned, so the instance slot is empty"
                );
                self.reloads.remove(name);
                self.emit_reload_abandoned(old_id);
            }
        }
    }

    /// Deregisters an id whose `Msg::Exited` is being handled, announcing it
    /// the way every other deregistration is announced.
    pub(super) fn deregister_on_exit(&mut self, id: u32) -> bool {
        let mut removed = self
            .sheep
            .remove(&id)
            .expect("deregister_on_exit: unknown id");
        removed.entry.status = ProcStatus::Stopped;
        removed.entry.reload = ReloadState::None;
        let info = to_info(&removed.entry, &self.smits);
        self.emit(ProcessEventKind::Delete, info.clone(), true);
        self.disarm_extras(id, &info.name);
        self.resolve_pending(id, info)
    }

    /// The app whose swap `id` is half of, while that swap has not committed
    /// yet: the window in which ending either half loses the reload's overlap.
    ///
    /// The one spelling of that rule, shared by both of
    /// [`Self::handle_exited`]'s reload arms and by [`Self::begin_manual`].
    pub(super) fn uncommitted_swap_of(&self, id: u32) -> Option<String> {
        self.reloads
            .iter()
            .find(|(_, job)| {
                job.swap.phase == ReloadPhase::AwaitReady
                    && (job.swap.old_id == id || job.swap.new_id == Some(id))
            })
            .map(|(name, _)| name.clone())
    }

    /// Whether `id` is half of a swap that has not committed yet; see
    /// [`Self::uncommitted_swap_of`].
    pub(super) fn in_an_uncommitted_swap(&self, id: u32) -> bool {
        self.uncommitted_swap_of(id).is_some()
    }

    /// The app whose in-flight reload names `id`, in either role.
    pub(super) fn reload_of(&self, id: u32) -> Option<String> {
        self.reloads
            .iter()
            .find(|(_, job)| job.swap.old_id == id || job.swap.new_id == Some(id))
            .map(|(name, _)| name.clone())
    }

    /// Takes `id` out of any reload it is half of, leaving an ordinary entry.
    pub(super) fn clear_reload(&mut self, id: u32) {
        if let Some(slot) = self.sheep.get_mut(&id) {
            slot.entry.reload = ReloadState::None;
        }
    }

    /// Announces that the reload `id` belonged to ended without replacing
    /// anything, read off the entry that reload leaves behind.
    ///
    /// Emits nothing once that entry is deregistered. Callers drop the job and
    /// clear the markers first, so the event never names a swap still in
    /// flight, and they set the status the event should carry before calling.
    pub(super) fn emit_reload_abandoned(&mut self, id: u32) {
        if let Some(slot) = self.sheep.get(&id) {
            let info = to_info(&slot.entry, &self.smits);
            self.emit(ProcessEventKind::ReloadAbandoned, info, true);
        }
    }

    /// Commits a swap that succeeded: announces the replacement as reloaded,
    /// then moves the reload on to its next instance.
    ///
    /// The event goes out before the next swap starts, so a clustered app's
    /// reload reads in order. Callers owe it only to a replacement still
    /// registered and serving, and have already dropped the job it belongs to.
    pub(super) fn commit_swap_success(&mut self, name: &str, new_id: u32, queue: VecDeque<u32>) {
        let slot = self
            .sheep
            .get(&new_id)
            .expect("commit_swap_success: the replacement was read a moment ago");
        let info = to_info(&slot.entry, &self.smits);
        self.emit(ProcessEventKind::Reloaded, info, true);
        self.advance_reload(name, queue);
    }
}
