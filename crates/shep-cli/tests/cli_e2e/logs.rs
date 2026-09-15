//! Bleats, and what `reopen` and `flush` do to the files underneath them.

use super::*;

/// Both of a sheep's streams by default, only the requested one under `--out`.
#[test]
fn bleats_no_follow_prints_what_a_sheep_actually_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_logging_script(&dir, "bleater-out-marker", Some("bleater-err-marker"));
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("bleater")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let both = bleats_no_follow_until_contains(
        home,
        &["all"],
        &["bleater-out-marker", "bleater-err-marker"],
    );
    assert_eq!(
        both.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&both.stderr)
    );
    let stdout = String::from_utf8_lossy(&both.stdout);
    let stderr = String::from_utf8_lossy(&both.stderr);
    assert!(stdout.contains("bleater-out-marker"), "stdout={stdout}");
    assert!(stdout.contains("bleater-err-marker"), "stdout={stdout}");
    assert!(
        !stderr.contains("bleater-out-marker") && !stderr.contains("bleater-err-marker"),
        "a sheep's own lines must never reach shep's diagnostic stream: stderr={stderr}"
    );

    let out_only = bleats_no_follow_until_written(home, &["all", "--out"]);
    assert_eq!(
        out_only.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out_only.stderr)
    );
    let stdout_only = String::from_utf8_lossy(&out_only.stdout);
    assert!(
        stdout_only.contains("bleater-out-marker"),
        "stdout={stdout_only}"
    );
    assert!(
        !stdout_only.contains("bleater-err-marker"),
        "--out must select the out file only: stdout={stdout_only}"
    );

    graceful_kill(home);
}

/// `create`-mode rotation: rename the live log, run `shep reopen`, and the
/// sheep's next line reaches the recreated path.
///
/// Both directions. The second line appearing rules out a reopen that did
/// nothing; the first one being absent rules out a `bleats` that found the
/// archive, or a pump still holding the old inode.
#[test]
fn reopen_puts_a_rotated_log_back_where_bleats_can_read_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let gate = home.join("rotated");
    let script = write_rotating_script(&dir, &gate);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("rotator")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    // Through the reading verb, so the precondition is the same observation the
    // assertion at the bottom makes.
    let before = bleats_no_follow_until_written(home, &["all"]);
    let printed = String::from_utf8_lossy(&before.stdout);
    assert!(
        printed.contains(ROTATE_BEFORE),
        "precondition: the sheep's first line must be readable before the \
         rotation: stdout={printed}"
    );

    let online = poll_flock(home, |info| info["status"] == "online");
    let out_file = PathBuf::from(
        online["out_file"]
            .as_str()
            .unwrap_or_else(|| panic!("the daemon reports its own log paths: {online}")),
    );
    let archive = out_file.with_extension("log.1");
    std::fs::rename(&out_file, &archive).unwrap();
    assert!(!out_file.exists(), "sanity: the rename really moved it");

    // No selector, the verb's default, as a `postrotate` stanza calls it. The
    // `command` label is asserted because `reopen` and `flush` render an
    // identical table, so the labels are swappable with nothing else moving.
    let reopened = shep(home)
        .arg("reopen")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert_success(&reopened);
    let envelope: serde_json::Value = serde_json::from_slice(&reopened.stdout).unwrap();
    assert_eq!(
        envelope["command"], "reopen",
        "a reopen's envelope must say so: {envelope}"
    );

    // Opened only now, so the line below cannot predate the reopen.
    std::fs::write(&gate, "").unwrap();

    let after = bleats_no_follow_until_written(home, &["all"]);
    assert_eq!(
        after.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&after.stderr)
    );
    let stdout = String::from_utf8_lossy(&after.stdout);
    assert!(
        stdout.contains(ROTATE_AFTER),
        "a rotated sheep's next line must reach the recreated path: stdout={stdout}"
    );
    assert!(
        !stdout.contains(ROTATE_BEFORE),
        "the recreated log starts empty — the first line belongs to the \
         archive now: stdout={stdout}"
    );
    assert_eq!(
        unstamped_file(&archive),
        format!("{ROTATE_BEFORE}\n"),
        "the renamed file must stop growing the moment the handle is swapped"
    );

    graceful_kill(home);
}

/// The chain from a pump that cannot open a path again, through
/// `SupervisorError::ReopenFailed` and `rpc_error`'s `Internal`, to exit 9.
///
/// A directory in stdout's place is the failure with no permission games in
/// it: `open(2)` on a directory fails for every uid. stderr's path is left
/// alone, so the message must name stdout's and only stdout's.
#[test]
fn a_reopen_that_cannot_open_a_path_again_exits_internal() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    const MARKER: &str = "blocked-out-marker";
    let script = write_logging_script(&dir, MARKER, None);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("blocked")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    // `online` says the daemon spawned the child, not that the pump opened the
    // file. Without this wait the rename fails ENOENT.
    let written = bleats_no_follow_until_written(home, &["all"]);
    let printed = String::from_utf8_lossy(&written.stdout);
    assert!(
        printed.contains(MARKER),
        "precondition: the pump must have opened the log before the rotation \
         renames it: stdout={printed}"
    );

    // Off the daemon's own snapshot, so the test cannot disagree about which
    // file it is blocking.
    let online = poll_flock(home, |info| info["status"] == "online");
    let out_file = PathBuf::from(
        online["out_file"]
            .as_str()
            .unwrap_or_else(|| panic!("the daemon reports its own log paths: {online}")),
    );

    // Renamed, not deleted: a real rotation leaves the pump holding an inode
    // under a different name, with the live path unopenable.
    std::fs::rename(&out_file, out_file.with_extension("log.1")).unwrap();
    std::fs::create_dir(&out_file).unwrap();

    let refused = shep(home)
        .arg("reopen")
        .arg("blocked")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert_json_error(&refused, 9, "internal");
    let err: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(out_file.to_str().unwrap()),
        "the operator's one message must name the path that failed: {err}"
    );
    // The whole `<name> (id <id>)` prefix: the log path already contains the
    // name, so a bare name check would hold against a message naming no sheep.
    assert!(
        message.contains(&format!("blocked (id {})", online["id"])),
        "and the sheep it belongs to: {err}"
    );

    // Out of the daemon's way before the shutdown that follows, so nothing
    // downstream trips over a directory where a log file belongs.
    std::fs::remove_dir(&out_file).unwrap();
    graceful_kill(home);
}

/// `copytruncate`-mode rotation: an external rotator copies the live log aside
/// and empties it in place, telling the daemon nothing.
///
/// It works because a log file is opened `O_APPEND`, so every write seeks to
/// end of file and the next one lands at offset 0. The file's length is the
/// whole assertion: `bleats` prints the line either way, since a sparse hole
/// reads back as NUL bytes in front of it.
#[test]
fn an_external_copytruncate_leaves_the_next_line_at_offset_zero() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let gate = home.join("copied");
    let script = write_rotating_script(&dir, &gate);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("truncated")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    // Through the reading verb, so the first line is known to be on disk before
    // the rotator below copies it.
    let before = bleats_no_follow_until_written(home, &["all"]);
    let printed = String::from_utf8_lossy(&before.stdout);
    assert!(
        printed.contains(ROTATE_BEFORE),
        "precondition: the sheep's first line must be readable before the \
         rotation: stdout={printed}"
    );

    // Off the daemon's own snapshot, so the test cannot disagree about which
    // file this is.
    let online = poll_flock(home, |info| info["status"] == "online");
    let out_file = PathBuf::from(
        online["out_file"]
            .as_str()
            .unwrap_or_else(|| panic!("the daemon reports its own log paths: {online}")),
    );

    // `logrotate copytruncate` spelled out. Nothing here is a shep verb, so
    // the daemon is never told and the pump holds the same inode at size zero.
    let archive = out_file.with_extension("log.1");
    std::fs::copy(&out_file, &archive).unwrap();
    std::fs::File::create(&out_file).unwrap();
    assert_eq!(
        unstamped_file(&archive),
        format!("{ROTATE_BEFORE}\n"),
        "sanity: the copy really took the line the truncate is about to drop"
    );
    assert_eq!(
        std::fs::metadata(&out_file).unwrap().len(),
        0,
        "sanity: the truncate really emptied it"
    );

    // Opened only now, so the line below cannot predate the truncation.
    std::fs::write(&gate, "").unwrap();

    let after = bleats_no_follow_until_written(home, &["all"]);
    assert_eq!(
        after.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&after.stderr)
    );
    let stdout = String::from_utf8_lossy(&after.stdout);
    assert!(
        stdout.contains(ROTATE_AFTER),
        "a truncated sheep must go on logging into the same file: stdout={stdout}"
    );
    // The line above is all the sheep wrote after the truncation, and the loop
    // that read it back already waited for it to reach disk.
    assert_eq!(
        std::fs::metadata(&out_file).unwrap().len(),
        // Stamp, line, newline: the claim is that the file holds one line's
        // worth of bytes with no hole in front of it.
        (shep_core::logstamp::LOG_STAMP_BYTES + ROTATE_AFTER.len() + 1) as u64,
        "the sheep's next line must land at offset 0 of the emptied file: a \
         handle that kept its offset across an external truncation would \
         leave a hole the size of what was emptied in front of it, and \
         `bleats` would print the line just the same"
    );

    graceful_kill(home);
}

/// [`ROTATE_BEFORE`] being gone proves the truncate happened; [`ROTATE_AFTER`]
/// arriving through the same untouched handle proves it survived.
///
/// The file's length proves where that line landed: `O_APPEND` puts it at
/// offset 0, while a preserved offset would put it past a sparse hole the
/// reading verb prints the same either way.
#[test]
fn flush_empties_a_log_the_sheep_goes_on_appending_to() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let gate = home.join("flushed");
    let script = write_rotating_script(&dir, &gate);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("flusher")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    // Through the reading verb, so the precondition is the same observation the
    // assertions below make.
    let before = bleats_no_follow_until_written(home, &["all"]);
    let printed = String::from_utf8_lossy(&before.stdout);
    assert!(
        printed.contains(ROTATE_BEFORE),
        "precondition: the sheep's first line must be readable before the \
         flush: stdout={printed}"
    );

    // Off the daemon's own snapshot, so the test cannot disagree about which
    // file this is.
    let online = poll_flock(home, |info| info["status"] == "online");
    let out_file = PathBuf::from(
        online["out_file"]
            .as_str()
            .unwrap_or_else(|| panic!("the daemon reports its own log paths: {online}")),
    );

    // The selector is explicit because the verb requires one. `--format json`
    // for the `command` label, since `flush` and `reopen` render one table.
    let flushed = shep(home)
        .arg("flush")
        .arg("all")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert_success(&flushed);
    let envelope: serde_json::Value = serde_json::from_slice(&flushed.stdout).unwrap();
    assert_eq!(
        envelope["command"], "flush",
        "a flush's envelope must say so: {envelope}"
    );

    // Opened only now, so the line below cannot predate the flush.
    std::fs::write(&gate, "").unwrap();

    let after = bleats_no_follow_until_written(home, &["all"]);
    assert_eq!(
        after.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&after.stderr)
    );
    let stdout = String::from_utf8_lossy(&after.stdout);
    assert!(
        stdout.contains(ROTATE_AFTER),
        "a flushed sheep must go on logging into the same file: stdout={stdout}"
    );
    assert!(
        !stdout.contains(ROTATE_BEFORE),
        "everything written before the flush is gone: stdout={stdout}"
    );
    // The line above is all the sheep wrote after the flush, and the loop that
    // read it back already waited for it to reach disk.
    assert_eq!(
        std::fs::metadata(&out_file).unwrap().len(),
        // Stamp, line, newline: the claim is that the file holds one line's
        // worth of bytes with no hole in front of it.
        (shep_core::logstamp::LOG_STAMP_BYTES + ROTATE_AFTER.len() + 1) as u64,
        "the sheep's next line must land at offset 0 of the emptied file: a \
         handle that kept its offset across the truncate would leave a hole \
         the size of what was emptied in front of it, and `bleats` would \
         print the line just the same"
    );

    graceful_kill(home);
}

/// The flock half runs first, while the shepherd's own logs still hold a
/// marker only this test wrote: `flush all` must leave it byte for byte, and
/// `flush --daemon` must leave the sheep's log untouched.
///
/// The daemon holds fd 1 on the same inode and writes nothing to stdout, so
/// nothing races the marker.
#[test]
fn a_daemon_flush_and_a_flock_flush_never_reach_each_others_files() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let gate = home.join("flushed");
    let script = write_rotating_script(&dir, &gate);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("flusher")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let online = poll_flock(home, |info| info["status"] == "online");
    let out_file = PathBuf::from(
        online["out_file"]
            .as_str()
            .unwrap_or_else(|| panic!("the daemon reports its own log paths: {online}")),
    );
    // Read back rather than assumed: this is the precondition both halves of
    // the case are checked against.
    let before = bleats_no_follow_until_written(home, &["all"]);
    assert!(
        String::from_utf8_lossy(&before.stdout).contains(ROTATE_BEFORE),
        "precondition: the sheep must have logged something to lose"
    );

    const MARKER: &[u8] = b"a line only the shepherd's own log holds\n";
    let shepd_out = home.join("logs").join("shepd.out.log");
    let shepd_err = home.join("logs").join("shepd.err.log");
    std::fs::write(&shepd_out, MARKER).unwrap();
    std::fs::write(&shepd_err, MARKER).unwrap();

    let flock_half = shep(home).arg("flush").arg("all").output().unwrap();
    assert_success(&flock_half);
    assert_eq!(
        std::fs::metadata(&out_file).unwrap().len(),
        0,
        "the flock half must still empty the sheep it named"
    );
    // Table mode: the paths ride the JSON whatever the table does, so only the
    // default rendering can show an operator losing them.
    let printed = String::from_utf8_lossy(&flock_half.stdout);
    assert!(
        printed.contains(&out_file.display().to_string()),
        "a flush table must name the files it emptied: {printed}"
    );
    assert_eq!(
        std::fs::read(&shepd_out).unwrap(),
        MARKER,
        "a flock flush must not reach the shepherd's own stdout log"
    );
    assert_eq!(
        std::fs::read(&shepd_err).unwrap(),
        MARKER,
        "a flock flush must not reach the shepherd's own stderr log"
    );

    // The sheep has written nothing since the truncate above, so its log is
    // refilled first and "untouched" is a fact with bytes behind it.
    std::fs::write(&gate, "").unwrap();
    let after = bleats_no_follow_until_written(home, &["all"]);
    assert!(
        String::from_utf8_lossy(&after.stdout).contains(ROTATE_AFTER),
        "the sheep must have written again before the --daemon flush"
    );
    let sheep_len = std::fs::metadata(&out_file).unwrap().len();
    assert!(sheep_len > 0, "precondition: the sheep's log is not empty");

    let daemon_half = shep(home)
        .arg("flush")
        .arg("--daemon")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert_success(&daemon_half);
    let envelope: serde_json::Value = serde_json::from_slice(&daemon_half.stdout).unwrap();
    assert_eq!(envelope["command"], "flush", "{envelope}");
    let files: Vec<&str> = envelope["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["file"].as_str().unwrap())
        .collect();
    assert!(
        files.contains(&shepd_out.display().to_string().as_str())
            && files.contains(&shepd_err.display().to_string().as_str()),
        "the answer must name both files it emptied: {envelope}"
    );

    assert_eq!(std::fs::metadata(&shepd_out).unwrap().len(), 0);
    assert_eq!(std::fs::metadata(&shepd_err).unwrap().len(), 0);
    assert_eq!(
        std::fs::metadata(&out_file).unwrap().len(),
        sheep_len,
        "a --daemon flush must not reach any sheep's log"
    );

    graceful_kill(home);
}

/// The files belong to the CLI, since `launch::launch_command` creates them,
/// so there is nothing to ask. The socket is asserted too: a `connect_or_spawn`
/// here would autostart a daemon in order to be told to do nothing.
#[test]
fn a_daemon_flush_needs_no_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let logs = home.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(
        logs.join("shepd.out.log"),
        b"left behind by a dead shepherd",
    )
    .unwrap();

    let flushed = shep(home).arg("flush").arg("--daemon").output().unwrap();

    assert_success(&flushed);
    assert_eq!(
        std::fs::metadata(logs.join("shepd.out.log")).unwrap().len(),
        0
    );
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "this verb must not autostart a daemon to empty files the CLI owns"
    );
}

/// A `default_value` on the selector would make a bare `shep flush` empty
/// every log in the flock and exit 0. Clap must refuse before anything
/// connects, which the socket assertion is for.
#[test]
fn flush_without_a_selector_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let bare = shep(dir.path()).arg("flush").output().unwrap();

    assert_eq!(
        bare.status.code(),
        Some(2),
        "clap's usage exit code; stdout={}",
        String::from_utf8_lossy(&bare.stdout)
    );
    assert!(
        !dir.path().join("run").join("shep.sock").exists(),
        "a usage error must not have autostarted a daemon"
    );
}
