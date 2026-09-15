//! A piped table stays bare: no box drawing, no escape.

use super::*;

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
