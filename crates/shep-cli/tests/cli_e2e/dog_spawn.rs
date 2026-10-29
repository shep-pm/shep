//! A refused dog spawn, fixed before a daemon reload and enabled again.

#![cfg(unix)]

use super::*;

#[test]
fn enable_recovers_a_refused_spawn_after_the_entry_is_fixed_and_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let dog = write_script(
        &dir,
        "kelpie",
        &format!(
            "#!/bin/sh\ncase \"$1\" in --*) exit 0 ;; esac\n{}exec sleep 300\n",
            record_pid_line(&dir),
        ),
    );
    let missing = "{{secret:MISSING}}";
    let config = format!(
        "[daemon]\nenabled_dogs = []\n[daemon.adopted_dogs.kelpie]\npath = '{}'\nenv = {{ TOKEN = '{missing}', PRIVATE = 'never-print-this-value' }}\n",
        dog.display(),
    );
    std::fs::write(home.join("shep.toml"), config).unwrap();
    let mut guard = DaemonGuard::default();
    let boot = shep(home).arg("muster").output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    for _ in 0..2 {
        let enabled = shep(home)
            .args(["--format", "json", "enable", "--yes", "kelpie"])
            .output()
            .unwrap();
        assert_eq!(enabled.status.code(), Some(7));
        assert!(enabled.stdout.is_empty());
        // `--yes` prints the permissions notice first; the error is the last line.
        let stderr = String::from_utf8_lossy(&enabled.stderr);
        let error: serde_json::Value =
            serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        assert_eq!(error["error"]["code"], "spawn_failed", "{error}");
        let described = shep(home)
            .args(["--format", "json", "describe", "kelpie"])
            .output()
            .unwrap();
        assert_success(&described);
        let json: serde_json::Value = serde_json::from_slice(&described.stdout).unwrap();
        let row = &json["data"][0];
        assert_eq!(row["status"], "errored");
        let reason = row["spawn_failure"]
            .as_str()
            .expect("a refused spawn has a reason");
        assert!(reason.contains("MISSING"), "{reason}");
        assert!(error["error"]["message"].as_str().unwrap().contains(reason));
        let table = shep(home).args(["describe", "kelpie"]).output().unwrap();
        assert_success(&table);
        assert!(String::from_utf8_lossy(&table.stdout).contains(reason));
        for bytes in [&enabled.stderr, &described.stdout, &table.stdout] {
            assert!(!String::from_utf8_lossy(bytes).contains("never-print-this-value"));
        }
    }

    let config = std::fs::read_to_string(home.join("shep.toml")).unwrap();
    std::fs::write(home.join("shep.toml"), config.replace(missing, "fixed")).unwrap();
    let reloaded = shep(home).args(["daemon", "reload"]).output().unwrap();
    assert_success(&reloaded);
    let enabled = shep(home)
        .args(["--format", "json", "enable", "--yes", "kelpie"])
        .output()
        .unwrap();
    assert_success(&enabled);
    let listed = poll_flock_listing(home, FLOCK_DEADLINE, |rows| {
        rows.as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "kelpie" && row["status"] == "online")
    });
    let dog = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "kelpie")
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(dog["pid"].as_i64().unwrap() as i32);
    guard.adopt_dog_pid(pid);
    assert!(dog.get("spawn_failure").is_none(), "{dog}");

    assert_success(
        &shep(home)
            .args(["enable", "--yes", "kelpie"])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_for_dog_pid(home, "kelpie"), pid);
    graceful_kill(home);
}
