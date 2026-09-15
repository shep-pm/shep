//! `shep init` and what it writes, refuses and replaces.

use super::*;

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
