//! Tests for a gated sheep: one that must prove itself before it counts as online.
//!
//! A gated app stays `starting` until its channel signals, its probe passes, or
//! its deadline elapses. The awkward cases are the ones where it is stopped,
//! restarted or exits while that wait is still outstanding.
//!
//! The probe cases wait on the probe's own traffic rather than on a wall-clock
//! window, and let the probe pass only where the test itself says so. Their
//! readiness deadlines sit far past the test's patience, so an `Online` can
//! only have come from a passing probe.

use super::*;
use crate::testing_http::{HttpReply, loopback_http};

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

// Real time, no `start_paused`: this dials a real listener, and a paused
// test waiting on real socket I/O can deadlock, since the virtual clock
// does not move the OS.
#[tokio::test]
async fn readiness_probe_app_stays_starting_until_the_probe_passes() {
    // The fake holds its port for the whole test. A port released between "the
    // probe must fail" and "the probe must pass" is a race: a sibling test
    // could bind that ephemeral port. The fake answers only what this test
    // queues, so no probe can pass before the `queue_reply` that says so.
    let mut gate = loopback_http(Vec::new()).await;

    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.readiness_probe = Some(ProbeConfig {
        // Quick re-probing costs nothing here: the gate cannot hand a probe
        // a passing verdict on its own, whatever the interval.
        interval: UpDuration::from_millis(50),
        // Three orders of magnitude above a loopback round trip, so a
        // loaded runner cannot turn a queued reply into a timed-out probe.
        timeout: UpDuration::from_millis(2_000),
        ..probe_config(
            ProbeKind::Http,
            &format!("http://127.0.0.1:{}/healthz", gate.addr.port()),
        )
    });
    // Far longer than this test's own patience, so an Online can only have
    // come from a passing probe, never from the deadline path.
    app.listen_timeout = UpDuration::from_millis(60_000);
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    // Wait on the probe's own traffic: this request is proof the probe loop
    // is running. Nothing is queued yet, so the fake owes that connection a
    // reply and no probe could have succeeded.
    gate.next_request().await;
    gate.queue_reply(HttpReply::Status(500));
    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    // The passing reply is queued here and nowhere earlier: the next probe
    // is the first connection the fake will answer 2xx, so Online can only
    // follow this point.
    gate.next_request().await;
    gate.queue_reply(HttpReply::Status(200));
    tokio::time::timeout(
        Duration::from_secs(5),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("Online once the queued reply passes the probe");
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
    let attempt_file = dir.path().join("probe-attempt-0");
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.readiness_probe = Some(ProbeConfig {
        interval: UpDuration::from_millis(50),
        // A `sh` fork and two file operations, bounded three orders of
        // magnitude above what they cost on a quiet runner, so a loaded one
        // cannot time out a probe that would really pass.
        timeout: UpDuration::from_millis(2_000),
        ..probe_config(
            ProbeKind::Exec,
            &format!(
                r#": > "{dir}/probe-attempt-$SHEP_INSTANCE"; test -f "{dir}/ready-$SHEP_INSTANCE""#,
                dir = dir.path().display(),
            ),
        )
    });
    // Far longer than this test's own patience, so an Online can only have
    // come from a passing probe, never from the deadline path.
    app.listen_timeout = UpDuration::from_millis(60_000);
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Starting);

    // Wait on the probe's own traffic: the marker is named through
    // `$SHEP_INSTANCE`, so `probe-attempt-0` proves one probe ran with the
    // assembled env. `ready-0` is absent until this test writes it, so that
    // probe's verdict was a failure and `Starting` is settled, not a guess.
    await_file(&attempt_file, Duration::from_secs(5)).await;
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

// Both values at their production defaults, the arrangement a gated wait has
// to survive: the probe's 10s `interval` is past the 3000ms `listen_timeout`,
// so following `interval` lands the second look after the readiness deadline
// has already elapsed. Real clock, since the probe spawns a real `sh`.
#[cfg(unix)]
#[tokio::test]
async fn a_reload_of_a_probed_app_reaches_online_at_the_default_probe_interval() {
    let dir = tempfile::tempdir().unwrap();
    let ready_file = dir.path().join("ready-0");
    let attempts_file = dir.path().join("probe-attempts-0");
    let mut app = AppConfig::minimal("web", "./srv");
    app.readiness_probe = Some(ProbeConfig {
        // A `sh` fork and two file operations, bounded three orders of
        // magnitude above what they cost on a quiet runner. `interval` stays
        // at the 10s this fixture copies from production: it is the number
        // under test.
        timeout: UpDuration::from_millis(2_000),
        ..probe_config(
            ProbeKind::Exec,
            &format!(
                r#"echo >> "{dir}/probe-attempts-$SHEP_INSTANCE"; test -f "{dir}/ready-$SHEP_INSTANCE""#,
                dir = dir.path().display(),
            ),
        )
    });
    // The start's own probe has to pass for there to be an `Online` instance
    // to reload, so the marker is there before the flock is.
    std::fs::write(&ready_file, b"").unwrap();
    let (handle, _runner, mut rx) = started(
        &dir,
        app,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    tokio::time::timeout(
        Duration::from_secs(10),
        await_event(&mut rx, 0, ProcessEventKind::Online),
    )
    .await
    .expect("the start's probe passes the marker written for it");
    assert_eq!(probe_attempts(&attempts_file), 1, "one probe for the start");

    // Gone before the reload is asked for, so the replacement's first probe
    // cannot be the one that finds it.
    std::fs::remove_file(&ready_file).unwrap();

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");

    // The second attempt is the replacement's first look, taken with nothing
    // to answer it. Restoring the marker here is what makes a later poll the
    // one that passes.
    await_probe_attempts(&attempts_file, 2, Duration::from_secs(10)).await;
    std::fs::write(&ready_file, b"").unwrap();

    tokio::time::timeout(
        Duration::from_secs(10),
        await_event(&mut rx, 1, ProcessEventKind::Reloaded),
    )
    .await
    .expect("a replacement that failed its first probe and passed a later one is serving");

    let after = handle.list().await;
    assert_eq!(after.len(), 1, "the drainee's registration goes with it");
    assert_eq!(after[0].id, 1);
    assert_eq!(
        after[0].status,
        ProcStatus::Online,
        "a probe that passed inside listen_timeout must put the replacement online"
    );
}

/// How many probe runs have appended a line to `path`.
fn probe_attempts(path: &std::path::Path) -> usize {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().count(),
        Err(_) => 0,
    }
}

/// Polls `path` until it holds at least `want` probe attempts.
///
/// Polling is how a test waits on a probe child's write: the line lands
/// through the OS, not through the runtime's clock.
async fn await_probe_attempts(path: &std::path::Path, want: usize, bound: Duration) {
    let reached = tokio::time::timeout(bound, async {
        loop {
            if probe_attempts(path) >= want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        reached.is_ok(),
        "{} never reached {want} probe attempts",
        path.display(),
    );
}

/// Polls `path` until it exists, giving up after `bound`.
///
/// Polling is how a test waits on a probe child's write: the file lands
/// through the OS, not through the runtime's clock.
async fn await_file(path: &std::path::Path, bound: Duration) {
    let found = tokio::time::timeout(bound, async {
        loop {
            match path.try_exists() {
                Ok(true) => break,
                Ok(false) => {}
                Err(err) => panic!("cannot poll {}: {err}", path.display()),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        found.is_ok(),
        "{} never appeared: the exec probe never ran to completion with \
         $SHEP_INSTANCE expanded to 0",
        path.display()
    );
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
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
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
