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

// --- Interpreter / spawn-failure parity -----------------------------------

#[cfg(unix)]
/// `Response::Restarted` has no per-id error slot, so a respawn that cannot
/// spawn answers `Ok` with an `errored` row rather than an RPC error;
/// `resume`'s `any_restart_failed` check is what closes that gap.
///
/// The script is valid shell but not executable (`0o644`), so every spawn of
/// it fails `EACCES` whichever request drove it.
#[test]
fn starting_an_errored_sheep_by_name_reports_the_same_failure_as_by_path() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("broken.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&script, perms).unwrap();
    let mut guard = DaemonGuard::default();

    // Also autostarts the daemon the second command reuses, and registers the
    // sheep the second half restarts by name.
    let by_path = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_json_error(&by_path, 7, "spawn_failed");

    // Must be `errored` in the flock, or the second command takes
    // `resolve_target`'s path arm instead of `resume`'s.
    let flock = poll_flock(dir.path(), |info| info["status"] == "errored");
    assert_eq!(
        flock["status"], "errored",
        "the by-path failure must leave the sheep registered as errored: {flock}"
    );

    // By name, same broken script, same failure.
    let name = script.file_stem().unwrap().to_str().unwrap();
    let by_name = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(name)
        .output()
        .unwrap();
    assert_json_error(&by_name, 7, "spawn_failed");

    graceful_kill(dir.path());
}

#[cfg(unix)]
/// Same `Response::Restarted` gap as the sibling above. `restart` still prints
/// its table, being a multi-target verb; the exit code and the stderr line are
/// what change.
///
/// The script is valid shell at `0o644`, so every spawn fails `EACCES`, and it
/// has no extension: `.sh` maps to `sh` through the interpreter mapping, which
/// would run a non-executable file and delete the premise.
#[test]
fn restarting_a_sheep_that_cannot_spawn_reports_it_rather_than_exiting_zero() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("noexec");
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&script, perms).unwrap();
    let mut guard = DaemonGuard::default();

    let by_path = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_json_error(&by_path, 7, "spawn_failed");

    // Must be registered and errored, or the restart below is not
    // exercising the reply shape this test is about.
    let flock = poll_flock(dir.path(), |info| info["status"] == "errored");
    assert_eq!(
        flock["status"], "errored",
        "the by-path failure must leave the sheep registered as errored: {flock}"
    );

    let name = script.file_stem().unwrap().to_str().unwrap();
    let restarted = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("restart")
        .arg(name)
        .output()
        .unwrap();
    assert_json_error(&restarted, 7, "spawn_failed");

    graceful_kill(dir.path());
}

#[cfg(unix)]
/// The missing-node sentence, produced for real rather than quoted.
///
/// It needs a `PATH` with no node on it, which a unit test could only get by
/// mutating its own process. `docs/migration.md` quotes this sentence, and
/// this is what holds the quote to the `format!` that produces it.
#[test]
fn a_js_flockfile_without_node_says_so_and_says_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.js");
    // Declares a real app, so the only thing that can fail is the missing
    // interpreter: with node present this Flockfile is valid.
    std::fs::write(
        &flockfile,
        "module.exports = { app: [{ name: 'web', script: './server.js' }] };\n",
    )
    .unwrap();
    let mut guard = DaemonGuard::default();

    // An empty PATH for the child only, so `node` cannot be found and the
    // parent's environment is untouched.
    let output = shep(dir.path())
        .env("PATH", "")
        .arg("start")
        .arg("--flockfile")
        .arg(&flockfile)
        .output()
        .unwrap();

    // `start` autostarts a shepherd before it ever opens the Flockfile, so
    // this case leaves one behind even though it fails.
    guard.adopt_home(dir.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a Flockfile that cannot be read must not succeed: {stderr}"
    );
    assert!(
        stderr.contains("node was not found on PATH"),
        "the message names the cause: {stderr}"
    );
    assert!(
        stderr.contains("install node, or convert"),
        "and what to do about it: {stderr}"
    );
    assert!(
        !stderr.contains('\u{2014}') && !stderr.contains('\u{2013}'),
        "no em or en dash in copy a user reads: {stderr}"
    );

    graceful_kill(dir.path());
}

// --- shep init ---------------------------------------------------------------
//
// Writing a file is the behaviour under test, and a subprocess is the only
// place `shep init` runs.

#[test]
fn shep_init_writes_a_flockfile_where_there_is_none() {
    let dir = tempfile::tempdir().unwrap();

    let output = shep(dir.path())
        .current_dir(dir.path())
        .arg("init")
        .output()
        .unwrap();
    assert_success(&output);

    let written = dir.path().join("Flockfile.toml");
    assert!(written.exists(), "shep init must write Flockfile.toml");

    let body = std::fs::read_to_string(&written).unwrap();
    assert!(
        body.contains("[[app]]"),
        "the scaffold shows an app entry: {body}"
    );
    assert!(
        body.lines().any(|l| l.trim_start().starts_with('#')),
        "and it arrives commented out: {body}"
    );
}

#[cfg(unix)]
/// The unit tests prove the scaffold parses; this proves the bytes that reach
/// disk are the same ones.
#[test]
fn what_shep_init_writes_is_a_flockfile_shep_can_read() {
    let dir = tempfile::tempdir().unwrap();
    shep(dir.path())
        .current_dir(dir.path())
        .arg("init")
        .output()
        .unwrap();

    // Uncommenting is what makes it a live Flockfile: as written it declares
    // no apps and `shep start` refuses it.
    let body = std::fs::read_to_string(dir.path().join("Flockfile.toml")).unwrap();
    let live: String = body
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            match trimmed.strip_prefix('#') {
                Some(rest) if !rest.starts_with(' ') => rest.to_string(),
                _ => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("Flockfile.toml"), &live).unwrap();
    let mut guard = DaemonGuard::default();

    let output = shep(dir.path())
        .current_dir(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg("--flockfile")
        .arg("Flockfile.toml")
        .output()
        .unwrap();

    guard.adopt_home(dir.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("invalid_config"),
        "the uncommented scaffold must be valid config: {stderr}"
    );

    graceful_kill(dir.path());
}

#[cfg(unix)]
/// Proved by metadata, not content: a refusal that still rewrites the file
/// leaves identical bytes while the inode has changed and a symlinked config
/// has become a regular file.
#[test]
fn shep_init_refuses_an_existing_flockfile_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("Flockfile.toml");
    std::fs::write(
        &existing,
        "# mine\n[[app]]\nname = \"web\"\nscript = \"./s\"\n",
    )
    .unwrap();

    let before = std::fs::metadata(&existing).unwrap();

    let output = shep(dir.path())
        .current_dir(dir.path())
        .arg("init")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "an existing Flockfile must not be overwritten silently"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Flockfile.toml"),
        "the refusal names the file: {stderr}"
    );

    let after = std::fs::metadata(&existing).unwrap();
    assert_eq!(
        before.ino(),
        after.ino(),
        "a refused write must not replace the file"
    );
    assert_eq!(
        before.permissions().mode(),
        after.permissions().mode(),
        "nor change its mode"
    );
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "# mine\n[[app]]\nname = \"web\"\nscript = \"./s\"\n",
        "nor its contents"
    );
}

#[test]
fn shep_init_force_replaces_an_existing_flockfile() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("Flockfile.toml");
    std::fs::write(&existing, "# mine\n").unwrap();

    let output = shep(dir.path())
        .current_dir(dir.path())
        .arg("init")
        .arg("--force")
        .output()
        .unwrap();
    assert_success(&output);

    let body = std::fs::read_to_string(&existing).unwrap();
    assert!(
        body.contains("[[app]]"),
        "--force writes the scaffold over what was there: {body}"
    );
}

/// The depth flag reaches the file, not just the function.
#[test]
fn shep_init_all_writes_the_full_scaffold() {
    let dir = tempfile::tempdir().unwrap();

    let output = shep(dir.path())
        .current_dir(dir.path())
        .arg("init")
        .arg("--all")
        .output()
        .unwrap();
    assert_success(&output);

    let body = std::fs::read_to_string(dir.path().join("Flockfile.toml")).unwrap();
    for field in ["max_restarts", "kill_timeout", "watch_delay"] {
        assert!(
            body.contains(field),
            "--all names every option, and is missing `{field}`"
        );
    }
}

// --- Reload ---------------------------------------------------------------

#[cfg(unix)]
/// The envelope's `command` is what pins which handler `Commands::Reload`
/// reaches, since `main`'s dispatch arms have no unit coverage. The polled id
/// is the other half: a reload ends in a new id in the same instance slot,
/// where a restart would leave the id alone and a stop the sheep down.
///
/// The reply carries the original id, which is the acceptance contract:
/// `shep reload` exits before the swap commits, and the poll waits for it.
#[test]
fn reload_swaps_a_sheep_for_a_fresh_instance_under_a_new_id() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);
    let envelope: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let original_id = envelope["data"][0]["id"]
        .as_u64()
        .unwrap_or_else(|| panic!("a started sheep must carry an id: {envelope}"));

    let reloaded = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("reload")
        .arg("sheep")
        .output()
        .unwrap();
    assert_success(&reloaded);
    let envelope: serde_json::Value = serde_json::from_slice(&reloaded.stdout).unwrap();
    assert_eq!(
        envelope["command"], "reload",
        "`shep reload` must reach the reload verb and no other: {envelope}"
    );
    assert_eq!(
        envelope["data"][0]["id"], original_id,
        "the answer is the flock as it stood when the reload was accepted: {envelope}"
    );

    let after = poll_flock(dir.path(), |info| info["id"] != original_id);
    assert_ne!(
        after["id"], original_id,
        "the swap must finish, leaving one entry under a new id: {after}"
    );
    assert_eq!(after["status"], "online", "{after}");

    graceful_kill(dir.path());
}

// --- Trigger ---------------------------------------------------------------

#[cfg(unix)]
/// The envelope's `command` is what pins which handler `Commands::Trigger`
/// reaches, since `main`'s dispatch arms have no unit coverage.
///
/// The sheep has no `channel`/`wait_ready`/`shutdown_with_message`, so its
/// reply is `no_channel` every time without a companion that speaks the
/// shepherd channel. The other three outcomes need one and are not covered.
#[test]
fn trigger_reaches_the_trigger_verb_and_names_the_missing_channel() {
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

    let triggered = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("trigger")
        .arg("sheep")
        .arg("reload-config")
        .output()
        .unwrap();
    assert_success(&triggered);
    let envelope: serde_json::Value = serde_json::from_slice(&triggered.stdout).unwrap();
    assert_eq!(
        envelope["command"], "trigger",
        "`shep trigger` must reach the trigger verb and no other: {envelope}"
    );
    assert_eq!(envelope["data"][0]["name"], "sheep", "{envelope}");
    assert_eq!(
        envelope["data"][0]["outcome"]["kind"], "no_channel",
        "a sheep with no channel/wait_ready/shutdown_with_message must answer \
         no_channel, never a reply it never opened a pipe to receive: {envelope}"
    );

    graceful_kill(dir.path());
}

// --- Signal ------------------------------------------------------------

#[cfg(unix)]
/// `SIGWINCH` is harmless to essentially everything, so the assertion is that
/// delivery reached the sheep, not what the child did with it.
#[test]
fn signal_reaches_the_signal_verb_and_delivers() {
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

    let signalled = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("signal")
        .arg("sheep")
        .arg("SIGWINCH")
        .output()
        .unwrap();
    assert_success(&signalled);
    let envelope: serde_json::Value = serde_json::from_slice(&signalled.stdout).unwrap();
    assert_eq!(
        envelope["command"], "signal",
        "`shep signal` must reach the signal verb and no other: {envelope}"
    );
    assert_eq!(envelope["data"][0]["name"], "sheep", "{envelope}");
    assert_eq!(
        envelope["data"][0]["outcome"]["kind"], "delivered",
        "a running sheep must answer delivered for a signal the kernel accepted: {envelope}"
    );

    graceful_kill(dir.path());
}

// --- Stock -------------------------------------------------------------

#[cfg(unix)]
/// Both directions are polled through `shep flock` rather than taken off
/// `stock`'s own exit: a stock-down accepts before the departing instances'
/// stop ladders finish, so the flock settling is the real assertion.
#[test]
fn stock_reaches_the_stock_verb_and_settles_the_flock() {
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

    let stocked_up = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("stock")
        .arg("sheep")
        .arg("3")
        .output()
        .unwrap();
    assert_success(&stocked_up);
    let envelope: serde_json::Value = serde_json::from_slice(&stocked_up.stdout).unwrap();
    assert_eq!(
        envelope["command"], "stock",
        "`shep stock` must reach the stock verb and no other: {envelope}"
    );

    let grown = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| rows.len() == 3)
    });
    assert_eq!(
        grown.as_array().unwrap().len(),
        3,
        "stocking up must settle at three instances: {grown}"
    );
    assert!(
        grown
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["name"] == "sheep"),
        "every instance must still belong to `sheep`: {grown}"
    );

    let stocked_down = shep(dir.path())
        .arg("stock")
        .arg("sheep")
        .arg("1")
        .output()
        .unwrap();
    assert_success(&stocked_down);

    let settled = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| rows.len() == 1)
    });
    assert_eq!(
        settled.as_array().unwrap().len(),
        1,
        "stocking down must settle back to one instance: {settled}"
    );

    graceful_kill(dir.path());
}

#[cfg(unix)]
/// `shep scale` is `stock`'s visible alias, and must produce the same primary
/// command name in its envelope.
#[test]
fn scale_alias_reaches_stock_against_a_real_daemon() {
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

    let scaled = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("scale")
        .arg("sheep")
        .arg("2")
        .output()
        .unwrap();
    assert_success(&scaled);
    let envelope: serde_json::Value = serde_json::from_slice(&scaled.stdout).unwrap();
    assert_eq!(
        envelope["command"], "stock",
        "`shep scale` is an alias for `stock`, and must reach it: {envelope}"
    );

    graceful_kill(dir.path());
}

// --- Lambs ---------------------------------------------------------------

#[cfg(unix)]
/// Polled, not asserted once: the daemon walks lambs inside `Describe` against
/// the live process table, so the forked child's appearance races this test.
#[test]
fn describe_renders_a_real_sheeps_lamb_tree() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_forking_script(&dir);
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

    // Polls for `sleep`, not merely a `Lambs of` section: a walk can catch the
    // child mid-exec still reporting the shell's name. It rides that out only
    // because `MemorySampler::identify` builds a process table per call;
    // sysinfo never revises a name it has recorded for a pid.
    let start = Instant::now();
    let described = loop {
        let output = shep(dir.path())
            .arg("describe")
            .arg("sheep")
            .output()
            .unwrap();
        assert_success(&output);
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if text.contains("sleep") || start.elapsed() >= FLOCK_DEADLINE {
            break text;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    };

    assert!(described.contains("Lambs of"), "{described}");
    assert!(described.contains("sleep"), "{described}");
    assert!(
        described.contains("not exactly the set a stop kills"),
        "{described}"
    );

    graceful_kill(dir.path());
}

// --- Save / Muster ---------------------------------------------------------

#[cfg(unix)]
/// Nothing goes down in between, so the muster exercises the already-running
/// idempotence rule in `snapshot::restorable`.
///
/// `flock.len()` pins exactly one instance and `pid` pins it as the process
/// `start` reported, so a muster that duplicated or restarted the sheep fails
/// here.
#[test]
fn saving_the_roll_then_mustering_reports_the_same_flock() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("roundtrip")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);
    let start_envelope: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(
        start_envelope["data"][0]["status"], "online",
        "{start_envelope}"
    );
    let original_pid = start_envelope["data"][0]["pid"]
        .as_i64()
        .unwrap_or_else(|| panic!("pid must be a real positive OS pid: {start_envelope}"));

    let saved = shep(home)
        .arg("--format")
        .arg("json")
        .arg("save")
        .output()
        .unwrap();
    assert_success(&saved);
    let save_envelope: serde_json::Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(
        save_envelope["command"], "save",
        "`shep save` must reach the save verb and no other: {save_envelope}"
    );
    assert_eq!(
        save_envelope["data"]["apps"], 1,
        "the roll must record the one app started above: {save_envelope}"
    );

    let mustered = shep(home)
        .arg("--format")
        .arg("json")
        .arg("muster")
        .output()
        .unwrap();
    assert_success(&mustered);
    let muster_envelope: serde_json::Value = serde_json::from_slice(&mustered.stdout).unwrap();
    assert_eq!(
        muster_envelope["command"], "muster",
        "`shep muster` must reach the muster verb and no other: {muster_envelope}"
    );
    let flock = muster_envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("muster data must be an array: {muster_envelope}"));
    assert_eq!(
        flock.len(),
        1,
        "muster against a daemon already running the flock the roll \
         describes must not spawn a duplicate: {muster_envelope}"
    );
    assert_eq!(flock[0]["name"], "roundtrip", "{muster_envelope}");
    assert_eq!(
        flock[0]["pid"].as_i64().unwrap(),
        original_pid,
        "muster must leave an already-running sheep alone and report the \
         SAME process, never restart it: {muster_envelope}"
    );

    graceful_kill(home);
}

// --- Import -----------------------------------------------------------

#[cfg(unix)]
/// The written file is parsed back through the real
/// `shep_core::config::Flockfile::parse`: a Flockfile shep refuses to read is
/// not an import. That no socket appears is the other half, since `import`
/// takes no `Client`.
#[test]
fn import_writes_a_flockfile_shep_can_read_back_and_starts_no_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let out = home.join("Flockfile.toml");
    let dump = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/commands/import/pm2/testdata/dump.pm2.json"
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home)
        .arg("--format")
        .arg("json")
        .arg("import")
        .arg("pm2")
        .arg("--from")
        .arg(dump)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&output);

    assert!(
        !home.join("run").join("shep.sock").exists(),
        "`shep import` reads a file and writes a file; it must never \
         autostart a daemon"
    );

    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["command"], "import",
        "`shep import` must reach the import verb and no other: {envelope}"
    );
    let rows = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("import data must be an array: {envelope}"));
    assert_eq!(rows.len(), 3, "{envelope}");

    let written = std::fs::read_to_string(&out).unwrap();
    let parsed =
        shep_core::config::Flockfile::parse(&written, shep_core::config::FlockFormat::Toml)
            .unwrap_or_else(|e| {
                panic!("shep import wrote a Flockfile shep cannot read back: {e}\n{written}")
            });
    assert_eq!(parsed.apps.len(), 3, "{written}");
}

/// A shepherd on `home`'s `$SHEP_HOME` with one sheep named `web`, which is
/// what every `shep import env` case below writes against: the verb records
/// an operator override, so the sheep has to be registered first.
#[cfg(unix)]
fn start_a_sheep_named_web(home: &TempDir) -> DaemonGuard {
    let script = write_test_script(home);
    let mut guard = DaemonGuard::default();
    let boot = shep(home.path())
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home.path());
    assert_success(&boot);
    guard
}

#[cfg(unix)]
/// The whole verb, end to end: two plain keys into the sheep's env, one
/// secret into the store with a reference left behind.
#[test]
fn import_env_splits_a_dotenv_between_the_two_stores() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "NODE_ENV=production\nPORT=8080\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("hunter2") && !combined.contains("8080"),
        "a value reached an output stream: {combined}"
    );

    // `describe` reads its secret references out of the muster roll on disk,
    // which only a save writes: the import records an operator override, and
    // the daemon holds the parked spec in memory until something rolls it.
    assert_success(&shep(home.path()).arg("save").output().unwrap());

    // The roll is where the reference itself lands, so it is where the
    // literal token is asserted. `describe` reports the reference resolved
    // rather than reprinting it, and that is the second half of the claim:
    // a bare `contains("DB_PASSWORD")` would pass on an unresolved one.
    let rolled = std::fs::read_to_string(home.path().join("flock.json")).unwrap();
    assert!(
        rolled.contains("{{secret:DB_PASSWORD}}"),
        "the reference did not reach the sheep: {rolled}"
    );

    let described = shep(home.path())
        .args(["describe", "web", "--format", "json"])
        .output()
        .unwrap();
    let described: serde_json::Value =
        serde_json::from_slice(&described.stdout).expect("describe --format json emits JSON");
    assert_eq!(
        described["secrets"],
        serde_json::json!([{
            "name": "web",
            "reference": "DB_PASSWORD",
            "environment": "production",
            "status": "resolved",
        }]),
        "{described}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A `.env` value is a value, not a shep template.
///
/// The sheep's env is read as a template grammar, where `{{world}}` is
/// refused at config time and `{{name}}` substitutes the sheep's own name.
/// A `.env` promises neither, so the child has to receive both exactly as
/// the file wrote them. The reload is what promotes the parked env.
#[test]
fn import_env_hands_the_child_a_braced_value_exactly_as_the_file_wrote_it() {
    let home = tempfile::tempdir().unwrap();
    let script = write_script(
        &home,
        "braces.sh",
        &format!(
            "{}{}echo \"motd=[$MOTD]\"\necho \"greeting=[$GREETING]\"\n{}",
            script_header(),
            record_pid_line(&home),
            sleep_line(SCRIPT_SLEEP_SECS)
        ),
    );
    let mut guard = DaemonGuard::default();
    let boot = shep(home.path())
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home.path());
    assert_success(&boot);

    std::fs::write(
        home.path().join("app.env"),
        "MOTD=hello {{world}}\nGREETING={{name}}-prod\n",
    )
    .unwrap();
    let imported = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_success(&imported);

    assert_success(&shep(home.path()).args(["reload", "web"]).output().unwrap());

    let bleats = bleats_no_follow_until_contains(
        home.path(),
        &["web"],
        &["motd=[hello {{world}}]", "greeting=[{{name}}-prod]"],
    );
    let printed = String::from_utf8_lossy(&bleats.stdout);
    assert!(
        printed.contains("motd=[hello {{world}}]"),
        "an unknown token must reach the child literally: {printed}"
    );
    assert!(
        printed.contains("greeting=[{{name}}-prod]"),
        "and a token shep does define must not be substituted: {printed}"
    );
    assert!(
        !printed.contains("greeting=[web-prod]"),
        "the sheep's name was substituted into a .env value: {printed}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// Re-running an unchanged file is a no-op. Changing one value refuses the
/// whole import until `--force`.
#[test]
fn import_env_refuses_a_changed_value_without_force() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "PORT=8080\nNODE_ENV=production\n",
    )
    .unwrap();
    let first = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_success(&first);

    std::fs::write(
        home.path().join("app.env"),
        "PORT=9090\nNODE_ENV=production\n",
    )
    .unwrap();
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("PORT"),
        "the colliding key was not named: {err}"
    );
    // The daemon compares against the sheep's intended config, so a key a
    // Flockfile declares collides without the override store holding it.
    // A line naming an "env store" sends the operator to a file the value
    // need not be in.
    assert!(
        err.contains("the sheep's env"),
        "the refusal must name what actually holds the key: {err}"
    );
    assert!(!err.contains("9090"), "the value reached stderr: {err}");
    // The refusal's whole claim is that nothing moved, and this is the only
    // case where a store could have been written before it: the two
    // neighbouring refusals are parse-level.
    let overrides = std::fs::read_to_string(home.path().join("overrides.json")).unwrap();
    assert!(
        overrides.contains("8080") && !overrides.contains("9090"),
        "the refused value reached the env store: {overrides}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A key the secret store already holds under a different value refuses the
/// import too, and names the secret store rather than the env one.
///
/// `--env production` on both halves so the seeded slot and the imported one
/// are the same slot whatever the sheep resolves to.
#[test]
fn import_env_refuses_a_changed_secret_without_force() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    assert_success(
        &shep(home.path())
            .args([
                "secret",
                "set",
                "DB_PASSWORD",
                "correct",
                "--env",
                "production",
            ])
            .output()
            .unwrap(),
    );
    std::fs::write(home.path().join("app.env"), "DB_PASSWORD=hunter2\n").unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
            "--env",
            "production",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("DB_PASSWORD") && err.contains("secret store"),
        "the secret arm did not report the collision: {err}"
    );
    assert!(!err.contains("hunter2"), "the value reached stderr: {err}");
    let stored = std::fs::read_to_string(home.path().join("secrets.json")).unwrap();
    assert!(
        stored.contains("correct") && !stored.contains("hunter2"),
        "the refused value reached the secret store"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// `--force` takes both stores over the values already in them.
///
/// `--format json` pins the envelope's `command`, which is `import` for both
/// halves of the verb: the noun names the command, as `shep secret`'s four
/// subcommands do.
#[test]
fn import_env_force_overwrites_both_stores() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    assert_success(
        &shep(home.path())
            .args([
                "secret",
                "set",
                "DB_PASSWORD",
                "stale",
                "--env",
                "production",
            ])
            .output()
            .unwrap(),
    );
    std::fs::write(home.path().join("app.env"), "PORT=8080\n").unwrap();
    assert_success(
        &shep(home.path())
            .args([
                "import",
                "env",
                home.path().join("app.env").to_str().unwrap(),
                "--app",
                "web",
                "--env",
                "production",
            ])
            .output()
            .unwrap(),
    );

    std::fs::write(
        home.path().join("app.env"),
        "PORT=9090\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();
    let forced = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
            "--env",
            "production",
            "--force",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_success(&forced);
    let envelope: serde_json::Value = serde_json::from_slice(&forced.stdout).unwrap();
    assert_eq!(
        envelope["command"], "import",
        "the envelope's command moved: {envelope}"
    );

    let overrides = std::fs::read_to_string(home.path().join("overrides.json")).unwrap();
    assert!(
        overrides.contains("9090") && !overrides.contains("8080"),
        "--force left the old env value: {overrides}"
    );
    let stored = std::fs::read_to_string(home.path().join("secrets.json")).unwrap();
    assert!(
        stored.contains("hunter2") && !stored.contains("stale"),
        "--force left the old secret"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A key that looks like a secret and was not named by `--secret` is warned
/// about, and the warning carries the key and not its value.
#[test]
fn import_env_warns_about_a_secretish_key_it_was_not_told_to_hide() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(home.path().join("app.env"), "STRIPE_TOKEN=hunter2\n").unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("STRIPE_TOKEN") && err.contains("--secret"),
        "the secretish warning did not appear: {err}"
    );
    assert!(!err.contains("hunter2"), "the value reached stderr: {err}");

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A pattern that matches nothing refuses before anything is written.
#[test]
fn import_env_refuses_a_pattern_that_matches_nothing() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "NODE_ENV=production\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "ABSENT_*",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        !home.path().join("secrets.json").exists(),
        "a refused pattern must write nothing"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// `--dry-run` writes to neither store.
#[test]
fn import_env_dry_run_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "PORT=8080\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    // Both stores, because the fixture's plain key never reaches the secret
    // one: a dry run that sent a non-dry batch would move `overrides.json`
    // alone and pass a check that only read `secrets.json`.
    let secrets_before = std::fs::read_to_string(home.path().join("secrets.json")).ok();
    let overrides_before = std::fs::read_to_string(home.path().join("overrides.json")).ok();
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(home.path().join("secrets.json")).ok(),
        secrets_before
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("overrides.json")).ok(),
        overrides_before
    );
    // The dry run is the path that prints a row per key, so it is the one
    // where a value would show up if a row ever grew one.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("hunter2") && !combined.contains("8080"),
        "a value reached an output stream: {combined}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A `.env` that names a variable shep injects itself is refused by the
/// dry-run probe at step 3, which runs `normalize` on the merged config, so
/// neither store is touched.
///
/// This used to reach the real send instead and leave an orphaned secret in
/// `secrets.json`. The refusal now lands before the secret write, and the
/// secret's value still reaches neither output stream.
#[test]
fn import_env_refuses_a_reserved_variable_before_either_store_is_written() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("bad.env"),
        "SHEP_NAME=nope\nAPI_TOKEN=sk_live_abcdef\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("bad.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "API_TOKEN",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("sk_live_abcdef"),
        "the value reached an output stream: {combined}"
    );
    assert!(
        !combined.contains("already written to the secret store"),
        "nothing was written, so nothing may be disclosed: {combined}"
    );

    let secrets = std::fs::read_to_string(home.path().join("secrets.json")).unwrap_or_default();
    assert!(
        !secrets.contains("API_TOKEN"),
        "the secret store was written by a refused import: {secrets}"
    );
    let overrides = std::fs::read_to_string(home.path().join("overrides.json")).unwrap_or_default();
    assert!(
        !overrides.contains("API_TOKEN") && !overrides.contains("SHEP_NAME"),
        "the override store was written by a refused import: {overrides}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// An unknown sheep is a `NotFound`, and it is reported before either store
/// is touched.
#[test]
fn import_env_refuses_an_unknown_app() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "NODE_ENV=production\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "absent",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        !home.path().join("secrets.json").exists(),
        "an unknown sheep must be refused before either store is touched"
    );

    graceful_kill(home.path());
}

// --- Dogs / Barks -----------------------------------------------------

#[cfg(unix)]
/// The only tier that exec's `shep dog metrics`: every other scripts the
/// runner or fakes the client.
///
/// Four things fail here as a refused connection: the dog being spawned, its
/// reaching the socket from `$SHEP_HOME`, its fetching its own
/// `[dog.metrics]` section, and its bind.
#[test]
fn a_real_shepherd_runs_a_real_metrics_dog_that_answers_a_scrape() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(
        &dir,
        &format!("[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"),
    );
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);

    let online = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(
        online["status"], "online",
        "the sheep must reach online before the dog's own exposition has \
         anything real to name: {online}"
    );

    let enabled = shep(home).arg("enable").arg("metrics").output().unwrap();
    assert_success(&enabled);

    // Registered before the scrape: one that hangs or panics on an assertion
    // must not leak the grandchild the daemon just spawned.
    let dog_pid = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(dog_pid);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let body = poll_metrics(addr);
    assert!(
        body.contains("HTTP/1.1 200"),
        "the metrics dog must answer 200 at /metrics: {body}"
    );
    assert!(
        body.contains(r#"shep_sheep_status{sheep="web",id="0",fold="",status="online"} 1"#),
        "the exposition must name the sheep, online: {body}"
    );
    assert!(
        body.contains(r#"shep_dog_up{dog="metrics",source="built-in"} 1"#),
        "the dog must report itself up while it is the one serving the \
         scrape that says so: {body}"
    );

    graceful_kill(home);
}

#[cfg(unix)]
/// [`poll_metrics`], retried until the exposition contains `needle` rather
/// than merely until it answers: a dog answering from a cached reading still
/// answers 200, so only content the predecessor never saw tells them apart.
fn poll_metrics_containing(addr: std::net::SocketAddr, needle: &str) -> String {
    let start = Instant::now();
    let mut last = String::new();
    loop {
        if let Ok(body) = scrape_metrics(addr) {
            if body.contains(needle) {
                return body;
            }
            last = body;
        }
        if start.elapsed() >= METRICS_SCRAPE_DEADLINE {
            return last;
        }
        std::thread::sleep(METRICS_SCRAPE_POLL_INTERVAL);
    }
}

#[cfg(unix)]
/// A real dog process, carried across a real `execve`, still able to talk to
/// the shepherd that replaced the one it handshook with.
///
/// A pid check cannot see the defect: a dog holding a dead socket reads as
/// healthy on every column a listing has. The decisive assertion is content, a
/// sheep started after the reload appearing in the exposition.
#[test]
fn a_carried_dog_answers_a_scrape_after_a_real_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(
        &dir,
        &format!("[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"),
    );
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);
    let online = poll_flock(home, |info| info["status"] == "online");
    let sheep_pid = online["pid"].as_u64().expect("an online sheep has a pid");

    let enabled = shep(home).arg("enable").arg("metrics").output().unwrap();
    assert_success(&enabled);
    let dog_pid = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(dog_pid);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let before = poll_metrics(addr);
    assert!(
        before.contains("HTTP/1.1 200"),
        "the dog must be answering BEFORE the reload, or this case proves nothing: {before}"
    );

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&reloaded.stdout),
        String::from_utf8_lossy(&reloaded.stderr)
    );
    // The exact sentence `handover::RefusedReason`'s `Display` ends with: the
    // stop arm restarts every dog from disk and would satisfy a looser probe.
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "a flock with a dog in it is carried now, not refused: {text}"
    );
    assert!(
        !text.contains("cannot talk to this shepherd"),
        "the carried dog must reconnect rather than be reported stale: {text}"
    );
    assert!(
        !text.contains("cannot say whether it came back"),
        "the carried dog must answer inside the reload's own wait: {text}"
    );

    let after = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.len() == 2 && rows.iter().all(|row| !row["pid"].is_null()))
    });
    let rows = after.as_array().expect("flock data is an array");
    let dog_row = rows
        .iter()
        .find(|row| row["name"] == "metrics")
        .unwrap_or_else(|| panic!("the dog must still be registered: {after}"));
    let sheep_row = rows
        .iter()
        .find(|row| row["name"] == "web")
        .unwrap_or_else(|| panic!("the sheep must still be registered: {after}"));

    assert_eq!(
        dog_row["pid"].as_u64(),
        Some(u64::try_from(dog_pid.as_raw()).unwrap()),
        "the dog was restarted rather than carried: {after}"
    );
    assert_eq!(
        sheep_row["pid"].as_u64(),
        Some(sheep_pid),
        "the sheep was restarted rather than carried: {after}"
    );
    assert_eq!(
        dog_row["restarts"], 0,
        "the dog's restart count moved: {after}"
    );
    assert_eq!(
        sheep_row["restarts"], 0,
        "the sheep's restart count moved: {after}"
    );
    // JSON carries both populations in one undivided array, so the marker is
    // what keeps them apart here; the tables below are what an operator sees.
    assert_eq!(
        dog_row["dog"]["kind"], "built_in",
        "a carried dog that lost its marker is one `shep dogs` has lost: {after}"
    );
    assert!(
        sheep_row["dog"].is_null(),
        "a sheep must not pick a marker up on the way across: {after}"
    );

    let dogs = shep(home).arg("dogs").output().unwrap();
    assert_success(&dogs);
    assert!(
        String::from_utf8_lossy(&dogs.stdout).contains("metrics"),
        "`shep dogs` must still list the carried dog: {}",
        String::from_utf8_lossy(&dogs.stdout)
    );
    let flock = shep(home).arg("flock").output().unwrap();
    assert_success(&flock);
    let flock_text = String::from_utf8_lossy(&flock.stdout);
    let (sheep_table, _dogs_table) = flock_text
        .split_once("Dogs")
        .unwrap_or_else(|| panic!("`shep flock` prints a dogs section: {flock_text}"));
    assert!(
        !sheep_table.contains("metrics"),
        "a carried dog must not be listed beside the operator's own apps: {flock_text}"
    );

    // The decisive one. A sheep that did not exist when the predecessor was
    // running, named by the dog that is answering now.
    let fresh = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("freshsheep")
        .output()
        .unwrap();
    assert_success(&fresh);
    let body = poll_metrics_containing(addr, r#"sheep="freshsheep""#);
    assert!(
        body.contains("HTTP/1.1 200"),
        "the carried dog must still answer a scrape after the exec: {body}"
    );
    assert!(
        body.contains(r#"sheep="freshsheep""#),
        "the exposition must name a sheep started AFTER the reload, which no cached reading \
         and no connection to the predecessor could produce: {body}"
    );

    // A successor rebuilds the roll from the blob, not from disk.
    // `spawn_enabled_dogs` never touches `FlockRegistry`, so a dog in the roll
    // would outlive the daemon and a later cold boot would restore `metrics`
    // as an ordinary unmarked sheep.
    let saved = shep(home)
        .arg("--format")
        .arg("json")
        .arg("save")
        .output()
        .unwrap();
    assert_success(&saved);
    let roll: serde_json::Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(
        roll["data"]["apps"], 2,
        "the roll holds the two sheep and no dog: {roll}"
    );

    graceful_kill(home);
}

#[cfg(unix)]
/// Table format, not JSON: `Format::Json`'s `flock` answer carries both
/// populations in one undivided array.
#[test]
fn dogs_and_flock_render_the_two_populations_the_right_way_round() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(
        &dir,
        &format!("[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"),
    );
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);
    poll_flock(home, |info| info["status"] == "online");

    let enabled = shep(home).arg("enable").arg("metrics").output().unwrap();
    assert_success(&enabled);
    guard.adopt_dog_pid(wait_for_dog_pid(home, "metrics"));

    let flock_table = String::from_utf8(shep(home).arg("flock").output().unwrap().stdout).unwrap();
    assert!(
        flock_table.contains("web"),
        "shep flock must still render the sheep: {flock_table}"
    );
    assert!(
        flock_table.contains("Dogs") && flock_table.contains("metrics"),
        "shep flock must render the dogs section beneath the sheep table: {flock_table}"
    );

    let dogs_table = String::from_utf8(shep(home).arg("dogs").output().unwrap().stdout).unwrap();
    assert!(
        dogs_table.contains("metrics"),
        "shep dogs must render the dog: {dogs_table}"
    );
    assert!(
        !dogs_table.contains("web"),
        "shep dogs must render nothing but dogs — not the sheep: {dogs_table}"
    );
    assert!(
        !dogs_table.contains("Dogs\n"),
        "shep dogs must not carry flock's own section header — it IS the \
         dogs table, not a listing with one embedded: {dogs_table}"
    );

    graceful_kill(home);
}

#[test]
fn barks_reads_the_history_with_no_shepherd_running() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let bark = shep_core::barks::Bark {
        at_ms: 1_700_000_000_000,
        rule: "watchdog".to_string(),
        subject: "web".to_string(),
        message: "restart budget exhausted".to_string(),
        sinks: vec![shep_core::barks::SinkOutcome {
            sink: "ops".to_string(),
            error: None,
        }],
    };
    shep_core::barks::append(
        &home.join("barks.jsonl"),
        &bark,
        shep_core::barks::DEFAULT_MAX_BYTES,
    )
    .unwrap();
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "this case never starts a daemon at all"
    );

    let output = shep(home)
        .arg("--format")
        .arg("json")
        .arg("barks")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "`shep barks` must never autostart a shepherd either"
    );

    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["command"], "barks",
        "`shep barks` must reach the barks verb and no other: {envelope}"
    );
    let rows = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("barks data must be an array: {envelope}"));
    assert_eq!(rows.len(), 1, "{envelope}");
    assert_eq!(rows[0]["subject"], "web", "{envelope}");
    assert_eq!(rows[0]["rule"], "watchdog", "{envelope}");
}

#[cfg(unix)]
/// The whole store through the real binary, with no shepherd anywhere:
/// provisioning happens when nothing is running. Also checks the `0600` mode
/// `shep_core::kv` documents, on the store the first `set` creates.
#[test]
fn the_kv_store_works_with_no_shepherd_running() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    let set1 = shep(home)
        .arg("set")
        .arg("bark.cooldown")
        .arg("30s")
        .output()
        .unwrap();
    assert_success(&set1);
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "shep set must never autostart a shepherd"
    );

    let mode = std::fs::metadata(home.join("kv.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");

    let get1 = shep(home).arg("get").arg("bark.cooldown").output().unwrap();
    assert_success(&get1);
    assert!(
        String::from_utf8_lossy(&get1.stdout).contains("30s"),
        "{}",
        String::from_utf8_lossy(&get1.stdout)
    );

    let missing = shep(home).arg("get").arg("missing").output().unwrap();
    assert_eq!(missing.status.code(), Some(3), "NotFound; {missing:?}");

    let set2 = shep(home)
        .arg("set")
        .arg("metrics_port")
        .arg("9615")
        .output()
        .unwrap();
    assert_success(&set2);

    let both = shep(home).arg("get").output().unwrap();
    assert_success(&both);
    let both_text = String::from_utf8_lossy(&both.stdout);
    assert!(both_text.contains("bark.cooldown"), "{both_text}");
    assert!(both_text.contains("metrics_port"), "{both_text}");

    let unset1 = shep(home)
        .arg("unset")
        .arg("bark.cooldown")
        .output()
        .unwrap();
    assert_success(&unset1);

    let gone = shep(home).arg("get").arg("bark.cooldown").output().unwrap();
    assert_eq!(gone.status.code(), Some(3), "NotFound; {gone:?}");

    let unset_all = shep(home).arg("unset").arg("--all").output().unwrap();
    assert_success(&unset_all);

    let empty = shep(home).arg("get").output().unwrap();
    assert_success(&empty);
    let empty_text = String::from_utf8_lossy(&empty.stdout);
    assert!(
        !empty_text.contains("metrics_port"),
        "store must be empty after unset --all: {empty_text}"
    );

    let bad_key = shep(home)
        .arg("set")
        .arg("bad key")
        .arg("x")
        .output()
        .unwrap();
    assert_eq!(bad_key.status.code(), Some(2), "usage; {bad_key:?}");
}

/// `data` is an array of `{key, value}` objects, never a JSON map keyed by
/// name. An absent key and a key outside the grammar surface as this
/// envelope's error half.
#[test]
fn kv_json_envelope_is_an_array_with_the_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    shep(home).arg("set").arg("a").arg("1").output().unwrap();
    shep(home).arg("set").arg("b").arg("2").output().unwrap();

    let get_all = shep(home)
        .arg("--format")
        .arg("json")
        .arg("get")
        .output()
        .unwrap();
    assert_success(&get_all);
    let envelope: serde_json::Value = serde_json::from_slice(&get_all.stdout).unwrap();
    assert!(envelope["data"].is_array(), "{envelope}");
    assert_eq!(envelope["data"].as_array().unwrap().len(), 2, "{envelope}");
    assert_eq!(envelope["schema_version"], 1, "{envelope}");

    let missing = shep(home)
        .arg("--format")
        .arg("json")
        .arg("get")
        .arg("ghost")
        .output()
        .unwrap();
    assert_json_error(&missing, 3, "not_found");

    let bad_key = shep(home)
        .arg("--format")
        .arg("json")
        .arg("set")
        .arg("bad key")
        .arg("x")
        .output()
        .unwrap();
    assert_json_error(&bad_key, 2, "usage");
}

/// Two real processes, not two threads sharing one open-file-description
/// table: only separate processes contend for `kv.json.lock`'s `flock(2)`.
/// The barrier matters: without it one writer can finish its whole batch
/// before the other starts, racing nothing.
#[test]
fn two_real_shep_processes_writing_concurrently_lose_no_keys() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    const PER_WRITER: usize = 15;

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (finished, racers) = std::sync::mpsc::channel();
    for writer in 0..2 {
        let home = home.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let finished = finished.clone();
        std::thread::spawn(move || {
            barrier.wait(); // both writers start their first `shep set` together
            for n in 0..PER_WRITER {
                let key = format!("writer{writer}.k{n}");
                let output = shep(&home).arg("set").arg(&key).arg("v").output().unwrap();
                // A closed receiver means the case already gave up on this
                // writer and failed; there is no one left to report to.
                let _ = finished.send((writer, key, output));
            }
        });
    }
    drop(finished); // the writer threads hold the only senders that matter

    for _ in 0..(PER_WRITER * 2) {
        let (writer, key, output) = racers
            .recv_timeout(RACER_DEADLINE)
            .expect("a writer never came back; see RACER_DEADLINE");
        assert!(
            output.status.success(),
            "writer {writer}, key {key}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let list = shep(&home)
        .arg("--format")
        .arg("json")
        .arg("get")
        .output()
        .unwrap();
    assert_success(&list);
    let envelope: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let data = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("get data must be an array: {envelope}"));
    assert_eq!(
        data.len(),
        PER_WRITER * 2,
        "two concurrent shep set processes must not lose each other's keys: {envelope}"
    );
}

/// `assert_cmd` captures stdout through a pipe, so this is the not-a-tty
/// refusal a `shep lookout > dash.txt` meets.
#[test]
fn shep_lookout_refuses_when_stdout_is_not_a_terminal() {
    let home = TempDir::new().unwrap();
    let output = shep(home.path())
        .arg("lookout")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("needs a terminal"));
}

#[test]
fn shep_dash_is_the_same_verb() {
    let home = TempDir::new().unwrap();
    let output = shep(home.path())
        .arg("dash")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("needs a terminal")
    );
}

/// The assertion is on `security boundary` alone: `wrap_help` re-wraps long
/// help at the detected terminal width, so a longer phrase can land across a
/// line break on one machine and not another.
#[test]
fn shep_lookout_help_names_the_gate() {
    let home = TempDir::new().unwrap();
    let output = shep(home.path())
        .args(["lookout", "--help"])
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("--read-only"));
    assert!(text.contains("security boundary"));
}

// ---------------------------------------------------------------------------
// whistle: the MCP interface, driven over real pipes.
// ---------------------------------------------------------------------------

/// Serializes `value` as compact JSON followed by `\n`, the newline-delimited
/// framing `transport-io`'s codec expects on both sides of the pipe.
fn push_mcp_line(buf: &mut Vec<u8>, value: &serde_json::Value) {
    buf.extend_from_slice(value.to_string().as_bytes());
    buf.push(b'\n');
}

/// Stdin for one MCP session: the `initialize` handshake (id `1`), the
/// `notifications/initialized`, then each of `requests`. `"2025-06-18"` is a
/// `ProtocolVersion::KNOWN_VERSIONS` entry rather than `LATEST`, so an rmcp
/// bump does not redden this suite.
fn mcp_session(requests: &[serde_json::Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    push_mcp_line(
        &mut buf,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "cli_e2e", "version": "0.0.0"},
            },
        }),
    );
    push_mcp_line(
        &mut buf,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    for request in requests {
        push_mcp_line(&mut buf, request);
    }
    buf
}

/// A `tools/list` request with the given id.
fn tools_list_request(id: i64) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"})
}

/// A `tools/call` request. `arguments` is omitted rather than sent as `{}`
/// when a tool takes none, matching what a real client sends.
fn call_tool_request(
    id: i64,
    name: &str,
    arguments: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut params = serde_json::json!({"name": name});
    if let Some(args) = arguments {
        params
            .as_object_mut()
            .expect("params is always an object")
            .insert("arguments".to_string(), args);
    }
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params})
}

/// Parses every line of `stdout` as JSON-RPC, panicking with the offending
/// line otherwise. A search for the wanted reply alone would pass with a stray
/// `println!` or a tracing record on the same wire. `str::lines` yields no
/// trailing empty entry, so an empty line is one the verb wrote.
fn assert_every_stdout_line_is_jsonrpc(stdout: &[u8]) -> Vec<serde_json::Value> {
    let text = String::from_utf8(stdout.to_vec()).expect("whistle's stdout is valid UTF-8");
    text.lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("stdout line is not JSON: {err}\nline: {line}"));
            assert_eq!(
                value.get("jsonrpc").and_then(serde_json::Value::as_str),
                Some("2.0"),
                "stdout line is not JSON-RPC: {line}"
            );
            value
        })
        .collect()
}

/// The reply among `lines` whose `"id"` matches, told apart from a request or
/// notification of the same shape by carrying `"result"` or `"error"`.
fn find_reply(lines: &[serde_json::Value], id: i64) -> &serde_json::Value {
    lines
        .iter()
        .find(|line| {
            line.get("id") == Some(&serde_json::Value::from(id))
                && (line.get("result").is_some() || line.get("error").is_some())
        })
        .unwrap_or_else(|| panic!("no reply with id {id} in {lines:#?}"))
}

/// A `shep` invocation reaching `$SHEP_HOME` through the environment rather
/// than `--home`; `GlobalArgs::home` carries `env = "SHEP_HOME"`.
fn shep_via_env(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("shep").unwrap();
    cmd.env("SHEP_HOME", home).timeout(CMD_TIMEOUT);
    cmd
}

/// Drives `cmd` (already carrying `--home` or `SHEP_HOME`, not yet the
/// `whistle` argument) through an `initialize` handshake and a
/// `tools/list`, and returns the tool names the gate produced.
fn whistle_tool_names(mut cmd: Command) -> Vec<String> {
    let stdin = mcp_session(&[tools_list_request(2)]);
    let output = cmd.arg("whistle").write_stdin(stdin).output().unwrap();
    assert_success(&output);
    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);
    find_reply(&lines, 2)["result"]["tools"]
        .as_array()
        .expect("tools/list result carries a tools array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("every tool has a name")
                .to_string()
        })
        .collect()
}

/// Drives the real binary: an `initialize` and a `tools/list` request,
/// newline-delimited on stdin, replies read back from stdout. Every stdout
/// line must parse as JSON-RPC.
#[test]
fn whistle_speaks_mcp_and_writes_nothing_else_to_stdout() {
    let home = TempDir::new().unwrap();
    let stdin = mcp_session(&[tools_list_request(2)]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);

    let init_reply = find_reply(&lines, 1);
    assert_eq!(init_reply["result"]["serverInfo"]["name"], "shep");
    assert!(init_reply["result"]["capabilities"]["tools"].is_object());

    let list_reply = find_reply(&lines, 2);
    assert!(list_reply["result"]["tools"].is_array());
}

/// Three runs against two `$SHEP_HOME`s: no `[whistle]` section (five tools),
/// `allow_control = true` (nine), and that same open directory again through
/// `--home`. The split is checked by name, not only by count: a count alone
/// would pass if the gate registered a read tool twice.
#[test]
fn the_shep_toml_gate_decides_the_tool_list_in_a_real_process() {
    let control_tools = ["start_sheep", "stop_sheep", "restart_sheep", "reload_sheep"];

    let closed_home = TempDir::new().unwrap();
    let names = whistle_tool_names(shep_via_env(closed_home.path()));
    assert_eq!(names.len(), 5, "read-only: {names:?}");
    for tool in control_tools {
        assert!(
            !names.contains(&tool.to_string()),
            "{tool} must be absent: {names:?}"
        );
    }

    let open_home = TempDir::new().unwrap();
    write_shep_toml(&open_home, "[whistle]\nallow_control = true\n");

    let names = whistle_tool_names(shep_via_env(open_home.path()));
    assert_eq!(names.len(), 9, "gate open via env: {names:?}");
    for tool in control_tools {
        assert!(
            names.contains(&tool.to_string()),
            "{tool} must be present: {names:?}"
        );
    }

    let names = whistle_tool_names(shep(open_home.path()));
    assert_eq!(names.len(), 9, "gate open via --home: {names:?}");
    for tool in control_tools {
        assert!(
            names.contains(&tool.to_string()),
            "{tool} must be present: {names:?}"
        );
    }
}

/// The malformed-config notice is the only thing whistle writes outside the
/// JSON-RPC wire, and it sits next to the stdout handle. A config that fails
/// to parse leaves the gate shut.
#[test]
fn a_malformed_shep_toml_stays_off_stdout_and_keeps_the_gate_shut() {
    let home = TempDir::new().unwrap();
    write_shep_toml(&home, "[whistle\n");

    let stdin = mcp_session(&[tools_list_request(2)]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);
    let list_reply = find_reply(&lines, 2);
    let names: Vec<String> = list_reply["result"]["tools"]
        .as_array()
        .expect("tools/list result carries a tools array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("every tool has a name")
                .to_string()
        })
        .collect();

    assert_eq!(
        names.len(),
        5,
        "a broken config must read as the gate SHUT, not open: {names:?}"
    );
    for tool in ["start_sheep", "stop_sheep", "restart_sheep", "reload_sheep"] {
        assert!(
            !names.contains(&tool.to_string()),
            "{tool} must be absent when shep.toml fails to parse: {names:?}"
        );
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid_config"),
        "the malformed-config notice must reach stderr: {stderr}"
    );
    assert!(
        stderr.contains("shep.toml"),
        "the notice must name the file: {stderr}"
    );
}

/// With the gate shut, `tools/call` for `stop_sheep` answers JSON-RPC error
/// `-32602`, rmcp's answer for a name its router does not hold. A tool that
/// existed and refused would answer a `result`.
#[test]
fn a_gated_off_control_tool_is_not_merely_refused_it_is_absent() {
    let home = TempDir::new().unwrap();
    let stdin = mcp_session(&[call_tool_request(
        2,
        "stop_sheep",
        Some(serde_json::json!({"name": "api"})),
    )]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);
    let reply = find_reply(&lines, 2);
    assert!(
        reply.get("result").is_none(),
        "a gated-off tool must be a protocol error, not a result: {reply:#?}"
    );
    let error = reply
        .get("error")
        .expect("a gated-off tool call must answer a JSON-RPC error");
    assert_eq!(error["code"], -32602);
    assert_eq!(error["message"], "tool not found");
}

/// Whistle's transport is the launcher's, not the shepherd's, so it answers
/// `initialize` against a home with no daemon and no socket, and reports the
/// missing shepherd per call.
#[test]
fn whistle_starts_with_no_shepherd_and_reports_it_per_call() {
    let home = TempDir::new().unwrap();
    let stdin = mcp_session(&[call_tool_request(2, "list_flock", None)]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);

    let init_reply = find_reply(&lines, 1);
    assert_eq!(init_reply["result"]["serverInfo"]["name"], "shep");
    assert!(init_reply["result"]["capabilities"]["tools"].is_object());

    let call_reply = find_reply(&lines, 2);
    assert_eq!(call_reply["result"]["isError"], true);
    let message = call_reply["result"]["structuredContent"]["message"]
        .as_str()
        .expect("a no-shepherd refusal carries a message");
    assert!(
        message.contains("no shepherd is running"),
        "message: {message}"
    );
}

// --- Dogs / Available index -----------------------------------------------

/// Rex's description carries a raw `\u{1b}[2J` screen-clear escape. The
/// assertion is on raw stdout bytes, so a regression cannot hide behind
/// `String::from_utf8_lossy`'s replacement character.
#[test]
fn available_dogs_lists_the_index_and_never_leaks_a_raw_escape() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert_success(&output);

    assert!(
        !output.stdout.contains(&0x1b),
        "a raw escape reached stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "NAME",
        "PACKAGE",
        "CATEGORY",
        "DESCRIPTION",
        "Spot",
        "shep-log-rotate",
        "logs",
        "Rex",
        "shep-watchdog",
        "health",
    ] {
        assert!(
            stdout.contains(expected),
            "table is missing {expected:?}: {stdout}"
        );
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 entry contained control characters"),
        "stderr must note the sanitised entry: {stderr}"
    );
}

/// A dog cannot learn the name it was adopted under, so a wrong name here
/// ships a copy-pasteable command that discards its whole config section.
#[test]
fn available_dogs_detail_view_uses_adopt_as_never_name() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .arg("spot")
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Spot . shep-log-rotate . logs"),
        "detail header line: {stdout}"
    );
    assert!(
        stdout.contains("$ cargo install --git https://github.com/shep-pm/shep-log-rotate"),
        "install command: {stdout}"
    );
    assert!(
        stdout.contains("$ shep adopt ~/.cargo/bin/shep-log-rotate --name log-rotate"),
        "adopt command must use adopt_as (log-rotate), not name (Spot): {stdout}"
    );
    assert!(
        !stdout.contains("--name Spot"),
        "adopt command must never use the display name: {stdout}"
    );
}

#[test]
fn available_dogs_zero_matches_exits_zero_and_says_so() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .arg("wombat")
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no dog matches \"wombat\""),
        "stdout: {stdout}"
    );
}

/// Neither a socket nor a pidfile may exist afterwards, so an autostart is
/// caught even when the command still answers successfully.
#[test]
fn available_dogs_needs_no_shepherd() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        !home.path().join("run").join("shep.sock").exists(),
        "--available must never bring up a shepherd"
    );
    assert!(
        !home.path().join("pids").join("shepd.pid").exists(),
        "--available must never bring up a shepherd"
    );
}

/// `IndexError` carries the URL on no variant but `InsecureUrl`, so
/// `available_dogs` is what names it.
#[test]
fn available_dogs_reports_a_server_error_naming_the_url() {
    let home = TempDir::new().unwrap();
    let url = serve_raw_response(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_string(),
    );

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a 500 must not exit success: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("reading the dog index from {url}")),
        "stderr must name the failing url: {stderr}"
    );
    assert!(stderr.contains("500"), "stderr: {stderr}");
}

/// The one url `available_dogs` does not name. `SHEP_DOG_INDEX` is an
/// operator's own string, so a password can reach it, and this message is
/// built outside `fetch` and outside `IndexError` where neither refusal
/// covers it.
#[test]
fn available_dogs_names_no_url_that_carries_credentials() {
    // A sentinel per component, none of them a substring of anything the
    // message says on its own. A password redacted while the username or
    // the host it was paired with still prints is a narrower leak, not a
    // closed one.
    for url in [
        "ftp://sentineluser:hunter2@sentinelhost.invalid/dogs.json",
        // Scheme-relative, so there is no `://` to split the authority on.
        "//sentineluser:hunter2@sentinelhost.invalid/dogs.json",
        // The `@` is in a path here, so the authority predicate says no
        // and only the blunt printing rule stands between this and
        // stderr. `parse_url` withheld this url while the sentence around
        // it printed the same one, until both asked the same question.
        "file:///etc/sentineluser:hunter2@sentinelhost.invalid",
    ] {
        let home = TempDir::new().unwrap();

        let output = shep(home.path())
            .env("SHEP_DOG_INDEX", url)
            .arg("dogs")
            .arg("--available")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{url}: an unfetchable url must not exit success: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        for secret in ["hunter2", "sentineluser", "sentinelhost.invalid"] {
            assert!(
                !stderr.contains(secret),
                "{url}: stderr printed {secret}: {stderr}"
            );
        }
        assert!(
            stderr.contains("a url that may carry credentials"),
            "{url}: stderr must say why it withheld the url: {stderr}"
        );
    }
}

#[test]
fn available_dogs_reports_a_truncated_body_naming_the_url() {
    let home = TempDir::new().unwrap();
    // Declares 100 bytes of body, sends 2, then closes: `fetch::get`'s
    // `Truncated` refusal.
    let url = serve_raw_response("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n[]".to_string());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a truncated body must not exit success: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("reading the dog index from {url}")),
        "stderr must name the failing url: {stderr}"
    );
    assert!(stderr.contains("truncated"), "stderr: {stderr}");
}

// --- `shep serve` --------------------------------------------------------

#[cfg(unix)]
/// The assertion is an HTTP GET against the port, not a `shep flock` row: a
/// row says the process is up, and up is not serving.
#[test]
fn serve_registers_a_sheep_that_answers_on_its_port() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("site");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("index.html"), "hello from shep serve").unwrap();
    let mut guard = DaemonGuard::default();
    let port = free_port();

    let output = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("serve")
        .arg(&root)
        .arg("--port")
        .arg(port.to_string())
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&output);

    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let (status, body) = poll_http_get(addr, "/", &[]);
    assert_eq!(status, 200, "body={body}");
    assert!(body.contains("hello from shep serve"), "{body}");

    graceful_kill(dir.path());
}

#[cfg(unix)]
#[test]
fn serve_refuses_a_docroot_that_is_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let mut guard = DaemonGuard::default();

    let output = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("serve")
        .arg(&missing)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());

    assert_json_error(&output, 2, "usage");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&missing.display().to_string()), "{stderr}");

    // No daemon was ever spawned to register anything against, so the
    // refusal happened before any `Request::Start`.
    assert!(
        daemon_pid(dir.path()).is_none(),
        "a refused root must not even bring a shepherd up"
    );
}

#[cfg(unix)]
/// A worker that only handles SIGINT rides the kill ladder to SIGKILL on
/// every `shep stop`. [`SERVE_STOP_DEADLINE`] carries the bound's basis.
#[test]
fn a_served_sheep_stops_on_sigterm_rather_than_riding_the_ladder_to_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("site");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("index.html"), "ok").unwrap();
    let mut guard = DaemonGuard::default();
    let port = free_port();
    let name = "sigterm-check";

    let output = shep(dir.path())
        .arg("serve")
        .arg(&root)
        .arg("--port")
        .arg(port.to_string())
        .arg("--name")
        .arg(name)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&output);

    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let (status, body) = poll_http_get(addr, "/", &[]);
    assert_eq!(status, 200, "body={body}");

    let started = Instant::now();
    let stop_output = shep(dir.path()).arg("stop").arg(name).output().unwrap();
    let elapsed = started.elapsed();
    assert_success(&stop_output);
    assert!(
        elapsed < SERVE_STOP_DEADLINE,
        "shep stop took {elapsed:?}, at or past SERVE_STOP_DEADLINE ({SERVE_STOP_DEADLINE:?}); \
         a worker riding the ladder to SIGKILL takes at least the 1600ms kill_timeout default"
    );

    graceful_kill(dir.path());
}

#[cfg(unix)]
/// Layout shared by the two `--follow-symlinks` cases below: a dated release
/// directory holding `index.html`, and a `current` symlink pointing at it.
fn write_deploy_layout(root: &Path) {
    let release = root.join("releases").join("2026-08-15");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(release.join("index.html"), "the deploy layout").unwrap();
    std::os::unix::fs::symlink(&release, root.join("current")).unwrap();
}

#[cfg(unix)]
/// Registered without `--follow-symlinks`. A registered sheep is a real child
/// with its own captured stderr, which is what `shep bleats` reads.
#[test]
fn a_refused_symlink_writes_the_path_and_the_flag_to_the_sheeps_bleats() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("site");
    std::fs::create_dir(&root).unwrap();
    write_deploy_layout(&root);
    let canonical_root = root.canonicalize().unwrap();
    let mut guard = DaemonGuard::default();
    let port = free_port();
    let name = "symlink-refused";

    let output = shep(dir.path())
        .arg("serve")
        .arg(&root)
        .arg("--port")
        .arg(port.to_string())
        .arg("--name")
        .arg(name)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&output);

    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let (status, body) = poll_http_get(addr, "/current/index.html", &[]);
    assert_eq!(status, 404, "body={body}");

    let bleats_output = bleats_no_follow_until_written(dir.path(), &[name, "--err"]);
    let bleats = String::from_utf8_lossy(&bleats_output.stdout);
    assert!(
        bleats.contains(&canonical_root.join("current").display().to_string()),
        "{bleats}"
    );
    assert!(bleats.contains("--follow-symlinks"), "{bleats}");

    graceful_kill(dir.path());
}

#[cfg(unix)]
/// One scenario: the flag that makes the deploy layout work is the flag
/// `follow_symlinks_notice` announces.
#[test]
fn a_served_sheep_with_follow_symlinks_serves_the_deploy_layout_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("site");
    std::fs::create_dir(&root).unwrap();
    write_deploy_layout(&root);
    let mut guard = DaemonGuard::default();
    let port = free_port();
    let name = "symlink-followed";

    let output = shep(dir.path())
        .arg("serve")
        .arg(&root)
        .arg("--port")
        .arg(port.to_string())
        .arg("--name")
        .arg(name)
        .arg("--follow-symlinks")
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&output);

    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let (status, body) = poll_http_get(addr, "/current/index.html", &[]);
    assert_eq!(status, 200, "body={body}");
    assert!(body.contains("the deploy layout"), "{body}");

    let bleats_output = bleats_no_follow_until_written(dir.path(), &[name, "--err"]);
    let bleats = String::from_utf8_lossy(&bleats_output.stdout);
    assert!(bleats.contains("--follow-symlinks"), "{bleats}");
    assert!(
        bleats.contains("race") || bleats.contains("TOCTOU"),
        "{bleats}"
    );

    graceful_kill(dir.path());
}

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

// --- `shep dev` -------------------------------------------------------

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

// --- Piped stdout stays bare ----------------------------------------------

/// Asserts no ANSI escape byte and none of the box-drawing glyphs
/// `render_boxed` draws.
fn assert_no_box_or_escape_reached_the_pipe(stdout: &str, verb: &str) {
    assert!(
        !stdout.contains('\u{1b}'),
        "shep {verb} piped: an escape byte reached a pipe: {stdout:?}"
    );
    for glyph in ['┌', '┬', '┐', '├', '┼', '┤', '└', '┴', '┘', '│', '─'] {
        assert!(
            !stdout.contains(glyph),
            "shep {verb} piped: a box-drawing glyph ({glyph:?}) reached a pipe:\n{stdout}"
        );
    }
}

#[cfg(unix)]
/// The only place in the suite a table verb runs with no `--format json` and
/// no `--style`. `.output()` captures stdout through an OS pipe, never a pty,
/// so `std::io::stdout().is_terminal()` is `false`, which is
/// `must_render_bare`'s trigger. Two verbs, since `emit_flock` and
/// `emit_described` wrap `table_of` separately and a regression scoped to one
/// would pass a case trying the other.
#[test]
fn piped_table_output_at_the_default_style_carries_no_box_or_escape() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("--format")
        .arg("json")
        .arg("start")
        .arg(&script)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);
    let envelope: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(envelope["data"][0]["status"], "online", "{envelope}");

    let flock = shep(dir.path()).arg("flock").output().unwrap();
    assert_success(&flock);
    let flock_stdout = String::from_utf8_lossy(&flock.stdout).into_owned();
    assert_no_box_or_escape_reached_the_pipe(&flock_stdout, "flock");
    assert!(
        flock_stdout.contains("online"),
        "precondition: the piped table must still say something: {flock_stdout}"
    );

    let describe = shep(dir.path())
        .arg("describe")
        .arg("all")
        .output()
        .unwrap();
    assert_success(&describe);
    let describe_stdout = String::from_utf8_lossy(&describe.stdout).into_owned();
    assert_no_box_or_escape_reached_the_pipe(&describe_stdout, "describe");
    assert!(
        describe_stdout.contains("online"),
        "precondition: the piped table must still say something: {describe_stdout}"
    );

    graceful_kill(dir.path());
}

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
mod constants;
mod daemon_log;
mod fixtures;
mod guard;
mod home_and_watch;
mod lifecycle;
mod logs;
mod polling;
mod real_clock;

pub(crate) use assertions::*;
pub(crate) use constants::*;
pub(crate) use fixtures::*;
pub(crate) use guard::*;
pub(crate) use polling::*;
