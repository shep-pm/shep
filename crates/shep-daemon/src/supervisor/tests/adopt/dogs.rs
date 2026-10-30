//! Dogs across a handover: which ones the blob carries, and what a successor
//! does with the ones its own `shep.toml` does not enable.
//!
//! The predecessor never reads `shep.toml`, so a dog's mark is its absence
//! from the enabled list the caller handed it. That list is the successor's to
//! read, which is why a parked dog is left behind rather than carried.

use super::super::adopt_fixtures::{
    AdoptingRunner, adoptable_child, carried, counters, daemon_fds, flock_until, without_handles,
};
use super::*;
use std::collections::BTreeMap;

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_successor_reports_a_carried_dogs_spawn_refusal() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let dir = tempfile::tempdir().unwrap();
        let (events, _rx) = crate::bus::test_bus(64);
        let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
            .spawn_adopted(
                vec![without_handles(carried("kelpie", 7, None, |entry| {
                    entry.dog = Some(DogSource::BuiltIn { grants: None });
                    entry.status = ProcStatus::Errored;
                    entry.spawn_failure = Some("kelpie: spawn refused".to_string());
                }))],
                counters(8),
                Vec::new(),
            )
            .unwrap();

        let rows = sup.list().await;
        assert_eq!(rows[0].status, ProcStatus::Errored);
        assert_eq!(
            rows[0].spawn_failure.as_deref(),
            Some("kelpie: spawn refused")
        );
        sup.shutdown().await;
    })
    .await
    .expect("a successor must report a carried spawn refusal");
}

/// One dog as the boot hands it over: built-in, and named as `shep.toml`
/// would name it.
fn dog(name: &str) -> crate::dogs::DogSpec {
    crate::dogs::DogSpec {
        name: name.to_string(),
        source: DogSource::BuiltIn { grants: None },
        env: BTreeMap::new(),
    }
}

/// The app [`dog`] names, assembled the way the boot assembles it.
fn dog_app(name: &str, paths: &shep_core::paths::ShepPaths) -> shep_core::config::ResolvedApp {
    crate::dogs::dog_app(&dog(name), paths).expect("a built-in dog assembles")
}

fn names(wanted: &[&str]) -> BTreeSet<String> {
    wanted.iter().map(|name| (*name).to_string()).collect()
}

/// The blob describes the flock this daemon supervises, minus the dogs its
/// caller's config leaves out. A dog an operator stopped with `shep stop`
/// carries, since the config still names it: only a mark with no process on it
/// leaves a dog behind.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_snapshot_carries_every_dog_that_is_not_parked_and_leaves_one_that_is() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let (fds, _held) = daemon_fds(&dir);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 3]);
    let sup = spawn_supervisor(runner, paths.clone(), events);
    sup.start(vec![normalize(AppConfig::minimal("api", "./srv")).unwrap()])
        .await
        .unwrap();
    // Running, and enabled: the ordinary carried dog.
    sup.start_dog(
        dog_app("metrics", &paths),
        DogSource::BuiltIn { grants: None },
    )
    .await
    .unwrap();
    // `Stopped` with no mark, which is the shape `shep stop` leaves.
    sup.register_dogs_at_rest(
        vec![(dog_app("bark", &paths), DogSource::BuiltIn { grants: None })],
        BTreeSet::new(),
    )
    .await
    .unwrap();
    // Disabled: absent from the enabled list, which is a dog's whole mark.
    sup.register_dogs_at_rest(
        vec![(
            dog_app("log-rotate", &paths),
            DogSource::Adopted {
                grants: None,
                path: "/opt/bin/shep-log-rotate".to_string(),
                channel: false,
            },
        )],
        names(&["log-rotate"]),
    )
    .await
    .unwrap();

    let (_candidates, blob, _parked) = sup.handover_snapshot(fds).await.unwrap();

    let carried: Vec<(&str, Option<DogSource>)> = blob
        .sheep()
        .iter()
        .map(|sheep| (sheep.name(), sheep.dog().cloned()))
        .collect();
    assert_eq!(
        carried,
        [
            ("api", None),
            ("metrics", Some(DogSource::BuiltIn { grants: None })),
            ("bark", Some(DogSource::BuiltIn { grants: None })),
        ],
        "a parked dog belongs to the successor's file, not to this flock's blob"
    );
}

/// The shape `shep disable` holds from the moment it sets the mark until its
/// stop finishes: marked, and still holding a process. It carries, because the
/// snapshot is the only way a successor learns a pid.
///
/// The mark arrives through [`SupervisorHandle::register_at_rest`], the actor's
/// door for a mark with no stop attached, and gets there without a kill ladder
/// a paused clock has to hold mid-flight.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_snapshot_carries_a_disabled_dog_that_still_holds_a_process() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let (fds, _held) = daemon_fds(&dir);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 2]);
    let sup = spawn_supervisor(runner, paths.clone(), events);
    sup.start(vec![normalize(AppConfig::minimal("api", "./srv")).unwrap()])
        .await
        .unwrap();
    sup.start_dog(
        dog_app("metrics", &paths),
        DogSource::BuiltIn { grants: None },
    )
    .await
    .unwrap();
    sup.register_at_rest(vec![dog_app("metrics", &paths)], names(&["metrics"]))
        .await
        .unwrap();

    let flock = sup.list().await;
    let metrics = flock.iter().find(|row| row.name == "metrics").unwrap();
    assert_eq!(metrics.status, ProcStatus::Online, "{metrics:?}");
    assert_eq!(metrics.disabled, Some(true), "{metrics:?}");

    let (_candidates, blob, _parked) = sup.handover_snapshot(fds).await.unwrap();
    let carried: Vec<(&str, Option<DogSource>)> = blob
        .sheep()
        .iter()
        .map(|sheep| (sheep.name(), sheep.dog().cloned()))
        .collect();
    assert_eq!(
        carried,
        [
            ("api", None),
            ("metrics", Some(DogSource::BuiltIn { grants: None }))
        ],
        "a marked dog still holding a process must cross the exec described"
    );
}

/// The successor's half. `metrics` crossed the `execve` running, because the
/// predecessor booted while the file named it, and the file this successor
/// boots from no longer does: it goes down through the same door `shep disable`
/// uses, so it ends stopped, marked, and with `shep enable <name>` as the way
/// back. `bark` was never carried, and lands at rest under the id this
/// successor issues.
#[cfg(unix)]
#[tokio::test]
async fn a_successor_stops_a_carried_dog_its_config_no_longer_enables() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), paths.clone(), events)
        .spawn_adopted(
            vec![
                without_handles(carried("api", 0, None, |entry| {
                    entry.status = ProcStatus::Stopped;
                })),
                without_handles(carried("metrics", 1, Some(pid), |entry| {
                    entry.dog = Some(DogSource::BuiltIn { grants: None });
                })),
            ],
            counters(2),
            Vec::new(),
        )
        .expect("a carried flock installs");

    crate::dogs::register_dogs_at_rest(&[dog("metrics"), dog("bark")], &[], &paths, &sup).await;

    let rows = flock_until(
        &sup,
        |rows| {
            rows.iter()
                .any(|row| row.name == "metrics" && row.status == ProcStatus::Stopped)
        },
        "a dog the successor's config does not enable must stop, not just be listed",
    )
    .await;

    let metrics = rows.iter().find(|row| row.name == "metrics").unwrap();
    assert_eq!(metrics.id, 1, "the carried row is the successor's to stop");
    assert_eq!(metrics.pid, None, "{metrics:?}");
    assert_eq!(metrics.disabled, Some(true), "{metrics:?}");
    assert_eq!(
        metrics.last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(15),
        }),
        "the process that crossed the exec must have been signalled, not dropped from the flock"
    );

    let bark = rows.iter().find(|row| row.name == "bark").unwrap();
    assert_eq!(bark.status, ProcStatus::Stopped, "{bark:?}");
    assert_eq!(bark.disabled, Some(true), "{bark:?}");
    assert_eq!(
        bark.id, 2,
        "a dog left behind gets the id this successor issues"
    );
    assert_eq!(
        rows.iter().find(|row| row.name == "api").unwrap().id,
        0,
        "the flock the successor inherited keeps its rows"
    );
}

/// The other half of the same file read: a dog the successor's config still
/// enables keeps its carried row, its pid and its process. Stopping it here
/// would turn every reload into a restart of the monitoring.
#[cfg(unix)]
#[tokio::test]
async fn a_successor_leaves_a_carried_dog_its_config_still_enables_running() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), paths.clone(), events)
        .spawn_adopted(
            vec![without_handles(carried("metrics", 1, Some(pid), |entry| {
                entry.dog = Some(DogSource::BuiltIn { grants: None });
            }))],
            counters(2),
            Vec::new(),
        )
        .expect("a carried flock installs");

    crate::dogs::register_dogs_at_rest(&[dog("metrics")], &[dog("metrics")], &paths, &sup).await;

    let rows = sup.list().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let metrics = &rows[0];
    assert_eq!(metrics.status, ProcStatus::Online, "{metrics:?}");
    assert_eq!(
        metrics.pid,
        Some(pid),
        "a dog the config enables must keep the process it carried"
    );
    assert_eq!(metrics.disabled, None, "{metrics:?}");
}

/// The other way a name leaves the file: the dog was already down, by `shep
/// stop` rather than `shep disable`, so it carried with its row and no mark.
/// The successor's config does not enable it either, so the same call marks
/// it, and the row stops reading as one `shep restart` could start.
#[cfg(unix)]
#[tokio::test]
async fn a_successor_marks_a_carried_dog_it_stopped_and_the_config_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(TokioRunner::new(), paths.clone(), events)
        .spawn_adopted(
            vec![without_handles(carried("metrics", 1, None, |entry| {
                entry.dog = Some(DogSource::BuiltIn { grants: None });
                entry.status = ProcStatus::Stopped;
            }))],
            counters(2),
            Vec::new(),
        )
        .expect("a carried flock installs");

    crate::dogs::register_dogs_at_rest(&[dog("metrics")], &[], &paths, &sup).await;

    let rows = sup.list().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let metrics = &rows[0];
    assert_eq!(
        metrics.id, 1,
        "a stopped row the predecessor left is the successor's to mark, not to reissue"
    );
    assert_eq!(metrics.status, ProcStatus::Stopped, "{metrics:?}");
    assert_eq!(metrics.disabled, Some(true), "{metrics:?}");
}

/// The other way a carried dog escapes what this boot registered: its own
/// config would not assemble, so it never reached the actor at all, while the
/// process it carried is in the flock regardless. `stop_unenabled_dogs` reads
/// the flock, so that dog goes down too.
///
/// The assembly fails on `adopted_dogs = { metrics = "" }`: an adopted source
/// with no binary, which `normalize` refuses as `MissingScript`.
#[cfg(unix)]
#[tokio::test]
async fn a_successor_stops_a_carried_dog_whole_config_could_not_assemble() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), paths.clone(), events)
        .spawn_adopted(
            vec![without_handles(carried("metrics", 1, Some(pid), |entry| {
                entry.dog = Some(DogSource::BuiltIn { grants: None });
            }))],
            counters(2),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let unassemblable = crate::dogs::DogSpec {
        name: "metrics".to_string(),
        source: DogSource::Adopted {
            grants: None,
            path: String::new(),
            channel: false,
        },
        env: BTreeMap::new(),
    };
    crate::dogs::register_dogs_at_rest(&[unassemblable], &[], &paths, &sup).await;

    let rows = flock_until(
        &sup,
        |rows| {
            rows.iter()
                .any(|row| row.name == "metrics" && row.status == ProcStatus::Stopped)
        },
        "a carried dog this boot could not assemble must still stop",
    )
    .await;

    let metrics = rows.iter().find(|row| row.name == "metrics").unwrap();
    assert_eq!(metrics.id, 1, "the carried row is the successor's to stop");
    assert_eq!(metrics.pid, None, "{metrics:?}");
    assert_eq!(metrics.disabled, Some(true), "{metrics:?}");
    assert_eq!(
        metrics.last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(15),
        }),
        "the dog must have gone down the ordinary ladder"
    );
    assert!(
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
            None
        )
        .is_err(),
        "the process must have been reaped, not merely marked"
    );
}
