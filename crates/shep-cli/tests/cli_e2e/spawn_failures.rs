//! A sheep that cannot spawn, reported the same way whether it was named
//! or given a path.

use super::*;

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
