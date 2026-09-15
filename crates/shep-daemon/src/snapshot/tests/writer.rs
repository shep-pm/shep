//! The debounced background writer: a burst of lifecycle events collapses
//! into one write, a registry recording with no bus event of its own still
//! schedules one, and log traffic never triggers one.

use super::super::*;
use super::info;

use shep_core::config::{AppConfig, normalize};
use shep_core::protocol::{BusEvent, ProcessEventKind};
use shep_core::status::ProcStatus;
use std::time::Duration;

use crate::fake::{ProcScript, ScriptedRunner};
use crate::supervisor::spawn_supervisor;
use crate::testing::test_paths;

#[tokio::test(start_paused = true)]
async fn writer_coalesces_a_burst_into_one_write() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(&paths.home).unwrap();
    let (events, _keep) = crate::bus::test_bus(64);
    let supervisor = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();
    let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
    registry.record(std::slice::from_ref(&app));
    supervisor.start(vec![app]).await.unwrap();

    // Subscribing here means the start's own events are already behind us.
    let writer = spawn_snapshot_writer(
        paths.snapshot.clone(),
        supervisor.clone(),
        registry,
        events.subscribe(),
    );
    for event in [
        ProcessEventKind::Exit,
        ProcessEventKind::Restart,
        ProcessEventKind::Online,
    ] {
        events
            .send(
                BusEvent::Process {
                    event,
                    info: info(0, "web", ProcStatus::Online),
                    manually: false,
                    at_ms: 0,
                }
                .into(),
            )
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(SNAPSHOT_DEBOUNCE_MS + 1)).await;

    assert_eq!(writer.writes(), 1, "one debounce window is one write");
    let roll = read(&paths.snapshot).unwrap();
    assert_eq!(roll.apps.len(), 1);
    assert_eq!(roll.apps[0].instances_running, 1);
    writer.stop().await;
}

/// fails if the bus is the writer's only schedule. A config write that
/// parks a field moves no process, so it publishes no
/// [`BusEvent::Process`], and the registry's own recording is the roll's
/// only route to disk before the next unrelated lifecycle event or a
/// graceful shutdown.
#[tokio::test(start_paused = true)]
async fn writer_schedules_a_write_for_a_recording_the_bus_says_nothing_about() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(&paths.home).unwrap();
    let (events, _keep) = crate::bus::test_bus(64);
    let supervisor = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();
    let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
    supervisor.start(vec![app.clone()]).await.unwrap();

    // Subscribing here means the start's own events are already behind us,
    // so the bus has nothing left to say about this flock.
    let writer = spawn_snapshot_writer(
        paths.snapshot.clone(),
        supervisor.clone(),
        registry.clone(),
        events.subscribe(),
    );
    assert!(!paths.snapshot.exists(), "nothing has recorded yet");

    registry.record(std::slice::from_ref(&app));
    tokio::time::sleep(Duration::from_millis(SNAPSHOT_DEBOUNCE_MS + 1)).await;

    assert_eq!(writer.writes(), 1, "a recording leaves the roll dirty");
    let roll = read(&paths.snapshot).unwrap();
    assert_eq!(roll.apps.len(), 1);
    writer.stop().await;
}

#[tokio::test(start_paused = true)]
async fn writer_ignores_log_traffic() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(&paths.home).unwrap();
    let (events, _keep) = crate::bus::test_bus(64);
    let supervisor = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        paths.clone(),
        events.clone(),
    );
    let writer = spawn_snapshot_writer(
        paths.snapshot.clone(),
        supervisor,
        FlockRegistry::new(),
        events.subscribe(),
    );
    for id in 0..50 {
        events
            .send(
                BusEvent::LogOut {
                    id,
                    line: "chatty".to_string(),
                }
                .into(),
            )
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(SNAPSHOT_DEBOUNCE_MS * 4)).await;
    assert_eq!(writer.writes(), 0, "log lines must never rewrite the roll");
    assert!(!paths.snapshot.exists());
    writer.stop().await;
}
