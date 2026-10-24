//! `shep delete` on a dog, end to end: it stays gone across a shepherd
//! restart, a built-in is refused by the running shepherd, and the hidden
//! `rehome` still works while leaving `--help`.

use super::*;

/// Writes a `/bin/sh` dog that answers both probe flags and then runs until
/// it is stopped, recording its pid the way every fixture script does.
#[cfg(unix)]
fn write_dog(dir: &TempDir, name: &str) -> PathBuf {
    let body = format!(
        r#"#!/bin/sh
case "$1" in
  --version) printf 'shep-fixture 0.1.0\nshep-protocol: {protocol}\n'; exit 0 ;;
  --*) exit 0 ;;
esac
{record}trap 'exit 0' TERM
while :; do sleep 1 & wait $!; done
"#,
        protocol = shep_core::protocol::PROTOCOL_VERSION,
        record = record_pid_line(dir),
    );
    write_script(dir, &format!("{name}.sh"), &body)
}

/// Brings a shepherd up over an empty home, so `adopt` has one to register
/// the dog with and start it in.
#[cfg(unix)]
fn mustered_shepherd(home: &Path, guard: &mut DaemonGuard) {
    let mustered = shep(home)
        .arg("muster")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&mustered);
}

#[cfg(unix)]
/// The acceptance criterion, run against a real shepherd and a real dog
/// binary: `delete` is the verb that removes a dog, and the next shepherd
/// does not bring it back.
///
/// `shep daemon reload` is the restart, because it is the one an operator
/// reaches for after `cargo install shep`, and it is the restart that would
/// re-read `shep.toml` and re-spawn anything still enabled there.
#[test]
fn deleting_an_adopted_dog_keeps_it_gone_across_a_shepherd_restart() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let dog = write_dog(&dir, "log-rotate");
    std::fs::write(home.join("dogs.toml"), "[log-rotate]\ndebounce = \"30s\"\n").unwrap();
    let mut guard = DaemonGuard::default();
    mustered_shepherd(home, &mut guard);

    let adopted = shep(home)
        .arg("adopt")
        .arg("--yes")
        .arg(&dog)
        .arg("--name")
        .arg("log-rotate")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_success(&adopted);

    let listed = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "log-rotate"))
    });
    assert!(
        listed
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "log-rotate")),
        "the adopted dog never reached the flock: {listed}"
    );

    let deleted = shep(home)
        .arg("delete")
        .arg("log-rotate")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_success(&deleted);
    assert!(
        String::from_utf8_lossy(&deleted.stderr).contains("kept [log-rotate] in dogs.toml"),
        "the settings that survive a dog's removal are named: {}",
        String::from_utf8_lossy(&deleted.stderr)
    );

    let gone = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| !rows.iter().any(|row| row["name"] == "log-rotate"))
    });
    assert!(
        gone.as_array()
            .is_some_and(|rows| !rows.iter().any(|row| row["name"] == "log-rotate")),
        "the shepherd kept the entry: {gone}"
    );

    let reloaded = shep(home)
        .arg("daemon")
        .arg("reload")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_success(&reloaded);

    // `metrics` is listed at rest on any home the shepherd booted, so its
    // row is the proof the successor answered.
    let after = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "metrics"))
    });
    assert!(
        after
            .as_array()
            .is_some_and(|rows| !rows.iter().any(|row| row["name"] == "log-rotate")),
        "the successor re-adopted a dog the operator deleted: {after}"
    );

    let written = std::fs::read_to_string(home.join("shep.toml")).unwrap();
    assert!(
        !written.contains("log-rotate"),
        "the adoption is what a boot re-spawns from: {written}"
    );
    let kept = std::fs::read_to_string(home.join("dogs.toml")).unwrap();
    assert!(
        kept.contains("[log-rotate]") && kept.contains("debounce"),
        "the section an operator wrote is theirs, not the adoption's: {kept}"
    );

    graceful_kill(home);
}

#[cfg(unix)]
/// The refusal lives in the shepherd, so a CLI that has not been upgraded
/// still cannot deregister a built-in dog.
#[test]
fn deleting_a_built_in_dog_is_refused_by_the_shepherd() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let mut guard = DaemonGuard::default();
    mustered_shepherd(home, &mut guard);

    let deleted = shep(home)
        .arg("delete")
        .arg("metrics")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert!(
        !deleted.status.success(),
        "a built-in dog is not deletable: {}",
        String::from_utf8_lossy(&deleted.stdout)
    );
    let said = String::from_utf8_lossy(&deleted.stderr);
    assert!(said.contains("metrics is built in"), "{said}");
    assert!(said.contains("shep disable metrics"), "{said}");

    let listing = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "metrics"))
    });
    assert!(
        listing
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "metrics")),
        "the refused dog is still listed: {listing}"
    );

    graceful_kill(home);
}

#[cfg(unix)]
/// `rehome` is off the menu and still on the wire: same removal, its own
/// `command`, so a script written against it keeps working.
#[test]
fn rehome_still_removes_a_dog_and_answers_under_its_own_command() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let dog = write_dog(&dir, "otel");
    let mut guard = DaemonGuard::default();
    mustered_shepherd(home, &mut guard);

    let adopted = shep(home)
        .arg("adopt")
        .arg("--yes")
        .arg(&dog)
        .arg("--name")
        .arg("otel")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_success(&adopted);

    let rehomed = shep(home)
        .arg("--format")
        .arg("json")
        .arg("rehome")
        .arg("otel")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_success(&rehomed);
    let envelope: serde_json::Value = serde_json::from_slice(&rehomed.stdout).unwrap();
    assert_eq!(envelope["command"], "rehome", "{envelope}");
    assert_eq!(envelope["data"]["name"], "otel", "{envelope}");

    let gone = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| !rows.iter().any(|row| row["name"] == "otel"))
    });
    assert!(
        gone.as_array()
            .is_some_and(|rows| !rows.iter().any(|row| row["name"] == "otel")),
        "rehome is still the removal it was: {gone}"
    );
    let written = std::fs::read_to_string(home.join("shep.toml")).unwrap();
    assert!(!written.contains("otel"), "{written}");

    graceful_kill(home);
}

/// `rehome` leaves `--help`, and `delete` is where an operator looks.
#[test]
fn rehome_is_absent_from_help() {
    let dir = tempfile::tempdir().unwrap();
    let help = shep(dir.path())
        .arg("--help")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_success(&help);
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(!text.contains("rehome"), "{text}");
    assert!(text.contains("delete"), "{text}");
}
