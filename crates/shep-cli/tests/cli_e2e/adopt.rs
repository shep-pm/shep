//! `shep adopt`, and what `shep <name>` dispatches to once a dog is
//! adopted.

use super::*;

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
