//! Real dogs answering real scrapes, the bark history, and the KV store,
//! including two shep processes writing it at once.

use super::*;

#[cfg(unix)]
/// The only tier that exec's `shep dog metrics`: every other scripts the
/// runner or fakes the client.
///
/// Four things fail here as a refused connection: the dog being spawned, its
/// reaching the socket from `$SHEP_HOME`, its fetching its own
/// `[dog.metrics]` section, and its bind.
#[test]
fn a_real_shepherd_runs_a_real_metrics_dog_that_answers_a_scrape() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(
        &dir,
        &format!("[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"),
    );
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);

    let online = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(
        online["status"], "online",
        "the sheep must reach online before the dog's own exposition has \
         anything real to name: {online}"
    );

    let enabled = shep(home).arg("enable").arg("metrics").output().unwrap();
    assert_success(&enabled);

    // Registered before the scrape: one that hangs or panics on an assertion
    // must not leak the grandchild the daemon just spawned.
    let dog_pid = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(dog_pid);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let body = poll_metrics(addr);
    assert!(
        body.contains("HTTP/1.1 200"),
        "the metrics dog must answer 200 at /metrics: {body}"
    );
    assert!(
        body.contains(r#"shep_sheep_status{sheep="web",id="0",fold="",status="online"} 1"#),
        "the exposition must name the sheep, online: {body}"
    );
    assert!(
        body.contains(r#"shep_dog_up{dog="metrics",source="built-in"} 1"#),
        "the dog must report itself up while it is the one serving the \
         scrape that says so: {body}"
    );

    graceful_kill(home);
}

#[cfg(unix)]
/// [`poll_metrics`], retried until the exposition contains `needle` rather
/// than merely until it answers: a dog answering from a cached reading still
/// answers 200, so only content the predecessor never saw tells them apart.
fn poll_metrics_containing(addr: std::net::SocketAddr, needle: &str) -> String {
    let start = Instant::now();
    let mut last = String::new();
    loop {
        if let Ok(body) = scrape_metrics(addr) {
            if body.contains(needle) {
                return body;
            }
            last = body;
        }
        if start.elapsed() >= METRICS_SCRAPE_DEADLINE {
            return last;
        }
        std::thread::sleep(METRICS_SCRAPE_POLL_INTERVAL);
    }
}

#[cfg(unix)]
/// A real dog process, carried across a real `execve`, still able to talk to
/// the shepherd that replaced the one it handshook with.
///
/// A pid check cannot see the defect: a dog holding a dead socket reads as
/// healthy on every column a listing has. The decisive assertion is content, a
/// sheep started after the reload appearing in the exposition.
#[test]
fn a_carried_dog_answers_a_scrape_after_a_real_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(
        &dir,
        &format!("[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"),
    );
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);
    let online = poll_flock(home, |info| info["status"] == "online");
    let sheep_pid = online["pid"].as_u64().expect("an online sheep has a pid");

    let enabled = shep(home).arg("enable").arg("metrics").output().unwrap();
    assert_success(&enabled);
    let dog_pid = wait_for_dog_pid(home, "metrics");
    guard.adopt_dog_pid(dog_pid);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let before = poll_metrics(addr);
    assert!(
        before.contains("HTTP/1.1 200"),
        "the dog must be answering BEFORE the reload, or this case proves nothing: {before}"
    );

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&reloaded.stdout),
        String::from_utf8_lossy(&reloaded.stderr)
    );
    // The exact sentence `handover::RefusedReason`'s `Display` ends with: the
    // stop arm restarts every dog from disk and would satisfy a looser probe.
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "a flock with a dog in it is carried now, not refused: {text}"
    );
    assert!(
        !text.contains("cannot talk to this shepherd"),
        "the carried dog must reconnect rather than be reported stale: {text}"
    );
    assert!(
        !text.contains("cannot say whether it came back"),
        "the carried dog must answer inside the reload's own wait: {text}"
    );

    let after = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.len() == 2 && rows.iter().all(|row| !row["pid"].is_null()))
    });
    let rows = after.as_array().expect("flock data is an array");
    let dog_row = rows
        .iter()
        .find(|row| row["name"] == "metrics")
        .unwrap_or_else(|| panic!("the dog must still be registered: {after}"));
    let sheep_row = rows
        .iter()
        .find(|row| row["name"] == "web")
        .unwrap_or_else(|| panic!("the sheep must still be registered: {after}"));

    assert_eq!(
        dog_row["pid"].as_u64(),
        Some(u64::try_from(dog_pid.as_raw()).unwrap()),
        "the dog was restarted rather than carried: {after}"
    );
    assert_eq!(
        sheep_row["pid"].as_u64(),
        Some(sheep_pid),
        "the sheep was restarted rather than carried: {after}"
    );
    assert_eq!(
        dog_row["restarts"], 0,
        "the dog's restart count moved: {after}"
    );
    assert_eq!(
        sheep_row["restarts"], 0,
        "the sheep's restart count moved: {after}"
    );
    // JSON carries both populations in one undivided array, so the marker is
    // what keeps them apart here; the tables below are what an operator sees.
    assert_eq!(
        dog_row["dog"]["kind"], "built_in",
        "a carried dog that lost its marker is one `shep dogs` has lost: {after}"
    );
    assert!(
        sheep_row["dog"].is_null(),
        "a sheep must not pick a marker up on the way across: {after}"
    );

    let dogs = shep(home).arg("dogs").output().unwrap();
    assert_success(&dogs);
    assert!(
        String::from_utf8_lossy(&dogs.stdout).contains("metrics"),
        "`shep dogs` must still list the carried dog: {}",
        String::from_utf8_lossy(&dogs.stdout)
    );
    let flock = shep(home).arg("flock").output().unwrap();
    assert_success(&flock);
    let flock_text = String::from_utf8_lossy(&flock.stdout);
    let (sheep_table, _dogs_table) = flock_text
        .split_once("Dogs")
        .unwrap_or_else(|| panic!("`shep flock` prints a dogs section: {flock_text}"));
    assert!(
        !sheep_table.contains("metrics"),
        "a carried dog must not be listed beside the operator's own apps: {flock_text}"
    );

    // The decisive one. A sheep that did not exist when the predecessor was
    // running, named by the dog that is answering now.
    let fresh = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("freshsheep")
        .output()
        .unwrap();
    assert_success(&fresh);
    let body = poll_metrics_containing(addr, r#"sheep="freshsheep""#);
    assert!(
        body.contains("HTTP/1.1 200"),
        "the carried dog must still answer a scrape after the exec: {body}"
    );
    assert!(
        body.contains(r#"sheep="freshsheep""#),
        "the exposition must name a sheep started AFTER the reload, which no cached reading \
         and no connection to the predecessor could produce: {body}"
    );

    // A successor rebuilds the roll from the blob, not from disk.
    // `spawn_enabled_dogs` never touches `FlockRegistry`, so a dog in the roll
    // would outlive the daemon and a later cold boot would restore `metrics`
    // as an ordinary unmarked sheep.
    let saved = shep(home)
        .arg("--format")
        .arg("json")
        .arg("save")
        .output()
        .unwrap();
    assert_success(&saved);
    let roll: serde_json::Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(
        roll["data"]["apps"], 2,
        "the roll holds the two sheep and no dog: {roll}"
    );

    graceful_kill(home);
}

#[cfg(unix)]
/// Table format, not JSON: `Format::Json`'s `flock` answer carries both
/// populations in one undivided array.
#[test]
fn dogs_and_flock_render_the_two_populations_the_right_way_round() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let port = free_port();
    write_shep_toml(
        &dir,
        &format!("[dog.metrics]\nbind = \"127.0.0.1:{port}\"\n"),
    );
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);
    poll_flock(home, |info| info["status"] == "online");

    let enabled = shep(home).arg("enable").arg("metrics").output().unwrap();
    assert_success(&enabled);
    guard.adopt_dog_pid(wait_for_dog_pid(home, "metrics"));

    let flock_table = String::from_utf8(shep(home).arg("flock").output().unwrap().stdout).unwrap();
    assert!(
        flock_table.contains("web"),
        "shep flock must still render the sheep: {flock_table}"
    );
    assert!(
        flock_table.contains("Dogs") && flock_table.contains("metrics"),
        "shep flock must render the dogs section beneath the sheep table: {flock_table}"
    );

    let dogs_table = String::from_utf8(shep(home).arg("dogs").output().unwrap().stdout).unwrap();
    assert!(
        dogs_table.contains("metrics"),
        "shep dogs must render the dog: {dogs_table}"
    );
    assert!(
        !dogs_table.contains("web"),
        "shep dogs must render nothing but dogs — not the sheep: {dogs_table}"
    );
    assert!(
        !dogs_table.contains("Dogs\n"),
        "shep dogs must not carry flock's own section header — it IS the \
         dogs table, not a listing with one embedded: {dogs_table}"
    );

    graceful_kill(home);
}

#[test]
fn barks_reads_the_history_with_no_shepherd_running() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let bark = shep_core::barks::Bark {
        at_ms: 1_700_000_000_000,
        rule: "watchdog".to_string(),
        subject: "web".to_string(),
        message: "restart budget exhausted".to_string(),
        sinks: vec![shep_core::barks::SinkOutcome {
            sink: "ops".to_string(),
            error: None,
        }],
    };
    shep_core::barks::append(
        &home.join("barks.jsonl"),
        &bark,
        shep_core::barks::DEFAULT_MAX_BYTES,
    )
    .unwrap();
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "this case never starts a daemon at all"
    );

    let output = shep(home)
        .arg("--format")
        .arg("json")
        .arg("barks")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "`shep barks` must never autostart a shepherd either"
    );

    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["command"], "barks",
        "`shep barks` must reach the barks verb and no other: {envelope}"
    );
    let rows = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("barks data must be an array: {envelope}"));
    assert_eq!(rows.len(), 1, "{envelope}");
    assert_eq!(rows[0]["subject"], "web", "{envelope}");
    assert_eq!(rows[0]["rule"], "watchdog", "{envelope}");
}

#[cfg(unix)]
/// The whole store through the real binary, with no shepherd anywhere:
/// provisioning happens when nothing is running. Also checks the `0600` mode
/// `shep_core::kv` documents, on the store the first `set` creates.
#[test]
fn the_kv_store_works_with_no_shepherd_running() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    let set1 = shep(home)
        .arg("set")
        .arg("bark.cooldown")
        .arg("30s")
        .output()
        .unwrap();
    assert_success(&set1);
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "shep set must never autostart a shepherd"
    );

    let mode = std::fs::metadata(home.join("kv.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");

    let get1 = shep(home).arg("get").arg("bark.cooldown").output().unwrap();
    assert_success(&get1);
    assert!(
        String::from_utf8_lossy(&get1.stdout).contains("30s"),
        "{}",
        String::from_utf8_lossy(&get1.stdout)
    );

    let missing = shep(home).arg("get").arg("missing").output().unwrap();
    assert_eq!(missing.status.code(), Some(3), "NotFound; {missing:?}");

    let set2 = shep(home)
        .arg("set")
        .arg("metrics_port")
        .arg("9615")
        .output()
        .unwrap();
    assert_success(&set2);

    let both = shep(home).arg("get").output().unwrap();
    assert_success(&both);
    let both_text = String::from_utf8_lossy(&both.stdout);
    assert!(both_text.contains("bark.cooldown"), "{both_text}");
    assert!(both_text.contains("metrics_port"), "{both_text}");

    let unset1 = shep(home)
        .arg("unset")
        .arg("bark.cooldown")
        .output()
        .unwrap();
    assert_success(&unset1);

    let gone = shep(home).arg("get").arg("bark.cooldown").output().unwrap();
    assert_eq!(gone.status.code(), Some(3), "NotFound; {gone:?}");

    let unset_all = shep(home).arg("unset").arg("--all").output().unwrap();
    assert_success(&unset_all);

    let empty = shep(home).arg("get").output().unwrap();
    assert_success(&empty);
    let empty_text = String::from_utf8_lossy(&empty.stdout);
    assert!(
        !empty_text.contains("metrics_port"),
        "store must be empty after unset --all: {empty_text}"
    );

    let bad_key = shep(home)
        .arg("set")
        .arg("bad key")
        .arg("x")
        .output()
        .unwrap();
    assert_eq!(bad_key.status.code(), Some(2), "usage; {bad_key:?}");
}

/// `data` is an array of `{key, value}` objects, never a JSON map keyed by
/// name. An absent key and a key outside the grammar surface as this
/// envelope's error half.
#[test]
fn kv_json_envelope_is_an_array_with_the_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    shep(home).arg("set").arg("a").arg("1").output().unwrap();
    shep(home).arg("set").arg("b").arg("2").output().unwrap();

    let get_all = shep(home)
        .arg("--format")
        .arg("json")
        .arg("get")
        .output()
        .unwrap();
    assert_success(&get_all);
    let envelope: serde_json::Value = serde_json::from_slice(&get_all.stdout).unwrap();
    assert!(envelope["data"].is_array(), "{envelope}");
    assert_eq!(envelope["data"].as_array().unwrap().len(), 2, "{envelope}");
    assert_eq!(envelope["schema_version"], 1, "{envelope}");

    let missing = shep(home)
        .arg("--format")
        .arg("json")
        .arg("get")
        .arg("ghost")
        .output()
        .unwrap();
    assert_json_error(&missing, 3, "not_found");

    let bad_key = shep(home)
        .arg("--format")
        .arg("json")
        .arg("set")
        .arg("bad key")
        .arg("x")
        .output()
        .unwrap();
    assert_json_error(&bad_key, 2, "usage");
}

/// Two real processes, not two threads sharing one open-file-description
/// table: only separate processes contend for `kv.json.lock`'s `flock(2)`.
/// The barrier matters: without it one writer can finish its whole batch
/// before the other starts, racing nothing.
#[test]
fn two_real_shep_processes_writing_concurrently_lose_no_keys() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    const PER_WRITER: usize = 15;

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (finished, racers) = std::sync::mpsc::channel();
    for writer in 0..2 {
        let home = home.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        let finished = finished.clone();
        std::thread::spawn(move || {
            barrier.wait(); // both writers start their first `shep set` together
            for n in 0..PER_WRITER {
                let key = format!("writer{writer}.k{n}");
                let output = shep(&home).arg("set").arg(&key).arg("v").output().unwrap();
                // A closed receiver means the case already gave up on this
                // writer and failed; there is no one left to report to.
                let _ = finished.send((writer, key, output));
            }
        });
    }
    drop(finished); // the writer threads hold the only senders that matter

    for _ in 0..(PER_WRITER * 2) {
        let (writer, key, output) = racers
            .recv_timeout(RACER_DEADLINE)
            .expect("a writer never came back; see RACER_DEADLINE");
        assert!(
            output.status.success(),
            "writer {writer}, key {key}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let list = shep(&home)
        .arg("--format")
        .arg("json")
        .arg("get")
        .output()
        .unwrap();
    assert_success(&list);
    let envelope: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let data = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("get data must be an array: {envelope}"));
    assert_eq!(
        data.len(),
        PER_WRITER * 2,
        "two concurrent shep set processes must not lose each other's keys: {envelope}"
    );
}
