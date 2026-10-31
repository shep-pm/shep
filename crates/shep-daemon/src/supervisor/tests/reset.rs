//! Tests for `reset`: a selected sheep gets its restart count and budget
//! back, and nothing else about it moves.

use super::start_stop::{wait_for_restarts_online, wait_for_settled};
use super::*;

fn name(name: &str) -> ProcessSelector {
    ProcessSelector::Name(name.to_string())
}

/// Two unstable crashes, then a proc that dies unstable after 100ms, then one
/// that lives. With three restarts allowed, a budget that was not reset errors
/// on the third crash.
async fn svc_restarted_twice(
    dir: &tempfile::TempDir,
) -> (SupervisorHandle, Arc<ScriptedRunner>, ProcessInfo) {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
        ProcScript::stable_then_exit(100, 1),
        ProcScript::never_exits(),
    ]));
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    app.max_restarts = 3;
    // The sync busy-polls under a paused clock, so a backoff would spin it.
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    let info = wait_for_restarts_online(&handle, 2).await;
    assert_eq!(
        (info.status, info.restarts),
        (ProcStatus::Online, 2),
        "never reached the third proc: {info:?}"
    );
    (handle, runner, info)
}

/// Fails if a reset leaves the count or the budget behind, or touches the
/// process: the next unstable exit would be the third, and error the sheep.
#[tokio::test(start_paused = true)]
async fn reset_zeroes_the_count_and_the_budget_and_leaves_the_process_running() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, before) = svc_restarted_twice(&dir).await;

    let rows = handle.reset(name("svc")).await.unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].restarts, 0);
    assert_eq!(rows[0].status, ProcStatus::Online);
    assert_eq!(rows[0].pid, before.pid, "a reset must not respawn");
    assert_eq!(runner.spawn_count(), 3, "a reset must not spawn");

    // The third proc dies unstable: the first of three on a zeroed budget.
    // Sleeping lets the paused clock reach its exit, which polling would not.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let settled = wait_for_settled(&handle, 1).await;
    assert_eq!(
        (settled.status, settled.restarts),
        (ProcStatus::Online, 1),
        "the spent unstable exits stayed on the books: {settled:?}"
    );
}

/// Fails if a reset revives an errored sheep: only a start or a restart
/// spawns one.
#[tokio::test(start_paused = true)]
async fn reset_leaves_an_errored_sheep_errored() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    app.max_restarts = 2;
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    let waits = async {
        while handle.list().await[0].status != ProcStatus::Errored {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), waits)
        .await
        .expect("two unstable exits on two allowed restarts must error the sheep");
    assert_eq!(handle.list().await[0].restarts, 1);

    let rows = handle.reset(name("svc")).await.unwrap();

    assert_eq!(rows[0].status, ProcStatus::Errored);
    assert_eq!(rows[0].restarts, 0);
    assert_eq!(runner.spawn_count(), 2, "a reset must not spawn");
    assert_eq!(handle.list().await[0].status, ProcStatus::Errored);
}

fn exit_info() -> ExitInfo {
    ExitInfo {
        code: Some(1),
        signal: None,
    }
}

fn reset_in_place(
    actor: &mut Actor<ScriptedRunner>,
    selector: &ProcessSelector,
) -> Vec<ProcessInfo> {
    let (reply, mut answer) = oneshot::channel();
    actor.handle_reset(selector, reply);
    answer
        .try_recv()
        .expect("a reset answers on the actor loop")
        .expect("the selector matches")
}

/// Fails if a reset clears the exit record of a stopped dog: it would then
/// read as at rest, and an enable would hand back its id without spawning it.
#[tokio::test(start_paused = true)]
async fn reset_clears_last_exit_except_on_a_stopped_dog() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, Vec::new());
    let metrics = actor
        .register_dog_at_rest(
            &normalize(AppConfig::minimal("metrics", "/dogs/metrics")).unwrap(),
            DogSource::BuiltIn { grants: None },
            &BTreeSet::new(),
        )
        .id;
    let sheep_id = actor.next_id;
    actor.next_id += 1;
    let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
    let mut entry = armed_entry(sheep_id, 0, 4242, app, &test_paths(&dir));
    entry.last_exit = Some(exit_info());
    entry.restarts = 5;
    actor.sheep.insert(sheep_id, SheepSlot::new(entry));
    let dog = &mut actor.sheep.get_mut(&metrics).unwrap().entry;
    dog.last_exit = Some(exit_info());
    dog.restarts = 5;

    reset_in_place(&mut actor, &name("web"));
    reset_in_place(&mut actor, &name("metrics"));

    let sheep = &actor.sheep[&sheep_id].entry;
    assert_eq!((sheep.last_exit, sheep.restarts), (None, 0));
    let dog = &actor.sheep[&metrics].entry;
    assert_eq!(dog.restarts, 0);
    assert_eq!(
        dog.last_exit,
        Some(exit_info()),
        "a stopped dog that ran must stay not-at-rest"
    );
}

/// Fails if a reload copies a count the reset zeroed, or the drainee's spent
/// budget leaks onto the replacement.
#[tokio::test(start_paused = true)]
async fn a_reload_after_a_reset_carries_zero_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    let before = wait_for_restarts_online(&handle, 2).await;
    assert_eq!(
        (before.status, before.restarts),
        (ProcStatus::Online, 2),
        "never reached the drainee with two restarts to carry: {before:?}"
    );
    handle.reset(name("svc")).await.unwrap();

    handle.reload(name("svc")).await.unwrap();
    let waits = async {
        loop {
            let rows = handle.list().await;
            if rows.len() == 1 && rows[0].id == 1 && rows[0].status == ProcStatus::Online {
                return rows;
            }
            // Sleeping, not yielding: the reload's own deadline needs the
            // paused clock to advance.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let rows = tokio::time::timeout(Duration::from_secs(30), waits)
        .await
        .expect("the reload must swap in its replacement");

    assert_eq!(rows[0].restarts, 0);
}

#[tokio::test(start_paused = true)]
async fn reset_matching_nothing_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(ScriptedRunner::new(vec![]), test_paths(&dir), events);

    let result = handle.reset(name("ghost")).await;

    assert_eq!(result, Err(SupervisorError::NotFound));
}

/// Fails if a reset cancels or re-arms the restart timer: the respawn would
/// come early, or never, instead of at the delay the exit chose.
#[tokio::test(start_paused = true)]
async fn reset_leaves_a_waiting_restart_timer_running() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
    ]));
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    app.restart_delay = Some("1h".parse().unwrap());
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    // Not under the sync helpers: they busy-poll, and the clock would not
    // advance to the hour.
    for _ in 0..2 {
        let waits = async {
            while handle.list().await[0].status != ProcStatus::WaitingRestart {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(30), waits)
            .await
            .expect("an exit with autorestart on must reach WaitingRestart");
        if runner.spawn_count() == 1 {
            tokio::time::advance(Duration::from_secs(3600)).await;
        }
    }
    assert_eq!(handle.list().await[0].restarts, 1);
    // Halfway through the delay, so a timer re-armed by the reset would
    // still have a full hour to run.
    tokio::time::advance(Duration::from_secs(1800)).await;

    let rows = handle.reset(name("svc")).await.unwrap();

    assert_eq!(rows[0].status, ProcStatus::WaitingRestart);
    assert_eq!(rows[0].restarts, 0);
    assert_eq!(runner.spawn_count(), 2, "a reset must not respawn early");
    tokio::time::advance(Duration::from_secs(1800)).await;
    let waits = async {
        while handle.list().await[0].status != ProcStatus::Online {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), waits)
        .await
        .expect("the armed timer must still respawn the sheep");
    assert_eq!(runner.spawn_count(), 3);
}

/// Fails if the rows come back in id order: `stop` and every other verb
/// answer `(name, instance, id)`.
#[tokio::test(start_paused = true)]
async fn reset_answers_the_matched_rows_in_flock_order() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits(); 3]),
        test_paths(&dir),
        events,
    );
    let mut web = AppConfig::minimal("web", "./srv");
    web.instances = 2;
    handle
        .start(vec![
            normalize(web).unwrap(),
            normalize(AppConfig::minimal("api", "./api")).unwrap(),
        ])
        .await
        .unwrap();

    let rows = handle.reset(ProcessSelector::All).await.unwrap();

    assert_eq!(
        rows.iter()
            .map(|row| (row.name.as_str(), row.id))
            .collect::<Vec<_>>(),
        vec![("api", 2), ("web", 0), ("web", 1)]
    );
}
