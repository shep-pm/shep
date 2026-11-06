//! A daemon reload that keeps a pty sheep: its pid, its terminal and its
//! whisper all survive, and the exec does not hang the terminal up.

use super::*;

/// Reads `path` until a line satisfies `want`, or [`HANDOVER_DEADLINE`]
/// expires, and returns that line without its timestamp. Panics on expiry
/// with what the log held.
#[cfg(unix)]
#[track_caller]
fn await_log_line(path: &Path, what: &str, want: impl Fn(&str) -> bool) -> String {
    let start = Instant::now();
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let found = text
            .lines()
            .map(shep_core::logstamp::strip)
            .find(|line| want(line));
        if let Some(line) = found {
            return line.to_owned();
        }
        assert!(
            start.elapsed() < HANDOVER_DEADLINE,
            "{what}: no such line within {HANDOVER_DEADLINE:?}; the log held {text:?}"
        );
        std::thread::sleep(HANDOVER_POLL_INTERVAL);
    }
}

/// A pty sheep keeps its pid and its terminal across `shep daemon reload`,
/// and a whisper after it is answered on the same terminal.
///
/// The sheep traps `SIGHUP`, which a master closed by the exec would raise.
/// Each answer names the terminal: a new pty under the same pid would
/// still answer.
#[cfg(unix)]
#[test]
fn a_pty_sheep_keeps_its_pid_and_its_terminal_across_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        &dir,
        "talker.sh",
        "#!/bin/sh\ntrap 'echo got-hup' HUP\necho ready\n\
         while read line; do echo \"heard:$line on $(tty)\"; done\n",
    );
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"talker\"\nscript = '{}'\npty = true\n",
            script.display()
        ),
    );
    let mut guard = DaemonGuard::default();

    let before = start_a_sheep_and_wait_online(
        &mut guard,
        dir.path(),
        &["--format", "json", "start", flockfile.to_str().unwrap()],
    );
    let out_file = PathBuf::from(
        before["out_file"]
            .as_str()
            .unwrap_or_else(|| panic!("an online sheep reports its out file: {before}")),
    );
    await_log_line(&out_file, "the sheep must be up", |line| line == "ready");
    assert_success(
        &shep(dir.path())
            .args(["whisper", "talker", "first"])
            .output()
            .unwrap(),
    );
    let first = await_log_line(&out_file, "the first whisper's answer", |line| {
        line.starts_with("heard:first on ")
    });

    assert_success(
        &shep(dir.path())
            .arg("daemon")
            .arg("reload")
            .output()
            .unwrap(),
    );
    let after = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    assert_eq!(
        after["pid"], before["pid"],
        "a moved pid means the sheep was respawned, which is the stop arm: {after}"
    );

    assert_success(
        &shep(dir.path())
            .args(["whisper", "talker", "second"])
            .output()
            .unwrap(),
    );
    let second = await_log_line(&out_file, "the second whisper's answer", |line| {
        line.starts_with("heard:second on ")
    });
    assert_eq!(
        first.strip_prefix("heard:first on "),
        second.strip_prefix("heard:second on "),
        "the terminal moved across the reload"
    );
    let log = std::fs::read_to_string(&out_file).unwrap();
    assert!(
        !log.contains("got-hup"),
        "the reload hung the sheep's terminal up: {log}"
    );

    graceful_kill(dir.path());
}
