//! Tests for shutdown, and for commands racing each other.
//!
//! Shutdown has to end every sheep and then the engine, whatever else is in
//! flight: a pending restart timer, a readiness wait, a kill ladder part way
//! up. The races here are the ones where two commands could each claim an exit.

use super::*;

#[tokio::test(start_paused = true)]
async fn shutdown_kills_all_and_stops_the_engine() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    handle.shutdown().await; // kill ladder on every online sheep, then stop
    assert!(handle.list_checked().await.is_err());
}

async fn drain_kinds(
    rx: &mut tokio::sync::broadcast::Receiver<SharedEvent>,
) -> Vec<(u32, ProcessEventKind)> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv().map(|event| event.to_event()) {
        if let BusEvent::Process { event, info, .. } = ev {
            out.push((info.id, event));
        }
    }
    out
}

// A pending `RestartDue` timer must not respawn once shutdown has begun:
// that child is outside the shutdown's `online` snapshot, so never killed.
#[tokio::test(start_paused = true)]
async fn shutdown_ignores_a_pending_restart_timer() {
    let (events, mut rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![
        ProcScript::const_exit(1),     // crash: instant exit -> waiting-restart
        ProcScript::ignores_signals(), // web: full 1600ms kill ladder
        ProcScript::never_exits(),     // catches a pending timer respawning during shutdown
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut crash = AppConfig::minimal("crash", "./boom");
    crash.exp_backoff_restart_delay = Some("500".parse().unwrap());
    let web = AppConfig::minimal("web", "./srv");
    handle
        .start(vec![normalize(crash).unwrap(), normalize(web).unwrap()])
        .await
        .unwrap();
    // id 0 is now waiting-restart with a 500ms timer pending.
    await_event(&mut rx, 0, ProcessEventKind::Exit).await;

    handle.shutdown().await; // web's ladder burns 1600ms of virtual time

    let seen = drain_kinds(&mut rx).await;
    let ghost = seen
        .iter()
        .any(|(id, k)| *id == 0 && *k == ProcessEventKind::Restart);
    assert!(
        !ghost,
        "GHOST RESPAWN during shutdown: events after shutdown = {seen:?}"
    );
}

// A readiness wait that resolves after a shutdown has begun must not mark
// its sheep online. The sibling guards do not reach this: the shutdown
// leaves the slot at the same epoch and the same `Starting` status the wait
// was armed under, and its 1000ms deadline lands inside the kill ladder.
#[tokio::test(start_paused = true)]
async fn shutdown_ignores_a_pending_readiness_wait() {
    let (events, mut rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![ProcScript::ignores_signals()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("gated", "./g");
    app.wait_ready = true; // nobody ever signals ready
    app.listen_timeout = UpDuration::from_millis(1_000);
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert_eq!(
        handle.list().await[0].status,
        ProcStatus::Starting,
        "the readiness wait has to be armed for its result to be droppable"
    );

    handle.shutdown().await; // the ladder burns 1600ms of virtual time

    let seen = drain_kinds(&mut rx).await;
    assert!(
        seen.contains(&(0, ProcessEventKind::Stop)),
        "the kill ladder must have outlasted the readiness deadline: events = {seen:?}"
    );
    assert!(
        !seen.contains(&(0, ProcessEventKind::Online)),
        "a readiness wait resolved during shutdown marked a dying sheep online: \
         events = {seen:?}"
    );
}

// A Start racing a concurrent Shutdown must never leave an un-killed
// child: either Shutdown is processed first and the Start is rejected, or
// the Start lands first and Shutdown's `online` snapshot catches it.
#[tokio::test(start_paused = true)]
async fn late_start_racing_shutdown_never_orphans() {
    let (events, mut rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![
        ProcScript::ignores_signals(), // web: 1600ms ladder
        ProcScript::never_exits(),     // the late Start, if it lands before Shutdown
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let web = AppConfig::minimal("web", "./srv");
    handle.start(vec![normalize(web).unwrap()]).await.unwrap();

    let h2 = handle.clone();
    let late = tokio::spawn(async move {
        let app = AppConfig::minimal("late", "./l");
        h2.start(vec![normalize(app).unwrap()]).await
    });
    handle.shutdown().await;
    let outcome = late.await.unwrap();

    let seen = drain_kinds(&mut rx).await;
    match outcome {
        Err(SupervisorError::EngineStopped) => {} // rejected: no orphan possible
        Ok(infos) => {
            let late_id = infos[0].id;
            assert!(
                seen.iter().any(|(id, k)| *id == late_id
                    && matches!(k, ProcessEventKind::Stop | ProcessEventKind::Exit)),
                "late Start raced ahead of shutdown but was never killed: events = {seen:?}"
            );
        }
        Err(other) => panic!("unexpected error from a late Start during shutdown: {other:?}"),
    }
}

// A manual restart during a backoff wait leaves the original `RestartDue`
// timer scheduled; it must not fire later and short-circuit the new backoff
// the manual respawn's own exit schedules.
#[tokio::test(start_paused = true)]
async fn stale_restart_timer_never_short_circuits_a_newer_backoff() {
    let (events, mut rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![
        ProcScript::const_exit(1),             // t=0 exit -> T1 @ 2000
        ProcScript::stable_then_exit(1500, 1), // manual respawn, dies @1500 -> T2 @ 3500
        ProcScript::never_exits(),             // whoever respawns first takes this
        ProcScript::never_exits(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("crash", "./boom");
    app.exp_backoff_restart_delay = Some("2000".parse().unwrap());
    app.min_uptime = "5000".parse().unwrap(); // 1500ms uptime counts as unstable
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    await_event(&mut rx, 0, ProcessEventKind::Exit).await; // waiting-restart, T1 @ 2000

    let out = handle.restart(ProcessSelector::All).await.unwrap();
    assert_eq!(
        out[0].status,
        ProcStatus::Online,
        "manual restart respawned"
    );

    // t=1500 the respawn dies, giving a new timer @ 3500, so look at the
    // world at t=2500.
    tokio::time::advance(Duration::from_millis(2500)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let info = handle.list().await.remove(0);
    assert_eq!(
        (info.status, info.restarts),
        (ProcStatus::WaitingRestart, 1),
        "at t=2500 the sheep should still be waiting on its 3500ms backoff; \
         got {info:?} -- the stale 2000ms timer fired early"
    );
}

// Stop and Restart racing on the same running sheep: the first to reach it
// owns the `manual` marker and its one live Kill, and both callers get the
// same terminal snapshot back. Both have a caller awaiting an answer, so
// neither may displace the other.
#[tokio::test(start_paused = true)]
async fn overlapping_stop_and_restart_agree_on_one_outcome() {
    let (events, _rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![
        ProcScript::ignores_signals(), // 1600ms ladder: a wide race window
        ProcScript::never_exits(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    let h2 = handle.clone();
    let stopper = tokio::spawn(async move { h2.stop(ProcessSelector::All).await });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let restarted = handle.restart(ProcessSelector::All).await.unwrap();
    let stopped = stopper.await.unwrap().unwrap();

    assert_eq!(
        stopped[0].status,
        ProcStatus::Stopped,
        "stop() reported a non-stopped sheep"
    );
    assert_eq!(
        restarted[0].status,
        ProcStatus::Stopped,
        "restart() lost the race to the earlier stop() but got a different \
         answer than the stop() caller -- the two callers disagree about \
         what happened to the same sheep"
    );
}

// A flood of Stop commands against one sheep mid-kill must not delay
// processing an unrelated sheep's exit.
#[tokio::test(start_paused = true)]
async fn actor_never_blocks_behind_a_busy_kill_ladder() {
    let (events, mut rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![
        ProcScript::ignores_signals(),        // sheep a: 1600ms ladder
        ProcScript::stable_then_exit(800, 0), // sheep b: exits at t=800
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let a = AppConfig::minimal("a", "./a");
    let mut b = AppConfig::minimal("b", "./b");
    b.autorestart = false;
    handle
        .start(vec![normalize(a).unwrap(), normalize(b).unwrap()])
        .await
        .unwrap();

    let t0 = tokio::time::Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.stop(ProcessSelector::Name("a".to_string())).await
        }));
    }
    // Sheep b exits on its own at t=800 and is nobody's kill target.
    await_event(&mut rx, 1, ProcessEventKind::Stop).await;
    let seen_at = t0.elapsed();
    for t in tasks {
        let _ = t.await;
    }
    assert!(
        seen_at < Duration::from_millis(1000),
        "sheep b's own exit was only processed at {seen_at:?} -- the actor \
         was parked inside ctl.send() for sheep a's kill ladder"
    );
}

// The deadlock shape: the actor parked in `ctl.send()` with `ctl` full
// while the sheep task parks in `actor_tx.send()` with the mailbox full.
#[tokio::test(start_paused = true)]
async fn mailbox_flood_during_a_kill_never_deadlocks() {
    let (events, mut rx) = crate::bus::test_bus(4096);
    let runner = ScriptedRunner::new(vec![ProcScript::ignores_signals()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let a = AppConfig::minimal("a", "./a");
    handle.start(vec![normalize(a).unwrap()]).await.unwrap();

    for _ in 0..8 {
        let h = handle.clone();
        tokio::spawn(async move {
            let _ = h.stop(ProcessSelector::All).await;
        });
    }
    for _ in 0..40 {
        tokio::task::yield_now().await;
    }
    // Stuff the 256-slot mailbox while the actor is inside a kill ladder.
    for _ in 0..400 {
        let h = handle.clone();
        tokio::spawn(async move {
            let _ = h.list_checked().await;
        });
    }
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }

    let r = tokio::time::timeout(
        Duration::from_secs(600),
        await_event(&mut rx, 0, ProcessEventKind::Stop),
    )
    .await;
    assert!(
        r.is_ok(),
        "DEADLOCK: actor parked in ctl.send() while the sheep task is \
         parked in actor_tx.send() -- the daemon never recovers"
    );
}

// A `Delete` landing on an id `begin_shutdown` already claimed records its
// intent in `pending_delete` rather than in `manual`: `handle_exited`
// deregisters on either, so the caller is never told of a deletion that did
// not happen. The decoy outlives the target, keeping the actor alive.
#[tokio::test(start_paused = true)]
async fn delete_racing_shutdown_still_deregisters_the_sheep() {
    let (events, _rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![
        ProcScript::ignores_signals(), // target: default 1600ms kill_timeout ladder
        ProcScript::ignores_signals(), // decoy: kept alive far longer
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let target = AppConfig::minimal("svc", "./svc");
    let mut decoy = AppConfig::minimal("decoy", "./decoy");
    decoy.kill_timeout = "600000".parse().unwrap(); // outlives the target's ladder by far
    let started = handle
        .start(vec![normalize(target).unwrap(), normalize(decoy).unwrap()])
        .await
        .unwrap();
    let id = started
        .iter()
        .find(|info| info.name == "svc")
        .expect("target sheep registered")
        .id;

    let h2 = handle.clone();
    let shutter = tokio::spawn(async move { h2.shutdown().await });
    for _ in 0..10 {
        tokio::task::yield_now().await; // let Shutdown claim the manual marker first
    }
    let deleted = handle.delete(ProcessSelector::Id(id)).await.unwrap();

    assert_eq!(deleted, vec![id], "the caller was told this id was deleted");
    assert!(
        handle.list().await.iter().all(|info| info.id != id),
        "a Delete that raced a Shutdown must still deregister the sheep, \
         not just tell its caller it did"
    );

    // The decoy's ladder never resolves under the paused clock: drop the
    // in-flight Shutdown rather than waiting on it.
    drop(shutter);
}

// `handle_exited`'s manual-Restart branch is the one path that resolves an
// exit without consulting `decide_on_exit`, so it must still honour
// `pending_delete`: a racing `Delete` finds the marker already claimed and
// sets that field without touching `manual`.
#[tokio::test(start_paused = true)]
async fn delete_racing_restart_still_deregisters_the_sheep() {
    let (events, _rx) = crate::bus::test_bus(1024);
    let runner = ScriptedRunner::new(vec![
        ProcScript::ignores_signals(), // wide kill-ladder window
        // A second script, so a broken run's respawn succeeds into a live
        // process rather than landing in `Errored`.
        ProcScript::never_exits(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    let started = handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    let id = started[0].id;

    let h2 = handle.clone();
    let restarter = tokio::spawn(async move { h2.restart(ProcessSelector::All).await });
    for _ in 0..10 {
        tokio::task::yield_now().await; // let Restart claim the manual marker first
    }
    let deleted = handle.delete(ProcessSelector::Id(id)).await.unwrap();
    let restarted = restarter.await.unwrap().unwrap();

    assert_eq!(deleted, vec![id], "the caller was told this id was deleted");
    // A respawned `Online` here would mean a child spawned behind the
    // Delete's back.
    assert_eq!(restarted[0].id, id);
    assert_eq!(
        restarted[0].status,
        ProcStatus::Stopped,
        "restart() must not report a respawned process once a racing \
         Delete has claimed this id -- got {restarted:?}"
    );
    assert!(
        handle.list().await.iter().all(|info| info.id != id),
        "a Delete that raced a Restart must still deregister the sheep, \
         not respawn a brand-new live process while telling its caller \
         the sheep was deleted"
    );
}

// An automatic restart is mid-kill-ladder when an operator's `stop` lands
// on the same sheep. The operator's intent wins: the sheep ends `Stopped`,
// never respawned. `extra_restart` is the only command with no reply, so it
// can still be in flight when the next command arrives.
#[tokio::test(start_paused = true)]
async fn an_operators_stop_beats_an_automatic_restart_mid_ladder() {
    let (events, _rx) = crate::bus::test_bus(1024);
    // Two: the sheep, plus the respawn a broken implementation performs.
    let runner = ScriptedRunner::new(vec![
        ProcScript::ignores_signals(), // 1600ms ladder: a wide race window
        ProcScript::never_exits(),     // the respawn a broken implementation performs
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    let running = handle.list().await.remove(0);
    let pid = running.pid.expect("an online sheep has a pid");

    // Both sends land in the same mailbox in this order, so the actor sets
    // the restart's marker and starts its ladder before it sees the stop.
    handle.extra_restart(running.id, pid, None, None).await;
    let stopped = handle.stop(ProcessSelector::All).await.unwrap();

    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].id, running.id);
    assert_eq!(
        (stopped[0].status, stopped[0].restarts),
        (ProcStatus::Stopped, 0),
        "an operator's stop was silently converted into the automatic \
         restart it raced -- got {stopped:?}"
    );
    let listed = handle.list().await;
    assert_eq!(
        (listed[0].status, listed[0].pid),
        (ProcStatus::Stopped, None),
        "the sheep an operator stopped is running again -- got {listed:?}"
    );
}

// The `Delete` sibling of the case above. `claim_manual`'s carve-out and
// `pending_delete` each produce this outcome, so it takes disabling both.
#[tokio::test(start_paused = true)]
async fn an_operators_delete_beats_an_automatic_restart_mid_ladder() {
    let (events, _rx) = crate::bus::test_bus(1024);
    // Two: the sheep, plus the respawn a broken run performs.
    let runner = ScriptedRunner::new(vec![
        ProcScript::ignores_signals(),
        ProcScript::never_exits(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    let running = handle.list().await.remove(0);
    let pid = running.pid.expect("an online sheep has a pid");

    handle.extra_restart(running.id, pid, None, None).await;
    let deleted = handle
        .delete(ProcessSelector::Id(running.id))
        .await
        .unwrap();

    assert_eq!(deleted, vec![running.id]);
    assert!(
        handle.list().await.is_empty(),
        "a delete that raced an automatic restart must still deregister \
         the sheep, not leave one behind for the restart to bring back"
    );
}

/// `never_exits` obeys the ladder's first rung, so the wait resolves on
/// `SIGTERM` rather than `kill_tree`'s `SIGKILL`: the number pinned is 15.
#[tokio::test(start_paused = true)]
async fn an_operators_stop_still_shows_its_signal_as_the_last_exit() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    let stopped = handle.stop(ProcessSelector::All).await.unwrap();
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].status, ProcStatus::Stopped);
    assert_eq!(
        stopped[0].last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(15),
        }),
        "an operator's own stop must still show up as a last exit: {stopped:?}"
    );
}

/// Both failures land in `Errored`, so `last_exit` is all that separates
/// "your app crashed with 1" from "shep could not start your app at all".
///
/// One script that exits `1` gives the sequence: a real exit that records a
/// code, then a respawn that fails to spawn.
#[tokio::test(start_paused = true)]
async fn a_respawn_that_fails_to_spawn_clears_the_previous_exit_code() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(1)]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    // Let the exit, the restart decision and the failed respawn all land.
    tokio::time::sleep(Duration::from_secs(30)).await;

    let listing = handle.list().await;
    assert_eq!(listing.len(), 1);
    assert_eq!(
        listing[0].status,
        ProcStatus::Errored,
        "a respawn that cannot spawn is still terminal: {listing:?}"
    );
    assert_eq!(
        listing[0].last_exit, None,
        "nothing exited on the failed respawn, so the earlier code must \
         not still be showing: {listing:?}"
    );
}
