//! A dog's own connection through `shep daemon reload`: the dog neither
//! reconnects nor goes unattributed.

use super::*;

/// A `/bin/sh` dog that says `Hello` on its fd 4 once, pings every fifth of a
/// second from then on, and appends a line to `$SHEP_HOME/replies` for every
/// frame the shepherd writes back.
///
/// The replies are read by a background loop, so a ping unanswered because it
/// was in flight at the `execve` costs one line and not the dog's loop. The
/// dog never reopens fd 4: a reply after the reload can only have come back on
/// the connection it was spawned with.
#[cfg(unix)]
fn write_dog(dir: &TempDir) -> PathBuf {
    let hello = format!(
        r#"{{"client_version":"test","protocol":{}}}"#,
        shep_core::protocol::PROTOCOL_VERSION
    );
    let ping = r#"{"id":1,"deadline_ms":null,"body":{"kind":"ping"}}"#;
    let frame = |json: &str| {
        format!(
            "printf '\\000\\000\\000\\{:03o}%s' '{json}' >&4",
            json.len()
        )
    };
    let body = format!(
        r#"#!/bin/sh
case "$1" in
  --*) exit 0 ;;
esac
read_frame() {{
  n=$(dd bs=1 count=4 <&4 2>/dev/null | od -An -tu1 | awk '{{print $4}}')
  [ -n "$n" ] && dd bs=1 count="$n" <&4 2>/dev/null
}}
{hello}
( while read_frame >/dev/null; do echo reply >> "$SHEP_HOME/replies"; done ) &
while :; do
  {ping}
  sleep 0.2
done
"#,
        hello = frame(&hello),
        ping = frame(ping),
    );
    write_script(dir, "linkdog.sh", &body)
}

/// How many lines the dog has recorded so far.
#[cfg(unix)]
fn replies(home: &Path) -> usize {
    std::fs::read_to_string(home.join("replies")).map_or(0, |text| text.lines().count())
}

/// Waits until the dog has recorded `count` more lines than `since`.
#[cfg(unix)]
fn wait_for_replies_past(home: &Path, since: usize, count: usize) {
    let start = Instant::now();
    while replies(home) < since + count {
        assert!(
            start.elapsed() < HANDOVER_DEADLINE,
            "the dog heard {} replies, wanted {} more than the {since} it had: its connection \
             did not survive",
            replies(home),
            count
        );
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// The dog is written to in a loop, so the proof is a count: replies keep
/// arriving after the reload, to a process that never reopened its fd 4, and
/// the shepherd narrates its requests again into the dog's log because the
/// successor attributes the connection afresh.
///
/// The dog keeping its pid and `restarts` at zero is what makes this a
/// handover rather than a restart that would have given it a new connection
/// by respawning it.
#[cfg(unix)]
#[test]
fn a_dog_keeps_its_own_connection_and_its_attribution_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let dog = write_dog(&dir);
    let mut guard = DaemonGuard::default();

    let mustered = shep(home).arg("muster").output().unwrap();
    guard.adopt_home(home);
    assert_success(&mustered);
    let adopted = shep(home)
        .arg("adopt")
        .arg("--yes")
        .arg(&dog)
        .arg("--name")
        .arg("linkdog")
        .output()
        .unwrap();
    assert_success(&adopted);

    let pid = wait_for_dog_pid(home, "linkdog");
    guard.adopt_dog_pid(pid);
    wait_for_replies_past(home, 0, 3);

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);
    let after_reload = poll_flock_listing(home, HANDOVER_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "linkdog"))
    });
    let dog_row = after_reload
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "linkdog"))
        .expect("the listing was polled until it held the dog");
    assert_eq!(
        dog_row["pid"].as_i64(),
        Some(i64::from(pid.as_raw())),
        "the dog was respawned, so this reload never handed its connection over: {dog_row}"
    );
    assert_eq!(dog_row["restarts"].as_u64(), Some(0), "{dog_row}");

    // The reload has returned, and the successor is serving: a count taken
    // now and a count taken later differ only by what the successor answered.
    let before = replies(home);
    wait_for_replies_past(home, before, 3);

    let log = dog_row["err_file"]
        .as_str()
        .expect("a dog lists the log its shepherd narrates into");
    let narrated = std::fs::read_to_string(log)
        .unwrap()
        .matches("this dog sent shep a `ping` request on its own connection")
        .count();
    assert_eq!(
        narrated, 2,
        "once from each shepherd: the successor attributes the dog's requests on the connection \
         it inherited"
    );

    graceful_kill(home);
}
