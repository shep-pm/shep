//! Tests for a restart carried across a handover.
//!
//! A manual stop or restart that was in flight has to finish as itself, not as
//! a crash: the stop still escalates to sigkill, and a sheep owed a respawn
//! waits out only what is left of its delay.

use super::*;

/// The escalation is a `tokio::time::timeout` inside `kill_process`, which
/// runs on the sheep task the `execve` takes. A successor that re-sends
/// only the polite signal leaves a child that traps `SIGTERM` running
/// forever. The signal in `last_exit` is the whole assertion: a pid check
/// cannot tell a ladder that escalated from one that got lucky.
/// `kill_timeout` is shortened to a second, well inside [`flock_until`].
#[cfg(unix)]
#[tokio::test]
async fn a_carried_manual_stop_still_escalates_to_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    // A loop, not a bare `sleep`: the polite rung reaches the whole group,
    // so a shell waiting on one `sleep` would exit carrying that sleep's
    // status and look like it had obeyed. The touchfile closes the startup
    // race, since a `SIGTERM` arriving before `trap` runs kills the shell.
    let armed = dir.path().join("trap-armed");
    // Bounded, so a case that fails before the ladder reaches it still
    // leaves a child that goes away inside `flock_until`'s bound.
    let pid = adoptable_child(&format!(
        "trap '' TERM; : > {}; i=0; while [ $i -lt 60 ]; do sleep 1; i=$((i+1)); done",
        armed.display()
    ));
    tokio::time::timeout(Duration::from_secs(20), async {
        while !armed.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the defiant child must arm its trap before anything signals it");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_marked(
                "web",
                7,
                Some(pid),
                false,
                Some(PendingManual {
                    kind: ManualKind::Stop,
                    origin: CommandOrigin::Operator,
                }),
                false,
                respawnable(|app| {
                    app.autorestart = false;
                    app.kill_timeout = "1000".parse().unwrap();
                }),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = flock_until(
        &sup,
        |info| info[0].status == ProcStatus::Stopped,
        "a sheep carried mid-ladder must still die on its own, without a hand-sent SIGKILL",
    )
    .await;

    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(9)
        }),
        "the ladder must escalate: this child ignores SIGTERM, so anything else means it \
         was never killed"
    );
}

/// Kind and origin are two separate facts on one marker and neither may be
/// defaulted: a hardcoded `Stop` leaves the sheep down when an operator
/// asked for it back, and a hardcoded `Operator` broadcasts a memory
/// breach as a user action. `Automatic` here, so the flag can come back
/// `false`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_manual_restart_respawns_and_keeps_its_origin() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_marked(
                "web",
                7,
                Some(pid),
                false,
                Some(PendingManual {
                    kind: ManualKind::Restart,
                    origin: CommandOrigin::Automatic,
                }),
                false,
                // Off, so the respawn below can only be the carried
                // `Restart`: an `autorestart` app would come back anyway.
                respawnable(|app| app.autorestart = false),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let manually = tokio::time::timeout(
        Duration::from_secs(20),
        await_event(&mut rx, 7, ProcessEventKind::Restart),
    )
    .await
    .expect("a carried manual restart must respawn the sheep after the exec");
    assert!(
        !manually,
        "an automatic restart carried across a handover must not be broadcast as a user \
         action"
    );

    let info = flock_until(
        &sup,
        |info| info[0].restarts == 1,
        "the respawn must be counted against the sheep that was carried",
    )
    .await;
    assert_ne!(
        info[0].pid,
        Some(pid),
        "a restart is a new process, not the adopted one"
    );
    assert_eq!(
        info[0].status,
        ProcStatus::Online,
        "an ungated app is Online the moment it respawns"
    );

    // The respawn is a real `sleep 30`; the shutdown stops it being left
    // behind when the tempdir goes.
    sup.shutdown().await;
}

/// Counters are restored before any slot is installed. A successor that
/// starts `next_id` at zero hands a new sheep an id the predecessor
/// already gave out.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_successor_does_not_reissue_a_live_id() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |_| {}))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let fresh = sup
        .start(vec![normalize(AppConfig::minimal("api", "./srv")).unwrap()])
        .await
        .expect("a fresh app starts under a successor");

    assert!(fresh[0].id >= 9, "reissued a live id: {}", fresh[0].id);
}

/// `schedule_restart`'s timer task dies with the process image, and
/// `handle_restart_due` is the only thing that moves a sheep off
/// [`ProcStatus::WaitingRestart`]. The pid is the assertion, not the
/// status: [`STAND_IN_SPAWN_PID`] is what [`AdoptingRunner`] hands a fresh
/// spawn and is no carried sheep's pid, so a row reporting it can only
/// have been respawned by the re-armed timer.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_owed_a_restart_still_gets_one() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, None, |entry| {
                entry.status = ProcStatus::WaitingRestart;
            }))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    // Virtual, and instant: the paused clock advances itself once every
    // task is idle, so this waits out the re-armed backoff for free.
    tokio::time::sleep(Duration::from_secs(5)).await;

    let info = sup.list().await;
    assert_eq!(
        info[0].status,
        ProcStatus::Online,
        "a sheep owed a respawn was left waiting for a timer that died with the exec: {info:?}"
    );
    assert_eq!(
        info[0].pid,
        Some(STAND_IN_SPAWN_PID),
        "the restart must be a real respawn, not a status edit: {info:?}"
    );
}

/// Both sheep are paced by a fixed hour and owed a respawn. One carries a
/// due moment two seconds out, the other carries none, and they share one
/// paused clock and one advance: only the pair tells "it waited out what
/// was left" from "it did not wait at all". Ten virtual seconds is five
/// times the carried deadline and a fraction of the hour, so each status
/// after it can only have come from one of the two behaviours.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_waits_out_only_what_is_left_of_its_delay() {
    let hour = "1h".parse().unwrap();
    let anchored_dir = tempfile::tempdir().unwrap();
    let (anchored_events, _anchored_rx) = crate::bus::test_bus(64);
    let anchored =
        SupervisorBuilder::new(AdoptingRunner, test_paths(&anchored_dir), anchored_events)
            .spawn_adopted(
                vec![without_handles(carried_owed_a_restart(
                    "web",
                    7,
                    hour,
                    Some(SystemTime::now() + Duration::from_secs(2)),
                ))],
                counters(9),
                Vec::new(),
            )
            .expect("a carried flock installs");

    let control_dir = tempfile::tempdir().unwrap();
    let (control_events, _control_rx) = crate::bus::test_bus(64);
    let control = SupervisorBuilder::new(AdoptingRunner, test_paths(&control_dir), control_events)
        .spawn_adopted(
            vec![without_handles(carried_owed_a_restart(
                "web", 7, hour, None,
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    tokio::time::sleep(Duration::from_secs(10)).await;

    let anchored = anchored.list().await;
    assert_eq!(
        anchored[0].status,
        ProcStatus::Online,
        "a sheep two seconds from its own due time must not be made to wait another hour by \
         the handover: {anchored:?}"
    );
    assert_eq!(
        anchored[0].pid,
        Some(STAND_IN_SPAWN_PID),
        "the restart must be a real respawn, not a status edit: {anchored:?}"
    );

    let control = control.list().await;
    assert_eq!(
        control[0].status,
        ProcStatus::WaitingRestart,
        "with no carried deadline the whole delay is re-armed, so an hourly app must still \
         be waiting ten seconds in — a control that respawned would mean the case above \
         proved nothing: {control:?}"
    );
}

/// Drives a real exit through `decide_on_exit`, the only thing that writes
/// the moment. A window, not an equality: the deadline is wall-clock,
/// since a monotonic instant cannot cross an `execve`, and a paused tokio
/// clock does not pause the wall clock.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_snapshot_of_a_waiting_sheep_carries_the_moment_its_respawn_falls_due() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(1)]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    // An hour, so the sheep is still waiting when the snapshot is taken
    // rather than racing its own respawn.
    app.restart_delay = Some("1h".parse().unwrap());
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    // The sheep starts `Online`, so waiting for `WaitingRestart` stops
    // this passing on the state before the crash.
    let waits = async {
        while handle.list().await[0].status != ProcStatus::WaitingRestart {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), waits)
        .await
        .expect("an app that exits immediately with autorestart on must reach WaitingRestart");

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let due = blob.sheep()[0]
        .restart_due()
        .expect("a sheep owed a respawn must carry the moment it is owed at");
    let left = due
        .duration_since(SystemTime::now())
        .expect("a deadline an hour out has not passed");
    assert!(
        left > Duration::from_secs(3595) && left <= Duration::from_secs(3600),
        "the carried moment must be this sheep's own exit plus its own delay: {left:?}"
    );
}

/// The successor has to restore the moment onto its own slot, not merely
/// read it on the way past. An absolute moment keeps the chain flat: a
/// carried remainder would have each hop add its own handover duration
/// back on, which is the drift the window asserts is absent.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_second_handover_inside_one_delay_still_names_the_original_moment() {
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let (events, _rx) = crate::bus::test_bus(64);
    let due = SystemTime::now() + Duration::from_secs(3000);
    let handle = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_owed_a_restart(
                "web",
                7,
                "1h".parse().unwrap(),
                Some(due),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let carried = blob.sheep()[0]
        .restart_due()
        .expect("a successor must hand its own successor the moment it was given");
    let drift = carried
        .duration_since(due)
        .or_else(|_| due.duration_since(carried))
        .unwrap();
    assert!(
        drift < Duration::from_secs(1),
        "the second hop must name the same moment as the first, not a fresh one: {drift:?} \
         of drift"
    );
}
