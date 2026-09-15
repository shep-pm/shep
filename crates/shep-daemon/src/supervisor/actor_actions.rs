//! Talking to a running sheep, and the bookkeeping around it.
//!
//! Actions, signals and stdin writes all go out to a child that may take its
//! time answering, so each is armed and resolved across turns rather than
//! awaited. The rest are the small operations the other handlers lean on:
//! arming extras, setting a status, emitting an event.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Resolves `selector`, puts one action on the shepherd channel of every
    /// matched sheep that can take one, and answers with a row per match once
    /// the last of those waits has ended.
    ///
    /// Shaped like [`Self::begin_manual`], except that the rows are joined in
    /// [`spawn_trigger_task`] rather than in `pending`. Nothing is awaited
    /// here, for the reason [`Self::handle_reopen`] gives.
    ///
    /// Both refusals are decided ahead of the wait, since a sheep refused
    /// after one was armed would leave a wait nothing drives home: no open
    /// [`SheepSlot::open_channel`] is [`ActionOutcome::NoChannel`], a drainee
    /// is [`ActionOutcome::Skipped`]. A replacement is not skipped.
    pub(super) fn begin_action(
        &mut self,
        selector: &ProcessSelector,
        action: String,
        params: Option<String>,
        reply: oneshot::Sender<Result<Vec<ActionReply>, SupervisorError>>,
    ) {
        // `matching_ids` answers in id order, so delivery is not arbitrary;
        // the answer's own order rests on `spawn_trigger_task`'s final sort.
        let matched = self.matching_ids(selector);

        if matched.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }

        let mut refused = Vec::new();
        let mut waits = Vec::new();
        for id in matched {
            let slot = self
                .sheep
                .get(&id)
                .expect("begin_action: `matched` holds ids read off this map a moment ago");
            let config = slot.entry.spec.config();
            let name = config.name.clone();
            // Each sheep's own `action_timeout` bounds its own wait.
            let action_timeout = config.action_timeout.as_duration();
            if matches!(slot.entry.reload, ReloadState::Drainee { .. }) {
                refused.push(ActionReply {
                    id,
                    name,
                    outcome: ActionOutcome::Skipped,
                });
                continue;
            }
            let Some(to_child) = slot.open_channel().cloned() else {
                refused.push(ActionReply {
                    id,
                    name,
                    outcome: ActionOutcome::NoChannel,
                });
                continue;
            };
            let answer =
                self.arm_action(id, to_child, action.clone(), params.clone(), action_timeout);
            waits.push((id, name, answer));
        }

        if waits.is_empty() {
            // Not a silent success. The rows say what happened to each sheep;
            // this says it once in the daemon's own log, where a flock with
            // `channel` unset everywhere is one cause rather than a table of
            // refusals.
            let skipped = refused
                .iter()
                .filter(|row| row.outcome == ActionOutcome::Skipped)
                .count();
            tracing::warn!(
                action,
                matched = refused.len(),
                skipped,
                "no matched sheep could take this action; nothing was delivered"
            );
            let _ = reply.send(Ok(refused));
            return;
        }

        spawn_trigger_task(refused, waits, reply);
    }

    /// Delivers one signal to every matched sheep's own process.
    ///
    /// Off the actor loop, like [`Self::begin_action`]: each delivery is a
    /// round trip through a sheep task. There is nothing to wait out, so the
    /// fan-out is bounded by the syscall rather than a configured timeout.
    ///
    /// A sheep with no live task answers [`SignalOutcome::NotRunning`] without
    /// a round trip: `slot.signals` is `None` for exactly the states with no
    /// process. A reload drainee is signalled like any other live sheep,
    /// unlike in [`Self::begin_action`]: a signal expects nothing back.
    pub(super) fn begin_signal(
        &mut self,
        selector: &ProcessSelector,
        sig: OperatorSignal,
        reply: oneshot::Sender<Result<Vec<SignalReply>, SupervisorError>>,
    ) {
        let matched = self.matching_ids(selector);
        if matched.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }

        let mut settled = Vec::new();
        let mut waits = Vec::new();
        for id in matched {
            let slot = self
                .sheep
                .get(&id)
                .expect("begin_signal: `matched` holds ids read off this map a moment ago");
            let name = slot.entry.spec.config().name.clone();
            let Some(signals) = slot.signals.clone() else {
                settled.push(SignalReply {
                    id,
                    name,
                    outcome: SignalOutcome::NotRunning,
                });
                continue;
            };
            let (done, answer) = oneshot::channel();
            if signals.try_send(SignalRequest { sig, done }).is_err() {
                // A full queue means this sheep's task has not drained several
                // signals, which for a syscall-fast handler means it is busy
                // dying; a closed one means it already has.
                settled.push(SignalReply {
                    id,
                    name,
                    outcome: SignalOutcome::NotRunning,
                });
                continue;
            }
            waits.push((id, name, answer));
        }

        spawn_signal_task(settled, waits, reply);
    }

    /// Writes one line to every matched sheep's stdin. Off the actor loop,
    /// like [`Self::begin_signal`]: each write is a round trip through a sheep
    /// task.
    ///
    /// A sheep with no live task, or one running without `stdin = true`,
    /// answers [`LineOutcome::NoStdin`] off [`SheepSlot::open_stdin`], the one
    /// fact that decides whether a pipe exists. A reload drainee gets the
    /// line, as [`Self::begin_signal`] delivers a signal to one.
    ///
    /// The enqueue is `try_send`, never an awaited send: a full queue is the
    /// condition [`LineOutcome::NotWritten`] names, and awaiting into one
    /// would park the actor loop on a wedged sheep's pipe.
    pub(super) fn begin_send_line(
        &mut self,
        selector: &ProcessSelector,
        line: String,
        reply: oneshot::Sender<Result<Vec<LineReply>, SupervisorError>>,
    ) {
        let matched = self.matching_ids(selector);
        if matched.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }

        let mut settled = Vec::new();
        let mut waits = Vec::new();
        for id in matched {
            let slot = self
                .sheep
                .get(&id)
                .expect("begin_send_line: `matched` holds ids read off this map a moment ago");
            let name = slot.entry.spec.config().name.clone();
            let Some(to_stdin) = slot.open_stdin().cloned() else {
                settled.push(LineReply {
                    id,
                    name,
                    outcome: LineOutcome::NoStdin,
                });
                continue;
            };
            let (done, answer) = oneshot::channel();
            match to_stdin.try_send(StdinWrite {
                line: line.clone(),
                done,
            }) {
                Ok(()) => waits.push((id, name, answer)),
                // The queue is full: the writer is blocked on a pipe the app
                // is not draining. The reason names no duration, since
                // `try_send` looked at the queue once and an operator would
                // read an elapsed time as the timeout path's bound.
                Err(mpsc::error::TrySendError::Full(_)) => settled.push(LineReply {
                    id,
                    name,
                    outcome: LineOutcome::NotWritten {
                        reason: "the app is not reading its stdin (its queue was \
                                 already full when this line arrived)"
                            .to_string(),
                    },
                }),
                // Closed: the writer task is gone, so the process is too.
                Err(mpsc::error::TrySendError::Closed(_)) => settled.push(LineReply {
                    id,
                    name,
                    outcome: LineOutcome::NoStdin,
                }),
            }
        }

        spawn_send_line_task(settled, waits, reply);
    }

    /// Puts one action on `id`'s shepherd channel and arms the wait for its
    /// reply, handing back the receiver its outcome will arrive on.
    ///
    /// Infallible: every question an action can be refused over is answered in
    /// [`Self::begin_action`]'s selector pass, before this is called.
    pub(super) fn arm_action(
        &mut self,
        id: u32,
        to_child: mpsc::Sender<ShepherdMessage>,
        action: String,
        params: Option<String>,
        timeout: Duration,
    ) -> oneshot::Receiver<ActionOutcome> {
        let (reply, answer) = oneshot::channel();
        let stamp = self.next_action_stamp;
        self.next_action_stamp += 1;
        let waiter = spawn_action_task(
            id,
            stamp,
            ShepherdMessage::Action {
                name: action.clone(),
                params,
                id: stamp,
            },
            to_child,
            timeout,
            self.tx.clone(),
        );
        // After the task is spawned, and safely: its first act is a send that
        // must reach a child and come back through `run_sheep`, none of which
        // can happen before this handler returns.
        self.sheep
            .get_mut(&id)
            .expect("arm_action: the slot was read a moment ago")
            .actions
            .arm(PendingAction {
                stamp,
                action,
                waiter: Some(waiter),
                reply,
            });
        answer
    }

    /// Forwards one shepherd-channel reply to the action wait it belongs to,
    /// if it belongs to one. A reply with nowhere to go is dropped silently;
    /// see [`ActionWaits::answer`].
    pub(super) fn handle_action_reply(&mut self, id: u32, action: &str, body: String, stamp: Option<u64>) {
        let Some(slot) = self.sheep.get_mut(&id) else {
            return;
        };
        if let Some(waiter) = slot.actions.answer(action, stamp) {
            let _ = waiter.send(body);
        }
    }

    /// An action wait resolved: answer its caller.
    ///
    /// Guarded on the stamp alone, where [`Self::handle_ready_result`] guards
    /// on four things: an action's result changes no flock state, and no
    /// respawn can make the stamp ambiguous.
    pub(super) fn handle_action_result(&mut self, id: u32, stamp: u64, outcome: ActionOutcome) {
        let Some(slot) = self.sheep.get_mut(&id) else {
            return;
        };
        if let Some(reply) = slot.actions.resolve(stamp) {
            let _ = reply.send(outcome);
        }
    }

    /// Spawns the backoff timer for a scheduled restart. `None` still hops
    /// through a task and a mailbox send, so an immediate restart stays
    /// observable as a scheduling step.
    pub(super) fn schedule_restart(&self, id: u32, epoch: u64, delay: Option<Duration>) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let _ = tx.send(Msg::RestartDue { id, epoch }).await;
        });
    }

    /// Removes `id` from every pending reply's `remaining` set, appending
    /// `info` to each match; fulfills (and drops) any pending reply this
    /// empties. Returns `true` iff a `Shutdown` reply was just fulfilled.
    pub(super) fn resolve_pending(&mut self, id: u32, info: ProcessInfo) -> bool {
        let mut shutdown_completed = false;
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].remaining.remove(&id) {
                self.pending[i].results.push(info.clone());
            }
            if self.pending[i].remaining.is_empty() {
                let mut pending = self.pending.remove(i);
                sort_flock(&mut pending.results);
                if matches!(pending.reply, ReplyKind::Shutdown(_)) {
                    shutdown_completed = true;
                }
                send_reply(pending.reply, Ok(pending.results));
            } else {
                i += 1;
            }
        }
        shutdown_completed
    }

    /// One sheep's transition to `Online`: emits the event, then arms every
    /// lifecycle extra its configuration asks for.
    ///
    /// The single arming site, reached by all three transitions. Arming
    /// happens at the transition, not the spawn: a liveness probe armed
    /// against an app that has not finished starting fails its threshold and
    /// restarts the app before it ever comes up.
    pub(super) fn went_online(&mut self, id: u32, info: ProcessInfo, manually: bool) {
        // Whatever an earlier reload concluded about this instance, it is
        // serving now. See `SheepSlot::ready_failed`.
        if let Some(slot) = self.sheep.get_mut(&id) {
            slot.ready_failed = false;
        }
        self.emit(ProcessEventKind::Online, info, manually);
        self.arm_extras(id);
    }

    /// Arms `id`'s lifecycle extras, rebuilding the spec the running process
    /// was spawned from.
    ///
    /// Rebuilding is what makes one arming site possible:
    /// `handle_ready_result` holds an id and nothing else, and `describe` is
    /// pure over a `spec`, `instance` and `credentials` that never change.
    /// The store it reads can move under a running sheep, which is why this
    /// is [`describe`] and not [`assemble`].
    pub(super) fn arm_extras(&mut self, id: u32) {
        let Some(extras) = self.extras.as_ref() else {
            return;
        };
        let Some(slot) = self.sheep.get(&id) else {
            return;
        };
        let supervisor = SupervisorHandle {
            tx: self.tx.clone(),
        };
        // Rebuilt for the prober's sake alone, as `spawn_verify_task` does:
        // nothing spawns this sheep's own program from it. Extras are armed
        // only for a process already running, which resolved its identity to
        // start, so the unresolved arm below cannot be reached from here.
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
        self.registry
            .arm(&slot.entry, spec_prober(&described), extras, &supervisor);
    }

    /// Disarms `id`'s lifecycle extras, and its name-group's cron worker and
    /// watch when `id` was the last armed instance of `name`.
    ///
    /// Called from every terminal transition: `respawn_failed`,
    /// `apply_immediate`'s Stop and Delete arms, and each of `handle_exited`'s
    /// four. `spawn_fresh`'s runner-`Err` arm is the one terminal `Errored`
    /// that does not disarm, its id fresh from `next_id` and never armed; its
    /// refused-assembly arm reaches `respawn_failed` and so does disarm, which
    /// on a never-armed id is the no-op below. A sheep on its
    /// way to `WaitingRestart` keeps its arming, the respawn replacing its
    /// liveness loop.
    ///
    /// Re-disarming is a no-op, so a duplicate site costs nothing and a
    /// missing one leaks a task.
    pub(super) fn disarm_extras(&mut self, id: u32, name: &str) {
        self.registry.disarm(id, name);
    }

    /// Sets `id`'s status and returns its refreshed snapshot.
    pub(super) fn set_status(&mut self, id: u32, status: ProcStatus) -> ProcessInfo {
        let slot = self.sheep.get_mut(&id).expect("set_status: unknown id");
        slot.entry.status = status;
        to_info(&slot.entry, &self.smits)
    }

    /// Paints or clears one sheep's smit and answers with that sheep's
    /// instances as they now stand.
    ///
    /// Every instance of the name, not one row: the map is keyed by name, so
    /// every instance carries the mark.
    pub(super) fn handle_set_smit(
        &mut self,
        conn: ConnId,
        sheep: &str,
        smit: Option<Smit>,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        if !self
            .sheep
            .values()
            .any(|slot| slot.entry.spec.config().name == sheep)
        {
            // Refused rather than stored against a name nothing holds: an
            // orphan entry would show in no listing.
            return Err(SupervisorError::NotFound);
        }
        match smit {
            Some(smit) => {
                self.smits
                    .insert(sheep.to_string(), (conn, smit.to_string()));
            }
            // Only this connection's own; see `Command::SetSmit`'s doc.
            None => {
                if self
                    .smits
                    .get(sheep)
                    .is_some_and(|(painter, _)| *painter == conn)
                {
                    self.smits.remove(sheep);
                }
            }
        }
        Ok(self
            .snapshot_all()
            .into_iter()
            .filter(|info| info.name == sheep)
            .collect())
    }

    /// Full flock listing, grouped by app name.
    ///
    /// Sorted by [`sort_flock`], the one rule every operator-facing listing in
    /// shep takes: name, then instance slot, then id. The slot is in the key
    /// because a replacement gets a fresh id at the drainee's slot number, so
    /// id alone puts slot 0 last. A listing whose rows all report `None` for
    /// [`ProcessInfo::instance`] collapses to `(name, id)` order.
    ///
    /// Applied here once rather than per verb: every listing reply is built
    /// from this function, so the metrics dog and bark read the operator's
    /// order.
    pub(super) fn snapshot_all(&self) -> Vec<ProcessInfo> {
        let mut listing: Vec<ProcessInfo> = self
            .sheep
            .values()
            .map(|slot| to_info(&slot.entry, &self.smits))
            .collect();
        sort_flock(&mut listing);
        listing
    }

    /// Broadcasts one lifecycle transition. Send failures (no receivers)
    /// are not an error: the bus is fire-and-forget from the actor's side.
    pub(super) fn emit(&self, event: ProcessEventKind, info: ProcessInfo, manually: bool) {
        let _ = self.events.send(SharedEvent::new(BusEvent::Process {
            event,
            info,
            manually,
            at_ms: crate::now_ms(),
        }));
    }
}
