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

#[tokio::test]
async fn attach_forces_a_redraw_with_the_given_window_size() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_pty_web(&h).await;

    let mut first = client_of(&h).await;
    ask(&mut first, 2, attach("web", 24, 80))
        .await
        .result
        .unwrap();
    assert_eq!(h.runner.pty_sizes(0), [(24, 80)]);

    // A second client's own size reaches the terminal too, so the program
    // redraws for whoever attached last.
    let mut second = client_of(&h).await;
    ask(&mut second, 2, attach("web", 50, 132))
        .await
        .result
        .unwrap();
    assert_eq!(h.runner.pty_sizes(0), [(24, 80), (50, 132)]);
}

#[tokio::test]
async fn pty_input_reaches_the_sheep() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_pty_web(&h).await;
    let mut client = client_of(&h).await;

    let request = Request::PtyInput {
        name: "web".to_string(),
        data: "ls\r\u{e9}".to_string(),
    };
    let reply = ask(&mut client, 2, request).await;
    assert!(
        matches!(&reply.result, Ok(Response::Attached { name, .. }) if name == "web"),
        "{reply:?}"
    );
    assert_eq!(h.runner.pty_input(0), "ls\r\u{e9}".as_bytes());

    let resize = Request::PtyResize {
        name: "web".to_string(),
        rows: 40,
        cols: 100,
    };
    let reply = ask(&mut client, 3, resize).await;
    assert!(reply.result.is_ok(), "{reply:?}");
    assert_eq!(h.runner.pty_sizes(0), [(40, 100)]);
}

#[tokio::test]
async fn attaching_a_sheep_without_pty_is_refused_by_name() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;
    let mut client = client_of(&h).await;

    let refusal = "sheep web has no pty: set pty = true";
    for (id, request) in [
        (2, attach("web", 24, 80)),
        (
            3,
            Request::PtyInput {
                name: "web".to_string(),
                data: "x".to_string(),
            },
        ),
        (
            4,
            Request::PtyResize {
                name: "web".to_string(),
                rows: 24,
                cols: 80,
            },
        ),
    ] {
        let reply = ask(&mut client, id, request).await;
        let Err(err) = reply.result else {
            panic!("a sheep on pipes has no terminal to reach")
        };
        assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{err:?}");
        assert_eq!(err.message, refusal);
    }

    let reply = ask(&mut client, 5, attach("ghost", 24, 80)).await;
    let Err(err) = reply.result else {
        panic!("a name nothing is registered under must be refused")
    };
    assert_eq!(err.code, RpcErrorCode::NotFound, "{err:?}");
    assert_eq!(err.message, "no sheep named ghost");
}

#[tokio::test]
async fn a_split_multibyte_character_is_sent_whole() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_pty_web(&h).await;
    let mut client = client_of(&h).await;
    ask(&mut client, 2, attach("web", 24, 80))
        .await
        .result
        .unwrap();

    // `é` is 0xC3 0xA9: the first read ends between its two bytes.
    h.runner.pty_emit(0, b"caf\xc3");
    assert_eq!(next_event(&mut client).await, pty_out("web", "caf"));
    h.runner.pty_emit(0, b"\xa9!");
    assert_eq!(next_event(&mut client).await, pty_out("web", "\u{e9}!"));
}

#[tokio::test]
async fn the_stream_ends_with_pty_end_when_the_sheep_exits() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_pty_web(&h).await;
    let mut client = client_of(&h).await;
    ask(&mut client, 2, attach("web", 24, 80))
        .await
        .result
        .unwrap();

    h.runner.pty_emit(0, b"bye");
    h.runner.pty_end(0);
    assert_eq!(next_event(&mut client).await, pty_out("web", "bye"));
    assert_eq!(
        next_event(&mut client).await,
        BusEvent::PtyEnd {
            name: "web".to_string()
        }
    );
}

#[tokio::test]
async fn closing_the_connection_drops_the_subscription() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_pty_web(&h).await;
    let mut client = client_of(&h).await;
    ask(&mut client, 2, attach("web", 24, 80))
        .await
        .result
        .unwrap();
    assert_eq!(h.runner.pty_subscribers(0), 1, "the attach subscribed");

    drop(client);
    let dropped = tokio::time::timeout(Duration::from_secs(5), async {
        while h.runner.pty_subscribers(0) != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(dropped.is_ok(), "the subscription outlived its connection");
}

#[tokio::test]
async fn a_second_attach_on_one_connection_is_refused() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_pty_web(&h).await;
    let mut client = client_of(&h).await;
    ask(&mut client, 2, attach("web", 24, 80))
        .await
        .result
        .unwrap();

    let reply = ask(&mut client, 3, attach("web", 50, 132)).await;
    let Err(err) = reply.result else {
        panic!("one connection hears one pty at a time")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{err:?}");
    assert_eq!(err.message, "this connection is already attached to a pty");
    assert_eq!(
        h.runner.pty_sizes(0),
        [(24, 80)],
        "the refused attach left the terminal's size alone"
    );
    assert_eq!(h.runner.pty_subscribers(0), 1, "the first stream is intact");
}
