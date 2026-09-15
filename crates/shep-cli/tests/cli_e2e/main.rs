//! End-to-end tier: drives the real `shep` binary via `assert_cmd` against a
//! real daemon, a real socket, and real spawned sheep, each on a fresh
//! `$SHEP_HOME` in its own [`tempfile::TempDir`].
//!
//! Two rules every case follows: `.timeout(CMD_TIMEOUT)` before `.output()`,
//! so a hang fails as a named assertion; and a [`DaemonGuard`] adopting the
//! `$SHEP_HOME` immediately after the `Output` that might have spawned a
//! daemon, before any assertion that could panic.
//!
//! Windows scripts are `.cmd` (see `script_header`); cases that cannot port
//! carry their own `#[cfg(unix)]`.

// The `#[cfg(unix)]` cases take their helpers and constants with them, so on
// Windows those items compile unused.
#![cfg_attr(windows, allow(dead_code))]

#[cfg(unix)]
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use assert_cmd::cargo::CommandCargoExt as _;
use tempfile::TempDir;

// --- Issue 1/2/3: adopt ergonomics and `shep <dogname>` dispatch ---------

#[test]
fn shep_adopt_finds_a_binary_on_path_by_bare_name() {
    let home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    let binary = write_script(&bin_dir, "shep-log-rotate", "#!/bin/sh\nexit 0\n");

    let output = Command::cargo_bin("shep")
        .unwrap()
        .env("PATH", bin_dir.path())
        .arg("--home")
        .arg(home.path())
        .arg("adopt")
        .arg("shep-log-rotate")
        .arg("--name")
        .arg("lr")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();

    assert_success(&output);
    let written = std::fs::read_to_string(home.path().join("shep.toml")).unwrap();
    assert!(
        written.contains(&as_shep_spells_it(&binary)),
        "the $PATH hit must be the recorded binary: {written}"
    );
}

#[cfg(unix)]
/// A literal `~/` path, expanded by `shep adopt` as it is in a Flockfile.
#[test]
fn shep_adopt_expands_a_leading_tilde_path() {
    let shep_home = TempDir::new().unwrap();
    let fake_user_home = TempDir::new().unwrap();
    let bin_dir = fake_user_home.path().join(".cargo").join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let binary = bin_dir.join("shep-log-rotate");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    mode.set_mode(0o755);
    std::fs::set_permissions(&binary, mode).unwrap();

    let output = Command::cargo_bin("shep")
        .unwrap()
        .env("HOME", fake_user_home.path())
        .arg("--home")
        .arg(shep_home.path())
        .arg("adopt")
        .arg("~/.cargo/bin/shep-log-rotate")
        .arg("--name")
        .arg("lr")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();

    assert_success(&output);
    let written = std::fs::read_to_string(shep_home.path().join("shep.toml")).unwrap();
    assert!(
        written.contains(&as_shep_spells_it(&binary)),
        "the ~/-expanded binary must be the one recorded: {written}"
    );
}
/// Writes a script that records its own argv and `$SHEP_HOME` into `marker`
/// (inside `dir`), prints a distinctive stdout line, and exits `code`.
fn write_marker_script(dir: &TempDir, marker: &Path, code: u8) -> PathBuf {
    // `$*`/`$SHEP_HOME` in a shell script, `%*`/`%SHEP_HOME%` in a `.cmd`.
    // No space before `>` in the batch arm: `echo foo > x` writes a trailing
    // space in `cmd.exe`, and the assertion is on an exact line.
    #[cfg(unix)]
    let body = format!(
        "#!/bin/sh\necho \"argv:$*\" > \"{marker}\"\necho \"home:$SHEP_HOME\" >> \"{marker}\"\necho from-the-dog\nexit {code}\n",
        marker = marker.display(),
    );
    #[cfg(windows)]
    let body = format!(
        "@echo off\r\necho argv:%*>\"{marker}\"\r\necho home:%SHEP_HOME%>>\"{marker}\"\r\necho from-the-dog\r\nexit /b {code}\r\n",
        marker = marker.display(),
    );
    write_script(dir, "dog.sh", &body)
}

/// `shep <dogname> [args...]` runs an adopted dog with the operator's argv
/// passed through untouched and `$SHEP_HOME` set. The dispatch call carries no
/// `--home`, exercising `home_before`'s fallback to the real environment.
#[test]
fn an_adopted_dog_runs_directly_with_its_own_argv_and_shep_home() {
    let home = TempDir::new().unwrap();
    let marker = home.path().join("marker.txt");
    let script = write_marker_script(&home, &marker, 7);

    let adopted = shep(home.path())
        .arg("adopt")
        .arg(&script)
        .arg("--name")
        .arg("deploy")
        .output()
        .unwrap();
    assert_success(&adopted);

    let ran = Command::cargo_bin("shep")
        .unwrap()
        .env("SHEP_HOME", home.path())
        .arg("deploy")
        .arg("koji")
        .arg("--flag")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();

    assert_eq!(
        ran.status.code(),
        Some(7),
        "the dog's own exit code must pass through: {ran:?}"
    );
    assert!(
        String::from_utf8_lossy(&ran.stdout).contains("from-the-dog"),
        "stdio must be inherited, not captured away: {ran:?}"
    );
    let recorded = std::fs::read_to_string(&marker).unwrap();
    assert!(
        recorded.contains("argv:koji --flag"),
        "argv must reach the dog exactly as typed: {recorded}"
    );
    assert!(
        recorded.contains(&format!("home:{}", home.path().display())),
        "SHEP_HOME must reach the dog's own environment: {recorded}"
    );
}

/// `dispatch_adopted_dog` runs only once clap has failed to match a token
/// against a real subcommand, so an adopted dog named `stop` never shadows the
/// verb. Exit 5 (`DaemonUnreachable`, since `stop` does not autostart) and the
/// marker file never appearing are what say the built-in was dispatched.
#[test]
fn a_built_in_verb_always_wins_over_a_same_named_adopted_dog() {
    let home = TempDir::new().unwrap();
    let marker = home.path().join("marker.txt");
    let script = write_marker_script(&home, &marker, 0);
    std::fs::write(
        home.path().join("shep.toml"),
        format!(
            "[daemon]\nadopted_dogs = {{ stop = \"{}\" }}\nenabled_dogs = [\"stop\"]\n",
            script.display()
        ),
    )
    .unwrap();

    let output = shep(home.path()).arg("stop").arg("all").output().unwrap();

    assert_eq!(
        output.status.code(),
        Some(5),
        "must be the built-in `stop`'s own DaemonUnreachable, not the dog's exit 0: {output:?}"
    );
    assert!(
        !marker.exists(),
        "the adopted dog's script must never have run"
    );
}

/// `dispatch_adopted_dog` finding nothing falls through to clap's own
/// unknown-verb rendering, suggestions included.
#[test]
fn an_unknown_verb_with_no_matching_dog_keeps_claps_own_suggestion() {
    let home = TempDir::new().unwrap();

    let output = shep(home.path()).arg("flcok").output().unwrap();

    assert_eq!(output.status.code(), Some(2), "clap's own usage exit code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized subcommand"),
        "clap's own wording must survive untouched: {stderr}"
    );
    assert!(
        stderr.contains("flock"),
        "clap's own did-you-mean must still suggest the real verb: {stderr}"
    );
}

/// A Flockfile edit reaches a registered sheep only where the first load
/// established nothing, and what it reaches is reported by name, never by
/// value. A key the first load established belongs to whoever set it. A key
/// nobody has established is appended, applied where it can be and parked
/// where it cannot, and the line naming a parked field names `shep reload`.
#[test]
fn a_flockfile_edit_reaches_a_sheep_only_where_the_first_load_established_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let elsewhere = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    // `env` carries a value that must never be printed, and the third file
    // changes two fields at once, so a report that stopped at the first
    // difference fails here too.
    let body = |cwd: &Path, env: &str, extra: &str| {
        format!(
            "[[app]]\nname = \"edited\"\nscript = '{}'\ncwd = '{}'\n{extra}env = {{ {env} }}\n",
            script.display(),
            cwd.display(),
        )
    };
    let flockfile = write_flockfile(&dir, &body(home, "API_TOKEN = \"hunter2-before\"", ""));
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);
    poll_flock(home, |info| info["status"] == "online");

    // The edit, over the same path the daemon was told about. The first load
    // established both keys, so this load may do nothing at all.
    write_flockfile(
        &dir,
        &body(elsewhere.path(), "API_TOKEN = \"hunter2-after\"", ""),
    );
    let again = shep(home).arg("start").arg(&flockfile).output().unwrap();
    // A load reports, it does not fail.
    assert_success(&again);
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(
        !stderr.contains("cwd") && !stderr.contains("env"),
        "an established key is not the file's to change, so there is nothing \
         to report: {stderr}"
    );
    let info = poll_flock(home, |info| info["status"] == "online");
    assert!(
        info["pending"].is_null(),
        "an established key must not be parked either: {info}"
    );

    // A third load, adding a key nobody has established: one that reaches the
    // running child and one that cannot.
    write_flockfile(
        &dir,
        &body(
            elsewhere.path(),
            "API_TOKEN = \"hunter2-after\", MODE = \"blue\"",
            "max_memory = \"512M\"\n",
        ),
    );
    let third = shep(home).arg("start").arg(&flockfile).output().unwrap();
    assert_success(&third);

    let stderr = String::from_utf8_lossy(&third.stderr);
    assert!(
        stderr.contains("edited"),
        "the report must name the sheep: {stderr}"
    );
    assert!(
        stderr.contains("max_memory") && stderr.contains("env"),
        "the report must name every field that changed: {stderr}"
    );
    assert!(
        !stderr.contains("cwd"),
        "the established key is still nobody's to change: {stderr}"
    );
    assert!(
        !stderr.contains("hunter2") && !stderr.contains("blue"),
        "a field's VALUE must never reach an operator's terminal (IR-41): {stderr}"
    );
    // An env change is baked into a running child, so the report has to say
    // what brings it into effect, not only which field moved.
    assert!(
        stderr.contains("shep reload edited"),
        "a pending field travels with the verb that promotes it: {stderr}"
    );

    graceful_kill(home);
}

/// A load that refused one app exits non-zero, and still reports the app it
/// applied. The refusal is a real one: a plain load never reshapes a flock, so
/// a file that grows an `instances` line is refused that one field by name and
/// told which flag would take it.
#[test]
fn a_load_that_refused_one_app_exits_non_zero_and_still_reports_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    // `steady` carries the field that lands, `stocky` the one that cannot.
    // `max_restarts` is read when a sheep exits, so it is in force the moment
    // it reaches the stored spec and reports as applied.
    let body = |steady: &str, stocky: &str| {
        format!(
            "[[app]]\nname = \"steady\"\nscript = '{}'\n{steady}\
             [[app]]\nname = \"stocky\"\nscript = '{}'\n{stocky}",
            script.display(),
            script.display(),
        )
    };
    let flockfile = write_flockfile(&dir, &body("", ""));
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);
    poll_flock(home, |info| info["status"] == "online");

    // One field each: one the daemon applies, one it refuses.
    write_flockfile(&dir, &body("max_restarts = 9\n", "instances = 2\n"));
    let again = shep(home).arg("start").arg(&flockfile).output().unwrap();

    let stderr = String::from_utf8_lossy(&again.stderr);
    // Pinned at 4: `InvalidConfig` is the code the rest of this CLI uses for a
    // configuration the daemon would not accept.
    assert_eq!(
        again.status.code(),
        Some(4),
        "a refused load is a failed load, and an invalid-config one: {stderr}"
    );
    assert!(
        stderr.contains("stocky") && stderr.contains("reshapes a flock"),
        "the refusal names the app and what was refused: {stderr}"
    );
    assert!(
        stderr.contains("applied max_restarts"),
        "and the app that DID apply is still reported beside it: {stderr}"
    );

    graceful_kill(home);
}

/// One app whose script does not exist refuses the whole Flockfile, before
/// anything is registered, and names every app that failed. Without the check,
/// an app that fails to spawn partway through leaves the apps before it
/// registered and the apps after it unreached.
#[test]
fn one_absent_script_refuses_the_whole_flockfile_and_registers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    // `good` first, so a check that runs per app as it registers would have
    // registered it by the time it reached `unbuilt`.
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"good\"\nscript = '{}'\n\n\
             [[app]]\nname = \"unbuilt\"\nscript = '{}/never-built'\n",
            script.display(),
            dir.path().display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    assert_eq!(
        output.status.code(),
        Some(7),
        "the spawn-failed exit code: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unbuilt"),
        "the refusal must name the app: {stderr}"
    );
    assert!(
        stderr.contains("never-built"),
        "the refusal must name the path it looked at: {stderr}"
    );

    let flock = shep(home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&flock);
    let envelope: serde_json::Value = serde_json::from_slice(&flock.stdout).unwrap();
    assert_eq!(
        envelope["data"].as_array().map(Vec::len),
        Some(0),
        "a Flockfile refused as a whole must leave NOTHING registered: {}",
        envelope
    );

    graceful_kill(home);
}

/// A spawn that fails for a reason no preflight could see still names the
/// sheep, the path it tried, and the `cwd` it tried it in. A bare
/// `SpawnFailed` names none of them.
///
/// The script exists and cannot be exec'd, so this reaches the real `spawn`
/// and its real `EACCES` rather than the batch existence check.
#[test]
fn a_spawn_that_no_check_could_have_caught_still_names_the_sheep_and_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let unrunnable = dir.path().join("unrunnable.sh");
    std::fs::write(&unrunnable, "#!/bin/sh\nsleep 60\n").unwrap();
    // Present, so the batch check passes it; no execute bit anywhere, which
    // even a root-owned run cannot exec.
    #[cfg(unix)]
    {
        std::fs::set_permissions(&unrunnable, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"locked-out\"\nscript = '{}'\n",
            unrunnable.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    assert_eq!(
        output.status.code(),
        Some(7),
        "the spawn-failed exit code: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("locked-out"),
        "the error must name the sheep: {stderr}"
    );
    assert!(
        stderr.contains("unrunnable.sh"),
        "the error must name the script it tried: {stderr}"
    );
    // Canonicalized: `start` fills an app's absent `cwd` from the
    // Flockfile's own directory through `canonicalize`, and on macOS a
    // tempdir's `/var/...` resolves to `/private/var/...`.
    let flockfile_dir = as_shep_spells_it(dir.path());
    assert!(
        stderr.contains(&format!("in {flockfile_dir}")),
        "the error must name the cwd it tried it in: {stderr}"
    );

    graceful_kill(home);
}

/// A bare command not on the shepherd's PATH is reported, fails to spawn, and
/// takes no other app in the Flockfile down with it. A `script` with a `/` in
/// it is a filesystem claim the daemon can settle, so that one is refused as a
/// batch instead. The `PATH` deciding a bare command is the daemon's: under a
/// `shep startup` unit, whatever launchd or systemd hands it, with
/// `assemble`'s fallback of `/usr/local/bin:/usr/bin:/bin`.
#[test]
fn a_bare_command_off_the_path_takes_only_its_own_app_down() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    // `resolvable` first: it is the app that must survive, and a refusal of
    // the whole batch would leave it unregistered rather than online.
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"resolvable\"\nscript = '{}'\n\n\
             [[app]]\nname = \"no-interpreter\"\nscript = \"shep-no-such-interpreter-xyz\"\n",
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    // Exit 7 all the same: one app really did fail.
    assert_eq!(
        output.status.code(),
        Some(7),
        "the one app that cannot run still fails the command: {output:?}"
    );
    // The useful sentence reaches the operator's terminal, not only the
    // shepherd's log; `SpawnFailed` carries free-form text.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not on the shepherd's PATH"),
        "the reply must explain WHY the program was not found, not only that \
         it was not: {stderr}"
    );
    assert!(
        stderr.contains("shep-no-such-interpreter-xyz") && stderr.contains("no-interpreter"),
        "naming the program and the sheep: {stderr}"
    );

    let data = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["name"] == "resolvable" && row["status"] == "online")
        })
    });
    // Found by hand rather than through `sheep_named`, which panics with its
    // own message: the regression makes the row absent, and a red run has to
    // say that.
    let survivor = data
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "resolvable"));
    assert_eq!(
        survivor.map(|row| &row["status"]).map(ToString::to_string),
        Some("\"online\"".to_string()),
        "an app whose own script resolves must come up regardless of a \
         sibling's unresolvable interpreter, and must not be refused \
         registration over it: {data}"
    );

    let log = std::fs::read_to_string(home.join("logs").join("shepd.err.log")).unwrap();
    assert!(
        log.contains("shep-no-such-interpreter-xyz") && log.contains("PATH"),
        "the shepherd must still say which program it could not find: {log}"
    );
    assert!(
        log.contains("no-interpreter"),
        "and which sheep wanted it: {log}"
    );

    graceful_kill(home);
}

/// A real multi-instance flock through the real binary: distinct slots, a
/// grouped `shep flock` table, and a `merge_logs` app whose backlog prints each
/// line exactly once. A `shep bleats` that reads a file per matched row doubles
/// every line, which counting occurrences catches and a `contains` check does
/// not. A `sh`/`.cmd` script rather than node, so the case runs on Windows too.
#[test]
fn a_multi_instance_flock_gets_distinct_slots_a_grouped_table_and_undoubled_bleats() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let web = write_instance_logging_script(&dir, "web-instances", "web-slot");
    let merged = write_instance_logging_script(&dir, "merged-instances", "merged-slot");
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"web\"\nscript = '{}'\ninstances = 3\n\n\
             [[app]]\nname = \"merged\"\nscript = '{}'\ninstances = 2\nmerge_logs = true\n",
            web.display(),
            merged.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    // 1. Five processes, and every one of them reports the slot it occupies.
    let data = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.iter().filter(|row| row["status"] == "online").count() == 5)
    });
    let slots_of = |name: &str| {
        let mut slots: Vec<u64> = data
            .as_array()
            .expect("flock data is an array")
            .iter()
            .filter(|row| row["name"] == name)
            .map(|row| {
                row["instance"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("every instance reports a slot: {data}"))
            })
            .collect();
        slots.sort_unstable();
        slots
    };
    assert_eq!(slots_of("web"), vec![0, 1, 2], "distinct slots: {data}");
    assert_eq!(slots_of("merged"), vec![0, 1], "distinct slots: {data}");

    // 2. The table names each row by its own slot. The flat shape, not the
    // boxed one: `must_render_bare` drops any run whose stdout is not a
    // terminal to `StyleLevel::Bare`, and `--style plain` does not override it.
    let table = shep(home).arg("flock").output().unwrap();
    assert_success(&table);
    let rendered = String::from_utf8_lossy(&table.stdout);
    for slot in 0..3 {
        assert!(
            rendered.contains(&format!("web:{slot}")),
            "a row named for slot {slot}: {rendered}"
        );
    }
    assert!(
        rendered.contains("merged:0") && rendered.contains("merged:1"),
        "and the same for the merged app: {rendered}"
    );

    // 3. The regression guard. First, that `merge_logs` really did collapse
    // both instances onto one path: without that the count below is vacuous.
    let out_files: Vec<&str> = data
        .as_array()
        .expect("flock data is an array")
        .iter()
        .filter(|row| row["name"] == "merged")
        .map(|row| {
            row["out_file"]
                .as_str()
                .unwrap_or_else(|| panic!("a running sheep reports its out file: {data}"))
        })
        .collect();
    assert_eq!(out_files.len(), 2, "{data}");
    assert_eq!(
        out_files[0], out_files[1],
        "merge_logs points both instances at one file: {data}"
    );

    // Then that the shared file is read once, not once per matched row.
    let backlog =
        bleats_no_follow_until_contains(home, &["merged"], &["merged-slot-0", "merged-slot-1"]);
    assert_eq!(
        backlog.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&backlog.stderr)
    );
    let stdout = String::from_utf8_lossy(&backlog.stdout);
    for slot in 0..2 {
        let needle = format!("merged-slot-{slot}");
        assert_eq!(
            stdout.matches(&needle).count(),
            1,
            "a shared log file is read once, not once per instance: {stdout}"
        );
    }

    graceful_kill(home);
}

// --- Daemon handover -----------------------------------------------------

/// Writes a script that counts from 1 upwards on stdout, one number per line,
/// forever.
///
/// The sequence is what makes a log gap visible: a counting sheep proves
/// nothing between it and the file was lost, reordered or cut in half. A
/// restarted sheep starts again at 1.
#[cfg(unix)]
fn write_counting_script(dir: &TempDir) -> PathBuf {
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
fn counting_lines(path: &Path, want: usize) -> Vec<String> {
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
fn assert_unbroken_sequence(lines: &[String], what: &str) {
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

/// A script that says which slot it is and which process it is, on every line.
/// `$SHEP_INSTANCE` is injected at the spawn and fixed for the life of the
/// process, so a line naming a slot is the child's claim rather than the
/// shepherd's, which is what makes a slot swap visible.
#[cfg(unix)]
fn write_slot_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "slot.sh",
        &format!(
            "{}{}while :; do\n  echo \"slot=$SHEP_INSTANCE pid=$$\"\n  sleep 0.2\ndone\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// Every line of `path` that names a slot, as `(slot, pid)` pairs. Panics on a
/// line it cannot parse: a torn line is the failure these cases look for, and
/// dropping it would turn a lost write into a shorter list.
#[cfg(unix)]
fn slot_lines(path: &Path) -> Vec<(u32, u32)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(shep_core::logstamp::strip)
        .map(|line| {
            let (slot, pid) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("a torn line in {}: {line:?}", path.display()));
            let parse = |field: &str, prefix: &str| {
                field
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.parse::<u32>().ok())
                    .unwrap_or_else(|| panic!("a torn line in {}: {line:?}", path.display()))
            };
            (parse(slot, "slot="), parse(pid, "pid="))
        })
        .collect()
}

/// Waits until `path` holds at least `want` slot lines, or
/// [`HANDOVER_DEADLINE`] expires, and returns what it held on the last read.
#[cfg(unix)]
fn poll_slot_lines(path: &Path, want: usize) -> Vec<(u32, u32)> {
    let start = Instant::now();
    loop {
        let lines = slot_lines(path);
        if lines.len() >= want || start.elapsed() >= HANDOVER_DEADLINE {
            return lines;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// Polls `path` until the lines written after the first `before` of them
/// satisfy `ready`, or the handover deadline passes.
///
/// A line count is the wrong wait for a `merge_logs` app: both instances write
/// to one file, so "two more lines" is satisfied by either of them writing
/// twice. The caller therefore says what it is waiting for.
#[cfg(unix)]
fn poll_fresh_lines(
    path: &Path,
    before: usize,
    ready: impl Fn(&[(u32, u32)]) -> bool,
) -> Vec<(u32, u32)> {
    let start = Instant::now();
    loop {
        let lines = slot_lines(path);
        if lines.len() > before && ready(&lines[before..]) {
            return lines;
        }
        if start.elapsed() >= HANDOVER_DEADLINE {
            return lines;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// A clustered app is carried, and every instance comes back in its own slot.
///
/// Two apps, because `merge_logs` points every instance at one log file and
/// `handover::adopt::refuse_repeated_fds` refuses the entire blob when a
/// descriptor number appears twice. A slot swap leaves two live processes
/// adopted under each other's names, so each row's `out_file` is read back and
/// its lines have to agree with the row about slot and pid.
#[cfg(unix)]
#[test]
fn a_clustered_flock_keeps_every_pid_and_every_slot_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_slot_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"split\"\nscript = '{}'\ninstances = 2\n\n\
             [[app]]\nname = \"merged\"\nscript = '{}'\ninstances = 2\nmerge_logs = true\n",
            script.display(),
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    let before = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 4
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_before = rows_by_slot(&before);
    assert_eq!(
        rows_before.len(),
        4,
        "two apps at two instances each: {before}"
    );
    // The fixture check: `merge_logs` collapsing both instances onto one path
    // is the premise of half this case.
    assert_eq!(
        rows_before[&("merged".to_owned(), 0)].1,
        rows_before[&("merged".to_owned(), 1)].1,
        "merge_logs must really point both instances at one file"
    );
    assert_ne!(
        rows_before[&("split".to_owned(), 0)].1,
        rows_before[&("split".to_owned(), 1)].1,
        "without merge_logs each instance must have its own file"
    );
    for ((name, slot), (_, out_file)) in &rows_before {
        assert!(
            !poll_slot_lines(out_file, 1).is_empty(),
            "{name}:{slot} must be logging before the reload"
        );
    }
    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_success(&reloaded);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&reloaded.stdout),
        String::from_utf8_lossy(&reloaded.stderr)
    );
    // The exact sentence `handover::RefusedReason`'s `Display` ends with. A
    // looser probe would pass whether or not the reload was refused.
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "a clustered flock is carried now, not refused: {text}"
    );

    let after = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 4
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_after = rows_by_slot(&after);
    assert_eq!(
        rows_after, rows_before,
        "every instance keeps its pid and its own log file: {after}"
    );

    // The mark is taken after the reload returned, not before it was issued. A
    // pid is carried across a handover, so an early mark lets a pre-reload line
    // satisfy "this instance wrote again".
    let counts_before: HashMap<(String, u32), usize> = rows_after
        .iter()
        .map(|(key, (_, out_file))| (key.clone(), slot_lines(out_file).len()))
        .collect();

    // The slot assertion, the one a pid check cannot make. Each row is asked
    // for its own file, and every line written into that file after the
    // reload has to name that row's slot and that row's pid.
    for ((name, slot), (pid, out_file)) in &rows_after {
        let before = counts_before[&(name.clone(), *slot)];
        // A split app's file holds nobody else, so two more lines in it are
        // two more lines from this row. A merged app's file holds both
        // instances, so the wait is this row's own pid turning up.
        let lines = if *name == "merged" {
            poll_fresh_lines(out_file, before, |fresh| {
                fresh.iter().any(|(_, line_pid)| line_pid == pid)
            })
        } else {
            poll_fresh_lines(out_file, before, |fresh| fresh.len() >= 2)
        };
        let fresh = lines.get(before..).unwrap_or(&[]);
        assert!(
            !fresh.is_empty(),
            "{name}:{slot} stopped logging across the handover: {} lines, none of them new",
            lines.len()
        );
        if *name == "merged" {
            // One file for both slots, so the row's own lines are the ones
            // carrying its pid. Both must be present, or a handle was lost.
            assert!(
                fresh.iter().any(|(_, line_pid)| line_pid == pid),
                "merged:{slot} wrote nothing after the reload: {fresh:?}"
            );
            for (line_slot, line_pid) in fresh {
                assert_eq!(
                    rows_after[&("merged".to_owned(), *line_slot)].0,
                    *line_pid,
                    "a merged line's slot and pid disagree with the flock: {fresh:?}"
                );
            }
        } else {
            assert!(
                fresh.len() >= 2,
                "{name}:{slot} stopped logging across the handover: {} fresh lines, wanted 2",
                fresh.len()
            );
            for (line_slot, line_pid) in fresh {
                assert_eq!(
                    (*line_slot, *line_pid),
                    (*slot, *pid),
                    "{name}:{slot}'s own log holds another instance's output: {fresh:?}"
                );
            }
        }
    }

    graceful_kill(dir.path());
}

/// `shep flock`'s JSON rows as `(name, instance) -> (pid, out_file)`. Keyed on
/// the pair, since a name matches as many rows as the app has instances.
#[cfg(unix)]
fn rows_by_slot(data: &serde_json::Value) -> BTreeMap<(String, u32), (u32, PathBuf)> {
    data.as_array()
        .unwrap_or_else(|| panic!("flock data is an array: {data}"))
        .iter()
        .map(|row| {
            let name = row["name"]
                .as_str()
                .unwrap_or_else(|| panic!("a row names its app: {row}"))
                .to_owned();
            let instance = u32::try_from(
                row["instance"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("a row names its slot: {row}")),
            )
            .unwrap();
            let pid = u32::try_from(
                row["pid"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("an online row names its pid: {row}")),
            )
            .unwrap();
            let out_file = PathBuf::from(
                row["out_file"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a row names its out file: {row}")),
            );
            ((name, instance), (pid, out_file))
        })
        .collect()
}

/// A `/bin/sh` sheep that signals readiness on fd 3 and answers every shepherd
/// message with the same reply. The `ready` line is what `wait_ready` holds the
/// sheep at `starting` for, the loop is what `shep trigger` gets an answer
/// from, and `read -r line <&3` is a plain blocking read, which a channel that
/// came back non-blocking would break. The reply names the action verbatim:
/// `ActionWaits` correlates on the action name when the app echoes no id.
#[cfg(unix)]
fn write_channel_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "chatty.sh",
        &format!(
            "{}{}printf '{{\"kind\":\"ready\"}}\\n' >&3\nwhile read -r line <&3; do\n  \
             printf '{{\"kind\":\"action-reply\",\"action\":\"ping\",\"body\":\"pong\"}}\\n' \
             >&3\ndone\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// Runs `shep trigger chatty ping` and returns the one row's outcome. Asked
/// identically either side of the reload, and the point is that the two
/// answers are the same.
#[cfg(unix)]
fn trigger_ping(home: &Path) -> serde_json::Value {
    let triggered = shep(home)
        .arg("--format")
        .arg("json")
        .arg("trigger")
        .arg("chatty")
        .arg("ping")
        .output()
        .unwrap();
    assert_success(&triggered);
    let envelope: serde_json::Value = serde_json::from_slice(&triggered.stdout)
        .unwrap_or_else(|e| panic!("trigger stdout was not JSON: {e}"));
    envelope["data"][0]["outcome"].clone()
}

/// A sheep's shepherd channel survives `shep daemon reload`, in both
/// directions and against a real app. A socketpair can survive as a number
/// attached to the wrong end, or be adopted with only one of its two pumps
/// rebuilt, and both leave the flock healthy and the pid unmoved. `wait_ready`
/// is on as well as `channel`, so `online` before the reload is itself proof
/// the child's `{"kind":"ready"}` came up the channel.
#[cfg(unix)]
#[test]
fn a_channel_sheep_still_answers_a_trigger_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_channel_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"chatty\"\nscript = '{}'\nchannel = true\nwait_ready = true\n",
            script.display()
        ),
    );
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    // `online` rather than `starting`, which only the child's own readiness
    // line over fd 3 can produce.
    let before = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    let pid_before = before["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("an online sheep reports a pid: {before}"));
    let answered = trigger_ping(dir.path());
    assert_eq!(
        answered["kind"], "replied",
        "the channel must work before the reload, or this case proves nothing: {answered}"
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

    let still = trigger_ping(dir.path());
    assert_eq!(
        still["kind"], "replied",
        "the successor must reach the same fd 3 the child has had all along: {still}"
    );
    assert_eq!(still["body"], "pong", "{still}");

    graceful_kill(dir.path());
}

/// Rows [`a_flock_of_every_carried_kind_survives_a_daemon_reload`] expects:
/// four single-instance apps and two clustered ones at two instances each.
#[cfg(unix)]
const ROWS_IN_THE_MIXED_FLOCK: usize = 8;

/// Writes one line to `sheep`'s stdin and asserts shep accepted it. `sent`
/// says the bytes reached the pipe, never that the app read them, so the
/// caller still has to look in the sheep's own log for the echo.
#[cfg(unix)]
fn whisper(home: &Path, sheep: &str, line: &str) {
    let sent = shep(home)
        .arg("whisper")
        .arg(sheep)
        .arg(line)
        .output()
        .unwrap();
    assert_success(&sent);
}

/// Reads `path` until it holds a line equal to `want`, or
/// [`HANDOVER_DEADLINE`] expires. Equality rather than `contains`, so a prefix
/// of a longer line cannot answer for the line itself. Returns rather than
/// panicking, so the failure is the caller's own assertion.
#[cfg(unix)]
fn await_log_line(path: &Path, want: &str) -> bool {
    let start = Instant::now();
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text
            .lines()
            .any(|line| shep_core::logstamp::strip(line) == want)
        {
            return true;
        }
        if start.elapsed() >= HANDOVER_DEADLINE {
            return false;
        }
        std::thread::sleep(HANDOVER_POLL_INTERVAL);
    }
}

/// A `/bin/sh` sheep that waits for `gate` to appear, deletes it, and then
/// grows its resident set past [`BALLOON_BYTES`]. Growing only after the exec
/// makes the breach attributable to the successor's arming, and deleting the
/// gate on the way past makes it happen exactly once.
#[cfg(unix)]
fn write_gated_ballooning_script(dir: &TempDir, name: &str, gate: &Path) -> PathBuf {
    write_script(
        dir,
        name,
        &format!(
            "{header}{pid}while [ ! -f '{gate}' ]; do\n  sleep 0.1\ndone\nrm -f '{gate}'\n\
             s=x\nwhile [ ${{#s}} -lt {BALLOON_BYTES} ]; do s=\"$s$s\"; done\n{sleep}",
            header = script_header(),
            pid = record_pid_line(dir),
            gate = gate.display(),
            sleep = sleep_line(SLOW_SCRIPT_SLEEP_SECS),
        ),
    )
}

/// Every lifecycle extra is armed again by the successor, proved by behaviour
/// rather than by a handle existing. [`ExtrasRegistry::arm`] fans out to five
/// mechanisms across two scopes: sampling, the memory limit and the liveness
/// loop per instance, the cron worker and the filesystem watch per name.
///
/// Every trigger fires after the exec, and that ordering is the case: a watch
/// armed by the predecessor is indistinguishable from one armed by the
/// successor if the file is written before the reload. `control` configures no
/// extra at all, so its `restarts` of 0 says nothing here restarts sheep.
#[cfg(unix)]
#[test]
fn every_lifecycle_extra_is_re_armed_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // Its own tempdir, never `$SHEP_HOME`: every fixture script appends its
    // pid to `<home>/`[`FIXTURE_PIDS`] on each spawn, so a watch rooted at
    // the home would restart on its own sheep's restart, forever.
    let watched = tempfile::tempdir().unwrap();
    let greedy_gate = dir.path().join("greedy.gate");
    let control_gate = dir.path().join("control.gate");
    // A file the probe requires, so the test trips it by deleting and heals it
    // by writing. The opposite polarity races the fixture: a script clearing
    // its own trigger on the way up can do so after `shep flock` already called
    // the sheep `online`.
    let healthy = dir.path().join("probe.healthy");
    std::fs::write(&healthy, "ok").unwrap();
    let sleeper = write_slow_script(&dir);
    let greedy = write_gated_ballooning_script(&dir, "greedy.sh", &greedy_gate);
    let control = write_gated_ballooning_script(&dir, "control.sh", &control_gate);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"watched\"\nscript = '{sleeper}'\ncwd = '{root}'\nwatch = true\n\n\
             [[app]]\nname = \"scheduled\"\nscript = '{sleeper}'\ncron_restart = \"* * * * *\"\n\n\
             [[app]]\nname = \"greedy\"\nscript = '{greedy}'\nmax_memory = \"{BREACH_LIMIT}\"\n\n\
             [[app]]\nname = \"probed\"\nscript = '{sleeper}'\n\
             liveness_probe = {{ kind = \"exec\", target = \"test -f {healthy}\", \
             interval = \"1s\", timeout = \"2s\", failure_threshold = 2 }}\n\n\
             [[app]]\nname = \"control\"\nscript = '{control}'\n",
            sleeper = sleeper.display(),
            root = watched.path().display(),
            greedy = greedy.display(),
            control = control.display(),
            healthy = healthy.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 5
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let pids_before: BTreeMap<String, u64> = restart_counts(&before)
        .keys()
        .map(|name| {
            (
                name.clone(),
                sheep_named(&before, name)["pid"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        restart_counts(&before),
        BTreeMap::from([
            ("control".to_owned(), 0),
            ("greedy".to_owned(), 0),
            ("probed".to_owned(), 0),
            ("scheduled".to_owned(), 0),
            ("watched".to_owned(), 0),
        ]),
        "precondition: nothing has restarted yet: {before}"
    );

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);
    let after = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.len() == 5 && rows.iter().all(|row| row["status"] == "online"))
    });
    // The handover really happened, which every assertion below depends on:
    // a stop-and-start would re-arm everything from a fresh spawn and prove
    // nothing about `install_adopted`.
    for (name, pid) in &pids_before {
        assert_eq!(
            sheep_named(&after, name)["pid"].as_u64(),
            Some(*pid),
            "{name} was respawned, so this reload took the stop arm: {after}"
        );
    }
    // The sampling arm, the cheapest of the five to lose silently. A successor
    // that never called `stats.watch` leaves this null for the life of the
    // daemon while every other column looks right.
    for name in pids_before.keys() {
        assert!(
            !sheep_named(&after, name)["memory_bytes"].is_null(),
            "{name} is no longer sampled after the handover: {after}"
        );
    }

    // Every trigger, fired only now.
    std::fs::write(watched.path().join("app.txt"), "changed").unwrap();
    std::fs::write(&greedy_gate, "go").unwrap();
    std::fs::write(&control_gate, "go").unwrap();
    std::fs::remove_file(&healthy).unwrap();
    // One wait for the three fast arms. The enforcer's ticks are phased off
    // daemon boot rather than off the breach, so the memory limit's worst case
    // is a whole `MEMORY_POLL_INTERVAL` after the resident set moves.
    let fired = poll_flock_data(home, BREACH_DEADLINE, |data| {
        ["watched", "probed", "greedy"]
            .iter()
            .all(|name| sheep_named(data, name)["restarts"].as_u64().unwrap_or(0) >= 1)
    });
    assert!(
        sheep_named(&fired, "watched")["restarts"].as_u64().unwrap() >= 1,
        "a write under the watched tree after the exec must restart the sheep: {fired}"
    );
    assert!(
        sheep_named(&fired, "probed")["restarts"].as_u64().unwrap() >= 1,
        "a liveness probe failing after the exec must restart the sheep: {fired}"
    );
    assert!(
        sheep_named(&fired, "greedy")["restarts"].as_u64().unwrap() >= 1,
        "a resident set crossing max_memory after the exec must restart the sheep: {fired}"
    );
    // Healed now that the restart is on the books, so the probe stops
    // failing and `probed` cannot spend its `max_restarts` while the case
    // waits out the cron minute below.
    std::fs::write(&healthy, "ok").unwrap();

    // The cron worker, and the slow one. A `* * * * *` pattern armed at an
    // arbitrary moment lands uniformly in the minute, so the bound is a minute
    // plus the restart's round trip.
    let cronned = poll_flock_data(home, CRON_DEADLINE, |data| {
        sheep_named(data, "scheduled")["restarts"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    });
    assert!(
        sheep_named(&cronned, "scheduled")["restarts"]
            .as_u64()
            .unwrap()
            >= 1,
        "a cron occurrence after the exec must restart the sheep: {cronned}"
    );
    assert_eq!(
        sheep_named(&cronned, "control")["restarts"].as_u64(),
        Some(0),
        "the control ballooned through the same gate and configures no extra at all; \
         a restart it shares is this case restarting sheep rather than an extra firing: \
         {cronned}"
    );

    // The daemon's own log, the only place the observed resident set and the
    // ceiling it crossed are stated. Read rather than polled:
    // `spawn_extras_reporter` writes the record before it asks for the restart.
    let daemon_log = std::fs::read_to_string(home.join("logs").join("shepd.err.log")).unwrap();
    assert!(
        daemon_log.contains("exceeded its max_memory"),
        "the successor's own log must say why the sheep was restarted: {daemon_log:?}"
    );

    graceful_kill(home);
}

/// `restarts` per sheep name, for the two observations this case compares.
#[cfg(unix)]
fn restart_counts(data: &serde_json::Value) -> BTreeMap<String, u64> {
    data.as_array()
        .unwrap_or_else(|| panic!("flock data is an array: {data}"))
        .iter()
        .map(|row| {
            (
                row["name"].as_str().unwrap().to_owned(),
                row["restarts"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

/// A sheep already owed a respawn when the shepherd is replaced still gets it.
///
/// `Actor::schedule_restart` spawns a task that sleeps and then sends
/// `Msg::RestartDue`, and that task dies with the process image, while
/// `handle_restart_due` is the only thing that moves a sheep off
/// `WaitingRestart`. The precondition is asserted before and immediately after
/// the reload: if the delay elapsed during `daemon reload`, the predecessor
/// respawned the sheep and the final assertion proves nothing. `steady` never
/// exits, so a moved pid means the stop arm ran.
#[cfg(unix)]
#[test]
fn a_sheep_owed_a_restart_still_gets_one_after_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let steady = write_slow_script(&dir);
    let flapper = write_script(
        &dir,
        "flapper.sh",
        &format!("{}{}exit 1\n", script_header(), record_pid_line(&dir)),
    );
    let flockfile = write_flockfile(
        &dir,
        &format!(
            // Long enough that a loaded runner cannot let the wait expire
            // between the observation below and the reload after it, and short
            // enough that the case then waits it out once.
            "[[app]]\nname = \"flapper\"\nscript = '{flapper}'\nrestart_delay = \"8s\"\n\n\
             [[app]]\nname = \"steady\"\nscript = '{steady}'\n",
            flapper = flapper.display(),
            steady = steady.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        sheep_named(data, "flapper")["status"] == "waiting-restart"
            && sheep_named(data, "steady")["status"] == "online"
    });
    assert_eq!(
        sheep_named(&before, "flapper")["status"],
        "waiting-restart",
        "precondition: the sheep must be owed a respawn when the shepherd is replaced: {before}"
    );
    let steady_pid = sheep_named(&before, "steady")["pid"].as_u64().unwrap();

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&reloaded.stdout),
        String::from_utf8_lossy(&reloaded.stderr)
    );
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "a sheep in its restart backoff is carried, not refused: {text}"
    );

    let carried = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        !sheep_named(data, "steady")["pid"].is_null()
    });
    assert_eq!(
        sheep_named(&carried, "steady")["pid"].as_u64(),
        Some(steady_pid),
        "a moved pid means the stop arm ran, which restarts everything: {carried}"
    );
    assert_eq!(
        sheep_named(&carried, "flapper")["status"],
        "waiting-restart",
        "the wait must still have been pending at the exec, or this case proves nothing: \
         {carried}"
    );
    assert_eq!(
        sheep_named(&carried, "flapper")["restarts"].as_u64(),
        Some(0),
        "ditto: the predecessor must not have respawned it first: {carried}"
    );

    let restarted = poll_flock_data(home, RESTARTED_DEADLINE, |data| {
        sheep_named(data, "flapper")["restarts"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    });
    assert!(
        sheep_named(&restarted, "flapper")["restarts"]
            .as_u64()
            .unwrap()
            >= 1,
        "the sheep was left waiting for a timer that died with the exec: {restarted}"
    );

    graceful_kill(home);
}

/// A `/bin/sh` sheep that echoes every line it is whispered, prefixed.
/// `stdin = true` is the only thing that gives a sheep a readable fd 0, and the
/// echo comes back in the sheep's own log, so a pipe that survived as a number
/// attached to the wrong end delivers nothing. No trailing `sleep`: the `read`
/// parks the script for as long as the daemon holds the write end open.
#[cfg(unix)]
fn write_echoing_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "echoer.sh",
        &format!(
            "{}{}while read -r line; do\n  echo \"heard $line\"\n done\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// A `/bin/sh` sheep that writes down whatever the shepherd tells it and then
/// exits cleanly.
///
/// For `shutdown_with_message`: the daemon writes down the socket on the stop
/// path, with no reply to correlate. The line in the log is the evidence, and
/// the clean `exit 0` beside it says the message arrived rather than the kill
/// ladder.
#[cfg(unix)]
fn write_farewell_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "bye.sh",
        &format!(
            "{}{}while read -r line <&3; do\n  echo \"told $line\"\n  exit 0\ndone\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// Every kind of sheep, in one flock, across one reload.
///
/// One flock rather than five: `handover::adopt::refuse_repeated_fds` refuses
/// the entire blob over one repeated number, and a mixed flock is the only
/// place six kinds of descriptor (two log files, two pipe read ends, a stdin
/// pipe and a socketpair, times eight sheep) are numbered together. Every
/// assertion below is one a pid check cannot make.
#[cfg(unix)]
#[test]
fn a_flock_of_every_carried_kind_survives_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let counter = write_counting_script(&dir);
    let slot = write_slot_script(&dir);
    let chatty = write_channel_script(&dir);
    let echoer = write_echoing_script(&dir);
    let bye = write_farewell_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"counter\"\nscript = '{counter}'\n\n\
             [[app]]\nname = \"echoer\"\nscript = '{echoer}'\nstdin = true\n\n\
             [[app]]\nname = \"chatty\"\nscript = '{chatty}'\nchannel = true\nwait_ready = true\n\n\
             [[app]]\nname = \"bye\"\nscript = '{bye}'\nshutdown_with_message = true\n\n\
             [[app]]\nname = \"split\"\nscript = '{slot}'\ninstances = 2\n\n\
             [[app]]\nname = \"merged\"\nscript = '{slot}'\ninstances = 2\nmerge_logs = true\n",
            counter = counter.display(),
            echoer = echoer.display(),
            chatty = chatty.display(),
            bye = bye.display(),
            slot = slot.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    // `online` for all seven, which for `chatty` is already an assertion:
    // `wait_ready` holds it at `starting` until the child writes up fd 3.
    let before = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == ROWS_IN_THE_MIXED_FLOCK
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_before = rows_by_slot(&before);
    assert_eq!(
        rows_before.len(),
        ROWS_IN_THE_MIXED_FLOCK,
        "six apps, two of them clustered: {before}"
    );

    // Every feature is exercised before the reload too, so a whisper or a
    // trigger that never worked at all does not read as a handover defect.
    let out_file = |name: &str| rows_before[&(name.to_owned(), 0)].1.clone();
    let counter_log = out_file("counter");
    let echoer_log = out_file("echoer");
    let bye_log = out_file("bye");
    let seen_before = counting_lines(&counter_log, 3);
    assert!(
        seen_before.len() >= 3,
        "the counter must be logging before the reload: {seen_before:?}"
    );
    whisper(dir.path(), "echoer", "before");
    assert!(
        await_log_line(&echoer_log, "heard before"),
        "the whisper must reach the sheep before the reload, or this case proves nothing: {}",
        std::fs::read_to_string(&echoer_log).unwrap_or_default()
    );
    let answered = trigger_ping(dir.path());
    assert_eq!(
        answered["kind"], "replied",
        "the channel must work before the reload: {answered}"
    );
    let counts_before: HashMap<(String, u32), usize> = rows_before
        .iter()
        .filter(|((name, _), _)| name == "split" || name == "merged")
        .map(|(key, (_, out_file))| (key.clone(), slot_lines(out_file).len()))
        .collect();

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_success(&reloaded);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&reloaded.stdout),
        String::from_utf8_lossy(&reloaded.stderr)
    );
    // The exact sentence `handover::RefusedReason`'s `Display` ends with;
    // without it the case would pass on a stop-and-start.
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "every kind in this flock is carried now, not refused: {text}"
    );

    let after = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == ROWS_IN_THE_MIXED_FLOCK
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_after = rows_by_slot(&after);
    assert_eq!(
        rows_after, rows_before,
        "every sheep keeps its pid and its own log file: {after}"
    );

    // The log plane, on a plain sheep.
    let seen = counting_lines(&counter_log, seen_before.len() + 3);
    assert!(
        seen.len() > seen_before.len(),
        "the counter stopped logging across the handover: {seen:?}"
    );
    assert_unbroken_sequence(&seen, "the counter's log across a handover");

    // stdin, which nothing else in this file covers. A fresh line, so a
    // stale `heard before` in the file cannot answer for it.
    whisper(dir.path(), "echoer", "after");
    assert!(
        await_log_line(&echoer_log, "heard after"),
        "the carried stdin pipe must still reach the same child: {}",
        std::fs::read_to_string(&echoer_log).unwrap_or_default()
    );

    // The channel, both directions, against the child that has had fd 3
    // since before the exec.
    let still = trigger_ping(dir.path());
    assert_eq!(
        still["kind"], "replied",
        "the successor must reach the same fd 3 the child has had all along: {still}"
    );
    assert_eq!(still["body"], "pong", "{still}");

    // The clustered halves. Each row is asked for its own file, and every
    // line written into it after the reload has to agree with the row.
    for ((name, slot), (pid, out_file)) in &rows_after {
        if name != "split" && name != "merged" {
            continue;
        }
        let before = counts_before[&(name.clone(), *slot)];
        let want = before + 2;
        let lines = poll_slot_lines(out_file, want);
        assert!(
            lines.len() >= want,
            "{name}:{slot} stopped logging across the handover: {} lines, wanted {want}",
            lines.len()
        );
        for (line_slot, line_pid) in &lines[before..] {
            assert_eq!(
                rows_after[&(name.clone(), *line_slot)].0,
                *line_pid,
                "a {name} line's slot and pid disagree with the flock: {lines:?}"
            );
            if name == "split" {
                assert_eq!(
                    (*line_slot, *line_pid),
                    (*slot, *pid),
                    "{name}:{slot}'s own log holds another instance's output: {lines:?}"
                );
            }
        }
    }

    // `shutdown_with_message`, last because it ends its sheep. The message
    // goes down the carried socket, the child writes it to its own log and
    // exits 0.
    let stopped = shep(dir.path()).arg("stop").arg("bye").output().unwrap();
    assert_success(&stopped);
    // The row rides along in the message: a `bye` killed by the stop ladder
    // never got the message, and a `bye` that got it and had the line dropped
    // on the way to the file (what `tokio_runner`'s `FINAL_DRAIN` guards) both
    // leave the log empty.
    assert!(
        await_log_line(&bye_log, "told {\"kind\":\"shutdown\"}"),
        "the stop message must reach the child down the carried channel. \
         The log holds {:?}; the flock reads {}",
        std::fs::read_to_string(&bye_log).unwrap_or_default(),
        poll_flock_data(dir.path(), Duration::ZERO, |_| true),
    );

    graceful_kill(dir.path());
}

/// Gap between the dials [`the_control_socket_accepts_throughout_a_handover`]
/// makes at the control address. A dial is a `connect(2)` and a close, so
/// this decides how narrow an outage the case can see, not what it costs:
/// every real way the address goes away spans a daemon teardown or a fresh
/// bind, hundreds of milliseconds.
#[cfg(unix)]
const DIAL_INTERVAL: Duration = Duration::from_millis(5);

/// `ExitCode::DaemonUnreachable`, the one failing exit `shep ping` has,
/// whatever the reason. Any other failing exit is a usage error or a
/// refusal, which no handover produces, and the prober refuses it rather
/// than counting it as the one drop the exec is allowed.
#[cfg(unix)]
const PING_OFFLINE: i32 = 5;

/// The control socket answers throughout a handover.
///
/// The successor inherits the listening descriptor rather than binding the
/// address again, so a client that connects mid-replacement waits in the
/// kernel's backlog. Nothing may be refused, and the socket file may never
/// disappear: a rebind would race the predecessor's socket file.
///
/// Two probers and one file check. A `connect(2)` dialer sees an outage at
/// least `DIAL_INTERVAL` wide; the socket file's inode sees a rebind, which
/// is too brief for any poller; a ping loop sees a request still served.
/// Ping failures are counted, not read: `shep ping` prints nothing on
/// stderr and exits `DaemonUnreachable` for every reason it has.
#[cfg(unix)]
#[test]
fn the_control_socket_accepts_throughout_a_handover() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("sheep")
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);
    let _ = poll_flock(dir.path(), |info| info["status"] == "online");

    // One deadline for both threads, so the two answers describe the same
    // window.
    let deadline = Instant::now() + Duration::from_secs(8);
    let socket = dir.path().join("run").join("shep.sock");
    // The file's identity before anything happens to it. A successor that
    // binds fresh instead of adopting must unlink and recreate the file,
    // which changes the inode; that takes a microsecond no poller can see.
    let inode_before = std::fs::metadata(&socket)
        .expect("the control socket must exist before the handover")
        .ino();
    let dial_socket = socket.clone();
    let dialer = std::thread::spawn(move || {
        let mut refused = Vec::new();
        let mut dials = 0_usize;
        while Instant::now() < deadline {
            dials += 1;
            // Dropped where the `if let` ends: the daemon's accept loop meets
            // an EOF and logs it at `debug!`. The answer wanted is the
            // syscall's; anything more would be the bucket this case fixes.
            if let Err(err) = std::os::unix::net::UnixStream::connect(&dial_socket) {
                refused.push(format!("dial {dials}: {:?}: {err}", err.kind()));
            }
            std::thread::sleep(DIAL_INTERVAL);
        }
        (refused, dials)
    });

    let home = dir.path().to_path_buf();
    // The prober says when it is really probing, and the reload waits for
    // that. Without the handshake the reload could finish before the first
    // `ping` ran, and every probe would be served by the successor alone.
    let (probing, started_probing) = std::sync::mpsc::channel();
    let prober = std::thread::spawn(move || {
        let mut before_reload = Vec::new();
        let mut dropped = Vec::new();
        let mut pings = 0_usize;
        let mut announced = false;
        while Instant::now() < deadline {
            pings += 1;
            let out = shep(&home).arg("ping").output().unwrap();
            let served = out.status.success();
            if !served {
                // The failure has to be the one shape a handover can cause.
                // Its reason cannot be read (see the case's doc), but its
                // exit can: anything but `DaemonUnreachable` is a different
                // defect wearing the tolerated drop's clothes.
                assert_eq!(
                    out.status.code(),
                    Some(PING_OFFLINE),
                    "ping {pings} failed for a reason no handover produces: {}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            if !announced {
                // The reload waits for a ping the predecessor ANSWERED. A
                // failure before that has no exec to blame, since none has
                // been asked for, and it must not spend the one drop the
                // exec is allowed: it is kept apart and refused below.
                if served {
                    announced = true;
                    let _ = probing.send(());
                } else {
                    before_reload.push(pings);
                }
                continue;
            }
            if !served {
                dropped.push(pings);
            }
        }
        (before_reload, dropped, pings)
    });
    started_probing
        .recv_timeout(FLOCK_DEADLINE)
        .expect("the prober must reach the shepherd before the reload starts");

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_success(&reloaded);
    // The premise, checked. A reload that fell back to stopping and starting
    // really did unbind the address, and the dialer would report that as
    // the defect. Both fallback arms say so on stderr (`commands/daemon.rs`'s
    // two `aside("reload", ...)` calls).
    let reload_aside = String::from_utf8_lossy(&reloaded.stderr);
    assert!(
        !reload_aside.contains("starting one instead")
            && !reload_aside.contains("stopping and starting instead"),
        "this case is about the handover arm and the reload took the other one: {reload_aside}"
    );
    // Same file, same inode: the successor adopted the carried listener. A
    // rebind at the same path passed the dialer 10 of 10; the inode is the
    // deterministic reading of the same property.
    let inode_after = std::fs::metadata(&socket)
        .expect("the control socket must still exist after the handover")
        .ino();
    assert_eq!(
        inode_after, inode_before,
        "the successor bound a fresh listener instead of adopting the carried one: \
         the socket file's inode changed across the handover"
    );

    // The listener's descriptor is carried, so no client ever finds the
    // address unbound; an accepted connection is not, so the one reply in
    // flight at the exec may fail. One at most: the prober is sequential.
    let (refused, dials) = dialer.join().unwrap();
    let (before_reload, dropped, pings) = prober.join().unwrap();
    assert!(
        before_reload.is_empty(),
        "a ping failed before any reload was asked for, at {before_reload:?} of \
         {pings}: the predecessor was not answering, which is not the handover's \
         doing"
    );
    assert!(
        refused.is_empty(),
        "the control address must stay bound across the handover, \
         {} of {dials} dials refused: {refused:?}",
        refused.len()
    );
    assert!(
        dropped.len() <= 1,
        "at most the one request in flight at the exec may drop, got {} of {pings} \
         pings, at {dropped:?}",
        dropped.len()
    );

    graceful_kill(dir.path());
}

/// Reads `roll` until it records exactly `want` apps, or [`FLOCK_DEADLINE`]
/// expires, and returns the bytes it held on the last read.
///
/// The muster roll is written by a debounced task, so "the flock changed" and
/// "the roll on disk says so" are two events, and callers here need the second.
#[cfg(unix)]
fn roll_recording(roll: &Path, want: usize) -> Vec<u8> {
    let start = Instant::now();
    loop {
        let bytes = std::fs::read(roll).unwrap_or_default();
        let apps = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value["apps"].as_array().map(Vec::len));
        if apps == Some(want) || start.elapsed() >= FLOCK_DEADLINE {
            return bytes;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// A successor that inherited an empty flock must not fall back to the roll.
/// A boot either installs the flock it was handed or restores the roll, and
/// what decides is whether it was handed a flock at all, not how large.
///
/// SIGHUP directly, not `shep daemon reload`, which would start `ghost`
/// through `shep muster` whatever the boot decided. A failed exec leaves no
/// shepherd, so the poll fails on its own `assert_success` and the pid check
/// says a successor answered. The wait is for a sheep to appear, so asserting
/// none did cannot pass by looking too early; it goes through
/// [`poll_flock_data_across_a_handover`] since the raw signal can drop one reply.
#[cfg(unix)]
#[test]
fn a_successor_inheriting_an_empty_flock_does_not_restore_the_roll() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("ghost")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);
    let _ = poll_flock(home, |info| info["status"] == "online");
    let shepherd = wait_for_daemon_pid(home).expect("the shepherd must record a pid");

    assert_success(&shep(home).arg("save").output().unwrap());
    let roll = home.join("flock.json");
    let stale = roll_recording(&roll, 1);
    assert!(
        !stale.is_empty(),
        "the roll must record `ghost` before it can go stale: {}",
        String::from_utf8_lossy(&stale)
    );

    assert_success(&shep(home).arg("delete").arg("ghost").output().unwrap());
    let emptied = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(Vec::is_empty)
    });
    assert_eq!(
        emptied.as_array().map(Vec::len),
        Some(0),
        "the delete must leave an idle shepherd: {emptied}"
    );
    // Waited out rather than assumed: the debounced writer is about to record
    // the empty flock, and a stale roll put back before that write lands would
    // be overwritten by it.
    let _ = roll_recording(&roll, 0);
    std::fs::write(&roll, &stale).unwrap();

    nix::sys::signal::kill(shepherd, nix::sys::signal::Signal::SIGHUP).unwrap();

    let after = poll_flock_data_across_a_handover(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| !rows.is_empty())
    });
    assert_eq!(
        after.as_array().map(Vec::len),
        Some(0),
        "a successor must install the flock it was handed and nothing else; \
         this one restored a stale roll: {after}"
    );
    assert_eq!(
        wait_for_daemon_pid(home),
        Some(shepherd),
        "the shepherd must have been replaced in place; a moved pid means \
         SIGHUP stopped it and the polling above started a fresh one, which \
         is not the boot this test is about"
    );

    graceful_kill(home);
}

/// `shep add` registers a sheep, starts nothing, and a later `shep start`
/// brings that same sheep up. Without it, `shep start Flockfile.toml` on a
/// template shipping `env = { DB_HOST = "", DB_PASSWORD = "" }` spawns against
/// an empty database URL and spends the restart budget before it can be
/// configured.
///
/// The listing is read once, not polled: `Request::Add` is answered after the
/// actor has registered, so a build routing `add` through the start path
/// reports `online` on this first read.
#[test]
fn add_registers_a_stopped_sheep_that_a_later_start_brings_up() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"pending\"\nscript = '{}'\n",
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let added = shep(home)
        .arg("--format")
        .arg("json")
        .arg("add")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&added);

    let envelope: serde_json::Value = serde_json::from_slice(&added.stdout)
        .unwrap_or_else(|e| panic!("add stdout was not JSON: {e}"));
    assert_eq!(
        envelope["command"], "add",
        "the envelope names the verb the operator typed: {envelope}"
    );
    assert_eq!(
        envelope["data"][0]["status"], "stopped",
        "registered, not started: {envelope}"
    );
    assert!(
        envelope["data"][0]["pid"].is_null(),
        "a sheep that was never spawned has no pid: {envelope}"
    );

    // The script appends its pid to this file on every run, so an empty one is
    // the child's own evidence that it never executed. Unix only, because
    // `record_pid_line` writes nothing on Windows.
    #[cfg(unix)]
    assert!(
        !dir.path().join(FIXTURE_PIDS).exists(),
        "the script never ran, so it never recorded a pid"
    );

    // Registered rather than merely reported: a row `shep flock` cannot see is
    // not a flock member.
    let listed = shep(home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&listed);
    let flock: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(flock["data"][0]["name"], "pending", "it is in the flock");
    assert_eq!(flock["data"][0]["status"], "stopped", "and still at rest");

    // By name: a name reads no file, so this can only reach a sheep the flock
    // already holds.
    let started = shep(home).arg("start").arg("pending").output().unwrap();
    assert_success(&started);
    let running = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(
        running["status"], "online",
        "the registered sheep came up: {running}"
    );

    graceful_kill(home);
}

/// `shep add` with no target and no Flockfile in the current directory is a
/// usage error, where bare `shep start` brings a shepherd up: `start`'s
/// empty-directory case means "give me a shepherd with nothing running yet",
/// and `add` produces a shepherd holding nothing either way. The temporary
/// directory is the working directory as well as the home, so there is no
/// Flockfile to discover.
#[test]
fn add_with_nothing_to_add_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    let output = shep(home).arg("add").current_dir(home).output().unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "clap's own code for bad arguments: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no target and no Flockfile"),
        "the refusal says what was missing: {stderr}"
    );
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "and no shepherd was started to answer a request nobody made"
    );
}

/// `$SUDO_USER` names `nobody` (passwd home `/var/empty` on macOS,
/// `/nonexistent` on Linux) and `$HOME` is a throwaway standing in for
/// root's. The refusal must name nobody's `~/.shep`, not `$HOME/.shep`.
/// Skipped as root: a broken gate would really install a unit.
#[cfg(unix)]
#[test]
fn a_sudo_startup_without_home_carries_the_target_users_home_not_this_processes() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: as root this would really install a unit if the gate were broken");
        return;
    }
    let Ok(Some(nobody)) = nix::unistd::User::from_name("nobody") else {
        eprintln!("skipping: no `nobody` user to stand in for $SUDO_USER");
        return;
    };
    let fake_root_home = TempDir::new().unwrap();

    let output = Command::cargo_bin("shep")
        .unwrap()
        .env("HOME", fake_root_home.path())
        .env("SUDO_USER", "nobody")
        .env_remove("SHEP_HOME")
        .arg("startup")
        .arg("--init")
        .arg("systemd")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let target_home = nobody.dir.join(".shep");
    let refusal = format!(
        "error[usage]: no directory at {}; create it first (any shep verb run as nobody \
         creates that user's own ~/.shep), or pass --home with the $SHEP_HOME this unit \
         should carry",
        target_home.display()
    );
    assert!(
        stderr.lines().any(|line| line == refusal),
        "the refusal names nobody's own home and both ways out: {stderr}"
    );
    assert!(
        !stderr.contains(fake_root_home.path().to_str().unwrap()),
        "and never this process's $HOME: {stderr}"
    );
    assert!(
        !fake_root_home.path().join(".shep").exists(),
        "nothing is created under a $HOME that is not the target user's"
    );
}

/// A three-app Flockfile drawing one chain: `db`, then `api`, then `web`.
///
/// Three stages is the point. Each gated stage is held for its own
/// `listen_timeout` before the next one spawns, so this is what a staged
/// start's wall clock is made of.
fn write_chained_flockfile(dir: &TempDir, script: &Path) -> PathBuf {
    write_flockfile(
        dir,
        &format!(
            "[[app]]\nname = \"db\"\nscript = '{script}'\n\
             [[app]]\nname = \"api\"\nscript = '{script}'\ndepends_on = [\"db\"]\n\
             [[app]]\nname = \"web\"\nscript = '{script}'\ndepends_on = [\"api\"]\n",
            script = script.display(),
        ),
    )
}

#[test]
fn starting_a_flockfile_with_a_cycle_refuses_and_names_it() {
    // fails if the cycle starts an arbitrary half of the flock anyway, or if
    // the refusal says only that a cycle exists without naming the path an
    // operator has to break
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"a\"\nscript = '{script}'\ndepends_on = [\"b\"]\n\
             [[app]]\nname = \"b\"\nscript = '{script}'\ndepends_on = [\"a\"]\n",
            script = script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let out = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    assert_eq!(out.status.code(), Some(4), "invalid_config");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(" -> "),
        "the cycle must be named as a path: {stderr}"
    );
    graceful_kill(home);
}

#[test]
fn a_three_stage_start_brings_every_stage_online() {
    // fails if a staged start does not survive the round trip: the reply
    // lands only after the last stage, so a deadline sized for one spawn
    // abandons a flock the shepherd is still bringing up correctly
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_chained_flockfile(&dir, &script);
    let mut guard = DaemonGuard::default();

    let out = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&out);

    let data = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.len() == 3 && rows.iter().all(|row| row["status"] == "online"))
    });
    let names: Vec<&str> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 3, "every stage came up: {data}");
    graceful_kill(home);
}

mod assertions;
mod available_dogs;
mod constants;
mod daemon_log;
mod dev;
mod dispatch;
mod dogs_kv;
mod fixtures;
mod guard;
mod home_and_watch;
mod import_env;
mod init_verb;
mod lifecycle;
mod logs;
mod lookout_whistle;
mod polling;
mod real_clock;
mod rendering;
mod runtime;
mod serve;
mod spawn_failures;

pub(crate) use assertions::*;
pub(crate) use constants::*;
pub(crate) use fixtures::*;
pub(crate) use guard::*;
pub(crate) use polling::*;
