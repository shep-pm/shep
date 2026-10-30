//! The disabled mark across a handover: the snapshot writes it and an
//! adopting successor reads it back.

use super::super::adopt_fixtures::{
    AdoptingRunner, STAND_IN_SPAWN_PID, carried, counters, daemon_fds, without_handles,
};
use super::*;

/// Fails if the snapshot stops reading the mark off the actor, so a
/// successor would never hear that `api` was parked.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_snapshot_carries_the_mark_of_a_disabled_app_and_no_other() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(vec![
            normalize(AppConfig::minimal("api", "./srv")).unwrap(),
            normalize(AppConfig::minimal("web", "./srv")).unwrap(),
        ])
        .await
        .unwrap();
    handle
        .disable(ProcessSelector::Name("api".to_string()))
        .await
        .unwrap();

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let marks: Vec<(&str, Option<bool>)> = blob
        .sheep()
        .iter()
        .map(|sheep| (sheep.name(), sheep.disabled()))
        .collect();
    assert_eq!(marks, [("api", Some(true)), ("web", Some(false))]);
}

/// Fails if adoption stops restoring the mark: the successor would list `api`
/// as enabled, open to every restart the mark exists to refuse. The two
/// carried shapes are a running instance and one registered at rest.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_keeps_the_disabled_mark() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried("api", 0, Some(4242), |_| {}).with_disabled(true)),
                without_handles(carried("job", 1, None, |_| {}).with_disabled(true)),
                without_handles(carried("web", 2, Some(4243), |_| {}).with_disabled(false)),
            ],
            counters(3),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let mut listed = sup.list().await;
    listed.sort_unstable_by_key(|row| row.id);
    let marks: Vec<(&str, Option<bool>)> = listed
        .iter()
        .map(|row| (row.name.as_str(), row.disabled))
        .collect();
    assert_eq!(
        marks,
        [("api", Some(true)), ("job", Some(true)), ("web", None)]
    );
}

/// The half of the downgrade the Upgrading page has to get right: a carried row
/// with no process is installed as a stopped slot and the successor spawns
/// nothing for it. A spawn would report `STAND_IN_SPAWN_PID`, and the `Online`
/// transition that arms a watch or a schedule never happens, so a parked sheep
/// crosses the handover down and simply stops being refused.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_parked_sheep_is_installed_stopped_and_left_down() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(
                carried("parked", 0, None, |entry| {
                    entry.status = ProcStatus::Stopped
                })
                .with_disabled(true),
            )],
            counters(1),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let listed = sup.list().await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    let row = &listed[0];
    assert_eq!(row.status, ProcStatus::Stopped, "{row:?}");
    assert_eq!(row.pid, None, "a spawn would report {STAND_IN_SPAWN_PID}");
    assert_eq!(row.disabled, Some(true), "{row:?}");
}
