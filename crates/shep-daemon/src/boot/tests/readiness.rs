//! The two readiness reports, and when each is made
//!
//! `ready_fd` answers the parent shep process the moment the socket is bound,
//! because that parent is waiting to exit. `notify_socket` is written last,
//! after the muster restore, because an init system that goes green at exec
//! time is reporting a flock that is not up.

use crate::boot::*;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::testing::{AnnouncingRunner, test_paths};
use shep_core::config::AppConfig;
use shep_core::values::UpDuration;
use std::time::Duration;

use super::seed_roll;

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

    let daemon = boot_with_idle_sweep(
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
    seed_roll(&paths, vec![AppConfig::minimal("web", "./srv")]);

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

    let daemon = boot_with_idle_sweep(
        runner,
        paths,
        BootOptions {
            restore: true,
            notify_socket: Some(notify_path.clone().into_os_string()),
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let mut buf = [0u8; 64];
    // The stage's own deadline extension, which `each_restored_stage_extends…`
    // owns; it is queued ahead of the spawn it announces.
    let read = listener.recv(&mut buf).unwrap();
    assert!(buf[..read].starts_with(b"EXTEND_TIMEOUT_USEC="));
    let read = listener.recv(&mut buf).unwrap();
    assert_eq!(
        &buf[..read],
        b"SPAWNED\n",
        "READY=1 arrived before the roll was restored: a unit that goes \
             green at exec time reports a flock that is not up yet, and a \
             restore that hangs reads as a healthy service supervising nothing"
    );
    // The extension for the setup after the restore, likewise owned by the
    // case below.
    let read = listener.recv(&mut buf).unwrap();
    assert!(buf[..read].starts_with(b"EXTEND_TIMEOUT_USEC="));
    let read = listener.recv(&mut buf).unwrap();
    assert_eq!(&buf[..read], b"READY=1\n");

    let ctx = daemon.context();
    let flock = ctx.supervisor.list_checked().await.unwrap();
    assert_eq!(flock.len(), 1, "the roll was actually restored");
    assert_eq!(flock[0].name, "web");

    ctx.shutdown();
    daemon.run().await.unwrap();
}

/// The deadline systemd holds a `Type=notify` unit to is one number, and a
/// staged restore spends it a stage at a time. Each stage announces the most
/// the walk will wait on it, so the deadline follows the walk instead of the
/// operator's guess at its depth.
///
/// A stage that is waited on announces twice, because each message sets a
/// fresh deadline from its receipt: once before its spawns, once before its
/// wait. The restore ends with one more for the setup before `READY=1`.
///
/// Different `listen_timeout`s, so a bound that was a constant, or the first
/// stage's repeated, would not pass.
#[tokio::test]
async fn each_restored_stage_extends_the_start_deadline_before_readiness() {
    // Real time: binds a real socket, so this obeys SIGNAL_TEST_LOCK's
    // rule like every other successful `boot()` in this module.
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let mut db = AppConfig::minimal("db", "./srv");
    db.listen_timeout = UpDuration::from_millis(30_000);
    let mut api = AppConfig::minimal("api", "./srv");
    api.listen_timeout = UpDuration::from_millis(7_000);
    api.depends_on = vec!["db".to_string()];
    seed_roll(&paths, vec![db, api]);

    let notify_path = dir.path().join("n.sock");
    let listener = std::os::unix::net::UnixDatagram::bind(&notify_path).unwrap();
    // Bounded, for the reason the case above gives.
    listener
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let daemon = boot_with_idle_sweep(
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]),
        paths,
        BootOptions {
            restore: true,
            notify_socket: Some(notify_path.clone().into_os_string()),
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    // Read to `READY=1`, which is sent last: whatever is queued behind it
    // arrived after it.
    let mut messages = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        let read = listener.recv(&mut buf).unwrap();
        let message = String::from_utf8(buf[..read].to_vec()).unwrap();
        let ready = message == "READY=1\n";
        messages.push(message);
        if ready {
            break;
        }
    }
    assert_eq!(
        messages,
        vec![
            // `db`, which `api` waits on: 30s + the 5s stage slack, in
            // microseconds, before its spawns and again before its wait.
            "EXTEND_TIMEOUT_USEC=35000000\n",
            "EXTEND_TIMEOUT_USEC=35000000\n",
            // `api`, which nothing waits on: 7s + the 5s stage slack, once.
            "EXTEND_TIMEOUT_USEC=12000000\n",
            // The setup after the restore: the stage slack alone.
            "EXTEND_TIMEOUT_USEC=5000000\n",
            "READY=1\n",
        ]
    );

    daemon.context().shutdown();
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

    let daemon = boot_with_idle_sweep(
        ScriptedRunner::new(vec![]),
        paths,
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
