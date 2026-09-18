//! The reporter: a report becomes a guarded restart, or nothing.

use super::*;

// Without `CommandOrigin::Automatic` a memory-breach restart reaches every
// subscriber as `manually: true`, indistinguishable from `shep restart`.
#[tokio::test(start_paused = true)]
async fn a_breach_naming_the_running_pid_restarts_that_sheep() {
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    // The ceiling the synthetic breach below names: the actor re-asks a
    // breach against the ceiling in force.
    handle
        .start(vec![app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(500));
        })])
        .await
        .unwrap();
    let pid = handle.list().await[0].pid.expect("a live sheep has a pid");
    let (breach_tx, breach_rx) = mpsc::channel(4);
    let (_live_tx, live_rx) = mpsc::channel(4);
    let _reporter = spawn_extras_reporter(breach_rx, live_rx, handle.clone());

    breach_tx
        .send(LimitBreach {
            id: 0,
            root_pid: pid,
            observed: MemSize::from_bytes(900),
            limit: MemSize::from_bytes(500),
        })
        .await
        .unwrap();

    let (info, manually) = expect_restart_event(&mut rx, "web", EVENT_WAIT).await;
    assert_eq!(info.id, 0);
    assert_eq!(info.restarts, 1);
    assert!(
        !manually,
        "nobody typed this: a memory breach is the daemon's own doing"
    );
}

// Separate from the breach case because the reporter's two arms are two
// `select!` branches: a broken `liveness` arm leaves the breach one green.
#[tokio::test(start_paused = true)]
async fn a_liveness_failure_naming_the_running_pid_restarts_that_sheep() {
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    handle.start(vec![app_with("web", |_| {})]).await.unwrap();
    let pid = handle.list().await[0].pid.expect("a live sheep has a pid");
    let (_breach_tx, breach_rx) = mpsc::channel(4);
    let (live_tx, live_rx) = mpsc::channel(4);
    let _reporter = spawn_extras_reporter(breach_rx, live_rx, handle.clone());

    // `spawn_test_fixture` wires no `Extras`, so the actor's epoch for id 0
    // stays at the `0` reported for an unseen id, which this report matches.
    live_tx
        .send(LivenessReport {
            id: 0,
            pid,
            epoch: 0,
        })
        .await
        .unwrap();

    let (info, manually) = expect_restart_event(&mut rx, "web", EVENT_WAIT).await;
    assert_eq!(info.id, 0);
    assert_eq!(info.restarts, 1);
    assert!(
        !manually,
        "nobody typed this: a liveness failure is the daemon's own doing"
    );
}

// A report for an id a `Delete` already removed is an ordinary race. The
// surviving `list()` is the proof: a panicked actor closes its mailbox.
#[tokio::test(start_paused = true)]
async fn an_extra_restart_for_an_unknown_id_leaves_the_engine_running() {
    let (handle, _rx, _fixture) = spawn_test_fixture();
    handle.start(vec![app_with("web", |_| {})]).await.unwrap();

    handle.extra_restart(99, 4242, None, None).await;

    assert_eq!(
        handle.list().await.len(),
        1,
        "the actor must still be serving after a report for an id it does not know"
    );
}

// A gated app between its spawn and its readiness result is `Starting` with
// a live pid, the one state in which the pid guard passes and the status
// guard is all that is left.
#[tokio::test(start_paused = true)]
async fn an_extra_restart_for_a_sheep_that_is_still_starting_restarts_nothing() {
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let app = app_with("web", |app| {
        app.wait_ready = true;
        // Long enough that the readiness wait cannot resolve inside this
        // test's windows.
        app.listen_timeout = UpDuration::from_millis(6 * 60 * 60 * 1_000);
    });
    handle.start(vec![app]).await.unwrap();
    let listing = handle.list().await;
    assert_eq!(
        listing[0].status,
        ProcStatus::Starting,
        "this case is only meaningful while the sheep is gated on readiness"
    );
    let pid = listing[0].pid.expect("a spawned sheep has a pid");

    handle.extra_restart(0, pid, None, None).await;

    assert_no_restart_within(&mut rx, "web", Duration::from_secs(30)).await;
    assert_eq!(handle.list().await[0].restarts, 0);
}

// `ProcessSelector::Id` matches regardless of status, so the public
// `restart` on a stopped sheep respawns it. The runner carries spare procs
// so that resurrection would have something to spawn from.
#[tokio::test(start_paused = true)]
async fn a_breach_for_a_stopped_sheep_restarts_nothing() {
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    handle.start(vec![app_with("web", |_| {})]).await.unwrap();
    let pid = handle.list().await[0].pid.expect("a live sheep has a pid");
    let (breach_tx, breach_rx) = mpsc::channel(4);
    let (_live_tx, live_rx) = mpsc::channel(4);
    let _reporter = spawn_extras_reporter(breach_rx, live_rx, handle.clone());

    handle
        .stop(ProcessSelector::Id(0))
        .await
        .expect("the sheep stops");
    breach_tx
        .send(LimitBreach {
            id: 0,
            root_pid: pid,
            observed: MemSize::from_bytes(900),
            limit: MemSize::from_bytes(500),
        })
        .await
        .unwrap();

    assert_no_restart_within(&mut rx, "web", Duration::from_secs(30)).await;
    let listing = handle.list().await;
    assert_eq!(listing[0].status, ProcStatus::Stopped);
    assert_eq!(listing[0].restarts, 0);
}

// A breach raised for the process a restart already replaced would restart
// its healthy successor.
#[tokio::test(start_paused = true)]
async fn a_breach_carrying_the_previous_pid_restarts_nothing() {
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    // The ceiling both synthetic breaches below name.
    handle
        .start(vec![app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(500));
        })])
        .await
        .unwrap();
    let stale_pid = handle.list().await[0].pid.expect("a live sheep has a pid");
    let (breach_tx, breach_rx) = mpsc::channel(4);
    let (_live_tx, live_rx) = mpsc::channel(4);
    let _reporter = spawn_extras_reporter(breach_rx, live_rx, handle.clone());

    handle
        .restart(ProcessSelector::Id(0))
        .await
        .expect("the sheep restarts");
    expect_restart(&mut rx, "web", EVENT_WAIT).await;
    let live_pid = handle.list().await[0].pid.expect("a live sheep has a pid");
    assert_ne!(stale_pid, live_pid, "the respawn must have a new pid");

    let breach = |root_pid| LimitBreach {
        id: 0,
        root_pid,
        observed: MemSize::from_bytes(900),
        limit: MemSize::from_bytes(500),
    };
    breach_tx.send(breach(stale_pid)).await.unwrap();
    assert_no_restart_within(&mut rx, "web", Duration::from_secs(30)).await;
    assert_eq!(
        handle.list().await[0].restarts,
        1,
        "a stale breach must not bump the restart count"
    );

    breach_tx.send(breach(live_pid)).await.unwrap();
    let info = expect_restart(&mut rx, "web", EVENT_WAIT).await;
    assert_eq!(
        info.restarts, 2,
        "the same reporter, fed the current pid, must restart"
    );
}
