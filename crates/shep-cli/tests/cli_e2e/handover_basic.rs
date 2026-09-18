//! A daemon reload that keeps the flock: pids and logs survive it, and a
//! config it cannot read refuses the reload rather than dropping anyone.

use super::*;

/// Writes a script that counts from 1 upwards on stdout, one number per line,
/// forever.
///
/// The sequence is what makes a log gap visible: a counting sheep proves
/// nothing between it and the file was lost, reordered or cut in half. A
/// restarted sheep starts again at 1.
#[cfg(unix)]
pub(crate) fn write_counting_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "counter.sh",
        &format!(
            "{}{}i=1\nwhile :; do\n  echo \"$i\"\n  i=$((i+1))\n  sleep 0.2\ndone\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// Reads `path` until it holds at least `want` lines, or
/// [`HANDOVER_DEADLINE`] expires, and returns what it held on the last read.
/// Returns rather than panicking on expiry, so the failure is the caller's own
/// assertion.
#[cfg(unix)]
pub(crate) fn counting_lines(path: &Path, want: usize) -> Vec<String> {
    let start = Instant::now();
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let lines: Vec<String> = text
            .lines()
            .map(shep_core::logstamp::strip)
            .map(str::to_owned)
            .collect();
        if lines.len() >= want || start.elapsed() >= HANDOVER_DEADLINE {
            return lines;
        }
        std::thread::sleep(HANDOVER_POLL_INTERVAL);
    }
}

/// Fails unless `lines` is `1, 2, 3, …`, one number per line, with nothing
/// missing, nothing repeated and nothing cut in half. The log pump reads
/// through a `BufReader`, so bytes consumed without yet forming a line die with
/// the process image and the successor's reader starts mid-line. The counter
/// emits five lines a second rather than as fast as it can: the handover's
/// flush empties the log file's write buffer, not the reader's.
#[cfg(unix)]
pub(crate) fn assert_unbroken_sequence(lines: &[String], what: &str) {
    for (index, line) in lines.iter().enumerate() {
        let want = index + 1;
        let got: usize = line.trim().parse().unwrap_or_else(|_| {
            panic!("{what}: line {want} is not a whole number, so it was torn: {line:?}")
        });
        assert_eq!(
            got,
            want,
            "{what}: expected {want} on line {want}, got {got}; the sequence so far is {:?}",
            &lines[..=index]
        );
    }
}

/// A sheep keeps its pid across `shep daemon reload`, and its log gains no
/// gap. Neither implies the other: a handover that respawned the sheep keeps
/// the log growing while moving the pid, and one that carried the pid while
/// dropping the pipe leaves the sheep blocked on `write()`.
///
/// Unix only, as the whole handover is: Windows has no `execve`.
#[cfg(unix)]
#[test]
fn a_sheep_keeps_its_pid_and_its_log_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_counting_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("counter")
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    let before = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    let pid_before = before["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("an online sheep reports a pid: {before}"));
    let out_file = PathBuf::from(
        before["out_file"]
            .as_str()
            .unwrap_or_else(|| panic!("an online sheep reports its out file: {before}")),
    );
    let seen_before = counting_lines(&out_file, 3);
    assert!(
        seen_before.len() >= 3,
        "the counter must be logging before the reload: {seen_before:?}"
    );

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_success(&reloaded);

    let after = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    assert_eq!(
        after["pid"].as_u64(),
        Some(pid_before),
        "a moved pid means the sheep was respawned, which is the stop arm: {after}"
    );

    let seen = counting_lines(&out_file, seen_before.len() + 3);
    assert!(
        seen.len() > seen_before.len(),
        "the sheep stopped logging across the handover: {seen:?}"
    );
    assert_unbroken_sequence(&seen, "the counter's log across a handover");

    graceful_kill(dir.path());
}

/// Fails if a bad `shep.toml` can orphan a running flock: the refusal must
/// happen before anything is signalled, on both the handover arm and the
/// stop-and-start arm. No `#[cfg(unix)]`, since the pre-flight in
/// `reload_with_wait` runs before the arm is chosen.
#[test]
fn a_bad_shep_toml_refuses_the_reload_and_leaves_the_flock_supervised() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("sheep")
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    let before = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    let pid_before = before["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("an online sheep reports a pid: {before}"));

    write_shep_toml(&dir, "[daemon]\nmax_cron_sleep = \"soon\"\n");

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_eq!(
        reloaded.status.code(),
        Some(4),
        "InvalidConfig; stderr={}",
        String::from_utf8_lossy(&reloaded.stderr)
    );

    let after = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&after);
    let envelope: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    let sheep = &envelope["data"][0];
    assert_eq!(sheep["status"], "online", "still supervised: {sheep}");
    assert_eq!(
        sheep["pid"].as_u64(),
        Some(pid_before),
        "the refusal must happen before anything is signalled: {sheep}"
    );

    graceful_kill(dir.path());
}

/// Fails if a dog section shep cannot move can orphan a running flock.
///
/// The dog-config migration runs at the top of every boot, so on the handover
/// arm it runs in a successor whose predecessor is already gone, and a refusal
/// there leaves the flock running with nothing supervising it. No
/// `#[cfg(unix)]`: the pre-flight runs before the arm is chosen.
#[test]
fn a_refused_dog_migration_refuses_the_reload_and_leaves_the_flock_supervised() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("sheep")
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    let before = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    let pid_before = before["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("an online sheep reports a pid: {before}"));

    write_shep_toml(&dir, "[dog.metrics]\nbind = \"127.0.0.1:19616\"\n");
    std::fs::write(
        dir.path().join("dogs.toml"),
        "[metrics]\nbind = \"127.0.0.1:19617\"\n",
    )
    .unwrap();

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_eq!(
        reloaded.status.code(),
        Some(4),
        "InvalidConfig; stderr={}",
        String::from_utf8_lossy(&reloaded.stderr)
    );

    let after = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&after);
    let envelope: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    let sheep = &envelope["data"][0];
    assert_eq!(sheep["status"], "online", "still supervised: {sheep}");
    assert_eq!(
        sheep["pid"].as_u64(),
        Some(pid_before),
        "the refusal must happen before anything is signalled: {sheep}"
    );

    graceful_kill(dir.path());
}

/// An env var set on the `shep daemon reload` invocation must not rescue a
/// file that is invalid on its own: a handover successor execs with the old
/// daemon's argv and environment. The variable is set on the child through
/// `Command::env`; mutating this process's own environment is `unsafe` in
/// edition 2024 and the crate forbids unsafe code.
#[test]
fn a_bad_shep_toml_an_env_var_would_rescue_is_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("sheep")
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    let before = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    let pid_before = before["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("an online sheep reports a pid: {before}"));

    // Below MIN_CRON_SLEEP (1s): valid TOML, refused only at DaemonConfig's
    // own validation pass.
    write_shep_toml(&dir, "[daemon]\nmax_cron_sleep = \"500ms\"\n");

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .env("SHEP_MAX_CRON_SLEEP", "5s")
        .output()
        .unwrap();
    assert_eq!(
        reloaded.status.code(),
        Some(4),
        "InvalidConfig, the env var on this invocation must not rescue a file the          daemon being replaced never saw it against; stderr={}",
        String::from_utf8_lossy(&reloaded.stderr)
    );

    let after = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&after);
    let envelope: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    let sheep = &envelope["data"][0];
    assert_eq!(sheep["status"], "online", "still supervised: {sheep}");
    assert_eq!(
        sheep["pid"].as_u64(),
        Some(pid_before),
        "the refusal must happen before anything is signalled: {sheep}"
    );

    graceful_kill(dir.path());
}
