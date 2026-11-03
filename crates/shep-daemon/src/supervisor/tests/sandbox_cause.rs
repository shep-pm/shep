//! `ProcessInfo::sandbox_refused`: why an `errored` row stopped.

use super::*;

#[cfg(unix)]
const DOG_PATH: &str = "/opt/dogs/box";

#[cfg(unix)]
fn sandbox_dog() -> BTreeMap<String, String> {
    BTreeMap::from([("sandbox".to_owned(), DOG_PATH.to_owned())])
}

#[cfg(unix)]
fn jailed_sheep() -> ResolvedApp {
    let mut app = AppConfig::minimal("jailed", "./srv");
    app.args = vec!["--port".to_owned(), "8080".to_owned()];
    app.dogs.insert(
        "sandbox".to_owned(),
        serde_json::from_value(serde_json::json!({})).unwrap(),
    );
    normalize(app).unwrap()
}

#[cfg(unix)]
fn through_the_dog() -> Vec<String> {
    ["launch", "jailed", "--", "./srv", "--port", "8080"]
        .map(String::from)
        .to_vec()
}

async fn wait_errored(handle: &SupervisorHandle) -> ProcessInfo {
    let waits = async {
        loop {
            let row = handle.list().await.remove(0);
            if row.status == ProcStatus::Errored {
                return row;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), waits)
        .await
        .expect("the sheep must reach errored")
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_refused_sheep_with_a_sandbox_dog_table_reports_the_refusal() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(125)]);
    let dir = tempfile::tempdir().unwrap();
    let handle = SupervisorBuilder::new(runner, test_paths(&dir), events)
        .dog_binaries(sandbox_dog())
        .spawn();
    handle.start(vec![jailed_sheep()]).await.unwrap();

    assert_eq!(wait_errored(&handle).await.sandbox_refused, Some(true));
}

#[tokio::test(start_paused = true)]
async fn an_exit_125_from_a_sheep_with_no_sandbox_is_not_a_refusal() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![
        ProcScript::const_exit(125),
        ProcScript::const_exit(125),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("plain", "./srv");
    app.max_restarts = 2;
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(wait_errored(&handle).await.sandbox_refused, Some(false));
}

#[tokio::test(start_paused = true)]
async fn an_errored_sheep_out_of_restarts_reports_no_refusal() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(1), ProcScript::const_exit(1)]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    app.max_restarts = 2;
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(wait_errored(&handle).await.sandbox_refused, Some(false));
}

#[tokio::test(start_paused = true)]
async fn a_running_sheep_has_no_refusal_to_report() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].sandbox_refused, None);
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_sandboxed_sheep_spawns_through_the_dog_the_builder_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events)
            .dog_binaries(sandbox_dog())
            .spawn();

    handle.start(vec![jailed_sheep()]).await.unwrap();

    assert_eq!(runner.spawned_program(0), DOG_PATH);
    assert_eq!(runner.spawned_args(0), through_the_dog());
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_sandboxed_sheep_with_no_dog_lands_errored_and_never_spawns() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events).spawn();

    let started = handle.start(vec![jailed_sheep()]).await;

    let refusal = started.expect_err("a sandboxed sheep must not start without its dog");
    assert!(
        refusal.to_string().contains("sandbox"),
        "the refusal names the dog: {refusal}"
    );
    let row = wait_errored(&handle).await;
    assert_eq!(row.restarts, 0, "the refusal is never retried");
    assert_eq!(runner.spawn_count(), 0, "nothing ran, sandboxed or not");
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_disabled_sandbox_dog_counts_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events)
            .dog_binaries(sandbox_dog())
            .spawn();
    handle
        .start_dog(dog_app("sandbox"), adopted(DOG_PATH))
        .await
        .unwrap();
    handle.disable_dog("sandbox".to_owned()).await.unwrap();
    let spawned_before = runner.spawn_count();

    handle.start(vec![jailed_sheep()]).await.unwrap_err();

    assert_eq!(runner.spawn_count(), spawned_before);
}

#[cfg(unix)]
fn adopted(path: &str) -> DogSource {
    DogSource::Adopted {
        path: path.to_owned(),
        channel: false,
        grants: None,
        events: Vec::new(),
    }
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_dog_started_after_boot_is_used_by_the_next_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events).spawn();
    handle.start(vec![jailed_sheep()]).await.unwrap_err();
    assert_eq!(runner.spawn_count(), 0);

    handle
        .start_dog(dog_app("sandbox"), adopted(DOG_PATH))
        .await
        .unwrap();
    handle
        .restart(ProcessSelector::Name("jailed".to_owned()))
        .await
        .unwrap();

    let sheep = runner.spawn_index_of("jailed").expect("the sheep spawned");
    assert_eq!(runner.spawned_program(sheep), DOG_PATH);
    assert_eq!(runner.spawned_args(sheep), through_the_dog());
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_restart_and_a_reload_spawn_through_the_dog_too() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events)
            .dog_binaries(sandbox_dog())
            .spawn();
    handle.start(vec![jailed_sheep()]).await.unwrap();
    let by_name = ProcessSelector::Name("jailed".to_owned());

    handle.restart(by_name.clone()).await.unwrap();
    handle.reload(by_name).await.unwrap();

    assert_eq!(runner.spawn_count(), 3);
    for spawn in 0..3 {
        assert_eq!(runner.spawned_program(spawn), DOG_PATH, "spawn {spawn}");
        assert_eq!(
            runner.spawned_args(spawn),
            through_the_dog(),
            "spawn {spawn}"
        );
    }
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_start_dog_that_loses_the_name_to_a_running_sheep_records_no_path() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events).spawn();
    handle
        .start(vec![
            normalize(AppConfig::minimal("sandbox", "./srv")).unwrap(),
        ])
        .await
        .unwrap();
    handle
        .start_dog(dog_app("sandbox"), adopted(DOG_PATH))
        .await
        .unwrap();
    let spawned_before = runner.spawn_count();

    handle.start(vec![jailed_sheep()]).await.unwrap_err();

    assert_eq!(runner.spawn_count(), spawned_before);
}

#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
#[tokio::test(start_paused = true)]
async fn a_second_start_dog_on_a_running_dog_leaves_its_path_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events).spawn();
    handle
        .start_dog(dog_app("sandbox"), adopted(DOG_PATH))
        .await
        .unwrap();
    handle
        .start_dog(dog_app("sandbox"), adopted("/elsewhere/box"))
        .await
        .unwrap();

    handle.start(vec![jailed_sheep()]).await.unwrap();

    let sheep = runner.spawn_index_of("jailed").expect("the sheep spawned");
    assert_eq!(runner.spawned_program(sheep), DOG_PATH);
}
