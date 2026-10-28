use super::origin::{Attribution, Origin, Side};
use super::peer_auth::ConnError;
#[cfg(unix)]
use super::peer_auth::{check_peer, daemon_uid, peer_pid};
use super::server_lifecycle::{CONN_QUEUE, HANDSHAKE_TIMEOUT_MS};
use super::tracked_codec::TrackedCodec;
use crate::bus::TopicFilter;
use crate::rpc::{Outcome, RpcContext, check_carried, dispatch, spawn_checked_forwarder};
use crate::supervisor::ConnId;
use bytes::{Bytes, BytesMut};
use core::time::Duration;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use shep_core::protocol::{
    BusEvent, Envelope, Hello, HelloAck, HelloReply, MIN_SUPPORTED, PROTOCOL_VERSION, RpcError,
    RpcErrorCode, codec, decode_frame, encode_frame,
};
use shep_core::transport::{ServerReadHalf, ServerStream};
use std::io;
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

type Frames = FramedRead<ServerReadHalf, TrackedCodec>;

// The ordering here is load-bearing (see `handshake` and `converse` below):
// auth before a single byte is read from the peer, the handshake before any
// request, and the writer task joined on every exit path so a protocol-skew
// refusal is guaranteed to reach the wire before the socket closes.
pub(super) async fn handle_conn(stream: ServerStream, ctx: RpcContext) -> Result<(), ConnError> {
    serve_conn(stream, ctx, Origin::Socket).await
}

/// [`handle_conn`] for a connection whose `origin` is already known.
pub(super) async fn serve_conn(
    stream: ServerStream,
    ctx: RpcContext,
    origin: Origin,
) -> Result<(), ConnError> {
    // Unix only. On Windows the pipe's own ACL refuses a foreign user's
    // open-for-write before a byte reaches this function, so the equivalent
    // check has already happened in the kernel.
    #[cfg(unix)]
    if origin.checks_peer() {
        check_peer(&stream, daemon_uid())?;
    }
    // Read once here and carried down to the handshake: this is what lets the
    // silence ladder tell a dog that never reached the socket from one that
    // reached it and would not say who it was. `None` on Windows.
    #[cfg(unix)]
    let peer = origin.pid(peer_pid(&stream));
    #[cfg(not(unix))]
    let peer: Option<u32> = origin.pid(None);
    // Recorded before a byte is read: a peer that connects and then says
    // nothing at all has still reached this daemon. A link is opened by this
    // daemon, not by the dog, so its contact waits for a frame in `handshake`.
    if origin.contacts_on_open()
        && let Some(pid) = peer
    {
        ctx.peer_contacts.connected(pid);
    }
    // Minted after the peer check, not before: a connection refused for its
    // uid never reaches a handler, so it has nothing to scope.
    let conn = ConnId::next();
    let (read_half, write_half) = shep_core::transport::split(stream);
    let mut frames = FramedRead::new(read_half, TrackedCodec::new(codec()));
    let (out_tx, out_rx) = mpsc::channel::<Bytes>(CONN_QUEUE);
    let writer = tokio::spawn(write_loop(
        FramedWrite::new(write_half, codec()),
        out_rx,
        origin.clone(),
    ));

    let outcome = converse(&mut frames, &out_tx, conn, peer, &origin, &ctx).await;

    // Drop the sender and join the writer on every path: a protocol-skew
    // refusal is written by that task, so returning early would close the
    // socket before the client saw why.
    drop(out_tx);
    let _ = writer.await;
    // On every path out, for the same reason. A smit belongs to the connection
    // that painted it. After the writer join, so a client that painted and
    // immediately read still sees its own mark in the reply.
    ctx.supervisor.forget_smits(conn).await;
    outcome
}

async fn converse(
    frames: &mut Frames,
    out: &mpsc::Sender<Bytes>,
    conn: ConnId,
    peer: Option<u32>,
    origin: &Origin,
    ctx: &RpcContext,
) -> Result<(), ConnError> {
    let mut forwarder: Option<JoinHandle<()>> = None;
    if let Some(topics) = origin.resumed() {
        forwarder = resume(out, peer, origin, ctx, topics).await?;
    } else {
        handshake(frames, out, peer, origin, ctx).await?;
    }
    let mut attribution = Attribution::new(origin);
    let outcome = read_loop(
        frames,
        out,
        conn,
        ctx,
        origin,
        &mut forwarder,
        &mut attribution,
    )
    .await;
    // Every path out of `read_loop` lands here, and a live forwarder must be
    // aborted rather than dropped: dropping a `JoinHandle` detaches the task,
    // which keeps its clone of `out` alive, which keeps `write_loop` from ever
    // seeing every sender gone and hangs `handle_conn`'s `writer.await`.
    if let Some(forwarder) = forwarder {
        forwarder.abort();
    }
    outcome
}

async fn read_loop(
    frames: &mut Frames,
    out: &mpsc::Sender<Bytes>,
    conn: ConnId,
    ctx: &RpcContext,
    origin: &Origin,
    forwarder: &mut Option<JoinHandle<()>>,
    attribution: &mut Attribution,
) -> Result<(), ConnError> {
    while let Some(frame) = next_frame(frames, origin).await {
        let frame = frame?; // oversize/short frame ends the connection
        let envelope: Envelope = decode_frame(&frame).map_err(ConnError::Decode)?;
        attribution.note(ctx, &envelope.body);
        match dispatch(envelope, conn, origin.bound_dog(), ctx).await {
            Outcome::Reply(reply) => send(out, &reply).await?,
            Outcome::Subscribe { reply, filter } => {
                // Recorded ahead of the reply: a handover's snapshot then never
                // says less than the dog has been told.
                origin.subscribed(filter.patterns()).await;
                send(out, &reply).await?; // ordered ahead of any event by the queue
                // A second Subscribe REPLACES the first: spec §6 gives a
                // connection one topic list, not a growing union.
                if let Some(old) = forwarder.replace(spawn_checked_forwarder(
                    ctx,
                    origin.bound_dog(),
                    filter,
                    out.clone(),
                )) {
                    old.abort();
                }
            }
            Outcome::Shutdown(reply) => {
                send(out, &reply).await?;
                ctx.shutdown();
                break;
            }
        }
    }
    Ok(())
}

/// Picks up a connection whose dog shook hands with this daemon's predecessor
///
/// No `Hello` is read: the dog sent it once, to the predecessor. Its next
/// frame is a request. The handshake's facts are recorded here, since a
/// successor starts with an empty silence watch and refusal ledger.
/// `topics` arms the subscription again, behind a [`BusEvent::Dropped`]
/// notice. Its count is 1, the least that is true of what the exec lost.
async fn resume(
    out: &mpsc::Sender<Bytes>,
    peer: Option<u32>,
    origin: &Origin,
    ctx: &RpcContext,
    topics: &[String],
) -> Result<Option<JoinHandle<()>>, ConnError> {
    if let Some(pid) = peer {
        ctx.peer_contacts.connected(pid);
        ctx.peer_contacts.named_a_dog(pid);
    }
    if let Some(dog) = origin.dog(None) {
        // Not narrated, unlike a handshake: the dog was accepted by the
        // predecessor, and this is the same connection continuing.
        ctx.dog_refusals.handshook(dog);
    }
    if topics.is_empty() {
        return Ok(None);
    }
    // The grants this shepherd read may be narrower than the ones the dog
    // subscribed under, and the stream would otherwise outlive the narrowing.
    // Refused whole, as a fresh `Subscribe` is, and recorded as empty so the
    // next handover does not carry it on.
    if let Some(dog) = origin.bound_dog()
        && let Err(refusal) = check_carried(ctx, dog, topics).await
    {
        tracing::warn!(%dog, reason = %refusal.message, "a carried dog subscription is outside its grants");
        origin.subscribed(&[]).await;
        return Ok(None);
    }
    let filter = match TopicFilter::new(topics) {
        Ok(filter) => filter,
        Err(error) => {
            // Compiled once already, by the predecessor that accepted it.
            tracing::warn!(%error, "a carried dog subscription no longer compiles");
            return Ok(None);
        }
    };
    send(out, &BusEvent::Dropped { count: 1 }).await?;
    Ok(Some(spawn_checked_forwarder(
        ctx,
        origin.bound_dog(),
        filter,
        out.clone(),
    )))
}

async fn handshake(
    frames: &mut Frames,
    out: &mpsc::Sender<Bytes>,
    peer: Option<u32>,
    origin: &Origin,
    ctx: &RpcContext,
) -> Result<(), ConnError> {
    let frame = tokio::time::timeout(
        Duration::from_millis(HANDSHAKE_TIMEOUT_MS),
        next_frame(frames, origin),
    )
    .await
    .map_err(|_| ConnError::HandshakeTimeout)?
    .ok_or(ConnError::NoHandshake)??;
    if !origin.contacts_on_open()
        && let Some(pid) = peer
    {
        ctx.peer_contacts.connected(pid);
    }
    let hello: Hello = decode_frame(&frame).map_err(ConnError::Decode)?;
    // The connection's identity where it has one, else what the peer claimed.
    let dog = origin.dog(hello.dog_name.as_deref());
    // Ahead of the protocol check: this records what the peer sent, not what
    // this daemon made of it. A dog refused on skew still named itself.
    if dog.is_some()
        && let Some(pid) = peer
    {
        ctx.peer_contacts.named_a_dog(pid);
    }
    if hello.protocol < MIN_SUPPORTED {
        // Version skew is a typed error, not silence (spec §6). There is no
        // upper bound: a newer peer is accepted deliberately. A request this
        // daemon cannot name is refused at dispatch by
        // `RpcErrorCode::Unsupported`.
        let refusal: HelloReply = Err(RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: format!(
                "client sent protocol {}, daemon speaks {PROTOCOL_VERSION} and accepts {MIN_SUPPORTED} and above",
                hello.protocol
            ),
            // The refusal names our protocol, which does not say which shep
            // is running. `shep daemon reload` chooses its mechanism by
            // version, and this is the one path where the ack never arrives.
            daemon_version: Some(ctx.daemon_version.clone()),
        });
        send(out, &refusal).await?;
        // The refusal is queued before anything else happens, so a peer about
        // to be restarted still learns why. `dog_name` is `None` for every
        // client that is not a dog: the name travels only on
        // `ReconnectingClient`, which no `shep` verb uses.
        match dog {
            Some(dog) => {
                crate::dogs::record_refused_dog(
                    dog,
                    &hello.client_version,
                    &ctx.dog_refusals,
                    &ctx.supervisor,
                );
                // Into the dog's own log too, carrying both protocol numbers:
                // that log is where an operator looks first.
                crate::dogs::narrate_by_name(
                    &ctx.supervisor,
                    &ctx.events,
                    dog,
                    format!(
                        "shep REFUSED this dog's handshake: this shepherd speaks protocol {PROTOCOL_VERSION} and the dog sent {}. Its own build is shep-client {}. Rebuild or reinstall it against this shep and run `shep restart {dog}`",
                        hello.protocol, hello.client_version
                    ),
                );
            }
            // An operator running an older `shep` already reads the skew from
            // their own CLI. `debug!`, not `warn!`: the CLI polls while it
            // waits for a successor, so one reload across a protocol bump
            // produced 442 of these in 9.8 seconds.
            None => tracing::debug!(
                client_protocol = hello.protocol,
                client_version = %hello.client_version,
                "refused a client on protocol skew"
            ),
        }
        return Err(ConnError::ProtocolMismatch {
            client: hello.protocol,
        });
    }
    // A dog that got in is not stale, including after the restart it was just
    // given, which is the case that has to clear.
    if let Some(dog) = dog {
        // Only the transition is narrated: a dog reconnects after a handover
        // or a daemon restart, and a line per connection would bury its own
        // output in its own log.
        if ctx.dog_refusals.handshook(dog) {
            crate::dogs::narrate_by_name(
                &ctx.supervisor,
                &ctx.events,
                dog,
                format!(
                    "shep accepted this dog's handshake; it is registered with this shepherd as `{dog}`, on protocol {PROTOCOL_VERSION}"
                ),
            );
        }
    }
    // Ahead of the ack, for the same reason as a subscription: a successor
    // that did not know the dog had greeted would wait for a `Hello` that is
    // never coming.
    origin.greeted().await;
    let ack: HelloReply = Ok(HelloAck {
        daemon_version: ctx.daemon_version.clone(),
        protocol: PROTOCOL_VERSION,
        pid: ctx.pid,
        min_supported: Some(MIN_SUPPORTED),
    });
    send(out, &ack).await
}

/// The next frame off `frames`, parking first while a handover has frozen
/// this connection
///
/// Parks only on a frame boundary: a frame whose bytes are partly read stays
/// being read, because the bytes taken off the socket would not survive the
/// exec and the successor would start in the middle of it.
async fn next_frame(frames: &mut Frames, origin: &Origin) -> Option<Result<BytesMut, io::Error>> {
    loop {
        if frames.decoder().holding() {
            return frames.next().await;
        }
        tokio::select! {
            biased;
            () = origin.frozen() => {
                // A cancelled read may have taken the start of a frame.
                if !frames.decoder().holding() {
                    origin.park(Side::Reader).await;
                }
            }
            frame = frames.next() => return frame,
        }
    }
}

/// Writes the frames queued on `rx`, a batch at a time, parking between
/// batches while a handover has frozen `origin`'s connection so that it never
/// execs with a frame half on the wire.
async fn write_loop<W: AsyncWrite + Unpin>(
    mut sink: FramedWrite<W, LengthDelimitedCodec>,
    mut rx: mpsc::Receiver<Bytes>,
    origin: Origin,
) {
    loop {
        let first = tokio::select! {
            biased;
            () = origin.frozen() => {
                origin.park(Side::Writer).await;
                continue;
            }
            first = rx.recv() => first,
        };
        let Some(first) = first else { return };
        // `send` is feed plus flush, one flush per frame. What is already
        // queued shares one flush, taken before waiting again. The batch is
        // capped because `try_recv` never yields: an endless producer would
        // hold this task off the scheduler.
        let mut next = Some(first);
        let mut fed = 0;
        while let Some(bytes) = next {
            if sink.feed(bytes).await.is_err() {
                return; // peer gone; nothing left to drain the queue
            }
            fed += 1;
            next = if fed < CONN_QUEUE {
                rx.try_recv().ok()
            } else {
                None
            };
        }
        if SinkExt::<Bytes>::flush(&mut sink).await.is_err() {
            return; // peer gone, surfaced by the flush
        }
    }
}

async fn send<T: Serialize>(out: &mpsc::Sender<Bytes>, value: &T) -> Result<(), ConnError> {
    let bytes = encode_frame(value).map_err(ConnError::Encode)?;
    out.send(bytes).await.map_err(|_| ConnError::PeerGone)
}

#[cfg(test)]
mod tests {
    use super::super::server_lifecycle::CONN_QUEUE;

    use futures_util::SinkExt;

    use shep_core::protocol::{
        Envelope, Hello, HelloReply, MIN_SUPPORTED, PROTOCOL_VERSION, RpcErrorCode,
    };

    use crate::bus::SharedEvent;
    use crate::fake::ProcScript;
    use crate::testing::harness;

    use shep_core::protocol::{
        BusEvent, ProcessEventKind, ProcessInfo, Request, Response, ServerFrame,
    };

    use super::super::testing::*;
    use crate::fake::FIRST_SCRIPTED_PID;
    use core::time::Duration;
    use shep_core::status::ProcStatus;

    #[tokio::test]
    async fn handshake_acks_a_matching_protocol() {
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client
            .send(&Hello {
                client_version: "0.1.0".to_string(),
                protocol: PROTOCOL_VERSION,
                dog_name: None,
            })
            .await;
        let ack: HelloReply = client.recv().await;
        let ack = ack.expect("a matching protocol must be acked");
        assert_eq!(ack.protocol, PROTOCOL_VERSION);
        assert_eq!(ack.pid, h.ctx.pid);
        assert_eq!(ack.daemon_version, h.ctx.daemon_version);
        assert_eq!(ack.min_supported, Some(MIN_SUPPORTED));
    }

    #[tokio::test]
    async fn handshake_accepts_a_peer_newer_than_the_daemon() {
        // No upper bound: anything a newer peer asks for that this daemon
        // cannot name is refused per request, not at connect time.
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client
            .send(&Hello {
                client_version: "9.9.9".to_string(),
                protocol: PROTOCOL_VERSION + 1,
                dog_name: None,
            })
            .await;
        let ack: HelloReply = client.recv().await;
        let ack = ack.expect("a peer above the daemon's own version must be accepted");
        assert_eq!(ack.protocol, PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn handshake_refuses_protocol_skew_before_closing() {
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client
            .send(&Hello {
                client_version: "9.9.9".to_string(),
                protocol: MIN_SUPPORTED - 1,
                dog_name: None,
            })
            .await;
        let refusal: HelloReply = client.recv().await;
        let err = refusal.expect_err("skew below the floor must be refused");
        assert_eq!(err.code, RpcErrorCode::ProtocolMismatch);
        assert!(
            client.closed().await,
            "the daemon must close after refusing"
        );
    }

    #[tokio::test]
    async fn a_protocol_refusal_carries_the_daemon_version() {
        // `shep daemon reload` picks between a handover and a stop-and-start
        // by crate version, which the protocol number does not give it.
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client
            .send(&Hello {
                client_version: "9.9.9".to_string(),
                protocol: MIN_SUPPORTED - 1,
                dog_name: None,
            })
            .await;
        let refusal: HelloReply = client.recv().await;
        let err = refusal.expect_err("skew below the floor must be refused");
        assert_eq!(err.code, RpcErrorCode::ProtocolMismatch);
        // The same field the ack uses: a client must never learn two versions
        // for one daemon.
        assert_eq!(err.daemon_version.as_deref(), Some(&*h.ctx.daemon_version));
    }

    #[tokio::test]
    async fn a_request_before_the_handshake_ends_the_connection() {
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client
            .send(&Envelope {
                id: 1,
                deadline_ms: None,
                body: Request::Ping,
            })
            .await;
        assert!(client.closed().await);
    }

    #[tokio::test]
    async fn ping_round_trips_over_the_socket() {
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client.handshake().await;
        client
            .send(&Envelope {
                id: 11,
                deadline_ms: Some(1000),
                body: Request::Ping,
            })
            .await;
        let frame: ServerFrame = client.recv().await;
        let ServerFrame::Reply(reply) = frame else {
            panic!("expected a reply frame")
        };
        assert_eq!(reply.id, 11);
        assert_eq!(reply.result.unwrap(), Response::Pong);
    }

    #[tokio::test]
    async fn subscribe_streams_only_matching_events() {
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client.handshake().await;
        client.subscribe(&["process.*"]).await;

        let event = |kind| -> SharedEvent {
            BusEvent::Process {
                event: kind,
                info: ProcessInfo::builder(0, "web", shep_core::status::ProcStatus::Online)
                    .pid(Some(1000))
                    .out_file(Some("/logs/web-0-out.log".to_string()))
                    .err_file(Some("/logs/web-0-err.log".to_string()))
                    .build(),
                manually: false,
                at_ms: 0,
            }
            .into()
        };
        h.ctx.events.send(event(ProcessEventKind::Start)).unwrap();
        h.ctx
            .events
            .send(
                BusEvent::LogOut {
                    id: 0,
                    line: "filtered".to_string(),
                }
                .into(),
            )
            .unwrap();
        h.ctx.events.send(event(ProcessEventKind::Online)).unwrap();

        // Back-to-back arrival is the filtering assertion: no negative wait.
        let first: ServerFrame = client.recv().await;
        let second: ServerFrame = client.recv().await;
        assert!(matches!(
            first,
            ServerFrame::Event(BusEvent::Process {
                event: ProcessEventKind::Start,
                ..
            })
        ));
        assert!(matches!(
            second,
            ServerFrame::Event(BusEvent::Process {
                event: ProcessEventKind::Online,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_garbage_frame_ends_the_connection_without_panicking() {
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client.handshake().await;
        client
            .frames
            .send(bytes::Bytes::from_static(b"not json"))
            .await
            .unwrap();
        assert!(client.closed().await);
    }

    #[tokio::test]
    async fn a_garbage_frame_after_subscribing_still_closes_the_connection() {
        // A live forwarder holds its own clone of `out`. Not aborting it on a
        // connection error leaves `out_tx`'s drop short of the last sender, so
        // `writer.await` hangs and the socket never closes. Subscribing first
        // is what makes that path reachable.
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client.handshake().await;
        client.subscribe(&["*"]).await;
        client
            .frames
            .send(bytes::Bytes::from_static(b"not json"))
            .await
            .unwrap();
        assert!(
            client.closed().await,
            "a live forwarder must not keep the connection open past a decode error"
        );
    }

    #[tokio::test]
    async fn a_slow_subscriber_gets_a_dropped_notice_instead_of_hanging_the_bus() {
        // Drives the Lagged-to-Dropped translation through the real
        // connection stack rather than in isolation. Real time: real socket.
        let h = harness(vec![]);
        let mut client = connected(h.ctx.clone()).await;
        client.handshake().await;
        client.subscribe(&["log.*"]).await;

        // Never call `client.recv()` again until after the flood: CONN_QUEUE
        // fills, the forwarder blocks on `out.send`, and the broadcast ring
        // takes the overflow from there.
        let flood = crate::bus::BUS_CAPACITY + CONN_QUEUE + 16;
        for i in 0..flood {
            h.ctx
                .events
                .send(
                    BusEvent::LogOut {
                        id: 0,
                        line: format!("line-{i}"),
                    }
                    .into(),
                )
                .unwrap();
        }

        // Resume reading. The count comes from tokio's own `Lagged(n)` inside
        // the forwarder, never hand-computed here.
        let dropped = loop {
            match client.recv::<ServerFrame>().await {
                ServerFrame::Event(BusEvent::Dropped { count }) => break count,
                ServerFrame::Event(_) => continue,
                other => panic!("expected eventually a Dropped notice, got {other:?}"),
            }
        };
        assert!(
            dropped > 0,
            "a flood past CONN_QUEUE + BUS_CAPACITY must report a real lag"
        );
    }

    /// fails if a dog that got back in stays condemned. Without the mark
    /// clearing, the dog is reported stale forever while answering perfectly.
    #[tokio::test]
    async fn a_dog_that_handshakes_is_no_longer_stale() {
        let h = harness(vec![]);
        refuse_as(&h.ctx, Some("metrics")).await;
        refuse_as(&h.ctx, Some("metrics")).await;
        assert_eq!(h.ctx.dog_refusals.stale(), vec!["metrics".to_string()]);

        let mut client = connected(h.ctx.clone()).await;
        client
            .send(&Hello {
                client_version: "0.1.22".to_string(),
                protocol: PROTOCOL_VERSION,
                dog_name: Some("metrics".to_string()),
            })
            .await;
        let ack: HelloReply = client.recv().await;
        ack.expect("a matching protocol must be acked");

        assert!(
            h.ctx.dog_refusals.stale().is_empty(),
            "a dog talking to this daemon is not stale by any definition it can apply"
        );
    }

    // --- The incident: a dog that reaches shep and never names itself ---
    /// fails if a dog connected to this shepherd is reported as a binary
    /// that cannot talk to it.
    ///
    /// End to end rather than against `DogRefusals`. The verdict was right;
    /// the sentence drawn from it was not. `harness_at_pid` runs the dog at
    /// this test's own pid, so the peer credentials name one process.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dog_that_connects_without_naming_itself_is_not_called_a_stale_binary() {
        // Three: the first spawn, the restart the ladder's first rung asks
        // for, and one spare so a spawn is never refused for want of script.
        let h = crate::testing::harness_at_pid(
            vec![
                ProcScript::never_exits(),
                ProcScript::never_exits(),
                ProcScript::never_exits(),
            ],
            std::process::id(),
        );
        // A real socket cannot pause its clock, so the attribution warm-up is
        // forced rather than walked;
        // `dogs::a_dog_that_never_calls_still_earns_its_rebuild_after_the_warm_up`
        // covers the boundary.
        h.ctx.peer_contacts.force_warm();
        let dog = start_dog(&h.ctx, "log-rotate").await;
        assert_eq!(
            dog.pid,
            Some(std::process::id()),
            "the fixture only means anything if the dog and this test are one process"
        );
        let err_log = dog
            .err_file
            .clone()
            .expect("a dog's listing resolves its log paths");

        // The current protocol, a real client version, and no `dog_name`.
        let mut client = connected(h.ctx.clone()).await;
        client
            .send(&Hello {
                protocol: PROTOCOL_VERSION,
                client_version: "0.1.22".to_string(),
                dog_name: None,
            })
            .await;
        let ack: HelloReply = client.recv().await;
        ack.expect("an anonymous handshake on the current protocol is ACCEPTED, not refused");

        // And it serves requests, which is the half the verdict has to admit.
        client
            .send(&Envelope {
                id: 1,
                deadline_ms: None,
                body: Request::ListFlock,
            })
            .await;
        match client.recv::<ServerFrame>().await {
            ServerFrame::Reply(reply) => match reply.result {
                Ok(Response::Flock(flock)) => assert!(
                    flock.iter().any(|info| info.name == "log-rotate"),
                    "the connection this daemon is about to call stale is serving requests"
                ),
                other => panic!("ListFlock must answer with the flock, got {other:?}"),
            },
            other => panic!("an accepted connection must serve ListFlock, got {other:?}"),
        }

        assert_eq!(
            h.ctx.peer_contacts.from_pid(Some(std::process::id())),
            crate::dogs::Contact::Anonymous,
            "the handshake path must record what actually arrived"
        );
        assert!(
            !h.ctx.dog_refusals.has_handshook("log-rotate"),
            "no `dog_name` means no handshake was recorded, which is the whole trap"
        );

        // The real ladder over two whole budgets. Instants rather than a
        // paused clock: the connection above is a real socket, and a paused
        // runtime auto-advances whenever it idles.
        let mut seen = crate::dogs::SilentDogs::default();
        let t0 = tokio::time::Instant::now();
        let ladder = async |seen: &mut crate::dogs::SilentDogs, at| {
            crate::dogs::check_silent_dogs(
                &h.ctx.supervisor,
                &h.ctx.dog_refusals,
                &h.ctx.peer_contacts,
                &h.ctx.events,
                seen,
                at,
            )
            .await
        };
        assert!(ladder(&mut seen, t0).await.is_empty());
        assert_eq!(
            ladder(&mut seen, t0 + crate::dogs::DOG_SILENCE_BUDGET).await,
            vec![("log-rotate".to_string(), crate::dogs::Refusal::Restart)]
        );
        assert_eq!(
            ladder(&mut seen, t0 + 2 * crate::dogs::DOG_SILENCE_BUDGET).await,
            vec![("log-rotate".to_string(), crate::dogs::Refusal::Stale)],
            "the ladder's verdict is unchanged; what changes is what it SAYS"
        );

        // What the operator reads: the dog's own log, the file the verdict
        // tells them to open.
        let written = std::fs::read_to_string(&err_log).expect("the narration must reach the log");
        assert!(
            written.contains("[shep]"),
            "shep's voice in a dog's log has to be marked as shep's: {written}"
        );
        assert!(
            written.contains("HAS connected to this shepherd"),
            "the verdict must say what this shepherd watched arrive: {written}"
        );
        assert!(
            written.contains("reinstalling the same build will NOT"),
            "the two days were spent on advice this line has to refuse: {written}"
        );
        assert!(
            !written.contains("cannot talk to this shep either"),
            "the sentence that cost two days must not appear on this path: {written}"
        );
        assert!(
            !written.contains("cannot reach this shep"),
            "this dog reached shep; nothing here may claim otherwise: {written}"
        );
    }

    /// fails if a refused dog is left mute. The daemon is the only party that
    /// can restart it: the dog's own client has stopped rather than spinning.
    ///
    /// The pid moving is the assertion, not the restart count: a restart that
    /// re-registered the row without re-spawning leaves the dog as mute.
    #[tokio::test]
    async fn a_refused_dog_is_restarted_once_from_the_binary_on_disk() {
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let started = start_dog(&h.ctx, "metrics").await;
        assert_eq!(started.pid, Some(FIRST_SCRIPTED_PID));

        refuse_as(&h.ctx, Some("metrics")).await;

        await_dog(&h.ctx, "metrics", FIRST_SCRIPTED_PID + 1).await;
        assert!(
            h.ctx.dog_refusals.stale().is_empty(),
            "one refusal buys a restart; it does not condemn the dog"
        );
    }

    /// fails if the daemon restarts a dog it has already restarted, the spin
    /// G8 forbids. A second refusal proves the binary on disk cannot satisfy
    /// this daemon either, since the restart already ran it.
    ///
    /// The pid must not move inside a window sized against the restart that
    /// really happened earlier in this test, and the harness is scripted with
    /// exactly the two spawns G8 permits.
    #[tokio::test]
    async fn a_twice_refused_dog_is_reported_stale_and_never_restarted_again() {
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        start_dog(&h.ctx, "metrics").await;

        refuse_as(&h.ctx, Some("metrics")).await;
        let restart_took = await_dog(&h.ctx, "metrics", FIRST_SCRIPTED_PID + 1).await;

        refuse_as(&h.ctx, Some("metrics")).await;
        assert_eq!(
            h.ctx.dog_refusals.stale(),
            vec!["metrics".to_string()],
            "the second refusal must be reported, not swallowed"
        );

        // Ten times the restart that did happen, floored so a fast machine
        // still watches for a real interval.
        let window = (restart_took * 10).max(Duration::from_millis(200));
        tokio::time::sleep(window).await;

        let after = dog_row(&h.ctx, "metrics").await;
        assert_eq!(
            after.pid,
            Some(FIRST_SCRIPTED_PID + 1),
            "a second restart within {window:?} is the spin G8 forbids"
        );
        assert_eq!(
            after.status,
            ProcStatus::Online,
            "a third spawn would exhaust the script and error the dog"
        );
    }

    /// fails if an operator running an older `shep` has a dog restarted under
    /// them. The CLI cannot name a dog, so a refusal carrying no name must
    /// leave the flock exactly as it was.
    #[tokio::test]
    async fn a_refused_client_that_is_not_a_dog_touches_nothing() {
        let h = harness(vec![ProcScript::never_exits()]);
        let started = start_dog(&h.ctx, "metrics").await;

        for _ in 0..3 {
            refuse_as(&h.ctx, None).await;
        }

        assert!(h.ctx.dog_refusals.stale().is_empty());
        let after = dog_row(&h.ctx, "metrics").await;
        assert_eq!(
            after.pid, started.pid,
            "a nameless refusal must not restart anything"
        );
        assert_eq!(after.status, ProcStatus::Online);
    }
}

#[cfg(test)]
mod write_loop_tests;
