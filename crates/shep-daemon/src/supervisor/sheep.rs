//! The per-sheep task.
//!
//! Every registered instance gets one task that owns its `(proc, ProcIo)`
//! pair for the process's whole lifetime, so the actor loop never awaits
//! process IO. `run_sheep` is that task body, and the one place the
//! one-exit-path invariant is enforced: however a sheep ends, the actor
//! hears about it as exactly one `Msg::Exited`.

use super::*;

/// One signal delivery a sheep task is asked to perform, plus where the answer
/// goes.
///
/// A mailbox of its own rather than a [`SheepCtl`] variant: see
/// [`SheepSlot::signals`].
#[derive(Debug)]
pub(super) struct SignalRequest {
    /// What to deliver, to this sheep's own pid.
    pub(super) sig: OperatorSignal,
    /// Fires with what the delivery came to. A dropped sender means the sheep
    /// task ended between the send and the delivery.
    pub(super) done: oneshot::Sender<Result<(), RunnerError>>,
}

/// The two mailboxes a live sheep task listens on.
pub(super) struct SheepHandles {
    /// The kill ladder's, whose one-message-kind invariant is documented on
    /// [`SheepSlot::signals`].
    pub(super) ctl: mpsc::Sender<SheepCtl>,
    /// Signal deliveries.
    pub(super) signals: mpsc::Sender<SignalRequest>,
}

/// Spawns the per-sheep task and returns its two mailbox senders.
pub(super) fn spawn_sheep_task<P: RunningProcess>(
    id: u32,
    proc: P,
    io: ProcIo,
    app: ResolvedApp,
    events: Bus,
    actor_tx: mpsc::Sender<Msg>,
) -> SheepHandles {
    let (ctl_tx, ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    let (signal_tx, signal_rx) = mpsc::channel(SIGNAL_CAPACITY);
    tokio::spawn(run_sheep(
        id, proc, io, app, ctl_rx, signal_rx, events, actor_tx,
    ));
    SheepHandles {
        ctl: ctl_tx,
        signals: signal_tx,
    }
}

/// The per-sheep task body: owns `(proc, io)` for the process's whole lifetime
/// and drains every `ProcIo` channel. Exactly one of the first two `select!`
/// branches ever fires per proc, which is the one-exit-path invariant, after
/// which the task reports `Msg::Exited` and returns.
///
/// A natural exit racing an in-flight `Kill` cannot produce two `Msg::Exited`s
/// or hang a caller: `tokio::select!` picks one ready branch per iteration,
/// that branch alone breaks, and the loop never revisits the other.
///
/// The `if <channel>_open` guards take a closed channel out of consideration;
/// without them its `recv()` resolves to `None` on every poll, busy-spinning
/// the `select!`. Eight parameters: each is threaded through that `select!`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_sheep<P: RunningProcess>(
    id: u32,
    mut proc: P,
    io: ProcIo,
    app: ResolvedApp,
    mut ctl_rx: mpsc::Receiver<SheepCtl>,
    mut signal_rx: mpsc::Receiver<SignalRequest>,
    events: Bus,
    actor_tx: mpsc::Sender<Msg>,
) {
    // Destructured in the task's own body, never in a shorter scope: dropping
    // the log pump's `logs` receiver or its last `log_ctl` sender ends the
    // pump, which drops the read ends of the child's stdout and stderr and
    // gets the child `EPIPE` on its next write.
    let ProcIo {
        mut logs,
        mut from_child,
        to_child,
        log_ctl: _log_ctl,
        to_stdin: _to_stdin,
        //        ^ bound, not `_`: `to_stdin: _` drops the sender inside the
        // `let`, closing the child's stdin at spawn and giving an opted-in app
        // immediate EOF. `SheepSlot` holds a clone, so nothing in the fast
        // loop would show it.
    } = io;
    let mut ctl_open = true;
    let mut logs_open = true;
    let mut from_child_open = true;
    let mut signals_open = true;

    loop {
        tokio::select! {
            outcome = proc.wait() => {
                let _ = actor_tx.send(Msg::Exited { id, outcome }).await;
                break;
            }
            maybe_ctl = ctl_rx.recv(), if ctl_open => {
                match maybe_ctl {
                    Some(SheepCtl::Kill { grace }) => {
                        let outcome =
                            kill_process(&mut proc, app.config(), Some(&to_child), grace).await;
                        let _ = actor_tx.send(Msg::Exited { id, outcome }).await;
                        break;
                    }
                    None => ctl_open = false,
                }
            }
            maybe_signal = signal_rx.recv(), if signals_open => {
                match maybe_signal {
                    Some(SignalRequest { sig, done }) => {
                        // Delivered from the task that owns the proc, never
                        // from the actor off a recorded pid: only the owning
                        // task knows the child has not been reaped, which
                        // closes the pid-reuse ABA race.
                        let _ = done.send(proc.signal_process(sig));
                    }
                    None => signals_open = false,
                }
            }
            maybe_line = logs.recv(), if logs_open => {
                match maybe_line {
                    Some(line) => {
                        // Through the bus's own gate rather than `send`: with
                        // nobody subscribed this costs one relaxed load, where
                        // a publish costs an allocation, a mutex and a wakeup.
                        events.publish_log(if line.err {
                            BusEvent::LogErr { id, line: line.line }
                        } else {
                            BusEvent::LogOut { id, line: line.line }
                        });
                    }
                    None => logs_open = false,
                }
            }
            maybe_msg = from_child.recv(), if from_child_open => {
                match maybe_msg {
                    Some(message) => {
                        // Forwarded before it is acted on, and
                        // unconditionally: a subscriber's view of fd 3 must
                        // not depend on this daemon having a consumer for that
                        // kind.
                        let _ = events.send(SharedEvent::new(BusEvent::Channel {
                            id,
                            message: message.clone(),
                        }));
                        match message {
                            ChildMessage::Ready => {
                                let _ = actor_tx.send(Msg::Ready { id }).await;
                            }
                            ChildMessage::Metric { name, value } => {
                                tracing::debug!(
                                    id,
                                    name,
                                    value,
                                    "child metric forwarded to the bus as channel.metric"
                                );
                            }
                            ChildMessage::ActionReply {
                                action,
                                body,
                                // The child's `id` is the dispatch's, not the
                                // sheep's. Renamed at the boundary so no line
                                // downstream holds both meanings.
                                id: stamp,
                            } => {
                                let _ = actor_tx
                                    .send(Msg::ActionReply { id, action, body, stamp })
                                    .await;
                            }
                        }
                    }
                    None => from_child_open = false,
                }
            }
        }
    }
}
