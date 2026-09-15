//! `shep dev`, which tidies up after itself whether the flock empties or a
//! signal arrives first.

use super::*;

/// A `shep dev` invocation with `$SHEP_DEV_HOME` set to `dev_home`, timeout
/// already attached. Never `--home`: `dev` ignores it.
fn shep_dev(dev_home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("shep").unwrap();
    cmd.env("SHEP_DEV_HOME", dev_home)
        .arg("dev")
        .timeout(CMD_TIMEOUT);
    cmd
}

/// Spawns `shep dev <flockfile>` with `$SHEP_DEV_HOME` set to `dev_home`,
/// stdout and stderr piped and drained in the background. Leaves the process
/// alive, so a caller can signal it.
fn spawn_shep_dev(dev_home: &Path, flockfile: &Path) -> Child {
    let mut child = std::process::Command::cargo_bin("shep")
        .expect("locate the built shep binary")
        .env("SHEP_DEV_HOME", dev_home)
        .arg("dev")
        .arg(flockfile)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shep dev");
    discard_in_background(child.stdout.take().unwrap());
    discard_in_background(child.stderr.take().unwrap());
    child
}

/// Polls `shep --home <dev_home> --format json flock` until the one app's row
/// reports `online`. Tolerates the early window before `shep dev` has bound
/// its socket, unlike [`poll_flock_data`].
fn wait_for_dev_online(dev_home: &Path, deadline: Duration) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let output = shep(dev_home)
            .arg("--format")
            .arg("json")
            .arg("flock")
            .output()
            .unwrap();
        if output.status.success()
            && let Ok(envelope) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            && envelope["data"][0]["status"] == "online"
        {
            return envelope["data"][0].clone();
        }
        if start.elapsed() >= deadline {
            panic!(
                "shep dev's flock never reached online within {deadline:?}; last stdout={}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Polls `child.try_wait()` until it exits, or `timeout` elapses with a named
/// panic. `CMD_TIMEOUT`'s kill lives inside `.output()`, which
/// [`spawn_shep_dev`] never calls.
fn wait_bounded(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll shep dev") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("shep dev did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The auto-exit fires after `commands::empty::STRIKES` × `INTERVAL` (3 × 2s).
/// The setting under test is `tidy_up: true`, which reddens the no-shepherd
/// assertion when flipped, not the socket one. `$SHEP_DEV_HOME` points at its
/// own tempdir, never the real `~/.shep-dev`.
#[test]
fn dev_tidies_up_after_itself() {
    let dir = tempfile::tempdir().unwrap();
    let dev_home = tempfile::tempdir().unwrap();
    let script = write_script(&dir, "batch.sh", "#!/bin/sh\nexit 0\n");
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"batch\"\nscript = '{}'\nautorestart = false\n",
            script.display(),
        ),
    );

    let output = shep_dev(dev_home.path()).arg(&flockfile).output().unwrap();
    assert_success(&output);

    let socket = dev_home.path().join("run").join("shep.sock");
    assert!(!socket.exists(), "dev must not leave a live socket behind");

    let flock_output = shep(dev_home.path()).arg("flock").output().unwrap();
    assert!(
        !flock_output.status.success(),
        "no shepherd should remain at the dev home to answer `flock`: {flock_output:?}"
    );
}

#[cfg(unix)]
/// A signal reaches `commands::foreground::run`'s `RunningDaemon::run`
/// teardown directly, never the `Stop`/`Delete` pair `tidy_up` sends over the
/// wire, so `BootOptions::delete_flock_on_shutdown` is what keeps `flock.json`
/// from still listing the sheep as running.
#[test]
fn dev_tidies_up_when_it_is_signalled_rather_than_when_the_flock_empties() {
    let dir = tempfile::tempdir().unwrap();
    let dev_home = tempfile::tempdir().unwrap();
    let script = write_script(&dir, "held.sh", "#!/bin/sh\nsleep 60\n");
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"held\"\nscript = '{}'\n",
            script.display()
        ),
    );

    let mut child = spawn_shep_dev(dev_home.path(), &flockfile);
    let dev_pid = child.id() as i32;

    let online = wait_for_dev_online(dev_home.path(), FLOCK_DEADLINE);
    let sheep_pid = online["pid"]
        .as_i64()
        .unwrap_or_else(|| panic!("a real pid: {online}")) as i32;

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(dev_pid),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("send SIGTERM to shep dev");

    let status = wait_bounded(&mut child, FLOCK_DEADLINE);
    assert!(
        status.success(),
        "a signalled dev session must still tidy up and exit cleanly: {status:?}"
    );

    let socket = dev_home.path().join("run").join("shep.sock");
    assert!(!socket.exists(), "dev must not leave a live socket behind");

    let flock_output = shep(dev_home.path()).arg("flock").output().unwrap();
    assert!(
        !flock_output.status.success(),
        "no shepherd should remain at the dev home to answer `flock`: {flock_output:?}"
    );

    assert!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(sheep_pid), None).is_err(),
        "the held sheep (pid {sheep_pid}) must not outlive the dev session"
    );

    let roll_text = std::fs::read_to_string(dev_home.path().join("flock.json"))
        .expect("teardown must still write a final flock.json, even an empty one");
    let roll: serde_json::Value =
        serde_json::from_str(&roll_text).expect("flock.json must still be valid JSON");
    assert_eq!(
        roll["apps"].as_array().map(Vec::len),
        Some(0),
        "a signalled dev session must not leave `held` in the roll for `shep muster` to \
         resurrect: {roll}"
    );
}

/// The assertion is the usage line, not the verb's name: the root `shep
/// --help` lists `dev` and `runtime` among its subcommands, so
/// `text.contains("dev")` passes even with `alias_argv` deleted.
#[test]
fn the_alias_binaries_exist_and_reach_their_own_verbs() {
    for (bin, verb) in [("shep-dev", "dev"), ("shep-runtime", "runtime")] {
        let output = Command::cargo_bin(bin)
            .unwrap_or_else(|err| panic!("{bin} must be a [[bin]] target: {err}"))
            .arg("--help")
            .timeout(CMD_TIMEOUT)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(&format!("Usage: shep {verb}")),
            "{bin} --help must be {verb}'s own help, not the root's:\n{text}"
        );
        assert!(
            !text.contains("lookout"),
            "{bin} printed the root verb list, so the alias supplied no verb:\n{text}"
        );
    }
}
