//! Tests for taking over processes a previous shepherd left running.
//!
//! An adopted sheep keeps its pid, its id, its counters and its last exit, and
//! a dog stays a dog. From the first exit onwards it travels the ordinary path.

use super::*;

/// The wait that would resolve it belonged to a task of the predecessor's,
/// which the `execve` took. Nothing else moves a sheep off `Starting`
/// except its own exit, so a successor that adopts one without re-arming
/// leaves it there with none of `arm_extras`'s watch, cron or memory
/// limits, which fire at the `Online` transition. Nothing here writes
/// `{"kind":"ready"}` and `handle_ready_result` puts a timed-out sheep
/// `Online` anyway, so reaching `Online` at all proves a wait was armed.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_that_was_still_starting_reaches_online() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |entry| {
                let mut app = AppConfig::minimal("web", "./srv");
                app.autorestart = false;
                app.wait_ready = true;
                entry.spec = normalize(app).unwrap();
                entry.status = ProcStatus::Starting;
            }))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    assert_eq!(
        sup.list().await[0].status,
        ProcStatus::Starting,
        "the blob said this sheep was still starting, so the successor must agree"
    );

    // Polled, not advanced by a fixed amount: the wait runs on a task, so
    // its result reaches the actor a message later than the deadline. The
    // bound sits past the app's 3s `listen_timeout`.
    let goes_online = async {
        while sup.list().await[0].status != ProcStatus::Online {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), goes_online)
        .await
        .expect("an adopted sheep left `Starting` must still have a readiness wait over it");
}

/// Every row of a listing by id, which is the order these cases name
/// their sheep in.
#[cfg(unix)]
fn by_id(mut info: Vec<ProcessInfo>) -> Vec<ProcessInfo> {
    info.sort_unstable_by_key(|row| row.id);
    info
}

/// A sheep whose pid moved was respawned.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_flock_keeps_its_pids_and_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried("web", 7, Some(4242), |_| {})),
                without_handles(carried("api", 8, Some(4243), |_| {})),
            ],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = by_id(sup.list().await);

    assert_eq!(
        info.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![7, 8],
        "the successor reissued ids instead of carrying them"
    );
    assert_eq!(
        info.iter().map(|row| row.pid).collect::<Vec<_>>(),
        vec![Some(4242), Some(4243)],
        "a sheep whose pid moved was respawned"
    );
}

/// Losing the marker is invisible to a pid check: the dog runs on. What
/// changes is that `matching_ids` stops passing it by, so `shep restart
/// all` reaches it and `shep dogs` loses it entirely.
///
/// A `stop` over a kennel with no sheep in it, so nothing waits on a kill
/// ladder and `NotFound` answers the wildcard. With the marker dropped the
/// wildcard matches the dog and parks on an exit `StandInProc` never
/// delivers, so the timeout turns a hung suite into a failure.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_dog_keeps_its_marker_and_stays_out_of_a_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried(
                "log-rotate",
                7,
                Some(4242),
                |entry| {
                    entry.dog = Some(DogSource::Adopted {
                        path: "/opt/bin/shep-log-rotate".to_string(),
                    });
                },
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = sup.list().await;
    assert_eq!(
        info[0].dog,
        Some(DogSource::Adopted {
            path: "/opt/bin/shep-log-rotate".to_string(),
        }),
        "a dog adopted as an ordinary sheep is one `shep dogs` has lost"
    );

    let swept = tokio::time::timeout(Duration::from_secs(5), sup.stop(ProcessSelector::All))
        .await
        .expect("a wildcard over a flock of nothing but dogs must answer rather than hang");
    assert!(
        matches!(swept, Err(SupervisorError::NotFound)),
        "`all` is the flock, not the kennel: {swept:?}"
    );
}

/// Losing these is silent. `restarts` resetting to zero hands a
/// crash-looping app amnesty it did not earn, and a lost `last_exit`
/// answers "why did it stop" with nothing.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_adopted_sheep_keeps_its_counters_and_last_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |entry| {
                entry.restarts = 4;
                entry.last_exit = Some(ExitInfo {
                    code: Some(2),
                    signal: None,
                });
            }))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = sup.list().await;

    assert_eq!(info[0].restarts, 4, "the restart count was reset");
    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: Some(2),
            signal: None
        }),
        "the last exit was lost"
    );
}

/// An adopted sheep that exits must reach `handle_exited` and be judged by
/// `decide_on_exit` like any other, not sit there `Online` forever.
///
/// A real child and the real runner, since a scripted exit would prove the
/// fake rather than the targeted `waitpid` an adopted pid needs. `/bin/sh`
/// is spawned by `std::process::Command`, so tokio holds no `Child` for it
/// and nothing but the reaper waits on it.
#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the adopted flock's reaper collects this status; a Child::wait would take it first"
)]
async fn an_adopted_sheeps_exit_flows_through_the_ordinary_path() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("a test host can spawn a shell");
    let pid = child.id();
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(pid), |_| {}))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("the adopted child is signalable");

    let info = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = sup.list().await;
            if info[0].status == ProcStatus::Stopped {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("an adopted sheep's exit reaches the actor");

    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(9)
        }),
        "the exit must be recorded, not lost"
    );
}

/// [`handover::fitness`] carries a pending delete rather than refusing it,
/// so a sheep whose delete was in flight when the predecessor exec'd must
/// still be deregistered on its next exit, not left registered or
/// respawned. A real child and the real runner: an adopted pid has no
/// `Child` handle, and only the reaper's `Msg::Exited` proves the carried
/// marker reaches `handle_exited`.
#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the adopted flock's reaper collects this status; a Child::wait would take it first"
)]
async fn a_carried_pending_delete_deregisters_on_the_next_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("a test host can spawn a shell");
    let pid = child.id();
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_marked(
                "web",
                7,
                Some(pid),
                true,
                None,
                false,
                |_| {},
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("the adopted child is signalable");

    let info = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = sup.list().await;
            if info.is_empty() {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect(
        "a carried pending delete must deregister the sheep on its next exit, not merely \
         stop it",
    );

    assert!(
        info.is_empty(),
        "the deleted sheep must be gone, not just stopped"
    );
}

/// The ladder ran inside the predecessor's sheep task and the `execve`
/// took it, so the sheep reaching a terminal status is what proves the
/// re-arm. `decide_on_exit` reads the marker as `manual_stop`, and this
/// app has `autorestart` on, so a ladder armed without it would kill the
/// sheep and then respawn it. A real child and the real runner: only the
/// reaper's `Msg::Exited` proves a carried marker reaches `handle_exited`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_manual_stop_stops_the_sheep_instead_of_respawning_it() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
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
                respawnable(|app| app.autorestart = true),
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    let info = flock_until(
        &sup,
        |info| info[0].status == ProcStatus::Stopped,
        "a carried manual stop must still stop the sheep after the exec",
    )
    .await;

    assert_eq!(
        info[0].restarts, 0,
        "a stop must not come back as a respawn"
    );
    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: None,
            signal: Some(15)
        }),
        "the re-armed ladder starts at its polite rung, not at SIGKILL"
    );
    assert_eq!(info[0].pid, None, "a stopped sheep holds no pid");
}
