//! Where `$SHEP_HOME` resolves from, what a write under a watched tree
//! does, and the config errors caught before a sheep starts.

use super::*;

#[cfg(unix)]
/// Asserted on the socket file's location, not on exit 0, so a child that
/// re-resolved `$SHEP_HOME` from the ambient environment and bound elsewhere
/// still fails.
///
/// Needs `env_remove` and a hand-built argv, so it cannot use the [`shep`]
/// helper but borrows [`CMD_TIMEOUT`]. That timeout reaps no daemon: the
/// launched child has its own process group, so a kill reaches the CLI only.
#[test]
fn home_reaches_the_spawned_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let output = Command::cargo_bin("shep")
        .unwrap()
        .args([
            "--home",
            dir.path().to_str().unwrap(),
            "start",
            script.to_str().unwrap(),
        ])
        .env_remove("SHEP_HOME") // the ambient value must not be what makes this pass
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();

    // Registered before anything that can panic: a failed autostart is when a
    // daemon is most likely to be left behind.
    guard.adopt_home(dir.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let socket = dir.path().join("run").join("shep.sock");
    assert!(
        socket.exists(),
        "the daemon bound somewhere other than --home"
    );

    graceful_kill(dir.path());
}

/// The default home resolves from whichever variable the platform actually
/// sets: `$HOME` on unix, `%USERPROFILE%` on Windows, which sets no `HOME` at
/// all. Both are cleared first, so an ambient value cannot be what passes it.
///
/// Spawned rather than unit-tested: the lookup reads this process's real
/// environment, and a test cannot unset a variable for one thread of it.
/// Asserted on the directory `ensure_home` creates, not on exit 0 alone, so a
/// shepherd that came up under some other root still fails.
#[test]
fn the_default_home_resolves_from_the_platform_s_own_variable() {
    let dir = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();

    let mut cmd = Command::cargo_bin("shep").unwrap();
    cmd.arg("start")
        .env_remove("SHEP_HOME")
        .env_remove("HOME")
        .env_remove("USERPROFILE");
    #[cfg(unix)]
    cmd.env("HOME", dir.path());
    #[cfg(windows)]
    cmd.env("USERPROFILE", dir.path());
    let output = cmd.timeout(CMD_TIMEOUT).output().unwrap();

    let home = dir.path().join(".shep");
    guard.adopt_home(&home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        home.is_dir(),
        "the shepherd came up under a root that is not {}",
        home.display()
    );

    graceful_kill(&home);
}

// --- Case 9 --------------------------------------------------------------

#[cfg(unix)]
/// The watched tree is its own [`TempDir`], never this case's `$SHEP_HOME`: a
/// watch rooted there would see [`FIXTURE_PIDS`] grow on each spawn and
/// restart on its own sheep.
#[test]
fn a_write_under_a_watched_tree_restarts_the_sheep() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let watched = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"watcher\"\nscript = '{}'\ncwd = '{}'\nwatch = true\n",
            script.display(),
            watched.path().display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(before["restarts"], 0, "precondition: {before}");

    std::fs::write(watched.path().join("app.txt"), "changed").unwrap();

    let after = poll_flock(home, |info| info["restarts"] == 1);
    assert_eq!(
        after["restarts"], 1,
        "a write under the watched tree must restart the sheep exactly once: {after}"
    );

    graceful_kill(home);
}

// --- Case 10 -------------------------------------------------------------

#[cfg(unix)]
/// A dot-file followed by a full [`FLOCK_DEADLINE`] of quiet is also what a
/// watcher that was never armed produces, so a plain file is written
/// afterwards and its restart must land. Two writes, exactly one restart.
#[test]
fn a_write_to_a_dot_file_under_a_watched_tree_restarts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let watched = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"watcher\"\nscript = '{}'\ncwd = '{}'\nwatch = true\n",
            script.display(),
            watched.path().display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(before["restarts"], 0, "precondition: {before}");

    std::fs::write(watched.path().join(".hidden.swp"), "editor churn").unwrap();
    // Polls for the restart that must not come, for the same deadline the
    // positive case gives the one that must: `done` never accepts.
    let quiet = poll_flock(home, |_| false);
    assert_eq!(
        quiet["restarts"], 0,
        "a dot-file is ignored by default and must not restart anything: {quiet}"
    );

    std::fs::write(watched.path().join("app.txt"), "changed").unwrap();
    let after = poll_flock(home, |info| info["restarts"] == 1);
    assert_eq!(
        after["restarts"], 1,
        "the watcher must have been armed and delivering all along: {after}"
    );

    graceful_kill(home);
}

// --- Case 11 -------------------------------------------------------------

#[cfg(unix)]
/// The only tier that exercises the real fd-3 channel end to end; every other
/// test of this gate hands the supervisor a `ChildMessage` directly.
///
/// `listen_timeout` is raised far above its 3000ms default because the daemon
/// takes a `wait_ready` sheep `Online` on elapse anyway, which would make the
/// observation window and the timeout window the same window.
#[test]
fn a_wait_ready_sheep_goes_online_only_once_it_signals_ready() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let sentinel = dir.path().join("go");
    let script = write_ready_script(&dir, &sentinel);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"gated\"\nscript = '{}'\nwait_ready = true\nlisten_timeout = \"120s\"\n",
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let envelope: serde_json::Value = serde_json::from_slice(&boot.stdout).unwrap();
    assert_eq!(
        envelope["data"][0]["status"], "starting",
        "a wait_ready sheep must not be online before it signals: {envelope}"
    );

    std::fs::write(&sentinel, "").unwrap();

    let ready = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(
        ready["status"], "online",
        "the sheep must reach online once it writes ready to fd 3: {ready}"
    );

    graceful_kill(home);
}

// --- Case 12 -------------------------------------------------------------

#[cfg(unix)]
/// Exit `4`, JSON on stderr, and the offending pattern in the message. Spans
/// `normalize`'s rejection, the daemon's `InvalidConfig` over RPC, and the
/// CLI's exit code. Asserted on the pattern, not the wording, which is
/// croner's.
#[test]
fn a_bad_cron_pattern_is_a_config_error() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"crony\"\nscript = '{}'\ncron_restart = \"not a cron\"\n",
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home)
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(home);

    assert_json_error(&output, 4, "invalid_config");
    let err: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("not a cron"),
        "the rejection must name the offending pattern: {err}"
    );

    graceful_kill(home);
}

// --- Case 13 -------------------------------------------------------------

#[cfg(unix)]
/// Exit `4`, JSON on stderr, and the offending target in the message. The
/// daemon's prober carries no TLS stack, and a probe failing every poll would
/// look like a down app, so the target is refused at config time. This case
/// configures the readiness probe; `normalize`'s unit tier covers liveness.
#[test]
fn an_https_probe_target_is_a_config_error() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"probed\"\nscript = '{}'\n\
             readiness_probe = {{ kind = \"http\", target = \"https://localhost:8443/health\" }}\n",
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home)
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(home);

    assert_json_error(&output, 4, "invalid_config");
    let err: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("https://localhost:8443/health"),
        "the rejection must name the offending target: {err}"
    );

    graceful_kill(home);
}
