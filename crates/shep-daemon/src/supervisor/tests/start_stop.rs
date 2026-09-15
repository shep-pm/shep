//! Tests for the ordinary lifecycle: start, list, crash, stop, delete.
//!
//! This is the path every other test builds on. It covers what a listing
//! reports, how the restart budget is spent and reset, and what an operator's
//! stop does that a crash does not.

use super::*;

#[tokio::test(start_paused = true)]
async fn start_lists_online_instances() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    let infos = handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert_eq!(infos.len(), 2);
    let list = handle.list().await;
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|i| i.status == ProcStatus::Online));
    assert_eq!(list.iter().map(|i| i.id).collect::<Vec<_>>(), vec![0, 1]);
}

#[tokio::test(start_paused = true)]
async fn listed_log_paths_are_the_derived_defaults() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let logs = paths.logs.clone();
    let handle = spawn_supervisor(runner, paths, events);
    handle
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let list = handle.list().await;
    assert_eq!(
        list[0].out_file.as_deref(),
        logs.join("web-0-out.log").to_str()
    );
    assert_eq!(
        list[0].err_file.as_deref(),
        logs.join("web-0-err.log").to_str()
    );
}

#[tokio::test(start_paused = true)]
async fn listed_log_paths_honour_an_explicit_out_file() {
    // `err_file` is left unset: the two resolve independently.
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let logs = paths.logs.clone();
    let handle = spawn_supervisor(runner, paths, events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.out_file = Some("/var/log/myapp.log".to_string());
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    let list = handle.list().await;
    assert_eq!(list[0].out_file.as_deref(), Some("/var/log/myapp.log"));
    assert_eq!(
        list[0].err_file.as_deref(),
        logs.join("web-0-err.log").to_str(),
        "err_file was not configured, so it must still be the default"
    );
}

#[tokio::test(start_paused = true)]
async fn crash_loop_erroreds_after_budget_with_pinned_delays() {
    let (events, mut rx) = crate::bus::test_bus(1024);
    // 16 spawns: initial + 15 restarts; every exit instant (unstable).
    let runner = ScriptedRunner::new((0..16).map(|_| ProcScript::const_exit(1)).collect());
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("crash", "./boom");
    app.exp_backoff_restart_delay = Some("100".parse().unwrap());
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    // The budget check (16 unstable exits errors) fires on the 16th exit,
    // the script's last entry.
    await_event(&mut rx, 0, ProcessEventKind::Errored).await;
    let list = handle.list().await;
    assert_eq!(list[0].status, ProcStatus::Errored);
    assert_eq!(list[0].restarts, 15); // respawns performed, not exits
    // `last_exit` is what tells a boot loop from a spawn failure.
    assert_eq!(
        list[0].last_exit,
        Some(ExitInfo {
            code: Some(1),
            signal: None,
        })
    );
}

#[tokio::test(start_paused = true)]
async fn crash_loop_budget_check_fires_before_script_exhaustion_at_real_default() {
    // Twenty scripts, more than either check needs, so only the budget
    // path can produce restarts==15: an `unstable_count > max_restarts`
    // check would consume a 17th spawn and report 16.
    let (events, mut rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new((0..20).map(|_| ProcScript::const_exit(1)).collect());
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("crash-default", "./boom"); // no max_restarts override
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    await_event(&mut rx, 0, ProcessEventKind::Errored).await;
    let list = handle.list().await;
    assert_eq!(list[0].status, ProcStatus::Errored);
    // Budget exhaustion, not script exhaustion: 4 scripted spawns unused.
    assert_eq!(list[0].restarts, 15);
}

#[tokio::test(start_paused = true)]
async fn stable_run_resets_budget() {
    let (events, mut rx) = crate::bus::test_bus(256);
    let mut script = vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
    ];
    script.push(ProcScript::stable_then_exit(2000, 1)); // > min_uptime 1000ms => stable
    script.extend((0..16).map(|_| ProcScript::const_exit(1)));
    let runner = ScriptedRunner::new(script);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("flappy", "./f");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    await_event(&mut rx, 0, ProcessEventKind::Errored).await;
    let list = handle.list().await;
    assert_eq!(list[0].status, ProcStatus::Errored);
    // 3 + 1 + 15 respawns after the initial spawn = 19
    assert_eq!(list[0].restarts, 19);
}

#[tokio::test(start_paused = true)]
async fn manual_stop_prevents_restart() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript {
        delay_ms: u64::MAX,
        outcome: ExitOutcome {
            code: None,
            signal: None,
        },
        obeys_signal: true,
        obeys_kill: true,
        lamb_holds_the_pipe: false,
        reads_stdin: true,
    }]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    let stopped = handle
        .stop(ProcessSelector::Name("svc".to_string()))
        .await
        .unwrap();
    assert_eq!(stopped[0].status, ProcStatus::Stopped); // deferred reply: already terminal
    // No restart is scheduled: a full minute yields no further events.
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopped);
}

#[tokio::test(start_paused = true)]
async fn stop_exit_codes_mean_clean_stop() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(0)]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("oneshot", "./job");
    app.stop_exit_codes = vec![0];
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    await_event(&mut rx, 0, ProcessEventKind::Stop).await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopped);
}

#[tokio::test(start_paused = true)]
async fn spawn_failure_surfaces_and_erroreds() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![]); // exhausted immediately
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("ghost", "./missing");
    let err = handle
        .start(vec![normalize(app).unwrap()])
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::SpawnFailed(_)));
    assert_eq!(handle.list().await[0].status, ProcStatus::Errored);
}

#[tokio::test(start_paused = true)]
async fn an_unresolvable_user_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        test_paths(&dir),
        events,
    );
    let mut app = AppConfig::minimal("svc", "./svc");
    app.user = Some("definitely-not-a-real-shep-user".to_string());
    let err = handle
        .start(vec![normalize(app).unwrap()])
        .await
        .unwrap_err();
    // `CannotStart`, not `SpawnFailed`: passwd resolution runs before
    // anything is registered, so nothing was spawned.
    assert!(matches!(err, SupervisorError::CannotStart(_)), "{err:?}");
    assert!(
        handle.list().await.is_empty(),
        "a refusal before the registering pass must leave nothing registered"
    );
}

/// An app failing two checks must push one refusal, or a two-app batch
/// reports "4 of 2 apps cannot start".
#[tokio::test(start_paused = true)]
async fn a_refusal_is_counted_once_per_app_not_once_per_failed_check() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        // `refusing`, or the fake answers `Preflight::Unknown` and only
        // the credentials check can refuse.
        ScriptedRunner::new(vec![ProcScript::never_exits()]).refusing(&["one", "two"]),
        test_paths(&dir),
        events,
    );
    // Two apps, each failing both checks: user, and script preflight.
    let both_bad = |name: &str| {
        let mut app = AppConfig::minimal(name, "./definitely-not-here");
        app.cwd = Some(dir.path().display().to_string());
        app.user = Some("definitely-not-a-real-shep-user".to_string());
        normalize(app).unwrap()
    };
    let err = handle
        .start(vec![both_bad("one"), both_bad("two")])
        .await
        .unwrap_err();
    let SupervisorError::CannotStart(msg) = &err else {
        panic!("expected CannotStart, got {err:?}");
    };
    assert!(
        msg.contains("2 of 2 apps cannot start"),
        "one refusal per app, never one per failed check: {msg}"
    );
}

#[tokio::test(start_paused = true)]
async fn delete_and_selectors_route() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut a = AppConfig::minimal("api", "./a");
    a.fold = Some("backend".to_string());
    let b = AppConfig::minimal("web", "./w");
    handle
        .start(vec![normalize(a).unwrap(), normalize(b).unwrap()])
        .await
        .unwrap();
    let hits = handle
        .stop(ProcessSelector::Fold("backend".to_string()))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "api");
    let deleted = handle
        .delete(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();
    assert_eq!(deleted, vec![1]);
    assert_eq!(handle.list().await.len(), 1);
}

/// Waits until the flock's single process reads `restarts` restarts and
/// `Online`.
///
/// Bounded: its callers set `exp_backoff_restart_delay = None`, and a delay
/// reintroduced for that shape would spin here forever.
async fn wait_for_restarts_online(handle: &SupervisorHandle, restarts: u32) -> ProcessInfo {
    let mut info = handle.list().await.remove(0);
    for _ in 0..200 {
        if info.restarts == restarts && info.status == ProcStatus::Online {
            break;
        }
        tokio::task::yield_now().await;
        info = handle.list().await.remove(0);
    }
    info
}

#[tokio::test(start_paused = true)]
async fn manual_restart_resets_budget_and_respawns() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Five procs: two unstable crashes, the long-lived proc they land on,
    // the unstable respawn the manual restart performs, and the proc a
    // still-solvent budget restarts onto.
    let runner = ScriptedRunner::new(vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    // Three rather than sixteen: two crashes leave the budget one short.
    app.max_restarts = 3;
    // The sync loop below busy-polls under a paused clock, so a non-zero
    // `exp_backoff_restart_delay` would spin it forever.
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    // Sync on state: immediate restarts mean restarts==2 once `never_exits`
    // is up.
    let info = wait_for_restarts_online(&handle, 2).await;
    assert_eq!(
        (info.status, info.restarts),
        (ProcStatus::Online, 2),
        "never reached the never_exits proc -- got {info:?}"
    );
    let restarted = handle
        .restart(ProcessSelector::Name("svc".to_string()))
        .await
        .unwrap();
    // The deferred reply is snapshotted at the respawn.
    assert_eq!(restarted[0].status, ProcStatus::Online);

    // The proc restart landed on is itself unstable: with the budget reset
    // its exit is the first of three, without it the third. Bounded, since
    // the failing outcome is a settled `Errored`.
    let mut settled = handle.list().await.remove(0);
    for _ in 0..200 {
        if settled.status == ProcStatus::Errored
            || (settled.status == ProcStatus::Online && settled.restarts == 4)
        {
            break;
        }
        tokio::task::yield_now().await;
        settled = handle.list().await.remove(0);
    }
    assert_eq!(
        (settled.status, settled.restarts),
        (ProcStatus::Online, 4),
        "an operator's restart left the two spent unstable exits on the \
         books -- got {settled:?}"
    );
}

// The budget reset belongs to `ManualKind::Restart` and to nothing else: a
// restart the daemon raised itself resets it as an operator's does.
// `CommandOrigin` governs only which of two racing commands owns a sheep's
// next exit (`claim_manual`).
#[tokio::test(start_paused = true)]
async fn an_automatic_restart_resets_the_budget_like_an_operators_does() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Five procs: two unstable crashes, the long-lived proc they land on,
    // the unstable respawn the automatic restart performs, and the proc a
    // still-solvent budget restarts onto.
    let runner = ScriptedRunner::new(vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    // Three rather than sixteen: two crashes leave the budget one short.
    app.max_restarts = 3;
    // The sync loop below busy-polls under a paused clock, so a non-zero
    // `exp_backoff_restart_delay` would spin it forever.
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    // Sync on state: immediate restarts mean restarts==2 once `never_exits`
    // is up.
    let info = wait_for_restarts_online(&handle, 2).await;
    assert_eq!(
        (info.status, info.restarts),
        (ProcStatus::Online, 2),
        "never reached the never_exits proc -- got {info:?}"
    );

    handle
        .restart_automatic(ProcessSelector::Name("svc".to_string()))
        .await
        .unwrap();

    // The proc restart landed on is itself unstable: with the budget reset
    // its exit is the first of three, without it the third. Bounded, since
    // the failing outcome is a settled `Errored`.
    let mut settled = handle.list().await.remove(0);
    for _ in 0..200 {
        if settled.status == ProcStatus::Errored
            || (settled.status == ProcStatus::Online && settled.restarts == 4)
        {
            break;
        }
        tokio::task::yield_now().await;
        settled = handle.list().await.remove(0);
    }
    assert_eq!(
        (settled.status, settled.restarts),
        (ProcStatus::Online, 4),
        "an automatic restart left the two spent unstable exits on the \
         books -- got {settled:?}"
    );
}

// A `restart` aimed at a sheep with no live task has no exit to ride, so it
// never reaches `handle_exited`: `apply_immediate` resets and respawns
// inline, and the reply is that respawn. `Stopped` is the settled
// not-running state; `WaitingRestart` still holds a `RestartDue` timer.
#[tokio::test(start_paused = true)]
async fn restarting_a_stopped_sheep_resets_the_budget() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Five procs: two unstable crashes, the long-lived proc they land on
    // and the stop below ends, the unstable respawn the restart performs,
    // and the proc a still-solvent budget restarts onto.
    let runner = ScriptedRunner::new(vec![
        ProcScript::const_exit(1),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
        ProcScript::const_exit(1),
        ProcScript::never_exits(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    // Three rather than sixteen: two crashes leave the budget one short.
    app.max_restarts = 3;
    // The sync loop below busy-polls under a paused clock, so a non-zero
    // `exp_backoff_restart_delay` would spin it forever.
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    // Sync on state: immediate restarts mean restarts==2 once `never_exits`
    // is up.
    let info = wait_for_restarts_online(&handle, 2).await;
    assert_eq!(
        (info.status, info.restarts),
        (ProcStatus::Online, 2),
        "never reached the never_exits proc -- got {info:?}"
    );

    // `stop` takes the sheep off its live task without touching the budget:
    // `decide_on_exit` short-circuits to `CleanStop` on `manual_stop`,
    // before it classifies the exit. The two spent exits stay on the books.
    let stopped = handle
        .stop(ProcessSelector::Name("svc".to_string()))
        .await
        .unwrap();
    assert_eq!(stopped[0].status, ProcStatus::Stopped);

    let restarted = handle
        .restart(ProcessSelector::Name("svc".to_string()))
        .await
        .unwrap();
    // `apply_immediate`'s reply is the respawn, sent in the same actor turn.
    assert_eq!(
        (restarted[0].status, restarted[0].restarts),
        (ProcStatus::Online, 3)
    );

    // The proc restart landed on is itself unstable: with the budget reset
    // its exit is the first of three, without it the third. Bounded, since
    // the failing outcome is a settled `Errored`.
    let mut settled = handle.list().await.remove(0);
    for _ in 0..200 {
        if settled.status == ProcStatus::Errored
            || (settled.status == ProcStatus::Online && settled.restarts == 4)
        {
            break;
        }
        tokio::task::yield_now().await;
        settled = handle.list().await.remove(0);
    }
    assert_eq!(
        (settled.status, settled.restarts),
        (ProcStatus::Online, 4),
        "restarting a stopped sheep left the two spent unstable exits on \
         the books -- got {settled:?}"
    );
}
