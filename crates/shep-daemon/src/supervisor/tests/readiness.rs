//! Tests for a gated sheep: one that must prove itself before it counts as online.
//!
//! A gated app stays `starting` until its channel signals, its probe passes, or
//! its deadline elapses. The awkward cases are the ones where it is stopped,
//! restarted or exits while that wait is still outstanding.

use super::*;

#[tokio::test(start_paused = true)]
async fn wait_ready_app_stays_starting_until_the_channel_signals() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);
    assert_no_event_within(
        &mut rx,
        0,
        ProcessEventKind::Online,
        Duration::from_millis(500),
    )
    .await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    // Reaches the actor where the sheep task's forwarded `Ready` would.
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();

    tokio::time::timeout(
        Duration::from_secs(1),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("Online once the channel signals");
    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
}

// Real time, no `start_paused`: a paused test waiting on real socket I/O
// can deadlock, since the virtual clock does not move the OS.
#[tokio::test]
async fn readiness_probe_app_stays_starting_until_the_probe_passes() {
    // Reserve a free port and release it: probes fail with connection
    // refused until the listener below binds it for real.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserved.local_addr().unwrap();
    drop(reserved);

    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.readiness_probe = Some(ProbeConfig {
        interval: UpDuration::from_millis(50),
        timeout: UpDuration::from_millis(200),
        ..probe_config(ProbeKind::Tcp, &addr.to_string())
    });
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);
    // Nothing is listening on `addr` yet: the probe fails every interval.
    assert_no_event_within(
        &mut rx,
        0,
        ProcessEventKind::Online,
        Duration::from_millis(220),
    )
    .await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let _accept = tokio::spawn(async move { while listener.accept().await.is_ok() {} });

    tokio::time::timeout(
        Duration::from_secs(2),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("Online once the probe starts passing");
    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
}

// The probe reads `$SHEP_INSTANCE`, which only `assemble` writes: a prober
// built from `config.env` expands it to nothing under `probe_exec`'s
// `env_clear()`. Real time, since this spawns a real `sh` per probe.
#[cfg(unix)]
#[tokio::test]
async fn an_exec_readiness_probe_sees_the_assembled_env_not_the_apps_own() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    // Instance 0 is the only slot a single-instance app gets.
    let ready_file = dir.path().join("ready-0");
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.readiness_probe = Some(ProbeConfig {
        interval: UpDuration::from_millis(50),
        timeout: UpDuration::from_millis(500),
        ..probe_config(
            ProbeKind::Exec,
            &format!(r#"test -f "{}/ready-$SHEP_INSTANCE""#, dir.path().display()),
        )
    });
    // Far longer than this test's own patience, so an Online can only have
    // come from a passing probe, never from the deadline path.
    app.listen_timeout = UpDuration::from_millis(60_000);
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);
    // Several probe intervals of real time with the file absent.
    assert_no_event_within(
        &mut rx,
        0,
        ProcessEventKind::Online,
        Duration::from_millis(220),
    )
    .await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    std::fs::write(&ready_file, b"").unwrap();

    tokio::time::timeout(
        Duration::from_secs(5),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("Online once the exec probe can resolve $SHEP_INSTANCE");
    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
}

#[tokio::test(start_paused = true)]
async fn a_gated_apps_online_carries_the_same_manually_flag_an_ungated_one_does() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![
        ProcScript::never_exits(), // id 0: gated
        ProcScript::never_exits(), // id 1: ungated
        ProcScript::never_exits(), // id 0 again, after the manual restart
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut gated = AppConfig::minimal("gated", "./srv");
    gated.wait_ready = true;
    let ungated = AppConfig::minimal("ungated", "./srv");
    handle
        .start(vec![normalize(gated).unwrap(), normalize(ungated).unwrap()])
        .await
        .unwrap();

    // The ungated app is the control for what a plain `Start` reports.
    let ungated_manually = tokio::time::timeout(
        Duration::from_secs(1),
        await_event(&mut rx, 1, ProcessEventKind::Online),
    )
    .await
    .expect("an ungated app is Online at spawn");
    assert!(ungated_manually, "sanity: a Start is a manual event");

    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    let gated_manually = tokio::time::timeout(
        Duration::from_secs(1),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("Online once the channel signals");
    assert_eq!(
        gated_manually, ungated_manually,
        "the same `shep start` must report the same flag, gated or not"
    );

    // The sheep is `Starting` with a live task, so `restart` takes the
    // deferred route and resolves at the respawn, still `Starting`.
    let restarted = handle.restart(ProcessSelector::Id(0)).await.unwrap();
    assert_eq!(restarted[0].status, ProcStatus::Starting);
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    let restarted_manually = tokio::time::timeout(
        Duration::from_secs(1),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("Online once the respawned sheep signals");
    assert!(
        restarted_manually,
        "a manual Restart's Online must stay manual through the readiness gate"
    );
}

#[tokio::test(start_paused = true)]
async fn an_operators_restart_is_reported_as_a_user_action() {
    let (events, mut rx) = crate::bus::test_bus(64);
    // Two: the sheep, and the respawn the restart performs.
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 2]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let restarted = handle.restart(ProcessSelector::All).await.unwrap();
    assert_eq!(restarted[0].restarts, 1, "the restart really respawned");

    let manually = tokio::time::timeout(
        Duration::from_secs(1),
        await_event(&mut rx, 0, ProcessEventKind::Restart),
    )
    .await
    .expect("the respawn `restart` performed");
    assert!(
        manually,
        "a person typed `shep restart`; the bus must say a user action caused it"
    );
}

#[tokio::test(start_paused = true)]
async fn a_gated_app_whose_deadline_elapses_goes_online_anyway() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals ready
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    tokio::time::timeout(
        Duration::from_secs(4), // > the 3000ms default listen_timeout
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("Online once the readiness deadline elapses");
    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
}

// The epoch guard: both processes are `Starting`, so status alone cannot
// tell them apart.
#[tokio::test(start_paused = true)]
async fn a_gated_app_that_exits_while_starting_never_reaches_online_from_the_old_wait() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![
        ProcScript::stable_then_exit(500, 1), // unstable exit while Starting
        ProcScript::never_exits(),            // the automatic respawn
    ]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals either instance's readiness
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    // The 500ms exit is unstable (< the 1000ms `min_uptime` default) and
    // respawns after the 100ms default backoff: `Starting` again, epoch up.
    tokio::time::timeout(
        Duration::from_secs(1),
        await_event(&mut rx, 0, ProcessEventKind::Restart),
    )
    .await
    .expect("the automatic respawn after the unstable exit");
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);
    assert_eq!(handle.list().await[0].restarts, 1);

    // The old wait's deadline (~3000ms from the first spawn) elapses next,
    // with both processes reading `Starting`.
    assert_no_event_within(
        &mut rx,
        0,
        ProcessEventKind::Online,
        Duration::from_millis(2_700),
    )
    .await;
    assert_eq!(
        handle.list().await[0].status,
        ProcStatus::Starting,
        "the OLD wait's stale TimedOut must not have marked the respawned process online"
    );

    // The respawned process's own deadline elapses next.
    tokio::time::timeout(
        Duration::from_secs(2),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("the new process's own readiness deadline");
    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
}

// No respawn, so the epoch never changes: only the status guard stands
// between the stale `TimedOut` and an incorrect `Online`.
#[tokio::test(start_paused = true)]
async fn a_gated_app_stopped_while_starting_ignores_the_old_wait() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::stable_then_exit(500, 1)]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals ready
    app.autorestart = false; // straight to Stopped: epoch never bumps
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    tokio::time::timeout(
        Duration::from_secs(1),
        await_event(&mut rx, 0, ProcessEventKind::Stop),
    )
    .await
    .expect("the natural exit at 500ms");
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopped);

    // The old wait resolves `TimedOut` at the epoch this slot still carries.
    assert_no_event_within(&mut rx, 0, ProcessEventKind::Online, Duration::from_secs(3)).await;
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopped);
}

#[tokio::test(start_paused = true)]
async fn a_gated_app_restarted_while_starting_ignores_the_old_wait() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner =
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals either instance's readiness
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    // A gap before the restart, so the old wait's deadline and the new one
    // land far enough apart to tell apart below.
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle
        .restart(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();
    assert_eq!(
        handle.list().await[0].status,
        ProcStatus::Starting,
        "the respawned process is gated too, so it must still be Starting"
    );

    // The old wait's deadline elapses first; the epoch guard must drop it.
    assert_no_event_within(
        &mut rx,
        0,
        ProcessEventKind::Online,
        Duration::from_millis(2_700),
    )
    .await;
    assert_eq!(
        handle.list().await[0].status,
        ProcStatus::Starting,
        "the old wait's stale TimedOut must not have marked the new process online"
    );

    // The respawned process's own deadline elapses next.
    tokio::time::timeout(
        Duration::from_secs(2),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("the new process's own readiness deadline");
    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
}
