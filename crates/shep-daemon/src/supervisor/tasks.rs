//! Work the actor hands off rather than awaiting itself.
//!
//! Readiness probes, action and trigger dispatch, signals and stdin writes
//! all block for as long as the sheep takes to answer. Each gets a task that
//! reports back as a `Msg`, so a slow or wedged child never holds the
//! mailbox.

use super::*;

/// The prober a gated readiness task, or a sheep's liveness loop, probes with:
/// a fresh [`OsProber`] scoped to the assembled spec's `cwd`/`env`.
///
/// Taking the [`SpawnSpec`] rather than the [`ResolvedApp`] it was assembled
/// from: `probe_exec` runs `env_clear().envs(&self.env)`, and `config.env` is
/// one of three things [`assemble`] folds into the child's environment, so an
/// app that sets no `env` would probe with no `PATH`. A `&ResolvedApp` also
/// cannot reach `instance`, so every instance would probe the same port.
pub(super) fn spec_prober(spec: &SpawnSpec) -> Arc<dyn Prober> {
    Arc::new(OsProber::new(spec.cwd.clone(), spec.env.clone()))
}

/// Spawns a readiness task for `id` at `epoch`, returning the oneshot sender
/// the actor stores (`SheepSlot::ready_tx`) so a later `Msg::Ready` can wake
/// it. `source` decides which signal [`await_ready`] waits for; `deadline` is
/// the app's `listen_timeout`. The task reports back through `actor_tx` as a
/// `Msg::ReadyResult`, which `Actor::handle_ready_result` drops against a
/// stale `epoch`.
///
/// `manually` is carried, never inspected here.
///
/// Must be called from within a Tokio runtime context.
pub(super) fn spawn_readiness_task(
    id: u32,
    epoch: u64,
    manually: bool,
    source: ReadinessSource,
    deadline: Duration,
    prober: Arc<dyn Prober>,
    actor_tx: mpsc::Sender<Msg>,
) -> oneshot::Sender<()> {
    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(async move {
        let readiness = await_ready(&source, deadline, ready_rx, prober).await;
        let _ = actor_tx
            .send(Msg::ReadyResult {
                id,
                epoch,
                manually,
                readiness,
            })
            .await;
    });
    ready_tx
}

/// Spawns the task that delivers one action to `id`'s child and waits for the
/// reply, returning the oneshot sender the actor stores
/// (`PendingAction::waiter`) so a later [`Msg::ActionReply`] can wake it. The
/// task reports back through `actor_tx` as a [`Msg::ActionResult`].
///
/// The send is in here rather than at the call site because both it and the
/// wait are awaits, which the actor loop may not do. `deadline` covers the two
/// together, since a child that has stopped reading can stall either.
///
/// Only [`ActionOutcome::Replied`] proves the action landed. A failed send or
/// a dropped waiter is [`ActionOutcome::NoChannel`]; a send that succeeds
/// proves nothing, the first one after a child exits being discarded.
pub(super) fn spawn_action_task(
    id: u32,
    stamp: u64,
    message: ShepherdMessage,
    to_child: mpsc::Sender<ShepherdMessage>,
    deadline: Duration,
    actor_tx: mpsc::Sender<Msg>,
) -> oneshot::Sender<String> {
    let (reply_tx, reply_rx) = oneshot::channel();
    tokio::spawn(async move {
        let delivered = tokio::time::timeout(deadline, async move {
            // The send is inside the deadline rather than ahead of it: a
            // child that has stopped reading fd 3 backs its socket up, and an
            // unbounded send would park this task, and its caller, for as long
            // as that child stays wedged.
            if to_child.send(message).await.is_err() {
                return None;
            }
            reply_rx.await.ok()
        })
        .await;
        let outcome = match delivered {
            Ok(Some(body)) => ActionOutcome::Replied { body },
            Ok(None) => ActionOutcome::NoChannel,
            Err(_elapsed) => ActionOutcome::TimedOut,
        };
        let _ = actor_tx
            .send(Msg::ActionResult { id, stamp, outcome })
            .await;
    });
    reply_tx
}

/// Spawns the task that collects one trigger's rows and answers its caller,
/// folding in the sheep already refused in [`Actor::begin_action`]. Must be
/// called from within a Tokio runtime context.
///
/// Awaiting them in a loop is not serial: every wait in `waits` is already
/// running its own task under its own deadline, so the loop is a join. Ten
/// unresponsive apps cost the longest `action_timeout`, not the sum.
///
/// A wait whose sender is dropped rather than answered reports
/// [`ActionOutcome::NoChannel`]: the sender lives on the sheep's slot, so
/// losing it means that slot let go of the wait.
pub(super) fn spawn_trigger_task(
    mut rows: Vec<ActionReply>,
    waits: Vec<(u32, String, oneshot::Receiver<ActionOutcome>)>,
    reply: oneshot::Sender<Result<Vec<ActionReply>, SupervisorError>>,
) {
    tokio::spawn(async move {
        for (id, name, answer) in waits {
            let outcome = answer.await.unwrap_or(ActionOutcome::NoChannel);
            rows.push(ActionReply { id, name, outcome });
        }
        // Keyed by name with the id breaking ties, so `shep flock` and `shep
        // trigger` do not read two orders. Parity with `sort_flock` is partial
        // and cannot be closed here: its key is `(name, instance, id)` and
        // `ActionReply` carries no slot.
        rows.sort_unstable_by(|a, b| (a.name.as_str(), a.id).cmp(&(b.name.as_str(), b.id)));
        let _ = reply.send(Ok(rows));
    });
}

/// One matched sheep's pending signal delivery: its id, name, and the receiver
/// its outcome will arrive on. An alias for `clippy::type_complexity`.
type SignalWait = (u32, String, oneshot::Receiver<Result<(), RunnerError>>);

/// Spawns the task that collects one signal's rows and answers its caller,
/// folding in the sheep already settled in [`Actor::begin_signal`]. Mirrors
/// [`spawn_trigger_task`], except that a dropped `done` sender, the sheep task
/// ending between the send and the delivery, reports
/// [`SignalOutcome::NotRunning`] rather than `NoChannel`.
pub(super) fn spawn_signal_task(
    mut rows: Vec<SignalReply>,
    waits: Vec<SignalWait>,
    reply: oneshot::Sender<Result<Vec<SignalReply>, SupervisorError>>,
) {
    tokio::spawn(async move {
        for (id, name, answer) in waits {
            let outcome = match answer.await {
                Ok(Ok(())) => SignalOutcome::Delivered,
                Ok(Err(err)) => SignalOutcome::Failed {
                    reason: err.to_string(),
                },
                Err(_dropped) => SignalOutcome::NotRunning,
            };
            rows.push(SignalReply { id, name, outcome });
        }
        // Name then id, per `spawn_trigger_task`.
        rows.sort_unstable_by(|a, b| (a.name.as_str(), a.id).cmp(&(b.name.as_str(), b.id)));
        let _ = reply.send(Ok(rows));
    });
}

/// One matched sheep's arming: its id and name, and the receiver its write's
/// acknowledgement will arrive on.
type LineWait = (u32, String, oneshot::Receiver<Result<(), RunnerError>>);

/// Awaits every write's acknowledgement, each under its own
/// [`STDIN_WRITE_TIMEOUT`], and answers `reply` with `settled` and the results
/// by name and then by id.
///
/// `join_all`, not a `for` loop: every wait here shares one bound, so awaiting
/// them in turn would make the total `STDIN_WRITE_TIMEOUT * matched` and put a
/// flock-wide `sendline` over an RPC caller's default budget.
pub(super) fn spawn_send_line_task(
    settled: Vec<LineReply>,
    waits: Vec<LineWait>,
    reply: oneshot::Sender<Result<Vec<LineReply>, SupervisorError>>,
) {
    tokio::spawn(async move {
        let mut rows = settled;
        rows.extend(
            futures_util::future::join_all(waits.into_iter().map(
                |(id, name, answer)| async move {
                    let outcome = match tokio::time::timeout(STDIN_WRITE_TIMEOUT, answer).await {
                        Ok(Ok(Ok(()))) => LineOutcome::Sent,
                        Ok(Ok(Err(err))) => LineOutcome::NotWritten {
                            reason: err.to_string(),
                        },
                        // The sender was dropped: the writer task ended before it
                        // served this request, which means the process did too.
                        Ok(Err(_recv)) => LineOutcome::NoStdin,
                        // The shepherd stopped waiting; it did not stop the
                        // write. The bytes may still land in full when the app
                        // drains, so the reason says so rather than letting an
                        // operator retry into a double delivery.
                        Err(_elapsed) => LineOutcome::NotWritten {
                            reason: format!(
                                "the app did not read its stdin within {}s; this line \
                                 may still land if it drains",
                                STDIN_WRITE_TIMEOUT.as_secs()
                            ),
                        },
                    };
                    LineReply { id, name, outcome }
                },
            ))
            .await,
        );
        // Name then id, per `spawn_trigger_task`.
        rows.sort_unstable_by(|a, b| (a.name.as_str(), a.id).cmp(&(b.name.as_str(), b.id)));
        let _ = reply.send(Ok(rows));
    });
}
