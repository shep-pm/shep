use futures_util::{SinkExt, StreamExt};
use shep_core::protocol::{
    BusEvent, DogSource, Envelope, ExitInfo, Hello, HelloAck, HelloReply, Lamb, PROTOCOL_VERSION,
    ProcessEventKind, ProcessInfo, Reply, Response, RpcError, RpcErrorCode, decode_frame,
    encode_frame,
};
use shep_core::status::ProcStatus;
use shep_core::transport::{Listener, ServerStream};
use std::path::{Path, PathBuf};
use tokio_util::codec::Framed;

/// A control address valid on the platform running the test, unique to
/// `dir`.
///
/// Unix uses `dir` directly. Windows names a pipe in a machine-global
/// namespace instead of a path under `dir`, matching
/// [`ShepPaths::pipe_name`](shep_core::paths::ShepPaths::pipe_name)'s own
/// derivation; the pid is folded in too, since each `cargo test` binary is
/// its own process and could otherwise collide on a shared `TempDir` name.
#[must_use]
pub fn control_address(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        dir.join("s.sock")
    }
    #[cfg(windows)]
    {
        let sanitized: String = dir
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        PathBuf::from(format!(
            r"\\.\pipe\shep-test-{}-{}",
            std::process::id(),
            sanitized.trim_matches('-')
        ))
    }
}

/// Binds `path`, naming it if the bind fails.
///
/// Thirteen fakes in this module bind a listener, and a bare `unwrap` on
/// any of them reports an `AddrInUse` or a `NotFound` without saying which
/// address it was.
///
/// # Panics
///
/// If `path` cannot be bound.
pub(super) fn bind(path: &Path) -> Listener {
    Listener::bind(path).unwrap_or_else(|error| panic!("bind {}: {error}", path.display()))
}

/// The framed transport a fake daemon holds for one accepted client.
///
/// Not [`crate::connection::Frames`], the client's side: the two coincide
/// on unix but differ on Windows, where a named pipe's server end is its
/// own type.
pub(super) type Frames = Framed<ServerStream, tokio_util::codec::LengthDelimitedCodec>;

/// A `HelloAck` with a distinctive version and pid, so a test that asserts
/// on either can tell a real read from a default.
#[must_use]
pub fn sample_ack() -> HelloAck {
    HelloAck {
        daemon_version: "9.9.9".into(),
        protocol: PROTOCOL_VERSION,
        pid: 4242,
        min_supported: None,
    }
}

/// One fully-populated [`ProcessInfo`]: every `Option` is `Some`, so an
/// anti-drift test sees every serialized field.
#[must_use]
pub fn sample_info() -> ProcessInfo {
    ProcessInfo::builder(1, "web", ProcStatus::Online)
        .pid(Some(4242))
        .restarts(3)
        .uptime_ms(60_000)
        .fold(Some("backend".to_string()))
        .out_file(Some("/home/ada/.shep/logs/web-0-out.log".to_string()))
        .err_file(Some("/home/ada/.shep/logs/web-0-err.log".to_string()))
        .cpu_percent(Some(12.5))
        .memory_bytes(Some(48 * 1024 * 1024))
        .dog(Some(DogSource::BuiltIn))
        .lambs(Some(vec![Lamb::new(4243, "node")]))
        // `restarts: 3` already implies an exit; give it a real outcome
        // rather than `None` so `last_exit`'s JSON shape gets exercised too.
        .last_exit(Some(ExitInfo {
            code: Some(1),
            signal: None,
        }))
        // Non-ASCII, like a real deploy dog's mark: every `Option` here
        // must be `Some`.
        .smit(Some("\u{25b2} main@a1b2c3".to_string()))
        .build()
}

/// Completes the handshake: reads the client's `Hello`, answers with
/// `ack`, and returns the `Hello`.
///
/// Panics on any accept, read, decode or write failure.
pub(super) async fn handshake(frames: &mut Frames, ack: HelloAck) -> Hello {
    let first = frames.next().await.unwrap().unwrap();
    let hello: Hello = decode_frame(&first).unwrap();
    let reply: HelloReply = Ok(ack);
    frames.send(encode_frame(&reply).unwrap()).await.unwrap();
    hello
}

/// Reads and decodes the next envelope. Panics on failure or a closed
/// connection.
pub(super) async fn read_envelope(frames: &mut Frames) -> Envelope {
    let frame = frames.next().await.unwrap().unwrap();
    decode_frame(&frame).unwrap()
}

/// Encodes and sends one successful [`Reply`] for `id`. Panics on failure.
pub(super) async fn write_reply(frames: &mut Frames, id: u64, response: Response) {
    let reply = Reply {
        id,
        result: Ok(response),
    };
    frames.send(encode_frame(&reply).unwrap()).await.unwrap();
}

/// Encodes and sends one error [`Reply`] for `id`. Panics on failure.
pub(super) async fn write_err(frames: &mut Frames, id: u64, code: RpcErrorCode, message: String) {
    let reply = Reply {
        id,
        result: Err(RpcError {
            code,
            message,
            daemon_version: None,
        }),
    };
    frames.send(encode_frame(&reply).unwrap()).await.unwrap();
}

/// Encodes and sends one [`BusEvent`] frame directly, not wrapped in a
/// [`Reply`]: the shape a real subscriber receives. Panics on failure.
pub(super) async fn write_event(frames: &mut Frames, event: BusEvent) {
    frames.send(encode_frame(&event).unwrap()).await.unwrap();
}

/// Sends a `BusEvent::Process` built from [`sample_info`]: a sheep's bus
/// event can legitimately arrive ahead of the reply for the request that
/// caused it. Panics on failure.
pub(super) async fn send_sample_event(frames: &mut Frames) {
    write_event(
        frames,
        BusEvent::Process {
            event: ProcessEventKind::Online,
            info: sample_info(),
            manually: false,
            at_ms: 0,
        },
    )
    .await;
}
