//! The scripts, Flockfiles, config files and canned HTTP bodies a case
//! writes before it runs anything.

use super::*;

/// The path of the committed `--format json` fixture named `name`.
pub(crate) fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(format!("{name}.json"))
}

/// Loads and parses a committed fixture, for the envelope fixtures compared
/// structurally as a `serde_json::Value`. `bleats_no_follow.json` is compared
/// byte for byte through `std::fs::read` instead.
pub(crate) fn load_fixture(name: &str) -> serde_json::Value {
    let path = fixture_path(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Writes a trivial long-running script into `dir` and returns its path.
///
/// The trailing `sleep` is bare, not `exec sleep`: a bare one is a forked
/// child of the `/bin/sh` the daemon tracks, sharing its process group, which
/// is what a stop signalling only the recorded pid would orphan.
pub(crate) fn write_test_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "sheep.sh",
        &format!(
            "{}{}{}",
            script_header(),
            record_pid_line(dir),
            sleep_line(SCRIPT_SLEEP_SECS)
        ),
    )
}

/// Writes a script that backgrounds a `sleep 300` and `wait`s on it, a real
/// forked lamb for [`describe_renders_a_real_sheeps_lamb_tree`].
///
/// `wait` keeps the top-level `sh` alive as long as its child, so the daemon's
/// pid stays the one this test started and a stop still reaches the lamb
/// through the shared process group.
pub(crate) fn write_forking_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "forker.sh",
        &format!("#!/bin/sh\n{}sleep 300 &\nwait\n", record_pid_line(dir)),
    )
}

/// The line every fixture script opens with: this spawn's own pid, appended
/// to `<home>/`[`FIXTURE_PIDS`].
///
/// `$$` is the pid the daemon tracks and leads its own process group, so
/// `-pid` reaches that sheep's lambs and [`DaemonGuard`]'s sweep can reap a
/// whole flock. Appended, so a restart adds a row; the dead pid is an `ESRCH`
/// no-op later.
///
/// The path is absolute, since a script's cwd is the sheep's `cwd`, and
/// quoted, since a tempdir path may carry shell metacharacters. It never goes
/// to stdout: one extra line breaks the byte-exact `bleats` fixture.
pub(crate) fn record_pid_line(dir: &TempDir) -> String {
    #[cfg(unix)]
    {
        format!(
            "echo $$ >> \"{}\"\n",
            dir.path().join(FIXTURE_PIDS).display()
        )
    }
    // No `$$` in `cmd.exe`, and none needed: a Windows sheep is in a job object
    // it cannot leave, so the daemon dying takes the whole tree with it.
    #[cfg(windows)]
    {
        let _ = dir;
        String::new()
    }
}

/// [`write_test_script`] with [`SLOW_SCRIPT_SLEEP_SECS`]' sleep, for
/// [`a_cron_occurrence_restarts_a_sheep_on_the_real_clock`], which runs one as
/// its subject and one as its control.
pub(crate) fn write_slow_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "slow.sh",
        &format!(
            "{}{}{}",
            script_header(),
            record_pid_line(dir),
            sleep_line(SLOW_SCRIPT_SLEEP_SECS)
        ),
    )
}

/// Writes a script that grows its own resident set past [`BALLOON_BYTES`] and
/// then sleeps for [`SLOW_SCRIPT_SLEEP_SECS`].
///
/// The growth is a shell string doubled in place, so `$$`, the pid the daemon
/// arms the enforcer against, is the process whose resident set moves. Pure
/// shell arithmetic, so it does not vary with a platform's coreutils, and it
/// costs about a quarter of a second, inside the gap before the enforcer's
/// first tick.
pub(crate) fn write_ballooning_script(dir: &TempDir) -> PathBuf {
    write_script(dir, "balloon.sh", &balloon_body(dir))
}

/// Writes a script that emits one marker line on stdout, optionally one on
/// stderr, and then sleeps.
///
/// `None` writes to stderr not at all: an empty line still reaches the err
/// file and gains the byte-exact fixture an object it did not predict. The
/// sleep keeps the output countable, since a script that exits is restarted
/// and appends another copy of every marker.
pub(crate) fn write_logging_script(
    dir: &TempDir,
    out_marker: &str,
    err_marker: Option<&str>,
) -> PathBuf {
    let mut script = format!(
        "{}{}{}",
        script_header(),
        record_pid_line(dir),
        echo_line(out_marker)
    );
    if let Some(err_marker) = err_marker {
        script.push_str(&echo_err_line(err_marker));
    }
    script.push_str(&sleep_line(SCRIPT_SLEEP_SECS));
    write_script(dir, "logging.sh", &script)
}

/// [`write_logging_script`] for a multi-instance app: one stdout line naming
/// the slot, read out of the `SHEP_INSTANCE` the daemon injects.
///
/// The slot comes from the child's own environment, not from anything this
/// harness substitutes, since the claim is that the daemon gave each instance
/// a different one. `name` is the script's basename, so several can share one
/// `$TMPDIR`.
pub(crate) fn write_instance_logging_script(dir: &TempDir, name: &str, prefix: &str) -> PathBuf {
    let echo = {
        #[cfg(unix)]
        {
            format!("echo \"{prefix}-$SHEP_INSTANCE\"\n")
        }
        #[cfg(windows)]
        {
            format!("echo {prefix}-%SHEP_INSTANCE%\r\n")
        }
    };
    write_script(
        dir,
        &format!("{name}.sh"),
        &format!(
            "{}{}{}{}",
            script_header(),
            record_pid_line(dir),
            echo,
            sleep_line(SCRIPT_SLEEP_SECS)
        ),
    )
}

/// Writes a script that prints [`ROTATE_BEFORE`], blocks until `gate` exists,
/// prints [`ROTATE_AFTER`], and sleeps.
///
/// A rotation is observable only in what happens to a line written after the
/// rename, and the gate makes "after" a fact rather than a timing bet: the
/// test creates it once the reopen has returned.
pub(crate) fn write_rotating_script(dir: &TempDir, gate: &Path) -> PathBuf {
    write_script(
        dir,
        "rotating.sh",
        &format!(
            "{}{}{}{}{}{}",
            script_header(),
            record_pid_line(dir),
            echo_line(ROTATE_BEFORE),
            wait_for_path_lines(gate),
            echo_line(ROTATE_AFTER),
            sleep_line(SCRIPT_SLEEP_SECS)
        ),
    )
}

/// Writes a script that blocks until `sentinel` exists, then announces
/// readiness on the shepherd channel and sleeps.
///
/// A file, not a delay: `listen_timeout` takes a `wait_ready` sheep `Online`
/// on elapse whether it signalled or not, so a script that merely slept would
/// give a loaded runner a `starting` window it could close early.
///
/// `>&3` is the fd the runner hands a sheep whose app asks for a channel, and
/// `{"kind":"ready"}` is the wire string `ChildMessage::Ready` pins.
pub(crate) fn write_ready_script(dir: &TempDir, sentinel: &Path) -> PathBuf {
    write_script(
        dir,
        "ready.sh",
        &format!(
            "{}{}{}{}{}",
            script_header(),
            record_pid_line(dir),
            wait_for_path_lines(sentinel),
            ready_message_line(),
            sleep_line(SCRIPT_SLEEP_SECS)
        ),
    )
}

/// Shared write-plus-chmod tail of the script helpers above.
pub(crate) fn write_script(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(script_name(name));
    std::fs::write(&path, contents).unwrap();
    // Windows has no execute bit: `CreateProcess` decides from the extension,
    // which `script_name` supplied.
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

/// `name` with the extension this platform will actually execute.
///
/// Callers all name their scripts `something.sh`. `CreateProcess` needs an
/// extension `%PATHEXT%` knows, which `.sh` is not and `.cmd` is.
pub(crate) fn script_name(name: &str) -> String {
    #[cfg(unix)]
    {
        name.to_string()
    }
    #[cfg(windows)]
    {
        format!("{}.cmd", name.trim_end_matches(".sh"))
    }
}

/// The first line of a generated script: a `#!` on unix, `@echo off` on
/// Windows so the interpreter does not echo every line into the sheep's own
/// stdout and corrupt what the log-reading cases assert.
pub(crate) fn script_header() -> String {
    #[cfg(unix)]
    {
        "#!/bin/sh\n".to_string()
    }
    #[cfg(windows)]
    {
        "@echo off\r\n".to_string()
    }
}

/// A line that keeps the script alive for roughly `secs` seconds.
///
/// `ping` rather than `timeout` on Windows: `timeout.exe` refuses a
/// non-console stdin, and every sheep gets a null one. `ping -n N` sends N
/// packets a second apart, so it takes one more than the seconds wanted.
pub(crate) fn sleep_line(secs: u32) -> String {
    #[cfg(unix)]
    {
        format!("sleep {secs}\n")
    }
    #[cfg(windows)]
    {
        format!("ping -n {} 127.0.0.1 >nul\r\n", secs + 1)
    }
}

/// A line writing `text` to stdout.
pub(crate) fn echo_line(text: &str) -> String {
    #[cfg(unix)]
    {
        format!("echo '{text}'\n")
    }
    #[cfg(windows)]
    {
        format!("echo {text}\r\n")
    }
}

/// Lines that block until `path` exists, polling.
///
/// `cmd.exe` has no `until` and no sub-second sleep, so the batch arm is an
/// `if exist`/`goto` loop polling with `ping -n 2`.
pub(crate) fn wait_for_path_lines(path: &Path) -> String {
    #[cfg(unix)]
    {
        format!("until [ -e \"{}\" ]; do sleep 0.1; done\n", path.display())
    }
    #[cfg(windows)]
    {
        format!(
            ":wait\r\nif exist \"{}\" goto ready\r\nping -n 2 127.0.0.1 >nul\r\ngoto wait\r\n:ready\r\n",
            path.display()
        )
    }
}

/// A line writing one `ready` shepherd-channel message.
///
/// The channel is inherited fd 3 on unix. Windows has no fd-3 inheritance, so
/// the daemon exports `%SHEP_CHANNEL_PIPE%` and the app opens it by name.
pub(crate) fn ready_message_line() -> String {
    #[cfg(unix)]
    {
        "printf '{\"kind\":\"ready\"}\\n' >&3\n".to_string()
    }
    #[cfg(windows)]
    {
        "echo {\"kind\":\"ready\"}>\"%SHEP_CHANNEL_PIPE%\"\r\n".to_string()
    }
}

/// A line writing `text` to stderr.
pub(crate) fn echo_err_line(text: &str) -> String {
    #[cfg(unix)]
    {
        format!("echo '{text}' 1>&2\n")
    }
    #[cfg(windows)]
    {
        format!("echo {text} 1>&2\r\n")
    }
}

/// The body of [`write_ballooning_script`]: hold [`BALLOON_BYTES`] live, then
/// stay up.
///
/// `cmd.exe` variables cap out around 8 KB, so the Windows arm allocates in
/// PowerShell. shep samples a sheep's whole tree, so a child's memory counts.
pub(crate) fn balloon_body(dir: &TempDir) -> String {
    // Only the unix arm records a pid.
    #[cfg(windows)]
    let _ = dir;
    #[cfg(unix)]
    {
        format!(
            "{}{}s=x\nwhile [ ${{#s}} -lt {BALLOON_BYTES} ]; do s=\"$s$s\"; done\nsleep {SLOW_SCRIPT_SLEEP_SECS}\n",
            script_header(),
            record_pid_line(dir),
        )
    }
    #[cfg(windows)]
    {
        format!(
            "{}powershell -NoProfile -Command \"$s = 'x' * {BALLOON_BYTES}; Start-Sleep -Seconds {SLOW_SCRIPT_SLEEP_SECS}; $s.Length > $null\"\r\n",
            script_header(),
        )
    }
}

/// Writes a Flockfile whose one app asks for a readiness handshake it never
/// performs, so [`NEVER_READY_TIMEOUT`] elapses and the daemon writes
/// [`READINESS_RECORD`] about it.
///
/// A plain [`write_test_script`] sheep is enough: `wait_ready` opens the
/// channel on fd 3 and the script never writes to it.
pub(crate) fn write_never_ready_flockfile(dir: &TempDir) -> PathBuf {
    let script = write_test_script(dir);
    write_flockfile(
        dir,
        &format!(
            "[[app]]\nname = \"gated\"\nscript = '{}'\n\
             wait_ready = true\nlisten_timeout = \"{NEVER_READY_TIMEOUT}\"\n",
            script.display(),
        ),
    )
}

/// Writes `Flockfile.toml` into `dir` and returns its path. The `.toml`
/// extension is what routes `shep start <path>` down `FlockFormat::from_path`
/// rather than the bare-script arm.
pub(crate) fn write_flockfile(dir: &TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("Flockfile.toml");
    std::fs::write(&path, body).unwrap();
    path
}

// --- Dog index helpers -------------------------------------------------

/// Serves `response`, a complete raw HTTP response, once on an ephemeral
/// loopback port from a background thread, and returns its `http://` URL.
///
/// `SHEP_DOG_INDEX`'s loopback carve-out is what allows `http://` here: this
/// file drives the real binary, so there is no seam to skip the `https://`
/// check from.
pub(crate) fn serve_raw_response(response: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        if let Ok((mut stream, _peer)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://127.0.0.1:{}/dogs.json", addr.port())
}

/// [`serve_raw_response`] wrapping `body` as a well-formed 200.
pub(crate) fn serve_dog_index(body: &str) -> String {
    serve_raw_response(format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ))
}

/// A two-entry community index shaped like the live `web/public/dogs.json`:
/// Spot, clean; and Rex, whose description carries a raw `\u{1b}[2J`
/// screen-clear for `dog_index::sanitise` to strip. Both are valid, so
/// `skipped` stays zero.
///
/// `parse_index` refuses a bare array, so the entries are wrapped in the
/// `$schema`/`version`/`dogs` object.
pub(crate) fn two_entry_index_json() -> String {
    serde_json::json!({
        "$schema": "https://shep-pm.com/dogs.schema.json",
        "version": 1,
        "dogs": [
            {
                "name": "Spot",
                "package": "shep-log-rotate",
                "adopt_as": "log-rotate",
                "description": "Rotates grown log files and asks the shepherd to reopen them.",
                "repo": "https://github.com/shep-pm/shep-log-rotate",
                "license": "MIT OR Apache-2.0",
                "category": "logs",
                "source": {
                    "kind": "cargo-git",
                    "url": "https://github.com/shep-pm/shep-log-rotate"
                }
            },
            {
                "name": "Rex",
                "package": "shep-watchdog",
                "adopt_as": "watchdog",
                "description": "Barks when a sheep stops answering.\u{1b}[2J",
                "repo": "https://github.com/example/shep-watchdog",
                "license": "Apache-2.0",
                "category": "health",
                "source": {
                    "kind": "go-install",
                    "module": "github.com/example/shep-watchdog"
                }
            }
        ]
    })
    .to_string()
}

/// Writes `$SHEP_HOME/shep.toml` directly, before any daemon boots off it.
/// Neither `shep enable` nor `shep adopt` has a flag for `[dog.metrics] bind`,
/// which every case below needs to avoid colliding with a real `9615`.
pub(crate) fn write_shep_toml(dir: &TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, body).unwrap();
    path
}
