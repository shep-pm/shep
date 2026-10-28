//! The connection actor: one task owning the framed transport, demuxing
//! [`ServerFrame::Reply`] to the request that asked for it and
//! [`ServerFrame::Event`] to subscribers.
//!
//! Only the actor touches the transport. That is what lets one
//! [`Client`](crate::client::Client) be shared across concurrent callers
//! (`&self`, not `&mut self`) despite owning a single, non-cloneable
//! [`Frames`]: each caller sends a [`Command`]
//! and awaits its own [`oneshot`] reply.

use std::collections::HashMap;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot};

use shep_core::protocol::{
    BusEvent, Envelope, Reply, Request, Response, ServerFrame, decode_frame, encode_frame, reply_id,
};

use crate::client::RequestError;
use crate::connection::Frames;

/// Capacity of the broadcast channel the actor fans [`BusEvent`]s out over.
///
/// Mirrors the daemon's own per-connection outbound queue (`CONN_QUEUE =
/// 64`, `shep-daemon/src/server.rs`): a smaller client-side buffer would
/// lag behind the daemon's backpressure for no reason.
pub(crate) const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Depth of the actor's own command queue.
///
/// A full queue only makes a caller's `send` await for room; it never
/// drops or fails a request. Matches [`EVENT_CHANNEL_CAPACITY`].
const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// One command a [`Client`](crate::client::Client) sends to its actor task.
pub(crate) enum Command {
    /// Send `body` with `deadline_ms`; resolve `reply_to` with the eventual
    /// outcome once the matching [`Reply`] arrives, or with
    /// [`RequestError::Closed`] if the connection dies first.
    Request {
        /// The request payload.
        body: Request,
        /// The deadline to put on the wire, verbatim.
        deadline_ms: Option<u64>,
        /// Resolved exactly once, by the actor.
        reply_to: oneshot::Sender<Result<Response, RequestError>>,
    },
    /// Hand back a fresh [`broadcast::Receiver`] over the actor's own
    /// [`broadcast::Sender`].
    ///
    /// `broadcast` closes a channel by sender count reaching zero, never by
    /// receiver activity, so the actor task holds the only clone: a
    /// `Client`-held one would keep every [`crate::EventStream`] open for
    /// as long as the `Client` lives.
    Subscribe {
        /// Resolved exactly once, by the actor.
        reply_to: oneshot::Sender<broadcast::Receiver<BusEvent>>,
    },
}

/// Spawns the actor task that owns `frames`, returning the command channel
/// a [`Client`](crate::client::Client) sends [`Command`]s through.
///
/// The spawned task keeps the only [`broadcast::Sender`] this connection
/// will ever have.
pub(crate) fn spawn(frames: Frames) -> mpsc::Sender<Command> {
    let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (events_tx, _events_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
    tokio::spawn(run(frames, commands_rx, events_tx));
    commands_tx
}

/// The actor's own loop: `select!` between new commands and incoming
/// frames until either side ends, then fail every still-pending request
/// with [`RequestError::Closed`] rather than leave a caller parked on
/// `.await` forever.
async fn run(
    mut frames: Frames,
    mut commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<BusEvent>,
) {
    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, oneshot::Sender<Result<Response, RequestError>>> = HashMap::new();

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(Command::Request { body, deadline_ms, reply_to }) => {
                        let alive = send_request(&mut frames, &mut next_id, &mut pending, body, deadline_ms, reply_to).await;
                        // drops entries whose caller already stopped waiting
                        pending.retain(|_, tx| !tx.is_closed());
                        if !alive {
                            break; // the write failed; the connection is dead
                        }
                    }
                    Some(Command::Subscribe { reply_to }) => {
                        // No wire traffic, no failure mode: the caller may
                        // already have stopped waiting, in which case this
                        // receiver is simply dropped unused.
                        let _ = reply_to.send(events.subscribe());
                    }
                    None => break, // every `Client` handle (and its command sender) is gone
                }
            }
            frame = frames.next() => {
                match frame {
                    Some(Ok(bytes)) => route_frame(&bytes, &mut pending, &events),
                    Some(Err(_)) | None => break, // a read failed, or the peer closed the connection
                }
            }
        }
    }

    for (_id, reply_to) in pending.drain() {
        let _ = reply_to.send(Err(RequestError::Closed)); // the caller may already have stopped listening; that's fine
    }
}

/// Assigns the next request id, encodes and writes the envelope, then
/// records `reply_to` under that id for [`route_frame`] to resolve.
///
/// Returns `false` if the write failed: the connection is dead and the
/// actor loop should stop.
async fn send_request(
    frames: &mut Frames,
    next_id: &mut u64,
    pending: &mut HashMap<u64, oneshot::Sender<Result<Response, RequestError>>>,
    body: Request,
    deadline_ms: Option<u64>,
    reply_to: oneshot::Sender<Result<Response, RequestError>>,
) -> bool {
    let id = *next_id;
    *next_id += 1;
    let envelope = Envelope {
        id,
        deadline_ms,
        body,
    };
    let payload = match encode_frame(&envelope) {
        Ok(payload) => payload,
        Err(err) => {
            let _ = reply_to.send(Err(RequestError::Wire(err)));
            return true; // this one request failed to encode; the connection itself is still fine
        }
    };
    if frames.send(payload).await.is_err() {
        let _ = reply_to.send(Err(RequestError::Closed));
        return false;
    }
    pending.insert(id, reply_to);
    true
}

/// Decodes one frame off the wire and routes it. A [`Reply`] resolves the
/// pending request with the matching id; a [`BusEvent`] is broadcast.
/// Matching is by id, not arrival order: the daemon may emit a
/// [`BusEvent`] for a request before that request's own reply.
///
/// A [`Reply`] with no entry in `pending` is dropped. An undecodable
/// frame naming one fails its caller with [`RequestError::Undecodable`].
fn route_frame(
    bytes: &[u8],
    pending: &mut HashMap<u64, oneshot::Sender<Result<Response, RequestError>>>,
    events: &broadcast::Sender<BusEvent>,
) {
    let frame = match decode_frame::<ServerFrame>(bytes) {
        Ok(frame) => frame,
        Err(err) => {
            if let Some(id) = reply_id(bytes)
                && let Some(reply_to) = pending.remove(&id)
            {
                let _ = reply_to.send(Err(RequestError::Undecodable(err)));
            }
            return;
        }
    };
    match frame {
        ServerFrame::Reply(Reply { id, result }) => {
            if let Some(reply_to) = pending.remove(&id) {
                let _ = reply_to.send(result.map_err(RequestError::Rpc));
            }
        }
        ServerFrame::Event(event) => {
            let _ = events.send(event); // Err means no subscribers yet; the event is simply dropped
        }
        // `ServerFrame` is `#[non_exhaustive]`: an unknown variant is ignored, not fatal.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;
    use shep_core::protocol::{ProcessEventKind, ProcessInfo, codec};
    use shep_core::status::ProcStatus;
    use tokio_util::codec::Framed;

    use super::*;
    use crate::client::RequestError;
    use crate::connection::ByteStream;

    #[tokio::test]
    async fn the_actor_answers_a_request_over_an_in_memory_stream() {
        let (near, far) = tokio::io::duplex(1024);
        let frames = Framed::new(Box::new(near) as Box<dyn ByteStream>, codec());
        let commands = spawn(frames);
        let mut far = Framed::new(far, codec());

        let (reply_to, answer) = oneshot::channel();
        commands
            .send(Command::Request {
                body: Request::Ping,
                deadline_ms: None,
                reply_to,
            })
            .await
            .unwrap();
        let sent: Envelope = decode_frame(&far.next().await.unwrap().unwrap()).unwrap();
        let reply = ServerFrame::Reply(Reply {
            id: sent.id,
            result: Ok(Response::Pong),
        });
        far.send(encode_frame(&reply).unwrap()).await.unwrap();
        assert!(matches!(answer.await.unwrap(), Ok(Response::Pong)));

        let (reply_to, answer) = oneshot::channel();
        commands
            .send(Command::Request {
                body: Request::Ping,
                deadline_ms: None,
                reply_to,
            })
            .await
            .unwrap();
        let _second = far.next().await.unwrap().unwrap();
        drop(far);
        assert!(
            matches!(answer.await.unwrap(), Err(RequestError::Closed)),
            "a request pending when the stream closes must resolve as closed"
        );
    }

    /// A reply this build cannot decode used to be dropped, leaving the
    /// caller to wait out its deadline for an answer that had already
    /// arrived. It must fail the caller by id instead.
    #[test]
    fn an_undecodable_reply_fails_its_caller_by_id_rather_than_hanging() {
        let (tx, rx) = oneshot::channel();
        let mut pending = HashMap::from([(7_u64, tx)]);
        let (events, _) = broadcast::channel(4);

        route_frame(
            br#"{"id":7,"result":{"Ok":{"kind":"from_the_future","data":{"a":1}}}}"#,
            &mut pending,
            &events,
        );

        let got = rx.now_or_never().expect("the caller must be answered now");
        assert!(matches!(got, Ok(Err(RequestError::Undecodable(_)))));
        assert!(pending.is_empty(), "the pending entry must be cleared");
    }

    /// The whole of what calling a future frame kind "additive" buys an older
    /// client: a frame keyed by none of `id`, `result`, `event`, `data` fails
    /// to decode, `reply_id` finds no request to fail (it needs `result` as
    /// well as `id`), and the actor drops the frame. The connection and every
    /// request still in flight are untouched.
    #[test]
    fn a_frame_of_a_kind_this_build_does_not_know_is_dropped_without_disturbing_the_connection() {
        let (tx, rx) = oneshot::channel();
        let mut pending = HashMap::from([(7_u64, tx)]);
        let (events, mut sub) = broadcast::channel(4);

        route_frame(br#"{"progress":{"done":1}}"#, &mut pending, &events);

        assert!(sub.try_recv().is_err(), "nothing decodable to deliver");
        assert_eq!(pending.len(), 1, "the pending request is still held");
        assert!(rx.now_or_never().is_none(), "its caller is not answered");
    }

    /// Half of the reply pair names no request. `reply_id` requires `id` and
    /// `result` together precisely so a future frame that carries one of them
    /// is not mistaken for an undecodable reply, so both halves on their own
    /// are dropped and the request in flight keeps waiting for its real
    /// answer.
    #[test]
    fn a_frame_with_one_half_of_the_reply_pair_is_dropped_too() {
        let (events, mut sub) = broadcast::channel(4);

        for frame in [
            r#"{"id":7,"kind":"progress","percent":40}"#,
            r#"{"result":{"done":1}}"#,
        ] {
            let (tx, rx) = oneshot::channel();
            let mut pending = HashMap::from([(7_u64, tx)]);

            route_frame(frame.as_bytes(), &mut pending, &events);

            assert!(sub.try_recv().is_err(), "{frame} delivered nothing");
            assert_eq!(pending.len(), 1, "{frame} left the request held");
            assert!(rx.now_or_never().is_none(), "{frame} answered no caller");
        }
    }

    /// An event this build cannot decode is still just dropped. Nothing is
    /// waiting on it, and a subscriber that cannot name it cannot act on it.
    #[test]
    fn an_undecodable_event_is_dropped_without_disturbing_the_connection() {
        let mut pending = HashMap::new();
        let (events, mut sub) = broadcast::channel(4);

        route_frame(
            br#"{"event":"from_the_future","data":{"a":1}}"#,
            &mut pending,
            &events,
        );

        assert!(sub.try_recv().is_err(), "nothing decodable to deliver");
    }

    /// A status this build does not know used to make the whole event
    /// undecodable, and so dropped. It now decodes with the status as
    /// `Unrecognized`, and subscribers get it.
    #[test]
    fn an_event_carrying_a_status_this_build_does_not_know_is_delivered() {
        let mut pending = HashMap::new();
        let (events, mut sub) = broadcast::channel(4);
        let event = BusEvent::Process {
            event: ProcessEventKind::Online,
            info: ProcessInfo::builder(3, "web", ProcStatus::Online).build(),
            manually: false,
            at_ms: 0,
        };
        let mut wire = serde_json::to_value(&event).unwrap();
        wire["data"]["info"]["status"] = serde_json::json!("invented_next_year");

        route_frame(wire.to_string().as_bytes(), &mut pending, &events);

        let BusEvent::Process { info, .. } = sub.try_recv().expect("the event is delivered") else {
            panic!("a process event");
        };
        assert_eq!(info.status, ProcStatus::Unrecognized);
    }
}
