//! Dogs through `shep daemon reload`: which ones the successor inherits, and
//! which ones it registers for itself.
//!
//! A dog's enablement lives in `shep.toml`, which only a booting shepherd
//! reads. A parked dog therefore crosses the `execve` as nothing at all and
//! comes back the way a cold boot makes it: registered at rest, under a new id.

use super::*;

/// `[dog.metrics] bind` on a port of this case's own, so a real metrics dog
/// never collides with a `9615` another test is holding.
fn metrics_toml(port: u16) -> String {
    format!(
        "[daemon]\nenabled_dogs = [\"metrics\"]\n\n[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"
    )
}

/// The row `name` holds in a `shep flock --format json` listing that includes
/// the dogs.
fn row(data: &serde_json::Value, name: &str) -> serde_json::Value {
    data.as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == name).cloned())
        .unwrap_or_else(|| panic!("no {name} in the flock listing: {data}"))
}

/// `shep disable metrics`, then a reload. The successor lists the dog at rest,
/// marked, and under an id of its own: the parked dog never entered the blob,
/// so the id it had died with the predecessor.
///
/// The sheep keeping its pid is what makes this a handover rather than a
/// stop-and-start: the stop arm would have re-registered every dog from
/// `shep.toml` anyway and proved nothing about the blob.
///
/// Unix only, as the whole handover is: Windows has no `execve`.
#[cfg(unix)]
#[test]
fn a_disabled_dog_comes_back_at_rest_under_a_new_id_after_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(&dir, &metrics_toml(port));
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("kept")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let dog_pid = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(dog_pid);
    let before = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        row(data, "kept")["status"] == "online" && row(data, "metrics")["status"] == "online"
    });
    let kept_pid = row(&before, "kept")["pid"]
        .as_u64()
        .expect("an online sheep reports a pid");
    let metrics_id = row(&before, "metrics")["id"]
        .as_u64()
        .expect("a listed dog has an id");

    let disabled = shep(home).args(["disable", "metrics"]).output().unwrap();
    assert_success(&disabled);
    poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        row(data, "metrics")["status"] == "stopped"
    });

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);

    let after = poll_flock_listing(home, HANDOVER_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 3
                && row(data, "kept")["status"] == "online"
                && row(data, "metrics")["status"] == "stopped"
                && row(data, "bark")["status"] == "stopped"
        })
    });
    assert_eq!(
        row(&after, "kept")["pid"].as_u64(),
        Some(kept_pid),
        "`kept` was respawned, so this reload took the stop arm and never handed anything over: \
         {after}"
    );
    let metrics = row(&after, "metrics");
    assert_eq!(metrics["status"], "stopped", "{after}");
    assert_eq!(metrics["disabled"].as_bool(), Some(true), "{after}");
    assert!(metrics["pid"].is_null(), "{metrics}");
    assert_ne!(
        metrics["id"].as_u64(),
        Some(metrics_id),
        "a dog the successor registers from `shep.toml` takes the next id it has, not the one \
         the predecessor issued"
    );
    assert_eq!(
        metrics["dog"]["kind"].as_str(),
        Some("built_in"),
        "a dog registered at rest is still listed as a dog: {metrics}"
    );

    graceful_kill(home);
}

/// A `/bin/sh` dog that answers `--version` asking for the shepherd channel,
/// so `shep adopt` records it in `channel_dogs` and its row carries
/// `DogSource::Adopted { channel: true }`.
#[cfg(unix)]
fn write_channel_dog(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "shep-kelpie.sh",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) printf 'shep-fixture 0.1.0\\nshep-channel: true\\n'; exit 0 ;;\n  --schema) printf '%s\\n' '{{\"type\":\"object\",\"properties\":{{\"ratio\":{{\"type\":\"number\"}}}}}}'; exit 0 ;;\n  --*) exit 0 ;;\nesac\n{}while :; do sleep 1 & wait $!; done\n",
            record_pid_line(dir)
        ),
    )
}

/// The carried PID distinguishes handover from registration after a stop-and-start.
///
/// Lookout uses the row's adopted path to enable env rows.
/// Unix only: Windows has no `execve`.
#[cfg(unix)]
#[test]
fn an_adopted_channel_dogs_pane_draws_env_before_and_after_handover() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let dog = write_channel_dog(&dir);
    write_shep_toml(
        &dir,
        &format!(
            "[daemon]\nenabled_dogs = [\"kelpie\"]\nchannel_dogs = [\"kelpie\"]\n\n[daemon.adopted_dogs]\nkelpie = \"{}\"\n",
            dog.display()
        ),
    );
    let mut guard = DaemonGuard::default();

    let mustered = shep(home).arg("muster").output().unwrap();
    guard.adopt_home(home);
    assert_success(&mustered);

    let before = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["name"] == "kelpie" && !row["pid"].is_null())
        })
    });
    let kelpie = row(&before, "kelpie");
    let pid = kelpie["pid"].as_u64().expect("an online dog reports a pid");
    guard.adopt_dog_pid(nix::unistd::Pid::from_raw(
        i32::try_from(pid).expect("a real OS pid fits i32"),
    ));
    assert_eq!(
        (
            kelpie["dog"]["kind"].as_str(),
            kelpie["dog"]["path"].as_str(),
            kelpie["dog"]["channel"].as_bool(),
        ),
        (
            Some("adopted"),
            Some(dog.display().to_string().as_str()),
            Some(true)
        ),
        "a bare `[daemon.adopted_dogs]` path and a name in `channel_dogs` is an adopted \
         channel dog: {kelpie}"
    );
    super::lookout_terminal::assert_adopted_dog_pane_draws_env(home);

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);

    let after = poll_flock_listing(home, HANDOVER_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["name"] == "kelpie" && row["dog"]["kind"].as_str() == Some("adopted")
            })
        })
    });
    let carried = row(&after, "kelpie");
    assert_eq!(
        carried["pid"].as_u64(),
        Some(pid),
        "`kelpie` was respawned, so this reload fell back to a stop-and-start and never \
         handed the dog over: {after}"
    );
    assert_eq!(
        (
            carried["dog"]["kind"].as_str(),
            carried["dog"]["path"].as_str(),
            carried["dog"]["channel"].as_bool(),
        ),
        (
            Some("adopted"),
            Some(dog.display().to_string().as_str()),
            Some(true)
        ),
        "the successor supervises the carried dog as the same adopted dog: {carried}"
    );
    super::lookout_terminal::assert_adopted_dog_pane_draws_env(home);

    graceful_kill(home);
}

/// The other way a dog ends up unenabled: `shep.toml` edited by hand while the
/// dog runs. The predecessor booted while the name was there and carried the
/// dog across the `execve`, so the successor has a running dog its own config
/// says nothing about. It stops it through the same door `shep disable` uses,
/// marks it, and keeps the row rather than replacing it.
#[cfg(unix)]
#[test]
fn a_carried_dog_a_hand_edit_dropped_from_enabled_dogs_is_stopped_and_marked() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(&dir, &metrics_toml(port));
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let boot = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("kept")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let dog_pid = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(dog_pid);
    let before = poll_flock_listing(home, FLOCK_DEADLINE, |data| {
        row(data, "kept")["status"] == "online" && row(data, "metrics")["status"] == "online"
    });
    let kept_pid = row(&before, "kept")["pid"]
        .as_u64()
        .expect("an online sheep reports a pid");
    let metrics_id = row(&before, "metrics")["id"]
        .as_u64()
        .expect("a listed dog has an id");

    // The hand edit: `enabled_dogs` is the only line that decides whether a
    // dog is enabled, so the edit removes that line and changes nothing else.
    // Writing the file from scratch would put `[dog.metrics]` back in
    // `shep.toml`, which the first boot already moved to `dogs.toml`, and a
    // dog configured in both files refuses the boot.
    let shep_toml = home.join("shep.toml");
    let edited = std::fs::read_to_string(&shep_toml)
        .unwrap()
        .lines()
        .filter(|line| !line.trim_start().starts_with("enabled_dogs"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&shep_toml, format!("{edited}\n")).unwrap();

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);

    let after = poll_flock_listing(home, HANDOVER_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 3
                && row(data, "kept")["status"] == "online"
                && row(data, "metrics")["status"] == "stopped"
        })
    });
    assert_eq!(
        row(&after, "kept")["pid"].as_u64(),
        Some(kept_pid),
        "`kept` was respawned, so this reload took the stop arm and never handed the dog over: \
         {after}"
    );
    let metrics = row(&after, "metrics");
    assert_eq!(metrics["status"], "stopped", "{after}");
    assert_eq!(metrics["disabled"].as_bool(), Some(true), "{after}");
    assert!(metrics["pid"].is_null(), "{metrics}");
    assert_eq!(
        metrics["id"].as_u64(),
        Some(metrics_id),
        "the successor stops the row it inherited rather than issuing a second one"
    );
    assert!(
        nix::sys::signal::kill(dog_pid, None).is_err(),
        "the process that crossed the exec must be gone, not merely unlisted"
    );

    // The way back is the ordinary one.
    let enabled = shep(home)
        .arg("enable")
        .arg("--yes")
        .arg("metrics")
        .output()
        .unwrap();
    assert_success(&enabled);
    let revived = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(revived);

    graceful_kill(home);
}
