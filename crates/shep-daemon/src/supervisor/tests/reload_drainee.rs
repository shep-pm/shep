//! Tests for what happens to the instance a reload is replacing.
//!
//! A drainee is on its way out but still running, so reports keep arriving
//! against it. It must not be restarted, must not be counted, and must hand its
//! restart count and last exit to the replacement.

use super::*;

// Pins the outcome rather than either mechanism behind it:
// `handle_extra_restart`'s guard 4 rejects a status that is not `Online`,
// and `begin_manual` drops an automatic restart against either half of an
// uncommitted swap.
#[tokio::test(start_paused = true)]
async fn a_report_raised_against_a_drainee_never_takes_it_off_the_reload() {
    let dir = tempfile::tempdir().unwrap();
    // Three scripts for two spawns: the third lets a wrongly restarted
    // drainee succeed, so the count below reads the extra spawn.
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(); 3],
    )
    .await;
    let pid = handle.list().await[0].pid.expect("a live sheep has a pid");

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;
    handle.extra_restart(0, pid, None, None).await;

    // `Restart`, not `Delete`: the bug respawns the drainee into the slot
    // its replacement holds, and never emits a `Delete` for this id.
    assert_no_event_within(
        &mut rx,
        0,
        ProcessEventKind::Restart,
        Duration::from_millis(1_000),
    )
    .await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopping);

    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;
    assert_eq!(
        runner.kill_counts().len(),
        2,
        "the report never caused a spawn"
    );
}

// The end-to-end form of
// `a_report_raised_against_a_drainee_never_takes_it_off_the_reload`: a real
// `liveness_probe`, the real `OsProber`, the daemon's own extras reporter,
// and a reload the supervisor performs.
#[tokio::test(start_paused = true)]
async fn a_drainee_whose_liveness_probe_fails_is_reaped_rather_than_restarted() {
    let dir = tempfile::tempdir().unwrap();
    // Reserve a port and release it: nothing ever listens there, so every
    // probe below fails.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserved.local_addr().unwrap();
    drop(reserved);

    let mut app = AppConfig::minimal("web", "./srv");
    // Both halves go online by hand, so the liveness failure lands inside
    // `AwaitReady` rather than racing a deadline.
    app.wait_ready = true;
    // The window cannot elapse while this case waits out a probe interval.
    app.listen_timeout = UpDuration::from_millis(60_000);
    app.liveness_probe = Some(ProbeConfig {
        // The floor `spawn_liveness_task` honours anyway.
        interval: UpDuration::from_millis(1_000),
        timeout: UpDuration::from_millis(500),
        failure_threshold: 1,
        ..probe_config(ProbeKind::Tcp, &addr.to_string())
    });

    // Four scripts for three spawns: the fourth lets the respawn a broken
    // implementation makes land live rather than `Errored`.
    let (events, mut rx) = crate::bus::test_bus(256);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits(); 4]));
    let (breaches_tx, breaches_rx) = mpsc::channel(8);
    let (liveness_tx, liveness_rx) = mpsc::channel(8);
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events)
            .extras(Extras {
                // The liveness half of `reports` is why the extras are
                // wired here; no `cron_restart` and no `max_memory`.
                clock: Arc::new(SystemClock),
                enforcer: Arc::new(RecordingEnforcer::default()),
                max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
                reports: ExtrasReports {
                    breaches: breaches_tx,
                    liveness: liveness_tx,
                },
                stats: idle_stats(),
            })
            .spawn();
    // The reporter is the step that turns a `LivenessFailure` into the
    // `extra_restart` the two rejections rule on.
    let _reporter = spawn_extras_reporter(breaches_rx, liveness_rx, handle.clone());
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    expect_event(&mut rx, 0, ProcessEventKind::Online).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    // Five probe intervals, and far short of the 60s readiness deadline.
    // `Restart`, not `Delete`: the bug respawns rather than deregisters.
    assert_no_event_within(
        &mut rx,
        0,
        ProcessEventKind::Restart,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopping);
    assert_eq!(
        runner.kill_counts().len(),
        2,
        "the failing probe never caused a spawn"
    );

    handle.tx.send(Msg::Ready { id: 1 }).await.unwrap();
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;
    let after = handle.list().await;
    assert_eq!(after.len(), 1, "the drainee left no registration behind");
    assert_eq!(after[0].id, 1);

    // The control: the replacement's own probe failure does restart it, so
    // the chain this case rests on is delivering.
    expect_event(&mut rx, 1, ProcessEventKind::Restart).await;
}

// An `autorestart` app's drainee handed to `decide_on_exit` would be
// respawned into the instance slot its replacement already holds.
#[tokio::test(start_paused = true)]
async fn a_drainee_that_exits_on_its_own_is_never_restarted() {
    let dir = tempfile::tempdir().unwrap();
    // The original ends 1000ms in, a stable run the policy would restart.
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![
            ProcScript::stable_then_exit(1_000, 1),
            ProcScript::never_exits(),
        ],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;

    let after = handle.list().await;
    assert_eq!(after.len(), 1, "the drainee left no registration behind");
    assert_eq!(after[0].id, 1);
    assert_eq!(after[0].status, ProcStatus::Online);
    assert_eq!(runner.kill_counts().len(), 2, "the drainee never respawned");
}

// A `stop` leaves a sheep registered and `Stopped`; deregistering both
// entries would take the app out of `shep flock`. The replacement defies
// its signal, putting its exit a whole `kill_timeout` behind the drainee's,
// which is the order that reaches the branch.
#[tokio::test(start_paused = true)]
async fn an_operators_stop_mid_reload_leaves_the_app_stopped_and_registered() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(), ProcScript::ignores_signals()],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    let stopped = handle
        .stop(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the stop reaches both entries");
    assert_eq!(stopped.len(), 2, "a stop answers for every id it matched");

    let after = handle.list().await;
    assert_eq!(
        after.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0],
        "the instance stays registered; only the abandoned replacement goes"
    );
    assert_eq!(after[0].status, ProcStatus::Stopped);
    assert_eq!(runner.kill_counts().len(), 2, "nothing was spawned again");
}

// Killing the replacement anyway empties the slot outright, with no entry
// and no restart. It stays `Starting`, never having been signalled; the
// abandonment on the bus is what says the reload gave up.
#[tokio::test(start_paused = true)]
async fn a_replacement_is_kept_but_not_online_when_the_deadline_elapses_with_no_drainee() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals the replacement
    // The original ends 1000ms in, before the replacement's 3000ms deadline.
    let (handle, runner, mut rx) = started(
        &dir,
        app,
        vec![
            ProcScript::stable_then_exit(1_000, 1),
            ProcScript::never_exits(),
        ],
    )
    .await;
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    expect_event(&mut rx, 0, ProcessEventKind::Online).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;
    expect_event(&mut rx, 1, ProcessEventKind::ReloadAbandoned).await;

    let after = handle.list().await;
    assert_eq!(after.len(), 1, "the app still has its instance");
    assert_eq!(after[0].id, 1);
    assert_eq!(
        after[0].status,
        ProcStatus::Starting,
        "a replacement that never answered is kept, and never called online"
    );
    assert_eq!(runner.kill_counts(), vec![0, 0], "neither was SIGKILLed");
}

// The restore is for a drainee that goes back to serving. One an operator's
// `stop` already claimed hands `shep flock` a live pid for a process on its
// way out, and re-opens `handle_extra_restart`'s `Online` guard for the
// length of the ladder.
#[tokio::test(start_paused = true)]
async fn an_abandoned_reload_never_reports_a_dying_drainee_as_online() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    // Nobody signals the replacement, so the swap is still `AwaitReady`
    // when the stop lands.
    app.wait_ready = true;
    let (handle, _runner, mut rx) = started(
        &dir,
        app,
        vec![ProcScript::ignores_signals(), ProcScript::never_exits()],
    )
    .await;
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    expect_event(&mut rx, 0, ProcessEventKind::Online).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    // Not awaited: the drainee defies its signal, so the stop's own reply
    // is a whole `kill_timeout` away.
    let stopper = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.stop(ProcessSelector::Name("web".to_string())).await })
    };
    expect_event(&mut rx, 1, ProcessEventKind::Delete).await;

    let mid = handle.list().await;
    assert_eq!(mid.len(), 1, "only the abandoned replacement has gone");
    assert_eq!(mid[0].id, 0);
    assert_eq!(
        mid[0].status,
        ProcStatus::Stopping,
        "a drainee an operator already claimed is not back to serving"
    );

    let stopped = stopper.await.unwrap().expect("the stop is answered");
    assert_eq!(stopped.len(), 2, "a stop answers for every id it matched");
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopped);
}

// A cron occurrence and a watched file reach `begin_manual`, which reads no
// status, so the `Stopping` transition does nothing for them.
#[tokio::test(start_paused = true)]
async fn an_automatic_restart_never_lands_on_either_half_of_a_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    // Signalled by hand, so the restart lands inside `AwaitReady`.
    app.wait_ready = true;
    let (handle, runner, mut rx) = started(
        &dir,
        app,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    expect_event(&mut rx, 0, ProcessEventKind::Online).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    let restarted = handle
        .restart_automatic(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the selector matches both halves of the swap");
    assert!(
        restarted.is_empty(),
        "neither half of a swap is an automatic restart's to take"
    );

    handle.tx.send(Msg::Ready { id: 1 }).await.unwrap();
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;

    let after = handle.list().await;
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, 1, "the replacement, not a restarted drainee");
    assert_eq!(after[0].status, ProcStatus::Online);
    assert_eq!(after[0].restarts, 0, "nothing counted a restart");
    assert_eq!(
        runner.kill_counts().len(),
        2,
        "one original and one replacement, and nothing else"
    );
}

// `reap_drainee` leaves the job at `DrainOld` with the drainee
// deregistered, so the replacement's readiness result is the last event
// that could end the job; clearing its `Replacement` marker cancels that
// too, and nothing is left that can reach `finish_swap`.
#[tokio::test(start_paused = true)]
async fn a_reload_ends_when_its_replacement_goes_with_the_drainee_already_gone() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals the replacement
    // The original ends 1000ms in, inside the replacement's 3000ms
    // readiness window, so the swap commits on its death.
    let (handle, _runner, mut rx) = started(
        &dir,
        app,
        vec![
            ProcScript::stable_then_exit(1_000, 1),
            ProcScript::never_exits(),
        ],
    )
    .await;
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    expect_event(&mut rx, 0, ProcessEventKind::Online).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;

    // An operator's `stop` before the replacement is ready, no crash needed.
    handle
        .stop(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the stop reaches the replacement");

    let seen = events_through(&mut rx, 1, ProcessEventKind::Stop).await;
    assert!(
        at(&seen, 1, ProcessEventKind::ReloadAbandoned) < at(&seen, 1, ProcessEventKind::Stop),
        "the reload gives up before the exit that ended it is reported, so a \
         subscriber reads the two in the order they happened: {seen:?}"
    );

    let again = handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .map(|infos| infos.iter().map(|info| info.id).collect::<Vec<_>>());
    assert_eq!(
        again,
        Ok(vec![1]),
        "the reload is over, so the app is reloadable again"
    );
}

// The count is an operator's view of that instance's history, and
// resetting it on every deploy makes the number useless.
#[tokio::test(start_paused = true)]
async fn a_reload_carries_the_drainees_restart_count_to_its_replacement() {
    let dir = tempfile::tempdir().unwrap();
    // Three: the original, the manual restart's, and the replacement.
    let (handle, _runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(); 3],
    )
    .await;
    handle
        .restart(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();
    assert_eq!(handle.list().await[0].restarts, 1);

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;

    let after = handle.list().await;
    assert_eq!(after[0].id, 1);
    assert_eq!(after[0].restarts, 1);
}

/// `spawn_replacement` reads the drainee's `last_exit` before the drainee
/// exits again, so the replacement carries the manual restart's kill.
#[tokio::test(start_paused = true)]
async fn a_reload_carries_the_drainees_last_exit_to_its_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(); 3],
    )
    .await;
    handle
        .restart(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();
    let restarted = handle.list().await;
    let last_exit = restarted[0].last_exit;
    assert!(
        last_exit.is_some(),
        "a manual restart is itself an exit, so this must not be None: {restarted:?}"
    );

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;

    let after = handle.list().await;
    assert_eq!(after[0].id, 1);
    assert_eq!(
        after[0].last_exit, last_exit,
        "a reload is not an exit -- the replacement must inherit the drainee's \
         last_exit rather than reset it to None"
    );
}
