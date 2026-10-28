//! Every dog the shepherd knows about is in the flock, running or not
//!
//! `BootOptions::known_dogs` is the list, `BootOptions::dogs` the subset an
//! operator switched on. The rest is registered `Stopped` and disabled, after
//! the restored flock so no saved sheep's id moves.

use super::seed_roll;
use crate::boot::*;
use crate::dogs::DogSpec;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::testing::test_paths;
use shep_core::config::AppConfig;
use shep_core::protocol::{DogSource, ProcessInfo};
use shep_core::status::ProcStatus;
use std::collections::BTreeMap;

fn built_in(name: &str) -> DogSpec {
    DogSpec {
        name: name.to_string(),
        source: DogSource::BuiltIn { grants: None },
        env: BTreeMap::new(),
    }
}

fn row<'a>(flock: &'a [ProcessInfo], name: &str) -> &'a ProcessInfo {
    flock
        .iter()
        .find(|info| info.name == name)
        .unwrap_or_else(|| panic!("`{name}` is not in the flock: {flock:?}"))
}

#[tokio::test]
async fn a_fresh_home_lists_both_built_ins_disabled_at_ids_0_and_1() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();

    let daemon = boot_with_idle_sweep(
        // No script: nothing may spawn.
        ScriptedRunner::new(vec![]),
        test_paths(&dir),
        BootOptions {
            known_dogs: vec![built_in("metrics"), built_in("bark")],
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let flock = daemon.context().supervisor.list_checked().await.unwrap();
    let mut ids: Vec<(String, u32)> = flock.iter().map(|i| (i.name.clone(), i.id)).collect();
    ids.sort_by_key(|(_, id)| *id);
    assert_eq!(ids, [("metrics".to_string(), 0), ("bark".to_string(), 1)]);
    for info in &flock {
        assert_eq!(info.status, ProcStatus::Stopped, "{info:?}");
        assert_eq!(info.disabled, Some(true), "{info:?}");
        assert_eq!(
            info.dog,
            Some(DogSource::BuiltIn { grants: None }),
            "{info:?}"
        );
        assert_eq!(info.pid, None, "{info:?}");
    }

    drop(daemon);
}

#[tokio::test]
async fn at_rest_dogs_register_after_the_restored_flock_so_sheep_ids_hold() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    seed_roll(
        &paths,
        vec![
            AppConfig::minimal("api", "./srv"),
            AppConfig::minimal("web", "./srv"),
        ],
    );

    let daemon = boot_with_idle_sweep(
        // The two restored sheep, nothing else.
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]),
        paths,
        BootOptions {
            restore: true,
            known_dogs: vec![built_in("metrics"), built_in("bark")],
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let flock = daemon.context().supervisor.list_checked().await.unwrap();
    let mut by_id: Vec<(u32, &str)> = flock.iter().map(|i| (i.id, i.name.as_str())).collect();
    by_id.sort_unstable();
    assert_eq!(
        by_id,
        [(0, "api"), (1, "web"), (2, "metrics"), (3, "bark")],
        "the sheep keep the ids they would have had without any dog listed"
    );

    drop(daemon);
}

#[tokio::test]
async fn an_enabled_dog_runs_and_only_the_others_are_disabled() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();

    let daemon = boot_with_idle_sweep(
        // One script, for `metrics`: `bark` must not consume one.
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        test_paths(&dir),
        BootOptions {
            dogs: vec![built_in("metrics")],
            known_dogs: vec![built_in("metrics"), built_in("bark")],
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let flock = daemon.context().supervisor.list_checked().await.unwrap();
    assert_eq!(flock.len(), 2, "{flock:?}");
    let metrics = row(&flock, "metrics");
    assert_eq!(metrics.status, ProcStatus::Online, "{metrics:?}");
    assert_eq!(metrics.disabled, None, "{metrics:?}");
    let bark = row(&flock, "bark");
    assert_eq!(bark.status, ProcStatus::Stopped, "{bark:?}");
    assert_eq!(bark.disabled, Some(true), "{bark:?}");

    drop(daemon);
}

/// An adopted dog that is not in `dogs` is as much a known dog as a
/// built-in is, and registers from its own recorded path.
#[tokio::test]
async fn an_adopted_disabled_dog_is_listed_with_the_source_it_would_run_from() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let adopted = DogSource::Adopted {
        grants: None,
        path: "/usr/local/bin/shep-otel".to_string(),
        channel: true,
    };

    let daemon = boot_with_idle_sweep(
        ScriptedRunner::new(vec![]),
        test_paths(&dir),
        BootOptions {
            known_dogs: vec![DogSpec {
                name: "otel".to_string(),
                source: adopted.clone(),
                env: BTreeMap::new(),
            }],
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let flock = daemon.context().supervisor.list_checked().await.unwrap();
    let otel = row(&flock, "otel");
    assert_eq!(otel.dog, Some(adopted));
    assert_eq!(otel.disabled, Some(true));

    drop(daemon);
}

/// A promoted dog spawns before the restore, so it takes the first id, and
/// the at-rest pass finds it already running.
#[tokio::test]
async fn a_boot_first_dog_still_spawns_first_and_is_not_registered_twice() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    seed_roll(&paths, vec![AppConfig::minimal("web", "./srv")]);

    let daemon = boot_with_idle_sweep(
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]),
        paths,
        BootOptions {
            restore: true,
            dogs: vec![built_in("log-rotate")],
            known_dogs: vec![built_in("log-rotate"), built_in("bark")],
            boot_first_dogs: vec!["log-rotate".to_string()],
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let flock = daemon.context().supervisor.list_checked().await.unwrap();
    assert_eq!(flock.len(), 3, "{flock:?}");
    assert_eq!(row(&flock, "log-rotate").id, 0);
    assert_eq!(row(&flock, "log-rotate").disabled, None);
    assert_eq!(row(&flock, "web").id, 1);
    assert_eq!(row(&flock, "bark").id, 2);

    drop(daemon);
}
