//! Where a dog runs relative to the flock it watches
//!
//! `[daemon] boot_first_dogs` splits one spawn pass into two, so a log-rotation
//! dog is up before a sheep writes a line and a metrics dog is not answering
//! for a flock that is not back yet. The rest of these cases are about what a
//! dog that cannot start, or starts under a name a sheep already holds, does
//! to the boot: warn, and never fail it.

use super::{boot_blocking, seed_roll};
use crate::boot::*;
use crate::dogs::DogSpec;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::testing::test_paths;
use shep_core::config::AppConfig;
use shep_core::protocol::DogSource;
use shep_core::status::ProcStatus;
use std::collections::BTreeMap;

/// A metrics dog that starts first answers for an empty flock for the
/// whole restore window, and a bark dog alerts on every restored sheep.
/// [`ScriptedRunner`] hands out pids as `FIRST_SCRIPTED_PID + index`, so
/// the spawn order is observable only as a pid order.
#[tokio::test]
async fn boot_restores_the_flock_before_it_lets_the_dogs_out() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    seed_roll(&paths, vec![AppConfig::minimal("web", "./srv")]);

    let daemon = boot_with_idle_sweep(
        // Two scripts: the restored sheep's spawn, then the dog's.
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]),
        paths,
        BootOptions {
            restore: true,
            dogs: vec![DogSpec {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn { grants: None },
                env: BTreeMap::new(),
            }],
            ..BootOptions::default()
        },
    )
    .await
    .unwrap();

    let ctx = daemon.context();
    let flock = ctx.supervisor.list_checked().await.unwrap();
    assert_eq!(flock.len(), 2, "the sheep and the dog must both be up");

    let sheep = flock
        .iter()
        .find(|p| p.name == "web")
        .expect("the restored sheep must be present");
    let dog = flock
        .iter()
        .find(|p| p.name == "metrics")
        .expect("the dog must be present");
    assert!(
        sheep.dog.is_none(),
        "the restored app must carry no dog marker"
    );
    assert_eq!(
        dog.dog,
        Some(DogSource::BuiltIn { grants: None }),
        "the dog entry must carry its source"
    );
    assert!(
        sheep.pid < dog.pid,
        "the sheep must be spawned before the dog: sheep={:?} dog={:?}",
        sheep.pid,
        dog.pid
    );

    drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
}

/// The dog gets no script, so `ScriptedRunner` answers
/// `SpawnFailed("script exhausted")` on its spawn and the flock must still
/// come up.
#[test]
fn a_dog_that_will_not_start_does_not_fail_the_boot() {
    let (flock, logs) = boot_blocking(
        // Nothing to restore: the boot is about the dog alone.
        vec![],
        // No scripts queued: the dog's spawn is the first (and
        // only) one attempted, and finds nothing to pop.
        vec![],
        BootOptions {
            dogs: vec![DogSpec {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn { grants: None },
                env: BTreeMap::new(),
            }],
            ..BootOptions::default()
        },
    );

    let dog = flock
        .iter()
        .find(|p| p.name == "metrics")
        .expect("the dog's entry must still be registered");
    assert_eq!(
        dog.status,
        ProcStatus::Errored,
        "a dog that could not spawn is errored, not silently absent"
    );
    assert!(
        logs.contains("metrics"),
        "the warning must name the dog that did not start: {logs:?}"
    );
    assert!(
        logs.contains("WARN"),
        "a dog failing to start is a warning, not silence: {logs:?}"
    );
}

/// `start_dog` is idempotent by name: enabling a dog under a name a sheep
/// already holds comes back `Ok` over the sheep, not a started dog. The
/// RPC arm inspects that reply for the missing `dog` marker, and this pins
/// that `spawn_enabled_dogs` does the same.
#[test]
fn a_dog_enabled_under_a_sheeps_name_does_not_start_and_does_not_fail_the_boot() {
    let (flock, logs) = boot_blocking(
        vec![AppConfig::minimal("metrics", "./srv")],
        // One script: the restored sheep's own spawn. The dog gives
        // way before it reaches the runner, so a second script here
        // would go unconsumed if that held.
        vec![ProcScript::never_exits()],
        BootOptions {
            restore: true,
            dogs: vec![DogSpec {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn { grants: None },
                env: BTreeMap::new(),
            }],
            ..BootOptions::default()
        },
    );

    assert_eq!(
        flock.len(),
        1,
        "the collision must not register a second entry: {flock:?}"
    );
    assert!(
        flock[0].dog.is_none(),
        "the sheep must not be relabeled as a dog by a same-named enable: {:?}",
        flock[0]
    );
    assert!(
        logs.contains("metrics"),
        "the warning must name the collision: {logs:?}"
    );
}
