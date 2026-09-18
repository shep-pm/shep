//! The verbs that reach a running daemon and change something: reload,
//! trigger, signal, stock, describe, save and muster.

use super::*;

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
