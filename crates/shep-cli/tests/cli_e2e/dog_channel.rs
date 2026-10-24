//! A dog that asks for the shepherd channel, end to end: the ask in its
//! `--version` answer, the record `shep adopt` writes, a trigger it answers,
//! and the message it is stopped with. A dog that does not ask is the control.

use super::*;

/// Writes a `/bin/sh` dog that answers both probe flags, asking for the
/// channel only when `asks`, and records how its supervised run was stopped:
/// `$SHEP_HOME/<name>.message` for the shutdown message on fd 3, and
/// `$SHEP_HOME/<name>.signal` for a stop signal.
#[cfg(unix)]
fn write_dog(dir: &TempDir, name: &str, asks: bool) -> PathBuf {
    let body = format!(
        r#"#!/bin/sh
case "$1" in
  --version) printf 'shep-fixture 0.1.0\n{ask}'; exit 0 ;;
  --*) exit 0 ;;
esac
{record}trap ': > "$SHEP_HOME/{name}.signal"; exit 0' TERM
if [ -n "$SHEP_CHANNEL_FD" ]; then
  while IFS= read -r line <&3; do
    case "$line" in
      *'"shutdown"'*) : > "$SHEP_HOME/{name}.message"; exit 0 ;;
      *) printf '{{"kind":"action-reply","action":"gc","body":"swept"}}\n' >&3 ;;
    esac
  done
fi
while :; do sleep 1 & wait $!; done
"#,
        ask = if asks { r"shep-channel: true\n" } else { "" },
        record = record_pid_line(dir),
    );
    write_script(dir, &format!("{name}.sh"), &body)
}

/// `shep trigger <name> gc` as JSON, asserting only that the RPC itself
/// succeeded: a row's outcome is never a request failure.
#[cfg(unix)]
fn trigger_gc(home: &Path, name: &str) -> serde_json::Value {
    let triggered = shep(home)
        .arg("--format")
        .arg("json")
        .arg("trigger")
        .arg(name)
        .arg("gc")
        .output()
        .unwrap();
    assert_success(&triggered);
    serde_json::from_slice(&triggered.stdout).unwrap()
}

/// Both dogs go through the live-shepherd half of `shep adopt`, so the ask
/// travels from a real probe answer through `shep.toml` and `EnableDog` to
/// the spawn. A silence restart cannot fake a pass: it stops `otel` by the
/// same message and `quiet` by the same signal.
#[cfg(unix)]
#[test]
fn an_adopted_dog_that_asks_takes_a_trigger_and_stops_on_the_shutdown_message() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let asking = write_dog(&dir, "otel", true);
    let quiet = write_dog(&dir, "quiet", false);
    let mut guard = DaemonGuard::default();

    let mustered = shep(home).arg("muster").output().unwrap();
    guard.adopt_home(home);
    assert_success(&mustered);
    for (script, name) in [(&asking, "otel"), (&quiet, "quiet")] {
        let adopted = shep(home)
            .arg("adopt")
            .arg("--yes")
            .arg(script)
            .arg("--name")
            .arg(name)
            .output()
            .unwrap();
        assert_success(&adopted);
    }
    let written = std::fs::read_to_string(home.join("shep.toml")).unwrap();
    let recorded = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(
        recorded.daemon.channel_dogs,
        vec!["otel".to_string()],
        "only the dog that asked is recorded: {written}"
    );

    let answered = trigger_gc(home, "otel");
    assert_eq!(
        answered["data"][0]["outcome"],
        serde_json::json!({"kind": "replied", "body": "swept"}),
        "{answered}"
    );
    let refused = trigger_gc(home, "quiet");
    assert_eq!(
        refused["data"][0]["outcome"]["kind"], "dog_no_channel",
        "{refused}"
    );

    for name in ["otel", "quiet"] {
        assert_success(&shep(home).arg("disable").arg(name).output().unwrap());
    }
    assert!(home.join("otel.message").exists(), "stopped by the message");
    assert!(
        !home.join("otel.signal").exists(),
        "a dog that asked is never signalled while it obeys the message"
    );
    assert!(home.join("quiet.signal").exists(), "stopped by its signal");
    assert!(
        !home.join("quiet.message").exists(),
        "a dog that did not ask has no channel to hear a message on"
    );

    graceful_kill(home);
}
