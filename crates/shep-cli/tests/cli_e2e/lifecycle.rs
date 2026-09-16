//! Autostart, daemon reuse, the lifecycle verbs, the committed JSON
//! fixtures, exit codes and `kill`.

use super::*;

#[cfg(unix)]
/// Also asserts the daemon is its own process-group leader, the
/// `Command::process_group(0)` contract `launch.rs` relies on and which
/// `std::process::Command` exposes no getter for.
#[test]
fn starting_with_no_daemon_running_autostarts_one_and_the_sheep_reaches_online() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let output = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&output);

    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["data"][0]["status"], "online", "{envelope}");

    let pid = read_daemon_pid(dir.path());
    assert_group_leader(pid);

    graceful_kill(dir.path());
}

// --- Case 2 ------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_second_command_reuses_the_daemon_rather_than_spawning_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let first = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("alpha")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&first);
    let first_pid = read_daemon_pid(home);

    let second = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("beta")
        .output()
        .unwrap();
    assert_success(&second);
    let second_pid = read_daemon_pid(home);

    assert_eq!(
        first_pid, second_pid,
        "the second command must reuse the first daemon, not spawn a new one"
    );

    let flock = shep(home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&flock);
    let envelope: serde_json::Value = serde_json::from_slice(&flock.stdout).unwrap();
    let names: Vec<&str> = envelope["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["alpha", "beta"],
        "both sheep must be registered against the one daemon: {envelope}"
    );

    graceful_kill(home);
}

/// Three sheep and a stop of one, so the narrow answer and the full one differ
/// by row count as well as by content. The name assertion is on the exact set:
/// a `contains("alpha")` would pass on a one-row table too.
#[test]
fn a_lifecycle_verb_prints_the_whole_flock_and_json_still_prints_what_it_touched() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    for name in ["alpha", "gamma", "beta"] {
        let started = shep(home)
            .arg("start")
            .arg(&script)
            .arg("--name")
            .arg(name)
            .output()
            .unwrap();
        guard.adopt_home(home);
        assert_success(&started);
    }

    let stopped = shep(home).arg("stop").arg("alpha").output().unwrap();
    assert_success(&stopped);
    let printed = String::from_utf8(stopped.stdout).unwrap();
    let named: Vec<&str> = printed
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|word| *word != "NAME")
        .collect();
    assert_eq!(
        named,
        ["alpha", "beta", "gamma"],
        "stopping one sheep prints the whole flock, in name order: {printed}"
    );

    let json = shep(home)
        .arg("--format")
        .arg("json")
        .arg("stop")
        .arg("beta")
        .output()
        .unwrap();
    assert_success(&json);
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let rows = envelope["data"].as_array().unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["beta"],
        "the machine surface still answers what it touched: {envelope}"
    );

    graceful_kill(home);
}

// --- Case 3 --------------------------------------------------------------

#[cfg(unix)]
/// `flock(2)` makes the race safe daemon-side and `connect_or_spawn`
/// client-side: the loser's child exits carrying `DAEMON_ALREADY_RUNNING` and
/// the client keeps probing rather than surfacing that as an error.
///
/// A `std::sync::Barrier` holds the two racers until both are ready, or
/// scheduling could let one finish before the other starts. Each is collected
/// over a channel, since `JoinHandle::join` has no bounded form and a stuck
/// racer joined directly would stop the suite.
#[test]
fn concurrent_cold_starts_produce_exactly_one_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();
    // Two racers means two Outputs and no single point that precedes every
    // panic path, so the earliest safe point is before either thread starts.
    guard.adopt_home(&home);

    let names = ["racer-a", "racer-b"];
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(names.len()));
    let (finished, racers) = std::sync::mpsc::channel();
    for name in names {
        let home = home.clone();
        let script = script.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let finished = finished.clone();
        std::thread::spawn(move || {
            barrier.wait(); // both racers launch together
            let output = shep(&home)
                .arg("start")
                .arg(&script)
                .arg("--name")
                .arg(name)
                .output()
                .unwrap();
            // A closed receiver means the case already gave up on this racer.
            let _ = finished.send((name, output));
        });
    }
    drop(finished); // the racers hold the only senders that matter

    let outputs: Vec<(&str, Output)> = (0..names.len())
        .map(|_| {
            racers
                .recv_timeout(RACER_DEADLINE)
                .expect("a racer never came back; see RACER_DEADLINE")
        })
        .collect();
    for (name, output) in &outputs {
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let pid = read_daemon_pid(&home);
    assert_group_leader(pid);

    let flock = shep(&home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&flock);
    let envelope: serde_json::Value = serde_json::from_slice(&flock.stdout).unwrap();
    let mut got: Vec<&str> = envelope["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    got.sort_unstable();
    assert_eq!(
        got,
        ["racer-a", "racer-b"],
        "both racers must have registered against the SAME daemon: {envelope}"
    );

    graceful_kill(&home);
}

// --- Case 4 ----------------------------------------------------------------

#[cfg(unix)]
/// Envelopes for `flock`, `describe`, `start` and `ping` are compared
/// structurally after normalizing what a real spawn cannot pin;
/// `bleats --no-follow` is one object with no envelope, compared byte for
/// byte.
///
/// One sheep named "fixture" at id 0, since a fresh home allocates ids from
/// zero. Its single stdout marker is what keeps `bleats` to the one line the
/// fixture pins.
#[test]
fn json_format_matches_the_committed_fixtures() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_logging_script(&dir, "fixture-line-1", None);
    let mut guard = DaemonGuard::default();

    let start_out = shep(home)
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("fixture")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&start_out);
    assert_envelope_matches_fixture(&start_out, home, "start", "fixture", Samples::None);

    let flock_out = shep(home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&flock_out);
    assert_envelope_matches_fixture(&flock_out, home, "flock", "fixture", Samples::Live);

    let describe_out = poll_describe_lambs(home, "fixture", FLOCK_DEADLINE);
    assert_envelope_matches_fixture(&describe_out, home, "describe", "fixture", Samples::Live);

    let ping_out = shep(home)
        .arg("--format")
        .arg("json")
        .arg("ping")
        .output()
        .unwrap();
    assert_success(&ping_out);
    let mut ping_envelope: serde_json::Value = serde_json::from_slice(&ping_out.stdout).unwrap();
    let ping_pid = ping_envelope["data"]["pid"]
        .as_i64()
        .unwrap_or_else(|| panic!("ping must report a real pid: {ping_envelope}"));
    assert!(ping_pid > 0);
    assert_eq!(
        nix::unistd::Pid::from_raw(i32::try_from(ping_pid).unwrap()),
        read_daemon_pid(home),
        "ping's pid must be the daemon's own pid"
    );
    ping_envelope["data"]["pid"] = serde_json::Value::Null;
    // `home` and `socket` are a tempdir here, so assert they are right and
    // then null them: a fixture cannot hold a path that changes every run.
    assert_eq!(
        ping_envelope["data"]["home"].as_str().unwrap(),
        home.to_str().unwrap(),
        "ping must name the home it probed"
    );
    assert!(
        ping_envelope["data"]["socket"]
            .as_str()
            .unwrap()
            .starts_with(home.to_str().unwrap()),
        "ping's socket must sit under that home"
    );
    ping_envelope["data"]["home"] = serde_json::Value::Null;
    ping_envelope["data"]["socket"] = serde_json::Value::Null;
    // Asserted and then nulled: a frozen version would turn every release bump
    // into a red test.
    assert_eq!(
        ping_envelope["data"]["daemon_version"].as_str().unwrap(),
        env!("CARGO_PKG_VERSION"),
        "ping must report this build's own version"
    );
    ping_envelope["data"]["daemon_version"] = serde_json::Value::Null;
    assert_eq!(
        ping_envelope,
        load_fixture("ping"),
        "ping envelope drifted from its committed fixture"
    );

    let bleats_out = bleats_no_follow_until_written(home, &["all", "--format", "json"]);
    assert_eq!(
        bleats_out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&bleats_out.stderr)
    );
    let expected = std::fs::read(fixture_path("bleats_no_follow")).unwrap();
    assert_eq!(
        bleats_out.stdout,
        expected,
        "bleats --no-follow --format json must match its fixture byte-for-byte: got {}",
        String::from_utf8_lossy(&bleats_out.stdout)
    );

    graceful_kill(home);
}

// --- Case 5 ------------------------------------------------------------

/// A selector matching nothing exits `NotFound`; the malformed `/[/` exits
/// `Usage` (`/unclosed` would parse as a literal name and exit `NotFound`); a
/// daemonless `--home` exits `DaemonUnreachable` and an absent one exits
/// `Usage`. For each, stdout stays empty and stderr parses as JSON.
///
/// The first two need a live daemon: every non-`Start` verb connects before
/// parsing its selector, so a cold `$SHEP_HOME` would exit
/// `DaemonUnreachable` first and hide both.
#[test]
fn exit_codes_and_stream_discipline() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("only")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let not_found = shep(home)
        .arg("--format")
        .arg("json")
        .arg("describe")
        .arg("ghost")
        .output()
        .unwrap();
    assert_json_error(&not_found, 3, "not_found");

    let usage = shep(home)
        .arg("--format")
        .arg("json")
        .arg("describe")
        .arg("/[/")
        .output()
        .unwrap();
    assert_json_error(&usage, 2, "usage");

    // A home that exists but has never had a daemon: `flock` never autostarts,
    // so nothing is listening for the whole invocation. Created, because an
    // absent `--home` is its own refusal and never reaches the connect.
    let cold = tempfile::tempdir().unwrap();
    let quiet_home = cold.path().join("no-daemon-here");
    std::fs::create_dir_all(&quiet_home).unwrap();
    let unreachable = shep(&quiet_home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_json_error(&unreachable, 5, "daemon_unreachable");

    // An absent `--home` is a usage error, not an unreachable daemon: there is
    // no flock at that path, and creating one would leave a second empty home.
    let missing_home = cold.path().join("gone");
    let absent = shep(&missing_home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_json_error(&absent, 2, "usage");
    assert!(
        !missing_home.exists(),
        "a refused --home must be left on disk exactly as it was found"
    );

    // Neither of those homes ever had a daemon, so there is nothing to kill.

    graceful_kill(home);
}

// --- Case 6 --------------------------------------------------------------

#[test]
fn kill_stops_the_daemon_and_removes_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&script).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    // On unix the control address is a socket file the daemon unlinks. On
    // Windows it is a named pipe with no directory entry, so `Path::exists`
    // would read `false` before the kill and pass vacuously.
    #[cfg(unix)]
    let socket = home.join("run").join("shep.sock");
    #[cfg(unix)]
    assert!(socket.exists(), "precondition: the daemon is up");
    #[cfg(windows)]
    assert_success(&shep(home).arg("flock").output().unwrap());

    let kill = shep(home).arg("kill").output().unwrap();
    assert_success(&kill);

    #[cfg(unix)]
    assert!(!socket.exists(), "kill must remove the socket file");
    #[cfg(windows)]
    {
        // `shep flock` against a departed shepherd exits `DaemonUnreachable`,
        // the same fact the missing socket file states on unix.
        let after = shep(home).arg("flock").output().unwrap();
        assert!(
            !after.status.success(),
            "kill must leave nothing answering on the control pipe; stderr={}",
            String::from_utf8_lossy(&after.stderr)
        );
    }
}
