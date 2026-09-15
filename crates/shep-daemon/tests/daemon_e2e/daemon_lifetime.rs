//! The daemon's own lifetime: what its death does to the flock and the
//! socket, what a crash leaves behind, what a successor restores, and what
//! a child inherits from it.

use super::*;

#[cfg(unix)]
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn kill_daemon_shuts_the_flock_down_and_unlinks_the_socket() {
    let mut fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let sleeper = |name: &str| {
        let mut app = AppConfig::minimal(name, "/bin/sh");
        app.interpreter = Some("none".to_string());
        app.args = vec!["-c".to_string(), "while :; do sleep 1; done".to_string()];
        app
    };
    let started = client
        .request(Request::Start {
            apps: vec![sleeper("one"), sleeper("two")],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let pids: Vec<i32> = infos
        .iter()
        .map(|i| i32::try_from(i.pid.expect("a real spawn reports a real pid")).unwrap())
        .collect();
    assert_eq!(pids.len(), 2);

    let killed = client.request(Request::KillDaemon).await;
    assert_eq!(killed.result.unwrap(), Response::ShuttingDown);

    let socket = fixture.paths.socket.clone();
    let pidfile_path = shep_daemon::boot::pidfile(&fixture.paths);
    let run = fixture.run.take().expect("run is only ever taken once");
    tokio::time::timeout(RECV_TIMEOUT, run)
        .await
        .expect("teardown must not hang")
        .unwrap()
        .unwrap();

    assert!(
        !socket.exists(),
        "the control socket must be unlinked on teardown"
    );
    assert!(
        !pidfile_path.exists(),
        "the pidfile must be removed on teardown"
    );

    // Reaped, not merely signalled: `kill(pid, None)` still returns `Ok` for a
    // zombie, so only ESRCH proves the daemon's own `wait()` ran. The `sleep 1`
    // grandchildren are out of reach here; `real_runner.rs` covers them.
    for pid in pids {
        assert_reaped(pid).await;
    }

    // A fresh connect on the unlinked path must fail, not hang.
    assert!(
        transport::connect(&socket).await.is_err(),
        "the daemon must not still be answering after KillDaemon"
    );
}

/// Waits until nothing answers a connection at `socket`, failing at
/// [`RECV_TIMEOUT`].
///
/// Dropping a `UnixListener` does not unbind the socket: it lives as long as
/// its last descriptor, and a child parked between `fork` and `exec` holds a
/// copy until close-on-exec clears it. `bind_socket` reads such a socket as a
/// live daemon and refuses the boot with `AlreadyRunning`.
async fn await_stale_socket(socket: &std::path::Path) {
    let refused = tokio::time::timeout(RECV_TIMEOUT, async {
        // tokio's connector, not `std`'s: a full backlog parks the caller on
        // some Unixes, inside a syscall no timer can interrupt.
        while !matches!(
            transport::connect(socket).await,
            Err(err) if matches!(err.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound)
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        refused.is_ok(),
        "{}: a crashed daemon's socket is still answering connections",
        socket.display()
    );
}

// `cfg(unix)`: a leftover socket file, which a named pipe never leaves.
#[cfg(unix)]
#[tokio::test]
async fn a_socket_left_behind_by_a_crash_does_not_block_the_next_boot() {
    let mut fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let socket = fixture.paths.socket.clone();

    // Simulate a crash: aborting the run loop unlinks neither the socket file
    // nor the pidfile. Awaiting the handle resolves once the task, and the
    // `UnixListener` it owned, has finished dropping.
    let run = fixture.run.take().expect("run is only ever taken once");
    run.abort();
    let outcome = run.await;
    assert!(
        outcome.is_err_and(|err| err.is_cancelled()),
        "the run task must have been cancelled, not completed on its own"
    );
    assert!(
        socket.exists(),
        "sanity: a crash leaves the socket file behind"
    );
    // Dropping that listener is not the socket going dead; the reboot needs
    // the second.
    await_stale_socket(&socket).await;

    // Same `$SHEP_HOME`: taking `dir` out of `fixture` keeps the leftover
    // socket file alive into the reboot.
    let dir = fixture.dir.take().expect("dir is only ever taken once");
    let rebooted = Fixture::boot(dir, false).await;
    let mut client = rebooted.connect().await;
    let pong = client.request(Request::Ping).await;
    assert_eq!(pong.result.unwrap(), Response::Pong);

    rebooted.shutdown().await;
}

#[cfg(unix)]
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn muster_restores_the_flock_across_a_daemon_lifetime() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let alpha = forever_app("alpha");
    let mut beta = forever_app("beta");
    beta.instances = 2;
    let started = client
        .request(Request::Start {
            apps: vec![alpha, beta],
        })
        .await;
    let Response::Started(before) = started.result.unwrap() else {
        panic!("expected started")
    };
    assert_eq!(before.len(), 3, "alpha (1 instance) + beta (2 instances)");
    let old_pids: std::collections::HashSet<u32> = before.iter().map(|i| i.pid.unwrap()).collect();

    // Explicit write, no polling: the roll write is a call, not a race.
    fixture.ctx.snapshot_now().await.unwrap();
    let roll = shep_daemon::snapshot::read(&fixture.paths.snapshot).unwrap();
    let running_by_name: std::collections::HashMap<_, _> = roll
        .apps
        .iter()
        .map(|a| (a.app.name.clone(), a.instances_running))
        .collect();
    assert_eq!(running_by_name.get("alpha"), Some(&1));
    assert_eq!(running_by_name.get("beta"), Some(&2));

    let dir = fixture.shutdown().await; // same $SHEP_HOME survives the reboot

    // Reaped, not merely recorded in the roll: a stale pid the OS had not
    // reused would pass the fresh-pid assertion for the wrong reason.
    for &pid in &old_pids {
        assert_reaped(i32::try_from(pid).unwrap()).await;
    }

    let rebooted = Fixture::boot(dir, true).await;
    let listed = rebooted.connect().await.request(Request::ListFlock).await;
    let Response::Flock(after) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(
        after.len(),
        3,
        "both apps' full instance counts must come back"
    );
    for info in &after {
        assert_eq!(info.status, ProcStatus::Online);
        let pid = info.pid.expect("a restored sheep is a real live process");
        assert!(
            !old_pids.contains(&pid),
            "a restored sheep gets a fresh pid, id {}",
            info.id
        );
    }
    rebooted.shutdown().await;
}

/// Prepends `dir` to `PATH` for one test, restoring the original on drop.
///
/// Prepending, never replacing: a concurrently spawned sleeper's `/bin/sh`
/// has to keep finding `sleep` while this guard is active.
struct PathGuard {
    pub(crate) original: Option<String>,
}

impl PathGuard {
    pub(crate) fn prepend(dir: &std::path::Path) -> Self {
        let original = std::env::var("PATH").ok();
        let combined = match &original {
            Some(existing) => format!("{}:{existing}", dir.display()),
            None => dir.display().to_string(),
        };
        // SAFETY: `set_var`'s hazard is a concurrent raw `getenv`. Every read
        // of `PATH` in this binary goes through `std::env::var`, which std
        // serializes against `set_var`/`remove_var`.
        unsafe { std::env::set_var("PATH", combined) };
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.original {
            // SAFETY: every `PATH` read in this binary goes through
            // `std::env::var`, which std serializes against `set_var`.
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            // SAFETY: every `PATH` read in this binary goes through
            // `std::env::var`, which std serializes against `remove_var`.
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

#[cfg(unix)]
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn a_bare_interpreter_resolves_via_the_inherited_path() {
    // A throwaway-tempdir shim, not a bare `"sh"`: `execvp` falls back to
    // `_PATH_DEFPATH` when PATH is absent from the child's env, so a bare name
    // would resolve even with `base_env()`'s seeding reverted.
    use std::os::unix::fs::PermissionsExt as _;

    let shim_home = tempfile::tempdir().unwrap();
    let shim_dir = shim_home.path().join("bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim_path = shim_dir.join("shep-test-interp");
    std::fs::write(&shim_path, "#!/bin/sh\necho shep-bare-interpreter-ok\n").unwrap();
    let mut perms = std::fs::metadata(&shim_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim_path, perms).unwrap();

    // The only test in this binary that mutates PATH.
    let _path_guard = PathGuard::prepend(&shim_dir);

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;
    // Subscribe before starting: a connection gets no events until it does.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    // Bare: only found via the seeded PATH now that it includes shim_dir.
    let mut app = AppConfig::minimal("bare", "unused");
    app.interpreter = Some("shep-test-interp".to_string());
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;

    // A failed exec lands the sheep in Errored, so Online is the assertion.
    let online = client
        .await_process_event(id, ProcessEventKind::Online)
        .await;
    assert_eq!(online.status, ProcStatus::Online);

    fixture.shutdown().await;
}
