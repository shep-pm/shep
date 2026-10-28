use super::*;
use crate::{Client, ReconnectingClient};
use futures_util::StreamExt;
use shep_core::protocol::{
    Envelope, HelloAck, Request, Response, RpcError, RpcErrorCode, codec, decode_frame,
};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

/// Binds `path`, handshakes with [`sample_ack`], and hands back a connected
/// [`Client`] alongside the still-live [`FakeDaemon`] script, for a test
/// that needs nothing daemon-specific.
pub async fn fake_client_on(path: &Path) -> (Client, FakeDaemon) {
    fake_client_with_ack(path, sample_ack()).await
}

/// As [`fake_client_on`], but with a caller-chosen [`HelloAck`], for a
/// test asserting on the ack a `Client` receives.
pub async fn fake_client_with_ack(path: &Path, ack: HelloAck) -> (Client, FakeDaemon) {
    let daemon = fake_daemon_scripted_on(path, ack);
    let client = Client::connect(path).await.unwrap();
    (client, daemon)
}

/// As [`fake_client_on`], but hands back a [`ReconnectingClient`], the
/// supervised wrapper a dog uses, instead of a bare [`Client`].
///
/// The [`FakeDaemon`] behind it still serves exactly one connection, so a
/// test that cuts it leaves the supervisor retrying against a gone
/// listener. Use [`fake_daemon_across_handovers`] to test the reconnect
/// itself.
pub async fn fake_reconnecting_client_on(path: &Path) -> (ReconnectingClient, FakeDaemon) {
    let daemon = fake_daemon_scripted_on(path, sample_ack());
    let client = ReconnectingClient::connect(path).await.unwrap();
    (client, daemon)
}

/// Binds `path` and starts the scripted fake without connecting a client
/// of its own, for a caller that performs its own connect.
///
/// Synchronous: the listener is bound before this returns, so a caller can
/// connect straight away without a sleep.
///
/// Panics if `path` cannot be bound.
#[must_use]
pub fn fake_daemon_scripted_on(path: &Path, ack: HelloAck) -> FakeDaemon {
    let listener = bind(path);
    let (script_tx, script_rx) = mpsc::channel(SCRIPT_CHANNEL_CAPACITY);
    let armed_list = Arc::new(Mutex::new(None));
    let armed_list_sequence = Arc::new(Mutex::new(VecDeque::new()));
    let armed_describe = Arc::new(Mutex::new(None));
    let armed_reply_then_event = Arc::new(Mutex::new(None));
    let armed_shutdown_then_unlink = Arc::new(Mutex::new(None));
    let armed_shutdown_never_unlink = Arc::new(AtomicBool::new(false));
    let list_flock_count = Arc::new(AtomicU64::new(0));
    let task = tokio::spawn(serve_scripted(
        listener,
        path.to_path_buf(),
        ack,
        script_rx,
        Arc::clone(&armed_list),
        Arc::clone(&armed_list_sequence),
        Arc::clone(&armed_describe),
        Arc::clone(&armed_reply_then_event),
        Arc::clone(&armed_shutdown_then_unlink),
        Arc::clone(&armed_shutdown_never_unlink),
        Arc::clone(&list_flock_count),
    ));
    FakeDaemon {
        script: script_tx,
        armed_list,
        armed_list_sequence,
        armed_describe,
        armed_reply_then_event,
        armed_shutdown_then_unlink,
        armed_shutdown_never_unlink,
        list_flock_count,
        task,
    }
}

/// Identical to [`fake_client_on`], named separately for tests whose whole
/// point is [`FakeDaemon::push`], [`FakeDaemon::overrun_by`] or
/// [`FakeDaemon::queue_reply_then_event`].
pub async fn fake_client_with_push(path: &Path) -> (Client, FakeDaemon) {
    fake_client_on(path).await
}

/// Binds `path` and serves every connection made to it. It handshakes with
/// `ack`, answers each request through `answer`, and forwards each
/// [`Envelope`] to the channel.
///
/// Unlike [`fake_client_answering`], which accepts exactly one connection:
/// a CLI verb opens its own. The task is detached, and runs until the
/// listener errors when the caller's `TempDir` goes away.
pub async fn fake_daemon_answering_with_ack(
    path: &Path,
    ack: HelloAck,
    answer: impl Fn(&Request) -> Response + Send + Sync + Clone + 'static,
) -> mpsc::UnboundedReceiver<Envelope> {
    let mut listener = bind(path);
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(stream) = listener.accept().await {
            let tx = tx.clone();
            let ack = ack.clone();
            let answer = answer.clone();
            tokio::spawn(async move {
                let mut frames = Framed::new(stream, codec());
                let _hello = handshake(&mut frames, ack).await;
                while let Some(Ok(frame)) = frames.next().await {
                    let Ok(envelope) = decode_frame::<Envelope>(&frame) else {
                        break;
                    };
                    let id = envelope.id;
                    let reply = answer(&envelope.body);
                    if tx.send(envelope).is_err() {
                        break;
                    }
                    write_reply(&mut frames, id, reply).await;
                }
            });
        }
    });
    rx
}

/// As [`fake_client_capturing_envelopes`], but `answer` decides each reply,
/// called with each decoded [`Request`] in arrival order. The envelope
/// reaches the channel before the reply is written.
///
/// The channel is unbounded, unlike every other one in this module: a test
/// reads these envelopes only after the call under test returns. The read
/// loop ends on a closed connection rather than panicking.
pub async fn fake_client_answering(
    path: &Path,
    answer: impl Fn(&Request) -> Response + Send + 'static,
) -> (Client, mpsc::UnboundedReceiver<Envelope>) {
    let mut listener = bind(path);
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut frames = accept_handshaked(&mut listener, sample_ack()).await;
        while let Some(Ok(frame)) = frames.next().await {
            let Ok(envelope) = decode_frame::<Envelope>(&frame) else {
                break;
            };
            let id = envelope.id;
            let reply = answer(&envelope.body);
            if tx.send(envelope).is_err() {
                break;
            }
            write_reply(&mut frames, id, reply).await;
        }
    });
    let client = Client::connect(path).await.unwrap();
    (client, rx)
}

/// As [`fake_client_answering`], but `answer` may refuse: an `Err` is
/// written as the reply's [`RpcError`], for a caller that has to see a
/// daemon-side refusal come back as a failed request rather than as an
/// unexpected [`Response`].
///
/// `answer` is called per request in arrival order, so it can refuse one
/// request of a multi-request verb and answer the rest.
pub async fn fake_client_answering_refusals(
    path: &Path,
    answer: impl Fn(&Request) -> Result<Response, RpcError> + Send + 'static,
) -> (Client, mpsc::UnboundedReceiver<Envelope>) {
    let mut listener = bind(path);
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut frames = accept_handshaked(&mut listener, sample_ack()).await;
        while let Some(Ok(frame)) = frames.next().await {
            let Ok(envelope) = decode_frame::<Envelope>(&frame) else {
                break;
            };
            let id = envelope.id;
            let answer = answer(&envelope.body);
            // Forwarded before the reply is written, as in
            // [`fake_client_answering`]: a test reads these envelopes after
            // the call under test returns.
            if tx.send(envelope).is_err() {
                break;
            }
            match answer {
                Ok(response) => write_reply(&mut frames, id, response).await,
                Err(err) => write_err(&mut frames, id, err.code, err.message).await,
            }
        }
    });
    let client = Client::connect(path).await.unwrap();
    (client, rx)
}

/// Binds `path`, handshakes with [`sample_ack`], and answers every request
/// with `Response::Pong` while forwarding each decoded [`Envelope`] onto
/// the returned channel, for asserting on what a `Client` puts on the wire
/// rather than on how the daemon answers.
pub async fn fake_client_capturing_envelopes(path: &Path) -> (Client, mpsc::Receiver<Envelope>) {
    let mut listener = bind(path);
    let (tx, rx) = mpsc::channel(SCRIPT_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        let mut frames = accept_handshaked(&mut listener, sample_ack()).await;
        loop {
            let envelope = read_envelope(&mut frames).await;
            let id = envelope.id;
            // Forwarded before the reply is sent, so an awaited
            // `Client::request` future finds its envelope already queued.
            if tx.send(envelope).await.is_err() {
                break;
            }
            write_reply(&mut frames, id, Response::Pong).await;
        }
    });
    let client = Client::connect(path).await.unwrap();
    (client, rx)
}

/// Binds `path`, handshakes with [`sample_ack`], and answers the one
/// request that arrives with an [`RpcError`] carrying `code` and `message`.
///
/// Backed by a [`FakeDaemon`] so the connection keeps serving afterward,
/// unlike a bespoke one-shot task that would die after the scripted reply.
pub async fn fake_client_replying_err(
    path: &Path,
    code: RpcErrorCode,
    message: &str,
) -> (Client, FakeDaemon) {
    let (client, daemon) = fake_client_on(path).await;
    daemon
        .script
        .send(ScriptCommand::ReplyErr(code, message.to_string()))
        .await
        .unwrap();
    (client, daemon)
}

/// Binds `path`, handshakes with [`sample_ack`], reads exactly two
/// envelopes, then answers the `ListFlock` one first and the `Ping` one
/// second, regardless of arrival order: proof that a `Client` routes
/// replies by id.
///
/// Backed by a [`FakeDaemon`], like [`fake_client_replying_err`].
pub async fn fake_client_out_of_order(path: &Path) -> (Client, FakeDaemon) {
    let (client, daemon) = fake_client_on(path).await;
    daemon
        .script
        .send(ScriptCommand::ArmOutOfOrder)
        .await
        .unwrap();
    (client, daemon)
}

/// Binds `path`, handshakes with [`sample_ack`], reads one envelope, and
/// sends a `BusEvent::Process` event before answering it: a sheep's bus
/// event can legitimately arrive ahead of the reply for the request that
/// caused it.
///
/// Backed by a [`FakeDaemon`], like [`fake_client_replying_err`].
pub async fn fake_client_event_then_reply(path: &Path) -> (Client, FakeDaemon) {
    let (client, daemon) = fake_client_on(path).await;
    daemon
        .script
        .send(ScriptCommand::EventThenReply)
        .await
        .unwrap();
    (client, daemon)
}

/// Binds `path`, handshakes with [`sample_ack`], then immediately drops the
/// connection, for testing that a `Client` fails every pending request with
/// `RequestError::Closed` rather than hanging.
pub async fn fake_client_that_closes_after_handshake(path: &Path) -> (Client, JoinHandle<()>) {
    let mut listener = bind(path);
    let task = tokio::spawn(async move {
        let _frames = accept_handshaked(&mut listener, sample_ack()).await;
        // Dropping `_frames` here closes the connection from this side.
    });
    let client = Client::connect(path).await.unwrap();
    (client, task)
}

/// Binds `path`, handshakes with [`sample_ack`], reads exactly one
/// envelope, then drops the connection without replying: for testing that
/// a request already accepted into the connection actor's `pending` map
/// fails with `RequestError::Closed` when the connection dies mid-flight.
/// Unlike [`fake_client_that_closes_after_handshake`], which never accepts
/// the request at all.
pub async fn fake_client_that_dies_mid_request(path: &Path) -> (Client, JoinHandle<()>) {
    let mut listener = bind(path);
    let task = tokio::spawn(async move {
        let mut frames = accept_handshaked(&mut listener, sample_ack()).await;
        let _envelope = read_envelope(&mut frames).await;
        // Dropping `frames` here, after reading, closes the connection
        // only once the actor has recorded the request as pending.
    });
    let client = Client::connect(path).await.unwrap();
    (client, task)
}

/// Binds `path`, handshakes with [`sample_ack`], then reads nothing and
/// replies to nothing, ever: for testing a `Client`'s own deadline against
/// a daemon that accepted the connection but stopped answering.
///
/// Returns `(Client, JoinHandle<()>)`, not `(Client, FakeDaemon)`:
/// `FakeDaemon`'s loop always answers some request promptly, and no
/// `ScriptCommand` means never answering.
pub async fn fake_client_that_never_replies(path: &Path) -> (Client, JoinHandle<()>) {
    let mut listener = bind(path);
    let task = tokio::spawn(async move {
        let _frames = accept_handshaked(&mut listener, sample_ack()).await;
        core::future::pending::<()>().await;
    });
    let client = Client::connect(path).await.unwrap();
    (client, task)
}

/// Binds `path`, handshakes with [`sample_ack`], and answers each request
/// with `response`. A request whose deadline is shorter than `needs` gets
/// `DeadlineExceeded`, as a real daemon answers it.
///
/// Decided from the envelope alone, with no sleep: a paused test clock
/// auto-advances past a real socket read, so a fake that waited out `needs`
/// would race the client's own timeout.
pub async fn fake_client_needing_budget(
    path: &Path,
    needs: Duration,
    response: Response,
) -> (Client, JoinHandle<()>) {
    let mut listener = bind(path);
    let task = tokio::spawn(async move {
        let mut frames = accept_handshaked(&mut listener, sample_ack()).await;
        loop {
            let envelope = read_envelope(&mut frames).await;
            let budget = Duration::from_millis(envelope.deadline_ms.unwrap_or(u64::MAX));
            if budget < needs {
                let message = format!("request deadline of {} ms expired", budget.as_millis());
                write_err(
                    &mut frames,
                    envelope.id,
                    RpcErrorCode::DeadlineExceeded,
                    message,
                )
                .await;
            } else {
                write_reply(&mut frames, envelope.id, response.clone()).await;
            }
        }
    });
    let client = Client::connect(path).await.unwrap();
    (client, task)
}
