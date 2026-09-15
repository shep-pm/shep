//! The two readiness reports, and when each is made
//!
//! `ready_fd` answers the parent shep process the moment the socket is bound,
//! because that parent is waiting to exit. `notify_socket` is written last,
//! after the muster restore, because an init system that goes green at exec
//! time is reporting a flock that is not up.

use crate::boot::*;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::snapshot::{FlockSnapshot, SNAPSHOT_VERSION, SavedApp};
use crate::testing::{AnnouncingRunner, test_paths};
use shep_core::config::AppConfig;
use std::time::Duration;

#[tokio::test]
async fn boot_writes_readiness_to_the_callers_pipe_after_the_socket_is_bound() {
    // Real time: binds a real socket. Locked per SIGNAL_TEST_LOCK's rule.
    // The only test driving a `Some` `ready_fd` through `boot`. A bad
    // descriptor cannot reach `BootOptions::ready_fd`, whose type is
    // `Option<std::fs::File>`, so `sys::tests` covers that refusal.
    use std::io::Read;
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);

    let (mut reader, writer) = std::io::pipe().unwrap();
    let pipe = std::fs::File::from(std::os::fd::OwnedFd::from(writer));

    let daemon = boot(
        ScriptedRunner::new(vec![]),
        paths.clone(),
        BootOptions {
            ready_fd: Some(pipe),
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(
        paths.socket.exists(),
        "boot must bind the socket before it returns"
    );

    // `write_ready` closes its `File`, so this read sees the line and then
    // EOF rather than blocking on a live writer.
    let mut line = String::new();
    reader.read_to_string(&mut line).unwrap();
    let ready: DaemonReady = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(ready.pid, std::process::id());
    assert!(line.ends_with('\n'), "the parent reads a line: {line:?}");

    drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
}

/// The ordering `Type=notify` was chosen for: a unit that goes green at
/// exec time reports a flock that is not up yet, and a hung restore reads
/// as a healthy service supervising nothing.
///
/// The restore announces its own spawn on the same socket, so what is
/// asserted is the queue order of two datagrams. Reading only `READY=1`
/// after `boot` returns would pass on a notify moved to the top of `boot`,
/// since the kernel keeps that datagram queued however early it was sent.
#[tokio::test]
async fn readiness_is_reported_only_once_the_roll_is_restored() {
    // Real time: binds a real socket, so this obeys SIGNAL_TEST_LOCK's
    // rule like every other successful `boot()` in this module.
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();
    crate::snapshot::write_atomic(
        &paths.snapshot,
        &FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app: AppConfig::minimal("web", "./srv"),
                instances_running: 1,
            }],
        },
    )
    .unwrap();

    // Inside the TempDir and short: macOS caps a unix socket path near
    // 97 characters, which `test_paths` already keeps this under.
    let notify_path = dir.path().join("n.sock");
    let listener = std::os::unix::net::UnixDatagram::bind(&notify_path).unwrap();
    // Bounded: a datagram that never arrives must fail this case, not
    // park it.
    listener
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // The marker sent from inside the restore's own spawn. AF_UNIX
    // SOCK_DGRAM enqueues synchronously and the two sends are sequential,
    // so the queue order is the program order.
    let runner = AnnouncingRunner::new(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        &notify_path,
    );

    let daemon = boot(
        runner,
        paths.clone(),
        BootOptions {
            restore: true,
            notify_socket: Some(notify_path.clone().into_os_string()),
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let mut buf = [0u8; 64];
    let read = listener.recv(&mut buf).unwrap();
    assert_eq!(
        &buf[..read],
        b"SPAWNED\n",
        "READY=1 arrived before the roll was restored: a unit that goes \
             green at exec time reports a flock that is not up yet, and a \
             restore that hangs reads as a healthy service supervising nothing"
    );
    let read = listener.recv(&mut buf).unwrap();
    assert_eq!(&buf[..read], b"READY=1\n");

    let ctx = daemon.context();
    let flock = ctx.supervisor.list_checked().await.unwrap();
    assert_eq!(flock.len(), 1, "the roll was actually restored");
    assert_eq!(flock[0].name, "web");

    ctx.shutdown();
    daemon.run().await.unwrap();
}

/// Nothing is bound at the address, so the send errors and the boot must
/// still succeed: what failed is the init system's knowledge of a daemon
/// that is otherwise up, which systemd reports through its own
/// `TimeoutStartSec`.
#[tokio::test]
async fn a_readiness_datagram_that_cannot_be_delivered_does_not_fail_the_boot() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);

    let daemon = boot(
        ScriptedRunner::new(vec![]),
        paths.clone(),
        BootOptions {
            // Bound by nothing, and never created by anything: the send
            // is an error, which is the whole premise of the case.
            notify_socket: Some(dir.path().join("nobody.sock").into_os_string()),
            ..BootOptions::default()
        },
    )
    .await
    .expect("a daemon nobody could be told about is still a daemon");

    // Up enough to serve, not merely constructed.
    assert!(daemon.context().supervisor.list_checked().await.is_ok());

    daemon.context().shutdown();
    daemon.run().await.unwrap();
}
