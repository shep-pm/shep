//! Tests for a reload carried across a handover.
//!
//! A swap part way through has to be picked up where it was left: asked again
//! if it was verifying, drained if it was draining, committed if its
//! replacement is ready, and abandoned on time if it cannot finish.

use super::*;

/// A serial drain is what holds still long enough to be snapshotted: a
/// `readiness_probe` with no `reuse_port` is the one arrangement
/// `ReloadMode::of` sends down the serial ordering, and
/// [`ProcScript::never_reports_its_exit`] models the child a kill ladder
/// cannot end, so the swap stays in [`ReloadPhase::DrainFirst`].
///
/// A real clock, unlike its neighbours: a paused one auto-advances
/// whenever every task is idle, and the awaits inside `handover_snapshot`
/// are such a window.
#[cfg(unix)]
#[tokio::test]
async fn a_snapshot_taken_mid_swap_carries_the_job_and_the_markers() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_reports_its_exit(); 2]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.listen_timeout = UpDuration::from_millis(200);
    app.readiness_probe = Some(probe_config(ProbeKind::Tcp, "127.0.0.1:9"));
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    // A probed app is `Starting` until its probe answers or
    // `listen_timeout` elapses, and `reload_eligible` refuses anything not
    // serving, so without this the reload skips the only instance there is.
    let online = loop {
        let info = handle.list().await;
        if info[0].status == ProcStatus::Online {
            break info;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let old_id = online[0].id;

    handle
        .reload(ProcessSelector::All)
        .await
        .expect("an online app reloads");

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    assert_eq!(
        blob.reloads(),
        &[CarriedReload {
            app: "web".to_owned(),
            queue: Vec::new(),
            mode: ReloadMode::Serial,
            swap: ReloadSwap {
                old_id,
                new_id: None,
                phase: ReloadPhase::DrainFirst,
            },
        }],
        "the job the successor continues has to be in the blob, whole"
    );
    assert_eq!(
        blob.sheep()[0].reload(),
        Some(ReloadState::Drainee { new_id: None }),
        "and the marker that routes this instance's exit with it"
    );
}

/// A carried sheep that is half of a swap, with the role and the status
/// the predecessor's own entry carried.
#[cfg(unix)]
fn carried_in_swap(
    name: &str,
    id: u32,
    pid: Option<u32>,
    role: ReloadState,
    status: ProcStatus,
    manual: Option<PendingManual>,
    mutate: impl FnOnce(&mut ProcessEntry),
) -> CarriedSheep {
    carried_marked(name, id, pid, false, manual, false, move |entry| {
        mutate(entry);
        entry.reload = role;
        entry.status = status;
    })
}

/// One app's in-flight reload, as a blob carries it, with an empty queue.
///
/// Empty because a queue behind the swap would let a case pass on the next
/// instance's swap rather than on the carried one.
#[cfg(unix)]
fn carried_job(
    app: &str,
    mode: ReloadMode,
    old_id: u32,
    new_id: Option<u32>,
    phase: ReloadPhase,
) -> CarriedReload {
    CarriedReload {
        app: app.to_owned(),
        queue: Vec::new(),
        mode,
        swap: ReloadSwap {
            old_id,
            new_id,
            phase,
        },
    }
}

/// The watchdog is a `tokio::spawn`ed sleep of the predecessor's that the
/// `execve` takes. With the job restored and no timer over it,
/// `handle_reload` refuses on the map key forever, and the refusal is
/// whole-selector, so `shep reload all` goes with it. That refusal is the
/// assertion both ways. Nothing here ever exits, since
/// [`AdoptingRunner`]'s `wait` never resolves, so only a timer this image
/// armed can produce the second half.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_carried_swap_that_cannot_finish_is_still_abandoned_on_time() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(4242),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    None,
                    |_| {},
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(4243),
                    ReloadState::Replacement,
                    ProcStatus::Online,
                    None,
                    |_| {},
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    let refused = sup.reload(ProcessSelector::All).await;
    assert!(
        matches!(refused, Err(SupervisorError::ReloadInFlight(ref name)) if name == "web"),
        "the carried job must be in the map, or this case proves nothing: {refused:?}"
    );

    // Parking on `recv` advances the paused clock, so this waits out the
    // 16s watchdog for free. The bound is load-bearing: with no timer
    // armed the clock has nothing to advance to, so an unbounded wait
    // hangs the suite instead of failing the case.
    tokio::time::timeout(
        Duration::from_secs(3600),
        await_event(&mut rx, 8, ProcessEventKind::ReloadAbandoned),
    )
    .await
    .expect("a carried swap must still be bounded by a watchdog this image armed");

    sup.reload(ProcessSelector::All)
        .await
        .expect("once the watchdog has ended the job, the app must be reloadable again");
}

/// A snapshot cannot produce one, but the blob is a file, the same
/// residual `refuse_repeated_fds` guards on the descriptor side. Arming a
/// watchdog against an absent entry panics, so a build without the guard
/// fails the reload below by aborting.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_carried_reload_naming_no_registered_instance_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried("web", 7, Some(4242), |_| {}))],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                98,
                Some(99),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    sup.reload(ProcessSelector::All)
        .await
        .expect("a job naming nothing must be dropped, not left refusing every later reload");
}

/// `spawn_verify_task` is a task of the predecessor's too, so a successor
/// that re-arms only the watchdog abandons a deploy that worked: the
/// replacement serves on, but `Reloaded` never fires and the rest of a
/// clustered app's queue is dropped.
///
/// A real listener and a real clock. The probe answers in microseconds
/// where the watchdog is 16s, so the bound below separates the two by an
/// order of magnitude whatever the host's speed.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_swap_in_verify_is_asked_again_rather_than_abandoned() {
    let probe_target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe_target.local_addr().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    // Bound, never used: dropping the handle would take a sender off the
    // actor's mailbox.
    let _sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_in_swap(
                "web",
                8,
                Some(4243),
                ReloadState::Replacement,
                ProcStatus::Online,
                None,
                move |entry| {
                    let mut app = AppConfig::minimal("web", "./srv");
                    app.autorestart = false;
                    // `Probe` readiness alone takes the serial ordering,
                    // which has no post-drain probe to re-arm. `reuse_port`
                    // puts this app on the overlapping one.
                    app.reuse_port = true;
                    app.readiness_probe = Some(probe_config(ProbeKind::Tcp, &addr.to_string()));
                    entry.spec = normalize(app).unwrap();
                },
            ))],
            counters(9),
            // No drainee: `Verify` is entered once the drainee is reaped,
            // so the re-ask happens with one process left.
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::Verify,
            )],
        )
        .expect("a carried flock installs");

    tokio::time::timeout(
        Duration::from_secs(5),
        await_event(&mut rx, 8, ProcessEventKind::Reloaded),
    )
    .await
    .expect("a carried swap in Verify must be probed again, not left to its watchdog");
}

/// [`ReloadPhase::DrainFirst`] is the one phase with no replacement yet:
/// `Drainee { new_id: None }` routes the exit to `reap_drainee` and so to
/// `spawn_serial_replacement`, not to `decide_on_exit`. A successor that
/// dropped it deregisters a `Stopping` sheep and leaves the slot empty
/// with the job still in the map. A real child and the real runner: only
/// the reaper's `Msg::Exited` proves a carried marker reaches
/// `handle_exited`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_serial_drain_still_spawns_its_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_in_swap(
                "web",
                7,
                Some(pid),
                ReloadState::Drainee { new_id: None },
                ProcStatus::Stopping,
                Some(PendingManual {
                    kind: ManualKind::Stop,
                    origin: CommandOrigin::Operator,
                }),
                swappable,
            ))],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Serial,
                7,
                None,
                ReloadPhase::DrainFirst,
            )],
        )
        .expect("a carried flock installs");

    let info = flock_until(
        &sup,
        |info| info.len() == 1 && info[0].id != 7,
        "a carried serial drain must spawn its replacement once the instance it drained goes",
    )
    .await;

    assert_eq!(
        info[0].id, 9,
        "the replacement takes the next carried id, not a reissued one"
    );
    assert_eq!(
        info[0].restarts, 0,
        "a reload is not a restart, so the count carries across the swap unchanged"
    );
    assert_ne!(info[0].pid, Some(pid), "the replacement is a new process");

    sup.shutdown().await;
}

/// The readiness wait, the `Replacement` marker and the job all have to
/// survive. Without the wait the replacement sits `Starting` forever
/// beside a drainee still serving: two live instances of a one-instance
/// app. Without the marker or the job, `handle_ready_result` takes the
/// ordinary path and the swap never commits; the `manually` flag below is
/// that route's fingerprint, since `spawn_replacement` passes `true` and
/// an adopted sheep that is not a replacement is armed with `false`.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_replacement_awaiting_readiness_commits_its_swap() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let drainee = adoptable_child("sleep 30");
    let replacement = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(drainee),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    // No marker: an overlapping swap does not ask the
                    // drainee to go until its replacement is serving.
                    None,
                    swappable,
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(replacement),
                    ReloadState::Replacement,
                    ProcStatus::Starting,
                    None,
                    swappable,
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::AwaitReady,
            )],
        )
        .expect("a carried flock installs");

    let manually = tokio::time::timeout(
        Duration::from_secs(20),
        await_event(&mut rx, 8, ProcessEventKind::Online),
    )
    .await
    .expect("a carried replacement must still resolve its readiness after the exec");
    assert!(
        manually,
        "a replacement's Online is an operator's doing; reporting otherwise broadcasts a \
         deploy as the daemon's own"
    );

    let info = flock_until(
        &sup,
        |info| info.len() == 1,
        "committing the swap must drain the instance it replaced",
    )
    .await;
    assert_eq!(info[0].id, 8, "the replacement is what is left: {info:?}");
    assert_eq!(info[0].pid, Some(replacement));

    sup.shutdown().await;
}

/// [`ReloadPhase::DrainOld`] is the committed phase: the replacement is
/// serving and the instance it replaced is on its ladder. The `Drainee`
/// marker sends that instance's exit to `reap_drainee` rather than to
/// `decide_on_exit`, and this app has `autorestart` on, so a successor
/// that dropped the marker would respawn the old code into an instance
/// slot the replacement owns.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_drainee_still_finishes_its_swap() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let drainee = adoptable_child("sleep 30");
    let replacement = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(drainee),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    Some(PendingManual {
                        kind: ManualKind::Stop,
                        origin: CommandOrigin::Operator,
                    }),
                    |entry| swappable_with(entry, |app| app.autorestart = true),
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(replacement),
                    ReloadState::Replacement,
                    ProcStatus::Online,
                    None,
                    |entry| swappable_with(entry, |app| app.autorestart = true),
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    tokio::time::timeout(
        Duration::from_secs(20),
        await_event(&mut rx, 8, ProcessEventKind::Reloaded),
    )
    .await
    .expect("a carried drainee's exit must finish the swap it was half of");

    let info = sup.list().await;
    assert_eq!(
        info.len(),
        1,
        "the drainee is deregistered by the swap, never respawned: {info:?}"
    );
    assert_eq!(info[0].id, 8);
    assert_eq!(info[0].pid, Some(replacement));

    sup.shutdown().await;
}

/// The cap is not recorded anywhere, so it is derived from the role: both
/// sites that pass `LadderCap::Drain` leave the `Drainee` marker on the
/// entry. The child ignores `SIGTERM`, so only the escalation ends it, and
/// the two timeouts are three orders of magnitude apart: under the drain's
/// cap the `SIGKILL` lands a quarter of a second in, under the stop's five
/// minutes later, past [`flock_until`]'s bound.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_drainee_is_capped_by_graceful_timeout_not_kill_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let drainee = adoptable_child("trap '' TERM; sleep 300");
    let replacement = adoptable_child("sleep 30");
    let capped = |entry: &mut ProcessEntry| {
        swappable_with(entry, |app| {
            app.graceful_timeout = UpDuration::from_millis(250);
            app.kill_timeout = UpDuration::from_millis(300_000);
        });
    };
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![
                without_handles(carried_in_swap(
                    "web",
                    7,
                    Some(drainee),
                    ReloadState::Drainee { new_id: Some(8) },
                    ProcStatus::Stopping,
                    Some(PendingManual {
                        kind: ManualKind::Stop,
                        origin: CommandOrigin::Operator,
                    }),
                    capped,
                )),
                without_handles(carried_in_swap(
                    "web",
                    8,
                    Some(replacement),
                    ReloadState::Replacement,
                    ProcStatus::Online,
                    None,
                    capped,
                )),
            ],
            counters(9),
            vec![carried_job(
                "web",
                ReloadMode::Overlap,
                7,
                Some(8),
                ReloadPhase::DrainOld,
            )],
        )
        .expect("a carried flock installs");

    flock_until(
        &sup,
        |info| info.len() == 1,
        "a carried drainee must escalate at graceful_timeout, not at kill_timeout",
    )
    .await;

    sup.shutdown().await;
}

/// An abandoned reload leaves its replacement `Starting`, and a reload
/// replaces `Online` instances, so `SheepSlot::ready_failed` is the whole
/// of what keeps the leftover reachable. Asserted through a real reload
/// rather than by reading the slot back: `handle_reload` replies `Ok` with
/// the row in it either way, before its selector pass has run.
#[cfg(unix)]
#[tokio::test]
async fn a_carried_ready_failed_instance_is_still_replaceable() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let pid = adoptable_child("sleep 30");
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_ready_failed(
                "web",
                7,
                Some(pid),
                |entry| {
                    swappable(entry);
                    entry.status = ProcStatus::Starting;
                },
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    sup.reload(ProcessSelector::All)
        .await
        .expect("a registered app is one a reload can name");

    let info = flock_until(
        &sup,
        |info| info.len() == 1 && info[0].id != 7,
        "a carried `ready_failed` instance must still be replaceable by the reload that \
         rolls its release back",
    )
    .await;

    assert_eq!(
        info[0].id, 9,
        "the replacement takes the next carried id, not a reissued one"
    );
    assert_ne!(info[0].pid, Some(pid), "the replacement is a new process");

    sup.shutdown().await;
}

/// Both sheep are `Starting` and only the carried flag tells them apart:
/// an ordinary one is mid-wait and owed a fresh one, this one's wait
/// already ran and failed. `handle_ready_result`'s `TimedOut` arm goes
/// `Online` anyway and `went_online` clears `ready_failed` on its way
/// past, so arming one would report an abandoned release as serving. The
/// clock runs past the app's `listen_timeout`, so any wait that was armed
/// has fired by the time the status is read.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_carried_ready_failed_instance_gets_no_fresh_readiness_wait() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let sup = SupervisorBuilder::new(AdoptingRunner, test_paths(&dir), events)
        .spawn_adopted(
            vec![without_handles(carried_ready_failed(
                "web",
                7,
                Some(4242),
                |entry| {
                    let mut app = AppConfig::minimal("web", "./srv");
                    app.autorestart = false;
                    app.wait_ready = true;
                    entry.spec = normalize(app).unwrap();
                    entry.status = ProcStatus::Starting;
                },
            ))],
            counters(9),
            Vec::new(),
        )
        .expect("a carried flock installs");

    tokio::time::sleep(Duration::from_secs(120)).await;

    assert_eq!(
        sup.list().await[0].status,
        ProcStatus::Starting,
        "an instance whose readiness already failed must not be handed a second verdict by \
         the successor that adopted it"
    );
}

/// The app a carried swap's two halves run: a real `sleep`, so a
/// replacement can be spawned and a drainee signalled, with a
/// `listen_timeout` short enough for a real-clock case.
#[cfg(unix)]
fn swappable(entry: &mut ProcessEntry) {
    swappable_with(entry, |_| {});
}

/// [`swappable`], with `mutate` free to change the app first.
#[cfg(unix)]
fn swappable_with(entry: &mut ProcessEntry, mutate: impl FnOnce(&mut AppConfig)) {
    let mut app = AppConfig::minimal("web", "/bin/sh");
    app.args = vec!["-c".to_owned(), "sleep 30".to_owned()];
    app.autorestart = false;
    app.listen_timeout = UpDuration::from_millis(200);
    mutate(&mut app);
    entry.spec = normalize(app).unwrap();
}
