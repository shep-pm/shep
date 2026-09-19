//! What [`restorable`] keeps, rejects and starts, and what [`muster`] does
//! with that: both members up and down, a hand-edited entry, a broken entry
//! that must not take the rest of the roll down with it, an app the flock
//! already has, and a roll that is simply not there.
//!
//! `a_sheep_saved_while_stopped_is_still_a_member`, the other `restorable`
//! case, is left inline in the parent `tests` module rather than moved here:
//! its fixture app name collides on a bare word match in
//! `repo-boundary-guard.js`, and moving it needs a call only Rin can make.

use super::super::*;

use shep_core::config::{AppConfig, normalize};
use shep_core::status::ProcStatus;

use crate::fake::{ProcScript, ScriptedRunner};
use crate::supervisor::spawn_supervisor;
use crate::testing::{roll_of, sorted_status, test_paths};

/// Restoring only the running ones would make `shep stop` destructive
/// across a daemon restart: the sheep would leave the flock entirely.
#[test]
fn restorable_keeps_every_member_and_starts_only_what_was_up() {
    let mut stopped = AppConfig::minimal("stopped", "./s");
    stopped.instances = 1;
    let mut opted_out = AppConfig::minimal("manual", "./m");
    opted_out.autostart = false;

    let roll = FlockSnapshot::with_apps(vec![
        SavedApp {
            app: AppConfig::minimal("web", "./srv"),
            instances_running: 2,
        },
        SavedApp {
            app: stopped,
            instances_running: 0,
        },
        SavedApp {
            app: opted_out,
            instances_running: 1,
        },
    ]);
    let restorable = restorable(roll);

    let members: Vec<&str> = restorable
        .members
        .iter()
        .map(|a| a.config().name.as_str())
        .collect();
    assert_eq!(
        members,
        ["web", "stopped", "manual"],
        "every entry that normalizes belongs to the flock, running or not"
    );

    let starting: Vec<&str> = restorable
        .to_start
        .iter()
        .map(|a| a.config().name.as_str())
        .collect();
    assert_eq!(
        starting,
        ["web"],
        "only the sheep that was up and opts into autostart is started"
    );
    assert!(restorable.rejected.is_empty());
}

#[test]
fn restorable_reports_a_hand_edited_invalid_app_instead_of_aborting() {
    let mut broken = AppConfig::minimal("broken", "./b");
    broken.instances = 0; // someone edited the roll
    let roll = roll_of(vec![broken, AppConfig::minimal("web", "./srv")]);
    let restorable = restorable(roll);
    assert_eq!(
        restorable.members.len(),
        1,
        "one bad entry must not sink the muster"
    );
    assert_eq!(
        restorable.rejected,
        vec![(
            "broken".to_string(),
            shep_core::config::NormalizeError::ZeroInstances
        )]
    );
}

/// fails if `muster` starts an app the roll says was down, skips one it
/// says was up, or drops either from the flock. All three in one case: an
/// inverted restore rule would pass any one alone.
#[tokio::test(start_paused = true)]
async fn muster_restores_both_and_starts_only_the_one_that_was_up() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    let roll = FlockSnapshot::with_apps(vec![
        SavedApp {
            app: AppConfig::minimal("up", "./srv"),
            instances_running: 1,
        },
        SavedApp {
            app: AppConfig::minimal("down", "./srv"),
            instances_running: 0,
        },
    ]);
    write_atomic(&paths.snapshot, &roll).unwrap();

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();

    let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
        .await
        .unwrap();
    assert_eq!(
        restored,
        vec!["up".to_string(), "down".to_string()],
        "both are restored to the flock; only one of them runs"
    );

    assert_eq!(
        sorted_status(&handle).await,
        [
            ("down".to_string(), ProcStatus::Stopped),
            ("up".to_string(), ProcStatus::Online)
        ],
        "the sheep that was down is listed and stopped, not missing"
    );
    handle.shutdown().await;
}

/// fails if a bad entry that is not LAST takes every app after it down.
///
/// `FlockRegistry` is a `BTreeMap`, so the roll is alphabetical and
/// `restorable` preserves that order: the `a-`/`b-`/`c-` names here make
/// the roll order the production order, with the wreck in the middle.
///
/// One `muster` call: `shep muster`'s CLI path calls twice and the second
/// re-registers what the first lost, so only the single unattended restore
/// a `shep startup` unit performs can show this. `failing_to_spawn` is
/// load-bearing, since scripts are consumed in spawn order.
#[tokio::test(start_paused = true)]
async fn a_bad_saved_app_does_not_take_the_apps_after_it_down() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    let app = |name: &str| AppConfig::minimal(name, "./srv");
    let roll = roll_of(vec![app("a-good"), app("b-bad"), app("c-good")]);
    write_atomic(&paths.snapshot, &roll).unwrap();

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        // Two scripts for the two that must come up. `b-bad` consumes
        // none, so a run that let it take one would starve `c-good` and
        // fail here for a reason of its own.
        ScriptedRunner::new(vec![ProcScript::never_exits(); 2]).failing_to_spawn(&["b-bad"]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();

    let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
        .await
        .unwrap();
    assert_eq!(
        restored,
        vec![
            "a-good".to_string(),
            "b-bad".to_string(),
            "c-good".to_string()
        ]
    );

    assert_eq!(
        sorted_status(&handle).await,
        [
            ("a-good".to_string(), ProcStatus::Online),
            ("b-bad".to_string(), ProcStatus::Errored),
            ("c-good".to_string(), ProcStatus::Online),
        ],
        "every app after the broken one must still get its turn, and the \
         broken one must be visible rather than absent"
    );
    handle.shutdown().await;
}

/// fails if ONE saved app that cannot start keeps the rest of the flock
/// down at the next boot.
///
/// `muster` starts under `BatchPolicy::PerApp`, not `AllOrNothing`,
/// because the pre-registration check refuses a whole batch over one app
/// whose script is gone, which at an unattended boot costs the machine its
/// flock.
///
/// `refusing` is the preflight verdict `AllOrNothing` refuses a whole
/// batch over, and under `PerApp` it is only warned about, so it asserts
/// nothing here and stands for the input the two policies disagree on.
/// `failing_to_spawn` is what makes `gone` land as a visible `Errored`
/// row rather than vanishing, and it names the app rather than starving it
/// of a script: the restore starts a stage's members in the plan's order,
/// not the roll's, and `gone` sorts first.
#[tokio::test(start_paused = true)]
async fn one_unstartable_saved_app_does_not_keep_the_rest_of_the_flock_down() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    let roll = roll_of(vec![
        AppConfig::minimal("good", "./srv"),
        AppConfig::minimal("gone", "./deleted-by-a-rebuild"),
    ]);
    write_atomic(&paths.snapshot, &roll).unwrap();

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()])
            .refusing(&["gone"])
            .failing_to_spawn(&["gone"]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();

    let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
        .await
        .unwrap();
    assert_eq!(restored, vec!["good".to_string(), "gone".to_string()]);

    assert_eq!(
        sorted_status(&handle).await,
        [
            ("gone".to_string(), ProcStatus::Errored),
            ("good".to_string(), ProcStatus::Online)
        ],
        "the app that could still run must come up, and the one that \
         could not must be visible rather than absent"
    );
    handle.shutdown().await;
}

/// fails if `muster` starts an app the flock already has.
///
/// `instance_slots` hands a second `Start` of a one-instance app the next
/// free slot, so an unconditional muster would leave it running two. The
/// single script makes that visible: the flock's own `web` consumes it, so
/// a duplicate lands as `Errored`.
#[tokio::test(start_paused = true)]
async fn muster_leaves_an_app_the_flock_already_has_where_it_stands() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    let roll = roll_of(vec![AppConfig::minimal("web", "./srv")]);
    write_atomic(&paths.snapshot, &roll).unwrap();

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();
    let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
    registry.record(std::slice::from_ref(&app));
    handle.start(vec![app]).await.unwrap();

    let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
        .await
        .unwrap();
    assert_eq!(
        restored,
        vec!["web".to_string()],
        "the roll's app is restored whether or not this call is what started it"
    );
    let listed = handle.list().await;
    assert_eq!(listed.len(), 1, "one instance of a one-instance app");
    assert_eq!(listed[0].status, ProcStatus::Online);
    handle.shutdown().await;
}

/// fails if a missing roll becomes an error. A fresh `$SHEP_HOME` has
/// none, and a daemon that refused over it could not boot on a clean
/// machine.
#[tokio::test(start_paused = true)]
async fn a_missing_roll_restores_nothing_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    assert!(!paths.snapshot.exists());

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();

    assert_eq!(
        muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .unwrap(),
        Vec::<String>::new()
    );
    assert!(handle.list().await.is_empty());
    handle.shutdown().await;
}
