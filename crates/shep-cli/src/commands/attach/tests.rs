use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use shep_client::testing::{control_address, fake_client_answering_with_events};
use shep_core::protocol::{BusEvent, Envelope, Request, Response, RpcError, RpcErrorCode};
use tokio::sync::mpsc;

use super::*;
use crate::cli::Format;

/// Long enough for a loaded CI box, short enough to name a hang.
const BOUND: Duration = Duration::from_secs(10);

const SIZE: Size = Size { rows: 24, cols: 80 };

/// A fake shepherd that accepts every attach and answers every request
/// `Attached`.
async fn accepting(dir: &std::path::Path) -> (Client, mpsc::UnboundedReceiver<Envelope>) {
    fake_client_answering_with_events(&control_address(dir), |request| {
        let name = match request {
            Request::Attach { name, .. }
            | Request::PtyInput { name, .. }
            | Request::PtyResize { name, .. } => name.clone(),
            _ => String::new(),
        };
        (Ok(Response::Attached { id: 1, name }), Vec::new())
    })
    .await
}

fn tty() -> (Tty, mpsc::Sender<Vec<u8>>, mpsc::Sender<Size>) {
    let (keys_tx, keys) = mpsc::channel(8);
    let (resizes_tx, resizes) = mpsc::channel(8);
    (
        Tty {
            size: SIZE,
            keys,
            resizes,
            restore: RestoreGuard::with_action(|| {}),
        },
        keys_tx,
        resizes_tx,
    )
}

/// The text of every `PtyInput` that has reached the shepherd.
fn typed(envelopes: &mut mpsc::UnboundedReceiver<Envelope>) -> String {
    let mut text = String::new();
    while let Ok(envelope) = envelopes.try_recv() {
        if let Request::PtyInput { data, .. } = envelope.body {
            text.push_str(&data);
        }
    }
    text
}

#[tokio::test]
async fn keys_are_sent_as_typed() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = accepting(dir.path()).await;
    let (tty, keys, _resizes) = tty();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);

    // The `é` is split across two reads, as a slow terminal can.
    let driver = async {
        for chunk in [&b"ls\r"[..], &[0xc3], &[0xa9], &[DETACH_KEY]] {
            keys.send(chunk.to_vec()).await.unwrap();
        }
    };
    let (code, ()) = tokio::time::timeout(BOUND, async {
        tokio::join!(session(&client, &mut streams, "web", tty), driver)
    })
    .await
    .expect("the session must end once the detach key is read");

    assert_eq!(code, ExitCode::Success);
    assert_eq!(typed(&mut envelopes), "ls\ré");
}

/// Runs [`session`] to its end with `keys` read first, one chunk each.
async fn run_keys(client: &Client, chunks: &[&[u8]]) -> (ExitCode, Vec<u8>, Vec<u8>) {
    let (tty, keys, _resizes) = tty();
    for chunk in chunks {
        keys.send(chunk.to_vec()).await.unwrap();
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = {
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        tokio::time::timeout(BOUND, session(client, &mut streams, "web", tty))
            .await
            .expect("the session must end")
    };
    (code, out, err)
}

#[tokio::test]
async fn ctrl_right_bracket_detaches_and_is_not_sent() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = accepting(dir.path()).await;

    let (code, _out, err) = run_keys(&client, &[b"ab\x1dcd"]).await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(typed(&mut envelopes), "ab");
    assert!(String::from_utf8(err).unwrap().contains("detached"));
}

#[tokio::test]
async fn the_window_size_is_sent_at_attach_and_on_resize() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = accepting(dir.path()).await;
    let (tty, keys, resizes) = tty();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);

    // Detaches only once the resize has reached the shepherd, so the order
    // of the session's own branches cannot swallow it.
    let driver = async {
        resizes
            .send(Size {
                rows: 30,
                cols: 100,
            })
            .await
            .unwrap();
        let mut seen = Vec::new();
        while !seen
            .iter()
            .any(|body| matches!(body, Request::PtyResize { .. }))
        {
            let envelope = tokio::time::timeout(BOUND, envelopes.recv())
                .await
                .expect("the resize must reach the shepherd")
                .expect("the connection must stay open");
            seen.push(envelope.body);
        }
        keys.send(vec![DETACH_KEY]).await.unwrap();
        seen
    };
    let (_code, seen) = tokio::time::timeout(BOUND, async {
        tokio::join!(session(&client, &mut streams, "web", tty), driver)
    })
    .await
    .expect("the session must end once the detach key is read");

    assert_eq!(
        seen,
        [
            Request::Attach {
                name: "web".to_owned(),
                rows: 24,
                cols: 80,
            },
            Request::PtyResize {
                name: "web".to_owned(),
                rows: 30,
                cols: 100,
            },
        ]
    );
}

#[tokio::test]
async fn a_refusal_for_a_sheep_without_pty_exits_non_zero_with_its_message() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) =
        fake_client_answering_with_events(&control_address(dir.path()), |_| {
            (
                Err(RpcError {
                    code: RpcErrorCode::InvalidConfig,
                    message: "web has no pty".to_owned(),
                    daemon_version: None,
                }),
                Vec::new(),
            )
        })
        .await;

    let (code, out, err) = run_keys(&client, &[]).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    assert!(out.is_empty());
    assert!(String::from_utf8(err).unwrap().contains("web has no pty"));
}

fn pty_out(data: &str) -> BusEvent {
    BusEvent::PtyOut {
        name: "web".to_owned(),
        data: data.to_owned(),
    }
}

/// An attach whose reply is followed by `events`. The receiver must outlive
/// the test: the fake shepherd hangs up when nothing reads its envelopes.
async fn attaching_then(
    dir: &std::path::Path,
    events: Vec<BusEvent>,
) -> (Client, mpsc::UnboundedReceiver<Envelope>) {
    fake_client_answering_with_events(&control_address(dir), move |_| {
        (
            Ok(Response::Attached {
                id: 1,
                name: "web".to_owned(),
            }),
            events.clone(),
        )
    })
    .await
}

#[tokio::test]
async fn output_is_written_unchanged_and_the_sheep_exiting_ends_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) = attaching_then(
        dir.path(),
        vec![
            pty_out("\x1b[2Jhi\r\n"),
            pty_out("é"),
            BusEvent::PtyEnd {
                name: "web".to_owned(),
            },
        ],
    )
    .await;

    // No keys, and a held-open channel: only the sheep's end can finish this.
    let (tty, _keys, _resizes) = tty();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = {
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        tokio::time::timeout(BOUND, session(&client, &mut streams, "web", tty))
            .await
            .expect("the sheep's end must end the session")
    };

    assert_eq!(code, ExitCode::Success);
    assert_eq!(out, "\x1b[2Jhi\r\né".as_bytes());
    assert!(String::from_utf8(err).unwrap().contains("the sheep exited"));
}

/// A stdout whose write panics, standing in for any bug in the session.
struct PanicsOnWrite;

impl std::io::Write for PanicsOnWrite {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        panic!("a deliberate panic in the middle of a session");
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn the_terminal_is_restored_after_a_panic() {
    use futures_util::FutureExt as _;

    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) = attaching_then(dir.path(), vec![pty_out("boom")]).await;
    let restored = Arc::new(AtomicUsize::new(0));
    let (mut tty, _keys, _resizes) = tty();
    tty.restore = RestoreGuard::with_action({
        let restored = Arc::clone(&restored);
        move || {
            restored.fetch_add(1, Ordering::SeqCst);
        }
    });
    let mut err = Vec::new();
    let mut out = PanicsOnWrite;
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);

    let outcome = tokio::time::timeout(
        BOUND,
        std::panic::AssertUnwindSafe(session(&client, &mut streams, "web", tty)).catch_unwind(),
    )
    .await
    .expect("the panic must surface");

    assert!(outcome.is_err(), "the session was meant to panic");
    assert_eq!(restored.load(Ordering::SeqCst), 1);
}
