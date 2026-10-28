//! `shep dial-stdio`: the remote end of the ssh transport.
//!
//! `ssh <dest> shep dial-stdio` runs this on the machine that holds the
//! shepherd, and the existing length-prefixed frames cross unchanged. The verb
//! copies bytes and nothing else: it does no handshake, no version check, and
//! never looks inside a frame.
//!
//! With `--autostart` it first boots the shepherd when none answers, which is
//! how `shep --host <dest> muster` brings up a host that has just rebooted.
//! No other verb asks for it.

use std::convert::Infallible;
use std::io;
use std::path::Path;
use std::process::Child;

use shep_client::spawn::{SpawnError, connect_or_spawn};
use shep_client::{ConnectError, HANDSHAKE_TIMEOUT};
use shep_core::paths::ShepPaths;
use shep_core::transport;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::cli::Format;
use crate::client::emit_error_locked;
use crate::exit::ExitCode;
use crate::launch::launch_daemon;

/// Copies bytes between stdin/stdout and the shepherd at `paths.socket`, then
/// ends the process. With `autostart`, a shepherd is booted first if none
/// answers.
///
/// Never returns. tokio's stdin read runs on a blocking thread that dropping
/// the runtime waits for, so returning an [`ExitCode`] would hang the process.
pub(crate) async fn dial_stdio(fmt: Format, paths: &ShepPaths, autostart: bool) -> Infallible {
    let launch_paths = paths.clone();
    let code = dial(
        fmt,
        &paths.socket,
        autostart,
        move || launch_daemon(&launch_paths),
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await;
    std::process::exit(i32::from(code as u8));
}

/// Connects to the shepherd at `socket` and runs [`bridge`] over it.
///
/// With `autostart`, `launch` boots the shepherd when none answers, through
/// the state machine a local `muster` uses. Its handshake is spent.
///
/// Nothing is written to `output` when no shepherd answers or the boot fails:
/// the one error goes to stderr.
async fn dial<I, O, L>(
    fmt: Format,
    socket: &Path,
    autostart: bool,
    launch: L,
    input: I,
    output: O,
) -> ExitCode
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    L: FnOnce() -> io::Result<Child> + Send + 'static,
{
    if autostart {
        match connect_or_spawn(socket, launch).await {
            Ok(_spent) => {}
            // A shepherd that refuses on version skew is still answering. The
            // bridge copies bytes, so the operator's client judges the skew
            // and names it, as it would without the flag.
            Err(SpawnError::Connect(ConnectError::ProtocolMismatch { .. })) => {}
            Err(err) => {
                let code = ExitCode::from(&err);
                emit_error_locked(fmt, code, &err.to_string());
                return code;
            }
        }
    }
    let unreachable = |why: &dyn core::fmt::Display| {
        emit_error_locked(
            fmt,
            ExitCode::DaemonUnreachable,
            &format!("no shepherd answered at {}: {why}", socket.display()),
        );
        ExitCode::DaemonUnreachable
    };
    let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, transport::connect(socket)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => return unreachable(&err),
        Err(_) => return unreachable(&"timed out"),
    };
    match bridge(input, output, stream).await {
        Ok(()) => ExitCode::Success,
        Err(err) => {
            emit_error_locked(fmt, ExitCode::Failure, &format!("connection lost: {err}"));
            ExitCode::Failure
        }
    }
}

/// Copies `input` to `shepherd` and `shepherd` to `output` until either side
/// reaches end of file or errors, then flushes `output`.
///
/// End of `input` shuts down the write half toward the shepherd, so the
/// shepherd sees an orderly end rather than a reset. A Windows named pipe has
/// no half-close: its shutdown only flushes, and the shepherd sees the end when
/// the pipe handle closes, which for [`dial_stdio`] is the process exiting.
///
/// # Errors
///
/// The first I/O error on either direction, or on the final flush.
async fn bridge<I, O, S>(mut input: I, mut output: O, shepherd: S) -> io::Result<()>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite,
{
    let (mut from_shepherd, mut to_shepherd) = tokio::io::split(shepherd);
    let ended = {
        let upstream = async {
            tokio::io::copy(&mut input, &mut to_shepherd).await?;
            to_shepherd.shutdown().await
        };
        let downstream = async {
            tokio::io::copy(&mut from_shepherd, &mut output)
                .await
                .map(drop)
        };
        tokio::select! {
            ended = upstream => ended,
            ended = downstream => ended,
        }
    };
    ended.and(output.flush().await)
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use tokio::io::{AsyncReadExt, duplex};

    use super::*;

    /// Long enough for a loaded CI machine, short enough to fail as a named
    /// assertion rather than a hung run.
    const BOUND: Duration = Duration::from_secs(10);

    const BLOB_BYTES: usize = 4 * 1024 * 1024;

    /// Every byte value once, then a blob whose bytes come from a small LCG so
    /// a shifted or dropped run changes the comparison.
    fn payloads() -> [Vec<u8>; 2] {
        let every_value = (0..=u8::MAX).collect();
        let mut state = 0x9e37_79b9_u32;
        let blob = (0..BLOB_BYTES)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect();
        [every_value, blob]
    }

    #[tokio::test]
    async fn bytes_cross_both_ways_unchanged() {
        for payload in payloads() {
            // stdin to the shepherd: the bridge ends on end of input. The
            // bridge owns `client`, so the shepherd's end of file arrives on
            // every platform when it returns.
            let (mut server, client) = transport::connected_pair().await.unwrap();
            let (mut stdin_tx, stdin) = duplex(64 * 1024);
            let (stdout, _stdout_rx) = duplex(64 * 1024);
            let (bridged, (), received) = tokio::time::timeout(BOUND, async {
                tokio::join!(
                    bridge(stdin, stdout, client),
                    async {
                        stdin_tx.write_all(&payload).await.unwrap();
                        drop(stdin_tx);
                    },
                    async {
                        let mut received = Vec::new();
                        server.read_to_end(&mut received).await.unwrap();
                        received
                    },
                )
            })
            .await
            .expect("upstream copy finished");
            bridged.unwrap();
            assert!(received == payload, "stdin to shepherd altered the bytes");

            // The shepherd to stdout: the bridge ends when the shepherd closes.
            let (mut server, client) = transport::connected_pair().await.unwrap();
            let (_stdin_tx, stdin) = duplex(64 * 1024);
            let (stdout, mut stdout_rx) = duplex(64 * 1024);
            let (bridged, (), received) = tokio::time::timeout(BOUND, async {
                tokio::join!(
                    bridge(stdin, stdout, client),
                    async {
                        server.write_all(&payload).await.unwrap();
                        drop(server);
                    },
                    async {
                        let mut received = Vec::new();
                        stdout_rx.read_to_end(&mut received).await.unwrap();
                        received
                    },
                )
            })
            .await
            .expect("downstream copy finished");
            bridged.unwrap();
            assert!(received == payload, "shepherd to stdout altered the bytes");
        }
    }

    #[tokio::test]
    async fn the_bridge_ends_when_the_shepherd_side_closes_with_input_still_open() {
        let (server, client) = transport::connected_pair().await.unwrap();
        let (_stdin_tx, stdin) = duplex(1024);
        let (stdout, _stdout_rx) = duplex(1024);
        drop(server);

        let bridged = tokio::time::timeout(BOUND, bridge(stdin, stdout, client))
            .await
            .expect("the bridge outlived the shepherd side");
        bridged.unwrap();
    }

    #[tokio::test]
    async fn end_of_input_closes_the_write_half_toward_the_shepherd_and_ends_the_bridge() {
        let (mut server, mut client) = transport::connected_pair().await.unwrap();
        let (mut stdin_tx, stdin) = duplex(1024);
        let (stdout, _stdout_rx) = duplex(1024);
        stdin_tx.write_all(b"last words").await.unwrap();
        drop(stdin_tx);

        // `client` is lent, so it is still open when the bridge returns: on
        // unix the shepherd's end of file below comes from the shutdown, not a
        // drop.
        let bridged = tokio::time::timeout(BOUND, bridge(stdin, stdout, &mut client))
            .await
            .expect("the bridge outlived end of input");
        bridged.unwrap();
        // A named pipe has no half-close, so only closing the handle ends the
        // shepherd's read there.
        #[cfg(windows)]
        drop(client);

        let mut received = Vec::new();
        tokio::time::timeout(BOUND, server.read_to_end(&mut received))
            .await
            .expect("the shepherd never saw end of file")
            .unwrap();
        assert_eq!(received, b"last words");
    }

    #[tokio::test]
    async fn a_missing_socket_is_daemon_unreachable_and_writes_nothing_to_the_output_half() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run").join("shep.sock");
        let (_stdin_tx, stdin) = duplex(1024);
        let (stdout, mut stdout_rx) = duplex(1024);

        let code = dial(Format::Table, &socket, false, no_launch, stdin, stdout).await;

        assert_eq!(code, ExitCode::DaemonUnreachable);
        let mut written = Vec::new();
        stdout_rx.read_to_end(&mut written).await.unwrap();
        assert!(written.is_empty(), "stdout got {written:?}");
    }

    /// A launcher for a run that must not boot anything.
    fn no_launch() -> io::Result<Child> {
        unreachable!("no flag asked for an autostart")
    }

    #[tokio::test]
    async fn without_autostart_nothing_is_launched() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (_stdin_tx, stdin) = duplex(1024);
        let (stdout, _stdout_rx) = duplex(1024);

        // The launcher panics, and a panic inside it resumes on the caller.
        let code = dial(Format::Table, &socket, false, no_launch, stdin, stdout).await;

        assert_eq!(code, ExitCode::DaemonUnreachable);
    }

    /// Unix only: the fake shepherd accepts the autostart handshake alone, and
    /// a named pipe has no second connection to bridge over.
    #[cfg(unix)]
    #[tokio::test]
    async fn autostart_boots_a_shepherd_before_bridging() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let launched = Arc::new(AtomicBool::new(false));
        let (stdin_tx, stdin) = duplex(1024);
        let (stdout, mut stdout_rx) = duplex(1024);
        drop(stdin_tx);
        let launcher = {
            let (socket, launched) = (socket.clone(), Arc::clone(&launched));
            move || {
                launched.store(true, Ordering::SeqCst);
                shep_client::testing::start_fake_daemon_answering_on(&socket);
                // The losing side of a cold-start race: a child that is not
                // the daemon, which the state machine tolerates.
                shep_client::testing::child_exiting_with(shep_client::spawn::DAEMON_ALREADY_RUNNING)
            }
        };

        let code = tokio::time::timeout(
            BOUND,
            dial(Format::Table, &socket, true, launcher, stdin, stdout),
        )
        .await
        .expect("the bridge outlived end of input");

        assert!(launched.load(Ordering::SeqCst), "nothing was launched");
        assert_eq!(code, ExitCode::Success);
        let mut written = Vec::new();
        stdout_rx.read_to_end(&mut written).await.unwrap();
        assert!(written.is_empty(), "stdout got {written:?}");
    }

    /// A shepherd outside this binary's protocol range is still a shepherd:
    /// the probe's refusal is not the bridge's to report, and the operator's
    /// client names the skew itself. Unix only, like the autostart test: a
    /// named pipe has no second connection to bridge over.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_protocol_mismatch_on_the_probe_still_bridges() {
        use shep_core::protocol::{HelloReply, RpcError, RpcErrorCode, encode_frame};
        use tokio::io::AsyncReadExt as _;

        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let mut listener = transport::Listener::bind(&socket).unwrap();
        let refusal: HelloReply = Err(RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: "daemon speaks protocol 2, client speaks 1".into(),
            daemon_version: None,
        });
        let refusal = encode_frame(&refusal).unwrap();
        let shepherd = tokio::spawn(async move {
            // The probe: read its hello frame, refuse it, close.
            let mut probe = listener.accept().await.unwrap();
            let mut prefix = [0_u8; 4];
            probe.read_exact(&mut prefix).await.unwrap();
            let mut hello = vec![0_u8; u32::from_be_bytes(prefix) as usize];
            probe.read_exact(&mut hello).await.unwrap();
            probe
                .write_all(&u32::try_from(refusal.len()).unwrap().to_be_bytes())
                .await
                .unwrap();
            probe.write_all(&refusal).await.unwrap();
            drop(probe);
            // The bridge: bytes the dialler never has to understand.
            let mut bridged = listener.accept().await.unwrap();
            bridged.write_all(b"frames").await.unwrap();
        });
        let (_stdin_tx, stdin) = duplex(1024);
        let (stdout, mut stdout_rx) = duplex(1024);

        let code = tokio::time::timeout(
            BOUND,
            dial(Format::Table, &socket, true, no_launch, stdin, stdout),
        )
        .await
        .expect("the bridge outlived the shepherd side");

        assert_eq!(code, ExitCode::Success);
        let mut written = Vec::new();
        stdout_rx.read_to_end(&mut written).await.unwrap();
        assert_eq!(written, b"frames");
        shepherd.await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_autostart_exits_with_its_spawn_code_and_writes_nothing_to_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let (_stdin_tx, stdin) = duplex(1024);
        let (stdout, mut stdout_rx) = duplex(1024);
        let refused = || Err(io::Error::other("no such binary"));

        let code = dial(Format::Table, &socket, true, refused, stdin, stdout).await;

        assert_eq!(code, ExitCode::DaemonUnreachable);
        let mut written = Vec::new();
        stdout_rx.read_to_end(&mut written).await.unwrap();
        assert!(written.is_empty(), "stdout got {written:?}");
    }
}
