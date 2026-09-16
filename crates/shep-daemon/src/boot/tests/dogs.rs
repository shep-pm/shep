//! Where a dog runs relative to the flock it watches
//!
//! `[daemon] boot_first_dogs` splits one spawn pass into two, so a log-rotation
//! dog is up before a sheep writes a line and a metrics dog is not answering
//! for a flock that is not back yet. The rest of these cases are about what a
//! dog that cannot start, or starts under a name a sheep already holds, does
//! to the boot: warn, and never fail it.

use crate::boot::*;
use crate::dogs::DogSpec;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::snapshot::{FlockSnapshot, SNAPSHOT_VERSION, SavedApp};
use crate::testing::{capture_logs, test_paths};
use shep_core::config::AppConfig;
use shep_core::protocol::DogSource;
use shep_core::status::ProcStatus;

/// A metrics dog that starts first answers for an empty flock for the
/// whole restore window, and a bark dog alerts on every restored sheep.
/// [`ScriptedRunner`] hands out pids as `FIRST_SCRIPTED_PID + index`, so
/// the spawn order is observable only as a pid order.
#[tokio::test]
async fn boot_restores_the_flock_before_it_lets_the_dogs_out() {
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();
    let roll = FlockSnapshot {
        version: SNAPSHOT_VERSION,
        saved_at_ms: 0,
        apps: vec![SavedApp {
            app: AppConfig::minimal("web", "./srv"),
            instances_running: 1,
        }],
    };
    crate::snapshot::write_atomic(&paths.snapshot, &roll).unwrap();

    let daemon = boot(
        // Two scripts: the restored sheep's spawn, then the dog's.
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]),
        paths.clone(),
        BootOptions {
            restore: true,
            dogs: vec![DogSpec {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn,
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
        Some(DogSource::BuiltIn),
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
///
/// `#[test]` with a `block_on` of its own, not `#[tokio::test]`:
/// `capture_logs` scopes its subscriber to a synchronous closure.
#[test]
fn a_dog_that_will_not_start_does_not_fail_the_boot() {
    let _guard = SIGNAL_TEST_LOCK.blocking_lock();
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let mut boot_result = None;
    let logs = capture_logs(|| {
        boot_result = Some(rt.block_on(boot(
            // No scripts queued: the dog's spawn is the first (and
            // only) one attempted, and finds nothing to pop.
            ScriptedRunner::new(vec![]),
            paths.clone(),
            BootOptions {
                dogs: vec![DogSpec {
                    name: "metrics".to_string(),
                    source: DogSource::BuiltIn,
                }],
                ..BootOptions::default()
            },
        )));
    });
    let daemon = boot_result
        .unwrap()
        .expect("a dog that will not start must not fail the boot");

    let flock = rt
        .block_on(daemon.context().supervisor.list_checked())
        .unwrap();
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

    drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
}

/// `start_dog` is idempotent by name: enabling a dog under a name a sheep
/// already holds comes back `Ok` over the sheep, not a started dog. The
/// RPC arm inspects that reply for the missing `dog` marker, and this pins
/// that `spawn_enabled_dogs` does the same.
///
/// `#[test]` plus `capture_logs` for the reason
/// `a_dog_that_will_not_start_does_not_fail_the_boot` gives.
#[test]
fn a_dog_enabled_under_a_sheeps_name_does_not_start_and_does_not_fail_the_boot() {
    let _guard = SIGNAL_TEST_LOCK.blocking_lock();
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();
    let roll = FlockSnapshot {
        version: SNAPSHOT_VERSION,
        saved_at_ms: 0,
        apps: vec![SavedApp {
            app: AppConfig::minimal("metrics", "./srv"),
            instances_running: 1,
        }],
    };
    crate::snapshot::write_atomic(&paths.snapshot, &roll).unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let mut boot_result = None;
    let logs = capture_logs(|| {
        boot_result = Some(rt.block_on(boot(
            // One script: the restored sheep's own spawn. `start_dog`
            // finds the name already registered and returns early
            // without ever touching the runner, so a second script
            // here would go unconsumed if that held.
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            BootOptions {
                restore: true,
                dogs: vec![DogSpec {
                    name: "metrics".to_string(),
                    source: DogSource::BuiltIn,
                }],
                ..BootOptions::default()
            },
        )));
    });
    let daemon = boot_result
        .unwrap()
        .expect("a name collision must not fail the boot");

    let flock = rt
        .block_on(daemon.context().supervisor.list_checked())
        .unwrap();
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

    drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
}

/// fails if a promoted dog takes a saved sheep's name in silence. The
/// unpromoted case above is the opposite way round: there the restore has
/// already registered the sheep and `start_dog` returns over it, and here
/// the dog registers against an empty flock and the sheep is what is
/// lost. Nothing refuses either collision, so a warning is all the
/// operator gets.
///
/// `#[test]` plus `capture_logs` for the reason
/// `a_dog_that_will_not_start_does_not_fail_the_boot` gives.
#[test]
fn a_promoted_dog_that_takes_a_saved_sheeps_name_says_so() {
    let _guard = SIGNAL_TEST_LOCK.blocking_lock();
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();
    let roll = FlockSnapshot {
        version: SNAPSHOT_VERSION,
        saved_at_ms: 0,
        apps: vec![SavedApp {
            app: AppConfig::minimal("metrics", "./srv"),
            instances_running: 1,
        }],
    };
    crate::snapshot::write_atomic(&paths.snapshot, &roll).unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let mut boot_result = None;
    let logs = capture_logs(|| {
        boot_result = Some(rt.block_on(boot(
            // One script, and the dog is what consumes it: the restore
            // reads `metrics` as already running and starts nothing.
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            BootOptions {
                restore: true,
                dogs: vec![DogSpec {
                    name: "metrics".to_string(),
                    source: DogSource::BuiltIn,
                }],
                boot_first_dogs: vec!["metrics".to_string()],
                ..BootOptions::default()
            },
        )));
    });
    let daemon = boot_result
        .unwrap()
        .expect("a name collision must not fail the boot");

    let flock = rt
        .block_on(daemon.context().supervisor.list_checked())
        .unwrap();
    assert_eq!(flock.len(), 1, "one name is one entry: {flock:?}");
    assert!(
        flock[0].dog.is_some(),
        "the promoted dog is what holds the name: {:?}",
        flock[0]
    );
    assert!(
        logs.contains("is not restored"),
        "the lost sheep must be named out loud: {logs:?}"
    );

    drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
}
