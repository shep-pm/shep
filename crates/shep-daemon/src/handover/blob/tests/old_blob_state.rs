//! Populated blobs an older release wrote, and the state that must come back.
//!
//! The fixtures in [`super::old_blobs`] hold every optional field as the word
//! its writer had for "nothing is happening", so they catch a key going
//! missing but not a key being renamed: an absent key and a `None` read the
//! same, and `AppConfig` carries `#[serde(default)]`.
//!
//! Each file here is the same release's bytes with every optional field that
//! release could carry filled in, dumped through that release's own code, so
//! these are blobs that release could really have put on disk.

use shep_core::protocol::DogSource;
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

use super::old_blob_shape::{assert_the_shape, fixture_value};
use crate::entry::ReloadState;
use crate::handover::Handover;
use crate::supervisor::{
    CarriedReload, CommandOrigin, ManualKind, PendingManual, ReloadMode, ReloadPhase, ReloadSwap,
};

/// Every populated fixture, oldest first, each paired with the test that loads
/// it. A test deleted here breaks the table at compile time, so a committed
/// blob cannot end up with nothing loading it.
pub(super) const POPULATED_FIXTURES: [(&str, fn()); 12] = [
    (
        "shep-daemon-v0.1.18-populated",
        a_populated_blob_from_shep_daemon_v0_1_18_still_loads,
    ),
    (
        "shep-daemon-v0.1.20-populated",
        a_populated_blob_from_shep_daemon_v0_1_20_still_loads,
    ),
    (
        "shep-daemon-v0.1.21-populated",
        a_populated_blob_from_shep_daemon_v0_1_21_still_loads,
    ),
    (
        "shep-daemon-v0.1.22-populated",
        a_populated_blob_from_shep_daemon_v0_1_22_still_loads,
    ),
    (
        "shep-daemon-v0.1.23-populated",
        a_populated_blob_from_shep_daemon_v0_1_23_still_loads,
    ),
    (
        "shep-daemon-v0.1.30-populated",
        a_populated_blob_from_shep_daemon_v0_1_30_still_loads,
    ),
    (
        "shep-daemon-v0.5.0-populated",
        a_populated_blob_from_shep_daemon_v0_5_0_still_loads,
    ),
    (
        "shep-daemon-v0.6.2-populated",
        a_populated_blob_from_shep_daemon_v0_6_2_still_loads,
    ),
    (
        "shep-daemon-v0.8.0-populated",
        a_populated_blob_from_shep_daemon_v0_8_0_still_loads,
    ),
    (
        "shep-daemon-v0.11.0-populated",
        a_populated_blob_from_shep_daemon_v0_11_0_still_loads,
    ),
    (
        "shep-daemon-v0.12.0-populated",
        a_populated_blob_from_shep_daemon_v0_12_0_still_loads,
    ),
    (
        "shep-daemon-v0.13.0-populated",
        a_populated_blob_from_shep_daemon_v0_13_0_still_loads,
    ),
];

/// The reload job the populated blob carries, as this image reads it.
fn the_swap_in_flight() -> CarriedReload {
    CarriedReload {
        app: "web".to_owned(),
        queue: vec![11, 12],
        mode: ReloadMode::Serial,
        swap: ReloadSwap {
            old_id: 1,
            new_id: Some(9),
            phase: ReloadPhase::AwaitReady,
        },
    }
}

/// The three sheep a populated blob carries, and the status each story needs.
fn assert_the_populated_flock(stem: &str, raw: &serde_json::Value, blob: &Handover) {
    assert_eq!(
        blob.sheep.len(),
        3,
        "{stem}: an ordinary sheep, a drainee, and a sheep owed a respawn"
    );
    assert_eq!(blob.sheep[0].id(), 1);
    assert_eq!(blob.sheep[1].id(), 2);
    assert_eq!(blob.sheep[2].id(), 3);
    assert_eq!(blob.sheep[0].status(), ProcStatus::Online);
    assert_eq!(
        blob.sheep[1].status(),
        ProcStatus::Stopping,
        "{stem}: the drainee half of a swap is going away"
    );
    assert_eq!(
        blob.sheep[2].status(),
        ProcStatus::WaitingRestart,
        "{stem}: the sheep owed a respawn is the only one that carries a deadline"
    );
    assert_eq!(
        blob.sheep[2].pid(),
        None,
        "{stem}: a sheep owed a respawn has no process to name"
    );

    // The swap marker and its job, on the two levels v0.1.21 added them to.
    if raw["sheep"][1].get("reload").is_some() {
        assert_eq!(
            blob.sheep[1].reload(),
            Some(ReloadState::Drainee { new_id: Some(9) }),
            "{stem}: a drainee that loads as anything else routes its exit to the \
             ordinary restart path and respawns the old code into the replacement's slot"
        );
    }
    if raw["reloads"]
        .as_array()
        .is_some_and(|jobs| !jobs.is_empty())
    {
        assert_eq!(
            blob.reloads(),
            &[the_swap_in_flight()],
            "{stem}: the job must come back whole: queue, mode and every field of the swap"
        );
    }

    if raw["sheep"][2].get("restart_due").is_some() {
        assert!(
            blob.sheep[2].restart_due().is_some(),
            "{stem}: a deadline that does not cross the wire is one the successor \
             cannot re-arm from, so the owed respawn fires on a fresh full delay"
        );
    }
}

/// The carried config, field by field, for each field that release's own
/// `AppConfig` had a place for.
fn assert_the_populated_config(stem: &str, raw: &serde_json::Value, blob: &Handover) {
    let wrote = &raw["sheep"][0]["app"];
    let app = blob.sheep[0].app();

    if wrote.get("args").is_some() {
        assert_eq!(
            app.args,
            ["--port", "8080"],
            "{stem}: the arguments the app runs with"
        );
    }
    if wrote.get("cwd").is_some() {
        assert_eq!(
            app.cwd.as_deref(),
            Some("/srv/app"),
            "{stem}: where the process runs"
        );
    }
    if wrote.get("interpreter").is_some() {
        assert_eq!(
            app.interpreter.as_deref(),
            Some("none"),
            "{stem}: what runs the script"
        );
    }
    if wrote.get("env").is_some() {
        assert_eq!(
            app.env.get("MODE").map(String::as_str),
            Some("blue"),
            "{stem}: an environment variable the successor would spawn without"
        );
        assert_eq!(
            app.env.len(),
            2,
            "{stem}: every entry of the map, not one probe"
        );
    }
    if wrote.get("instances").is_some() {
        assert_eq!(app.instances, 2, "{stem}: how many copies the flock holds");
    }
    if wrote.get("autorestart").is_some() {
        assert!(
            !app.autorestart,
            "{stem}: an app that must not be restarted"
        );
    }
    if wrote.get("stop_exit_codes").is_some() {
        assert_eq!(
            app.stop_exit_codes,
            [0, 3],
            "{stem}: the codes treated as a clean stop"
        );
    }
    if wrote.get("min_uptime").is_some() {
        assert_eq!(
            app.min_uptime,
            UpDuration::from_millis(2500),
            "{stem}: the uptime below which an exit counts as unstable"
        );
    }
    if wrote.get("max_restarts").is_some() {
        assert_eq!(app.max_restarts, 7, "{stem}: the restart budget");
    }
    if wrote.get("restart_delay").is_some() {
        assert_eq!(
            app.restart_delay,
            Some(UpDuration::from_millis(2000)),
            "{stem}: the fixed wait before every restart"
        );
    }
    if wrote.get("kill_timeout").is_some() {
        assert_eq!(
            app.kill_timeout,
            UpDuration::from_millis(2600),
            "{stem}: how long the drain may take before the kill escalates"
        );
    }
    if wrote.get("graceful_timeout").is_some() {
        assert_eq!(
            app.graceful_timeout,
            UpDuration::from_millis(9000),
            "{stem}: the wait for a clean shutdown"
        );
    }
    if wrote.get("watch").is_some() {
        assert!(app.watch, "{stem}: a sheep whose source files are watched");
    }
    if wrote.get("ignore_watch").is_some() {
        assert_eq!(app.ignore_watch, ["*.log"], "{stem}: what the watch skips");
    }
    if wrote.get("cron_restart").is_some() {
        assert_eq!(
            app.cron_restart.as_deref(),
            Some("0 3 * * *"),
            "{stem}: the schedule that restarts it"
        );
    }
    if wrote.get("depends_on").is_some() {
        assert_eq!(
            app.depends_on,
            ["api"],
            "{stem}: the app ordered before this one"
        );
    }
    if wrote.get("out_file").is_some() {
        assert_eq!(
            app.out_file.as_deref(),
            Some("web.out.log"),
            "{stem}: where its output goes"
        );
    }
    if wrote.get("err_file").is_some() {
        assert_eq!(
            app.err_file.as_deref(),
            Some("web.err.log"),
            "{stem}: where its errors go"
        );
    }
    if wrote.get("log_timestamps").is_some() {
        assert!(
            !app.log_timestamps,
            "{stem}: an app whose log lines carry no stamp"
        );
    }
}

/// Every carried marker, as the value it carries rather than as its absence.
fn assert_the_populated_markers(stem: &str, raw: &serde_json::Value, blob: &Handover) {
    let wrote = &raw["sheep"][0];
    let sheep = &blob.sheep[0];

    if wrote.get("pending_delete").is_some() {
        assert_eq!(
            sheep.pending_delete(),
            Some(true),
            "{stem}: a delete asked for before the exec, read as \"no\", deletes a \
             running sheep on the successor's first roll"
        );
    }
    if wrote.get("manual").is_some() {
        assert_eq!(
            sheep.manual(),
            Some(PendingManual {
                kind: ManualKind::Restart,
                origin: CommandOrigin::Automatic,
            }),
            "{stem}: an `autorestart` app with a command owning its next exit loses \
             that command to its ordinary respawn"
        );
    }
    if wrote.get("ready_failed").is_some() {
        assert_eq!(
            sheep.ready_failed(),
            Some(true),
            "{stem}: a failed readiness verdict the rollback still needs to reach"
        );
    }
    if wrote.get("dog").is_some() {
        assert_eq!(
            sheep.dog(),
            Some(&DogSource::BuiltIn { grants: None }),
            "{stem}: a dog that crossed as an ordinary sheep is one `shep dogs` has lost"
        );
    }
    if wrote.get("pending").is_some() {
        let parked = sheep
            .pending()
            .expect("{stem}: the config a load parked for the next spawn");
        assert_eq!(
            parked.env.get("MODE").map(String::as_str),
            Some("blue"),
            "{stem}: a parked config that arrives as nothing is config erasure the \
             next load cannot see"
        );
        assert_eq!(
            parked.args,
            ["--port", "8080"],
            "{stem}: the parked arguments"
        );
    }
    if wrote.get("pending_reidentifies").is_some() {
        assert_eq!(
            sheep.pending_reidentifies(),
            Some(true),
            "{stem}: promoting that config must re-resolve its identity"
        );
    }
    if wrote.get("disabled").is_some() {
        assert_eq!(
            sheep.disabled(),
            Some(true),
            "{stem}: a disabled app the successor starts, and its first roll write \
             erases from disk"
        );
    }
}

/// Load one populated fixture and assert every value it carries came back.
fn assert_the_populated_blob(stem: &str) {
    let raw = fixture_value(stem);
    let blob = Handover::load_value(raw.clone())
        .expect("a populated blob an older release wrote must still load whole");
    assert_eq!(blob.version, 1, "{stem} is a format version 1 blob");

    // Filling the values in added no key and dropped none: a populated blob
    // holds exactly the key set its release's plain fixture holds, at the
    // blob, at each sheep, at its descriptors and at its config.
    assert_the_shape(stem, 3);

    assert_the_populated_flock(stem, &raw, &blob);
    assert_the_populated_config(stem, &raw, &blob);
    assert_the_populated_markers(stem, &raw, &blob);
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_1_18_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.1.18-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_1_20_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.1.20-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_1_21_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.1.21-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_1_22_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.1.22-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_1_23_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.1.23-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_1_30_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.1.30-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_5_0_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.5.0-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_6_2_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.6.2-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_8_0_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.8.0-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_11_0_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.11.0-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_12_0_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.12.0-populated");
}

#[test]
fn a_populated_blob_from_shep_daemon_v0_13_0_still_loads() {
    assert_the_populated_blob("shep-daemon-v0.13.0-populated");
}

/// A populated fixture is only worth its values, and a fixture that quietly
/// went back to its release's empty sample would still load. Every field this
/// file filled in is named here, so the bytes are checked too.
#[test]
fn every_populated_fixture_actually_carries_what_it_claims() {
    for (stem, _) in POPULATED_FIXTURES {
        let raw = fixture_value(stem);
        let sheep = &raw["sheep"][0];
        let app = &sheep["app"];

        assert_eq!(
            raw["sheep"]
                .as_array()
                .expect("a blob carries a flock")
                .len(),
            3,
            "{stem}"
        );
        assert_eq!(app["args"], serde_json::json!(["--port", "8080"]), "{stem}");
        assert_eq!(
            app["env"],
            serde_json::json!({"MODE": "blue", "PORT": "8080"}),
            "{stem}"
        );
        assert_eq!(app["instances"], serde_json::json!(2), "{stem}");
        assert_eq!(app["max_restarts"], serde_json::json!(7), "{stem}");
        assert_eq!(app["kill_timeout"], serde_json::json!("2600"), "{stem}");

        // The markers, on the releases that had a key to put them on.
        for key in [
            "pending_delete",
            "manual",
            "ready_failed",
            "dog",
            "pending",
            "pending_reidentifies",
            "disabled",
        ] {
            if sheep.get(key).is_some() {
                assert_ne!(
                    sheep[key],
                    serde_json::Value::Null,
                    "{stem}: {key} is there and empty, which is the shape a rename follows"
                );
            }
        }
        assert_ne!(
            raw["reloads"],
            serde_json::json!([]),
            "{stem}: a blob with no job in flight cannot test the job crossing"
        );
        assert_ne!(
            raw["sheep"][1]["status"], raw["sheep"][0]["status"],
            "{stem}: the drainee half"
        );
        assert_ne!(
            raw["sheep"][2]["status"], raw["sheep"][0]["status"],
            "{stem}: the owed-respawn half"
        );
    }
}
