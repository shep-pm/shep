//! `shep runtime`: the exit code when the flock empties, and the dog
//! config it migrates on the way up.

use super::*;

/// Two runs in one test so the `Command::cargo_bin` lookup is paid once: an
/// app that exits 0 with `autorestart = false` (exit 0), and one that exits 1
/// with `max_restarts = 1` (exit 11), which errors on the first unstable exit
/// so neither run waits through a restart delay. Each takes at least 6 seconds
/// (`commands::empty::STRIKES` × `INTERVAL`).
#[test]
fn runtime_exits_when_the_flock_empties_with_a_code_that_says_why() {
    // Clean emptying: one app exits 0 and is told not to restart.
    let clean_dir = tempfile::tempdir().unwrap();
    let clean_script = write_script(&clean_dir, "clean.sh", "#!/bin/sh\nexit 0\n");
    let clean_flockfile = write_flockfile(
        &clean_dir,
        &format!(
            "[[app]]\nname = \"batch\"\nscript = '{}'\nautorestart = false\n",
            clean_script.display(),
        ),
    );
    let clean = shep(clean_dir.path())
        .arg("runtime")
        .arg(&clean_flockfile)
        .output()
        .unwrap();
    assert_eq!(
        clean.status.code(),
        Some(0),
        "a clean emptying is not a failure; stderr={}",
        String::from_utf8_lossy(&clean.stderr)
    );

    // Fail-fast emptying: one app exits 1 with no restart budget at all.
    let failed_dir = tempfile::tempdir().unwrap();
    let failed_script = write_script(&failed_dir, "fail.sh", "#!/bin/sh\nexit 1\n");
    let failed_flockfile = write_flockfile(
        &failed_dir,
        &format!(
            "[[app]]\nname = \"batch\"\nscript = '{}'\nmax_restarts = 1\n",
            failed_script.display(),
        ),
    );
    let failed = shep(failed_dir.path())
        .arg("runtime")
        .arg(&failed_flockfile)
        .output()
        .unwrap();
    assert_eq!(
        failed.status.code(),
        Some(11),
        "an errored sheep must fail the container; stderr={}",
        String::from_utf8_lossy(&failed.stderr)
    );
}

#[cfg(unix)]
/// Fails if `shep runtime` serves a dog the compiled default instead of the
/// bind an operator wrote. `runtime` reaches `boot_supervisor` directly, never
/// `run_daemon`, so the dog-config migration has to run on both paths; a
/// container that only runs `shep runtime` otherwise brings its dogs up on
/// compiled defaults with no warning and no file written.
///
/// `#[cfg(unix)]`: [`wait_for_dog_pid`] uses `nix::unistd::Pid`.
#[test]
fn runtime_migrates_dog_config_and_serves_the_bind_an_operator_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(
        &dir,
        &format!(
            "[daemon]\nenabled_dogs = [\"metrics\"]\n\n[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"
        ),
    );
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!("[[app]]\nname = \"web\"\nscript = '{}'\n", script.display()),
    );
    let mut guard = DaemonGuard::default();
    // Before the spawn, not after: `runtime` boots its shepherd in its own
    // process, so there is a supervisor to reap from the moment it starts.
    guard.adopt_home(home);

    let mut child = std::process::Command::cargo_bin("shep")
        .expect("locate the built shep binary")
        .arg("--home")
        .arg(home)
        .arg("runtime")
        .arg(&flockfile)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shep runtime");
    // `runtime` streams the flock's bleats to its own stdout for as long as
    // it runs; nothing draining those pipes wedges the child once they fill.
    discard_in_background(child.stdout.take().expect("piped stdout"));
    discard_in_background(child.stderr.take().expect("piped stderr"));

    // `wait_for_dog_pid` asserts success on its first `shep flock`, so it
    // cannot be the first thing aimed at a shepherd still booting.
    let start = Instant::now();
    while !shep(home).arg("flock").output().unwrap().status.success() {
        assert!(
            start.elapsed() < FLOCK_DEADLINE,
            "`shep runtime` never brought a shepherd up at {}",
            home.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let dog_pid = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(dog_pid);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let body = poll_metrics(addr);
    assert!(
        body.contains("HTTP/1.1 200"),
        "the metrics dog must answer at the bind shep.toml asked for, not at \
         the compiled default: {body}"
    );
    assert!(
        home.join("dogs.toml").is_file(),
        "`shep runtime` must migrate `[dog.metrics]` out of shep.toml"
    );

    graceful_kill(home);
    let _ = child.wait();
}
