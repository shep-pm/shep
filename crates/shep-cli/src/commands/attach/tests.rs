use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use shep_client::testing::{
    control_address, fake_client_answering_with_events, fake_client_leaving_unanswered,
};
use shep_core::protocol::{
    BusEvent, Envelope, MAX_FRAME_BYTES, Request, Response, RpcError, RpcErrorCode, encode_frame,
};
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

/// The `data` of every `PtyInput` that has reached the shepherd, in order.
fn batches(envelopes: &mut mpsc::UnboundedReceiver<Envelope>) -> Vec<Vec<u8>> {
    let mut batches = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        if let Request::PtyInput { data, .. } = envelope.body {
            batches.push(data);
        }
    }
    batches
}

/// The bytes of every `PtyInput` that has reached the shepherd.
fn typed(envelopes: &mut mpsc::UnboundedReceiver<Envelope>) -> Vec<u8> {
    batches(envelopes).concat()
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
    assert_eq!(typed(&mut envelopes), b"ls\r\xc3\xa9");
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
    assert_eq!(typed(&mut envelopes), b"ab");
    assert!(String::from_utf8(err).unwrap().contains("detached"));
}

/// An X10 mouse report for a column past 95, whose coordinate bytes are no
/// character, and a lone byte piped into stdin.
#[tokio::test]
async fn a_byte_that_is_not_utf8_reaches_the_shepherd_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = accepting(dir.path()).await;

    // `ESC [ M`, button 0x23 (press), column 200 as 0xe8, row 168 as 0xc8,
    // then 0x9b, which starts nothing.
    let (code, _out, _err) = run_keys(
        &client,
        &[&[0x1b, b'[', b'M', 0x23, 0xe8, 0xc8], &[0x9b, DETACH_KEY]],
    )
    .await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        typed(&mut envelopes),
        [0x1b, b'[', b'M', 0x23, 0xe8, 0xc8, 0x9b]
    );
}

#[tokio::test]
async fn the_detach_key_cuts_what_is_sent_at_that_byte() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = accepting(dir.path()).await;

    let (code, _out, _err) =
        run_keys(&client, &[&[0x1b, b'M', 0xe8, DETACH_KEY, 0x9b, b'x']]).await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(typed(&mut envelopes), [0x1b, b'M', 0xe8]);
}

/// A read bigger than one `PtyInput` may carry goes as several requests, in
/// order, rather than one frame the wire refuses.
///
/// The detach key waits until every byte has arrived: the detach flush is
/// bounded by wall-clock time, and a slow runner can spend longer than that
/// encoding a batch this large.
#[tokio::test]
async fn a_paste_over_the_wire_cap_reaches_the_shepherd_in_pieces() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = accepting(dir.path()).await;
    let (tty, keys, _resizes) = tty();

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
    let paste: Vec<u8> = vec![0xff; MAX_INPUT_BYTES + 4096];
    let driver = async {
        keys.send(paste.clone()).await.unwrap();
        let mut sent: Vec<Vec<u8>> = Vec::new();
        while sent.iter().map(Vec::len).sum::<usize>() < paste.len() {
            let envelope = envelopes.recv().await.expect("the shepherd went away");
            if let Request::PtyInput { data, .. } = envelope.body {
                sent.push(data);
            }
        }
        keys.send(vec![DETACH_KEY]).await.unwrap();
        sent
    };
    let (code, sent) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(session(&client, &mut streams, "web", tty), driver)
    })
    .await
    .expect("the paste must arrive and the session end");

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        sent.iter().map(Vec::len).collect::<Vec<usize>>(),
        [MAX_INPUT_BYTES, 4096],
        "no batch may exceed the cap"
    );
    let sent = sent.concat();
    assert_eq!(sent.len(), paste.len());
    assert!(
        sent.iter()
            .zip(paste.iter())
            .all(|(sent, pasted)| sent == pasted),
        "the paste arrived out of order or with bytes missing"
    );
}

#[test]
fn a_batch_at_the_cap_encodes_under_the_frame_limit() {
    // 0xff is the widest a byte goes: three digits and a comma.
    let mut queued: VecDeque<u8> = vec![0xff_u8; MAX_INPUT_BYTES].into();
    let Request::PtyInput { name, data } = input("web", &mut queued) else {
        panic!("`input` builds a `PtyInput`");
    };
    assert_eq!(data.len(), MAX_INPUT_BYTES);
    assert!(queued.is_empty());

    let frame = encode_frame(&Envelope {
        id: 1,
        deadline_ms: None,
        body: Request::PtyInput { name, data },
    })
    .unwrap();
    assert!(
        frame.len() < MAX_FRAME_BYTES,
        "the frame is {} bytes",
        frame.len()
    );
}

/// The keys a session read before stdin closed are not lost with it.
#[tokio::test]
async fn keys_read_before_stdin_closes_still_reach_the_sheep() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = accepting(dir.path()).await;

    let (tty, keys, _resizes) = tty();
    keys.send(b"ls\r".to_vec()).await.unwrap();
    drop(keys);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
    let code = tokio::time::timeout(BOUND, session(&client, &mut streams, "web", tty))
        .await
        .expect("a closed stdin must end the session");

    assert_eq!(code, ExitCode::Success);
    assert_eq!(typed(&mut envelopes), b"ls\r");
}

/// Each mode a sheep can turn on by printing an escape sequence is turned
/// off again, so the shell after a detach does not read them as input.
#[test]
fn leaving_turns_off_the_modes_a_sheep_can_switch_on() {
    let mut out = Vec::new();
    reset_modes(&mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    for (mode, off) in [
        ("mouse tracking", "\x1b[?1000l"),
        ("button-event mouse tracking", "\x1b[?1002l"),
        ("any-event mouse tracking", "\x1b[?1003l"),
        ("SGR mouse coordinates", "\x1b[?1006l"),
        ("focus reports", "\x1b[?1004l"),
        ("application cursor keys", "\x1b[?1l"),
        ("the kitty keyboard stack", "\x1b[<99u"),
    ] {
        assert!(out.contains(off), "{mode} is not turned off: {out:?}");
    }
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

fn pty_out(data: &[u8]) -> BusEvent {
    BusEvent::PtyOut {
        name: "web".to_owned(),
        data: data.to_vec(),
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
            pty_out(b"\x1b[2Jhi\r\n"),
            pty_out("é".as_bytes()),
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

/// A sheep printing Latin-1 or binary shows what it printed: the bytes are
/// written to the terminal as they arrived, and nothing decodes them.
///
/// 0x9b starts no character, 0xc0 0xc1 is an overlong pair UTF-8 refuses, and
/// 0xff is no character at all.
#[tokio::test]
async fn output_that_is_not_utf8_reaches_the_terminal_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let written: Vec<u8> = vec![0x9b, 0xc0, 0xc1, b'x', 0xff];
    let (client, _envelopes) = attaching_then(
        dir.path(),
        vec![
            pty_out(&written),
            BusEvent::PtyEnd {
                name: "web".to_owned(),
            },
        ],
    )
    .await;

    let (tty, _keys, _resizes) = tty();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = {
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        tokio::time::timeout(BOUND, session(&client, &mut streams, "web", tty))
            .await
            .expect("the sheep's end must end the session")
    };

    assert_eq!(code, ExitCode::Success);
    assert_eq!(out, written);
    assert!(String::from_utf8(err).unwrap().contains("the sheep exited"));
}

fn attached() -> Response {
    Response::Attached {
        id: 1,
        name: "web".to_owned(),
    }
}

#[tokio::test]
async fn a_sheep_that_exits_while_a_key_waits_ends_the_session_as_exited() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) =
        fake_client_answering_with_events(&control_address(dir.path()), |request| match request {
            Request::PtyInput { .. } => (
                Err(RpcError {
                    code: RpcErrorCode::InvalidConfig,
                    message: "sheep web has no pty".to_owned(),
                    daemon_version: None,
                }),
                Vec::new(),
            ),
            _ => (
                Ok(attached()),
                vec![BusEvent::PtyEnd {
                    name: "web".to_owned(),
                }],
            ),
        })
        .await;

    let (code, _out, err) = run_keys(&client, &[b"x"]).await;

    assert_eq!(code, ExitCode::Success);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("the sheep exited"), "{err}");
    assert!(!err.contains("no pty"), "{err}");
}

#[tokio::test]
async fn dropped_events_are_reported_rather_than_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) = attaching_then(
        dir.path(),
        vec![
            BusEvent::Dropped { count: 7 },
            pty_out(b"a"),
            BusEvent::PtyEnd {
                name: "web".to_owned(),
            },
        ],
    )
    .await;
    let (tty, _keys, _resizes) = tty();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = {
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        tokio::time::timeout(BOUND, session(&client, &mut streams, "web", tty))
            .await
            .expect("the sheep's end must end the session")
    };

    assert_eq!(code, ExitCode::Success);
    assert_eq!(out, b"a");
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("7 events were dropped"), "{err}");
}

/// A pty write the shepherd never answers must not keep the operator from
/// leaving.
#[tokio::test]
async fn ctrl_right_bracket_detaches_while_a_write_is_stalled() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) =
        fake_client_leaving_unanswered(&control_address(dir.path()), |request| match request {
            Request::PtyInput { .. } => None,
            _ => Some((Ok(attached()), Vec::new())),
        })
        .await;
    let (tty, keys, _resizes) = tty();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);

    let driver = async {
        keys.send(b"ab".to_vec()).await.unwrap();
        // The write is on the wire once the shepherd has read it.
        while !matches!(
            tokio::time::timeout(BOUND, envelopes.recv())
                .await
                .expect("the write must reach the shepherd")
                .expect("the connection must stay open")
                .body,
            Request::PtyInput { .. }
        ) {}
        // More text and the detach key, while the first write is unanswered.
        keys.send(b"cd\x1d".to_vec()).await.unwrap();
    };
    let (code, ()) = tokio::time::timeout(BOUND, async {
        tokio::join!(session(&client, &mut streams, "web", tty), driver)
    })
    .await
    .expect("the detach key must end a session whose write is unanswered");

    assert_eq!(code, ExitCode::Success);
    assert!(String::from_utf8(err).unwrap().contains("detached"));
}

#[tokio::test]
async fn the_terminal_is_restored_when_the_session_is_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) = accepting(dir.path()).await;
    let restored = Arc::new(AtomicUsize::new(0));
    let (mut tty, _keys, _resizes) = tty();
    tty.restore = RestoreGuard::with_action({
        let restored = Arc::clone(&restored);
        move || {
            restored.fetch_add(1, Ordering::SeqCst);
        }
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);

    // The way a SIGTERM ends it: the session future is dropped mid-run.
    tokio::select! {
        _code = session(&client, &mut streams, "web", tty) => panic!("nothing ends this session"),
        () = std::future::ready(()) => {}
    }

    assert_eq!(restored.load(Ordering::SeqCst), 1);
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
    let (client, _envelopes) = attaching_then(dir.path(), vec![pty_out(b"boom")]).await;
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
