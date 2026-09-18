use super::*;
use futures_util::StreamExt;
use shep_core::protocol::{
    BusEvent, Envelope, HelloAck, ProcessInfo, Request, Response, RpcErrorCode, codec, decode_frame,
};
use shep_core::transport::Listener;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

/// Depth of a [`FakeDaemon`]'s script channel and of
/// [`fake_client_capturing_envelopes`]'s capture channel: generous and
/// untuned, like `shep-daemon`'s own `CHANNEL_CAPACITY`.
pub(super) const SCRIPT_CHANNEL_CAPACITY: usize = 8;

/// One scripted step sent to a [`FakeDaemon`]'s background task.
///
/// [`FakeDaemon::reply_to_list`] and [`FakeDaemon::queue_reply_then_event`]
/// go through a `Mutex`-backed flag instead, since every call site invokes
/// them without `.await`. This enum carries the rest, each armed at most
/// once.
pub(super) enum ScriptCommand {
    /// Arms the next request to receive an [`RpcError`](shep_core::protocol::RpcError) with this `code`
    /// and `message` instead of a normal response.
    ReplyErr(RpcErrorCode, String),
    /// Arms the next request to receive a [`sample_info`]-based
    /// `BusEvent::Process` before its `Pong` reply.
    EventThenReply,
    /// Buffers the next two requests, then answers the `ListFlock` one
    /// first and the other second, regardless of arrival order.
    ArmOutOfOrder,
    /// Queues one [`BusEvent`] for this connection's subscriber, buffered
    /// until the connection has answered a `Request::Subscribe`.
    ///
    /// Boxed: `BusEvent::Process` carries a `ProcessInfo`, which is past
    /// clippy's `large_enum_variant` threshold; every other variant here is
    /// a few bytes.
    PushEvent(Box<BusEvent>),
    /// Ends the script: stop serving and let the task return.
    Close,
    /// Ends the script once this connection has answered its next
    /// `Request::Subscribe` and flushed anything queued via
    /// [`FakeDaemon::push`].
    CloseAfterSubscribe,
}

/// [`ScriptCommand::ArmOutOfOrder`]'s progress: idle, armed (waiting for
/// the first of the two requests), or holding the first request while it
/// waits for the second.
pub(super) enum OutOfOrder {
    /// Not armed; requests are answered by the normal script.
    Idle,
    /// Armed; the next request received is buffered rather than answered.
    Armed,
    /// The first of the two requests, buffered until the second arrives.
    Buffered(Envelope),
}

/// A scripted daemon over one accepted connection. Every request not
/// covered by an armed method answers `Response::Pong`.
///
/// A few `fake_client_*` constructors below arm a one-shot error, an
/// out-of-order reply, or an event-before-reply via a private
/// `ScriptCommand`, since nothing outside this module needs to arm those
/// directly.
#[derive(Debug)]
pub struct FakeDaemon {
    pub(super) script: mpsc::Sender<ScriptCommand>,
    pub(super) armed_list: Arc<Mutex<Option<Vec<ProcessInfo>>>>,
    pub(super) armed_list_sequence: Arc<Mutex<VecDeque<Vec<ProcessInfo>>>>,
    pub(super) armed_describe: Arc<Mutex<Option<Vec<ProcessInfo>>>>,
    pub(super) armed_reply_then_event: Arc<Mutex<Option<(Response, BusEvent)>>>,
    pub(super) armed_shutdown_then_unlink: Arc<Mutex<Option<Duration>>>,
    pub(super) armed_shutdown_never_unlink: Arc<Mutex<Option<()>>>,
    pub(super) list_flock_count: Arc<AtomicU64>,
    pub(super) task: JoinHandle<()>,
}

/// The [`FakeDaemon`] background task: accepts one connection, handshakes
/// with `ack`, then answers requests until [`ScriptCommand::Close`] arrives
/// or the connection ends.
///
/// Checked in priority order per request: a buffered out-of-order request,
/// then an armed error, event or shutdown script, then `Subscribe`, then
/// `ListFlock`/`Describe`, falling back to `Response::Pong`.
///
/// One `Arc<Mutex<..>>` slot per independently armed behavior, cloned from
/// [`FakeDaemon`]'s own fields. Eleven parameters: bundling them into a
/// struct would move the coupling around, not reduce it, for this
/// private, one-caller function.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_scripted(
    mut listener: Listener,
    socket_path: PathBuf,
    ack: HelloAck,
    mut script: mpsc::Receiver<ScriptCommand>,
    armed_list: Arc<Mutex<Option<Vec<ProcessInfo>>>>,
    armed_list_sequence: Arc<Mutex<VecDeque<Vec<ProcessInfo>>>>,
    armed_describe: Arc<Mutex<Option<Vec<ProcessInfo>>>>,
    armed_reply_then_event: Arc<Mutex<Option<(Response, BusEvent)>>>,
    armed_shutdown_then_unlink: Arc<Mutex<Option<Duration>>>,
    armed_shutdown_never_unlink: Arc<Mutex<Option<()>>>,
    list_flock_count: Arc<AtomicU64>,
) {
    let stream = listener.accept().await.unwrap();
    let mut frames = Framed::new(stream, codec());
    let _hello = handshake(&mut frames, ack).await;

    let mut armed_err: Option<(RpcErrorCode, String)> = None;
    let mut armed_event_then_reply = false;
    let mut out_of_order = OutOfOrder::Idle;
    // Before `Subscribe` is answered, a pushed event queues here: the
    // client's `broadcast::Receiver` does not exist yet.
    let mut subscribed = false;
    let mut pending_events: Vec<BusEvent> = Vec::new();
    let mut close_after_subscribe = false;

    loop {
        tokio::select! {
            command = script.recv() => {
                match command {
                    Some(ScriptCommand::ReplyErr(code, message)) => armed_err = Some((code, message)),
                    Some(ScriptCommand::EventThenReply) => armed_event_then_reply = true,
                    Some(ScriptCommand::ArmOutOfOrder) => out_of_order = OutOfOrder::Armed,
                    Some(ScriptCommand::PushEvent(event)) => {
                        if subscribed {
                            write_event(&mut frames, *event).await;
                        } else {
                            pending_events.push(*event);
                        }
                    }
                    Some(ScriptCommand::CloseAfterSubscribe) => {
                        close_after_subscribe = true;
                        // `select!`'s arms are unbiased: `Subscribe` may
                        // already be handled, so re-check and close now if
                        // it already happened.
                        if subscribed {
                            break;
                        }
                    }
                    Some(ScriptCommand::Close) | None => break,
                }
            }
            frame = frames.next() => {
                let Some(Ok(frame)) = frame else { break };
                let envelope: Envelope = decode_frame(&frame).unwrap();

                match std::mem::replace(&mut out_of_order, OutOfOrder::Idle) {
                    OutOfOrder::Armed => {
                        out_of_order = OutOfOrder::Buffered(envelope);
                    }
                    OutOfOrder::Buffered(first) => {
                        let (list_env, other_env) = if matches!(first.body, Request::ListFlock) {
                            (first, envelope)
                        } else {
                            (envelope, first)
                        };
                        write_reply(&mut frames, list_env.id, Response::Flock(Vec::new())).await;
                        write_reply(&mut frames, other_env.id, Response::Pong).await;
                    }
                    OutOfOrder::Idle => {
                        // Taken into owned locals before any `.await`:
                        // `MutexGuard` is not `Send`, and `tokio::spawn`
                        // requires the future to be.
                        let reply_then_event = armed_reply_then_event.lock().unwrap().take();
                        let shutdown_then_unlink =
                            armed_shutdown_then_unlink.lock().unwrap().take();
                        let shutdown_never_unlink =
                            armed_shutdown_never_unlink.lock().unwrap().take();
                        if let Some((reply, event)) = reply_then_event {
                            write_reply(&mut frames, envelope.id, reply).await;
                            write_event(&mut frames, event).await;
                            subscribed = true;
                        } else if let Some((code, message)) = armed_err.take() {
                            write_err(&mut frames, envelope.id, code, message).await;
                        } else if armed_event_then_reply {
                            armed_event_then_reply = false;
                            send_sample_event(&mut frames).await;
                            write_reply(&mut frames, envelope.id, Response::Pong).await;
                        } else if let Some(after) = shutdown_then_unlink {
                            write_reply(&mut frames, envelope.id, Response::ShuttingDown).await;
                            // Run inline: `kill` drops the `Client` right
                            // after this reply, closing the connection
                            // before a deferred unlink would get a turn.
                            tokio::time::sleep(after).await;
                            let _ = std::fs::remove_file(&socket_path);
                        } else if shutdown_never_unlink.is_some() {
                            write_reply(&mut frames, envelope.id, Response::ShuttingDown).await;
                            // Never unlinks: the branch a kill-teardown
                            // timeout exists to observe.
                        } else if matches!(envelope.body, Request::Subscribe { .. }) {
                            write_reply(&mut frames, envelope.id, Response::Subscribed).await;
                            subscribed = true;
                            for event in pending_events.drain(..) {
                                write_event(&mut frames, event).await;
                            }
                            if close_after_subscribe {
                                break;
                            }
                        } else {
                            let response = if matches!(envelope.body, Request::ListFlock) {
                                list_flock_count.fetch_add(1, Ordering::SeqCst);
                                // The sequence queue takes priority over a
                                // single `reply_to_list` slot armed at the
                                // same time.
                                let next = armed_list_sequence.lock().unwrap().pop_front();
                                Response::Flock(next.unwrap_or_else(|| {
                                    armed_list.lock().unwrap().take().unwrap_or_default()
                                }))
                            } else if matches!(envelope.body, Request::Describe { .. }) {
                                Response::Described(
                                    armed_describe.lock().unwrap().take().unwrap_or_default(),
                                )
                            } else {
                                Response::Pong
                            };
                            write_reply(&mut frames, envelope.id, response).await;
                        }
                    }
                }
            }
        }
    }
}

impl FakeDaemon {
    /// Arms the answer to the next `Request::ListFlock` this connection
    /// receives.
    ///
    /// Synchronous: an `async fn` here would trip `unused_must_use` since
    /// every call site invokes it without `.await`.
    pub fn reply_to_list(&self, flock: Vec<ProcessInfo>) {
        *self.armed_list.lock().unwrap() = Some(flock);
    }

    /// Arms a whole sequence of `Request::ListFlock` answers at once: the
    /// first call gets `responses[0]`, the second `responses[1]`, and so on.
    /// Once the queue empties, [`Self::reply_to_list`]'s single-slot arming
    /// takes back over.
    ///
    /// Synchronous, like [`Self::reply_to_list`].
    pub fn reply_to_list_sequence(&self, responses: Vec<Vec<ProcessInfo>>) {
        *self.armed_list_sequence.lock().unwrap() = responses.into();
    }

    /// Arms the answer to the next `Request::Describe { .. }` this
    /// connection receives, regardless of the selector inside it.
    ///
    /// Synchronous, like [`Self::reply_to_list`].
    pub fn reply_to_describe(&self, procs: Vec<ProcessInfo>) {
        *self.armed_describe.lock().unwrap() = Some(procs);
    }

    /// How many `Request::ListFlock` envelopes this connection has
    /// answered so far.
    #[must_use]
    pub fn list_flock_count(&self) -> u64 {
        self.list_flock_count.load(Ordering::SeqCst)
    }

    /// Arms the reply this connection sends for its next request, then
    /// immediately follows it with `event` written directly to the wire:
    /// matches the ordering a real subscribe produces, reply ahead of any
    /// event.
    ///
    /// Synchronous, like [`Self::reply_to_list`].
    pub fn queue_reply_then_event(&self, reply: Response, event: BusEvent) {
        *self.armed_reply_then_event.lock().unwrap() = Some((reply, event));
    }

    /// Arms the next request to be answered `Response::ShuttingDown`; this
    /// connection then waits `after` and unlinks its socket file.
    ///
    /// Synchronous, like [`Self::reply_to_list`].
    pub fn reply_shutting_down_then_unlink_after(&self, after: Duration) {
        *self.armed_shutdown_then_unlink.lock().unwrap() = Some(after);
    }

    /// Arms the next request to be answered `Response::ShuttingDown` and
    /// then nothing: the socket file stays.
    ///
    /// Synchronous, like [`Self::reply_to_list`].
    pub fn reply_shutting_down_and_never_unlink(&self) {
        *self.armed_shutdown_never_unlink.lock().unwrap() = Some(());
    }

    /// Queues `event` for this connection's subscriber.
    ///
    /// Buffered in arrival order until the connection has answered a
    /// `Request::Subscribe`, since a `broadcast::Receiver` never sees a
    /// value sent before it existed. Written straight to the wire once
    /// subscribed.
    ///
    /// Silently does nothing if the background task has already ended.
    pub async fn push(&self, event: BusEvent) {
        let _ = self
            .script
            .send(ScriptCommand::PushEvent(Box::new(event)))
            .await;
    }

    /// Pushes `EVENT_CHANNEL_CAPACITY + n` [`BusEvent::LogOut`] events in
    /// one go, enough to force a local lag notice instead of ordinary
    /// delivery.
    pub async fn overrun_by(&self, n: usize) {
        for i in 0..(crate::actor::EVENT_CHANNEL_CAPACITY + n) {
            self.push(BusEvent::LogOut {
                id: 1,
                line: i.to_string(),
            })
            .await;
        }
    }

    /// Ends the script and drops the connection: drains anything still
    /// queued, then lets the background task finish.
    ///
    /// Panics if the background task is gone or panicked.
    pub async fn close(self) {
        let _ = self.script.send(ScriptCommand::Close).await;
        self.task.await.unwrap();
    }

    /// Arms this connection to close itself once it has answered its next
    /// `Request::Subscribe` and flushed anything [`Self::push`] queued
    /// beforehand, unlike calling [`Self::close`] before the subscription
    /// exists.
    ///
    /// Goes through the script channel rather than a `Mutex` flag: it arms
    /// behavior on the same background task that drains [`Self::push`]'s
    /// queue.
    ///
    /// Does not consume `self`, unlike [`Self::close`]: a caller may still
    /// want [`Self::list_flock_count`] afterward.
    pub async fn close_after_subscribe(&self) {
        let _ = self.script.send(ScriptCommand::CloseAfterSubscribe).await;
    }
}
