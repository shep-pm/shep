use super::*;
use futures_util::{SinkExt, StreamExt};
use shep_core::protocol::{
    Envelope, Hello, HelloAck, HelloReply, Response, codec, decode_frame, encode_frame,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

/// Serves exactly one connection, replying to the `Hello` with `reply` and
/// closing. Returns the `Hello` the client actually sent.
///
/// Binds before returning, so a caller can `connect` immediately without a
/// sleep.
///
/// Panics if `path` cannot be bound or the connection fails partway.
pub async fn fake_daemon(path: &Path, reply: HelloReply) -> JoinHandle<Hello> {
    let mut listener = bind(path);
    tokio::spawn(async move {
        let stream = listener.accept().await.unwrap();
        let mut frames = Framed::new(stream, codec());
        let first = frames.next().await.unwrap().unwrap();
        let hello: Hello = decode_frame(&first).unwrap();
        frames.send(encode_frame(&reply).unwrap()).await.unwrap();
        hello
    })
}

/// Binds `path`, accepts one connection, handshakes with `ack`, answers the
/// first request with `response`, and returns the received envelope.
///
/// Unlike [`fake_client_on`] and its siblings, this only listens: the
/// caller performs its own `Client::connect`.
///
/// Panics on any accept, handshake, decode or encode failure.
pub async fn serve_one_request(
    path: &Path,
    ack: HelloAck,
    response: Response,
) -> JoinHandle<Envelope> {
    let mut listener = bind(path);
    tokio::spawn(async move {
        let mut frames = accept_handshaked(&mut listener, ack).await;
        let envelope = read_envelope(&mut frames).await;
        write_reply(&mut frames, envelope.id, response).await;
        envelope
    })
}

/// Binds `path`, accepts one connection, completes the handshake with `ack`,
/// and then answers nothing: a shepherd wedged past serving a request, whose
/// socket and handshake still look live. A request against it times out.
///
/// The `handshook` flag is returned separately because the task never ends.
///
/// Panics if `path` cannot be bound, or on any accept or handshake failure.
pub fn fake_daemon_wedged_after_handshake(
    path: &Path,
    ack: HelloAck,
) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let mut listener = bind(path);
    let handshook = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&handshook);
    let handle = tokio::spawn(async move {
        let _frames = accept_handshaked(&mut listener, ack).await;
        flag.store(true, Ordering::SeqCst);
        // Holds `_frames` open: dropping it would close the connection, and a
        // closed connection is a different answer from no answer at all.
        std::future::pending::<()>().await;
    });
    (handle, handshook)
}

/// Binds `path` and answers every connection, one handshake and one request
/// each, with `reply`, until the returned handle is aborted.
///
/// The `served` counter is returned separately because the accept loop
/// never ends on its own, so its `JoinHandle` could never be read.
///
/// Panics if `path` cannot be bound.
pub fn fake_daemon_accepting_repeatedly(
    path: &Path,
    reply: Response,
) -> (JoinHandle<()>, Arc<AtomicU32>) {
    fake_daemon_accepting_repeatedly_with_ack(path, sample_ack(), reply)
}

/// As [`fake_daemon_accepting_repeatedly`], but with a caller-chosen
/// [`HelloAck`], for a caller whose version-skew guard would otherwise
/// refuse [`sample_ack`]'s fixed `"9.9.9"`.
///
/// Panics if `path` cannot be bound.
pub fn fake_daemon_accepting_repeatedly_with_ack(
    path: &Path,
    ack: HelloAck,
    reply: Response,
) -> (JoinHandle<()>, Arc<AtomicU32>) {
    let mut listener = bind(path);
    let served = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&served);
    let handle = tokio::spawn(async move {
        while let Ok(stream) = listener.accept().await {
            let mut frames = Framed::new(stream, codec());
            let _hello = handshake(&mut frames, ack.clone()).await;
            let envelope = read_envelope(&mut frames).await;
            // `write_reply` already wraps the response in `Ok`.
            write_reply(&mut frames, envelope.id, reply.clone()).await;
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    (handle, served)
}
