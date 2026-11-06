//! Attach to a pty sheep over a connection: the output stream, the keys and
//! sizes sent back, and what ends the stream.
//!
//! Real clock, not paused: a connection runs over a real socketpair, and a
//! paused clock would fire `Client::recv`'s bound while a frame is in flight.
//! Every wait is bounded by that `recv`, or by `until`.

use super::*;
use crate::server::testing::{Client, connected};
use shep_core::protocol::{BusEvent, ServerFrame};

/// Starts `web` on a pty through `h`, asserting the start landed.
async fn start_pty_web(h: &Harness) {
    let mut config = AppConfig::minimal("web", "./srv");
    config.pty = true;
    let started =
        reply_of(dispatch(envelope(1, Request::Start { apps: vec![config] }), &h.ctx).await);
    assert!(
        started.result.is_ok(),
        "web must start: {:?}",
        started.result
    );
}

/// A handshaken client on `h`.
async fn client_of(h: &Harness) -> Client {
    let mut client = connected(h.ctx.clone()).await;
    client.handshake().await;
    client
}

/// Sends `request` as request `id` and reads the next frame, which must be
/// its reply.
async fn ask(client: &mut Client, id: u64, request: Request) -> Reply {
    client.send(&envelope(id, request)).await;
    match client.recv::<ServerFrame>().await {
        ServerFrame::Reply(reply) => {
            assert_eq!(reply.id, id, "the reply answers the request just sent");
            reply
        }
        other => panic!("expected the reply to request {id}, got {other:?}"),
    }
}

fn attach(name: &str, rows: u16, cols: u16) -> Request {
    Request::Attach {
        name: name.to_string(),
        rows,
        cols,
    }
}

/// Reads the next frame, which must be an event.
async fn next_event(client: &mut Client) -> BusEvent {
    match client.recv::<ServerFrame>().await {
        ServerFrame::Event(event) => event,
        other => panic!("expected an event, got {other:?}"),
    }
}

fn pty_out(name: &str, data: &str) -> BusEvent {
    BusEvent::PtyOut {
        name: name.to_string(),
        data: data.to_string(),
    }
}

#[tokio::test]
async fn attach_streams_the_sheeps_output_as_pty_out_events() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_pty_web(&h).await;
    let mut client = client_of(&h).await;

    let reply = ask(&mut client, 2, attach("web", 24, 80)).await;
    assert!(
        matches!(&reply.result, Ok(Response::Attached { name, .. }) if name == "web"),
        "{reply:?}"
    );

    h.runner.pty_emit(0, b"hello \x1b[1mworld\x1b[0m");
    assert_eq!(
        next_event(&mut client).await,
        pty_out("web", "hello \x1b[1mworld\x1b[0m")
    );
}
