//! Tests for the snapshot a successor daemon receives.
//!
//! The snapshot names every live process and the descriptors behind its log
//! pumps. A pump that never reports must not hang it, and a sweep of six wedged
//! pumps has to cost one deadline rather than six.

use super::*;

/// Whether `fd` names something open in this process.
#[cfg(unix)]
fn is_open(fd: std::os::fd::RawFd) -> bool {
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).is_ok()
}

/// One of the two sheep has a shepherd channel and the other does not. The
/// channel is the one number a snapshot may drop, so a case with only
/// channelled sheep could not tell "every number is open" from "every
/// number is present".
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_blob_from_a_live_flock_names_open_descriptors_per_sheep() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner =
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut talkative = AppConfig::minimal("api", "./srv");
    talkative.channel = true;
    handle
        .start(vec![
            normalize(AppConfig::minimal("web", "./srv")).unwrap(),
            normalize(talkative).unwrap(),
        ])
        .await
        .unwrap();

    let (candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    assert_eq!(candidates.len(), 2, "both sheep must reach the gate");
    assert_eq!(blob.sheep().len(), 2);
    for sheep in blob.sheep() {
        for fd in sheep.fds().all().into_iter().flatten() {
            assert!(is_open(fd), "blob names a closed descriptor: {fd}");
        }
    }
}

/// The pump reports the number it was told at the spawn and cannot learn
/// the channel is gone, so the number can name whatever the kernel has
/// since handed to the next `open` and the successor would write a
/// shepherd message into a log file. `SheepSlot::open_channel` decides
/// delivery, and the snapshot masks the field with it. The fake reports a
/// number for every field, so without the mask this case sees one.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_snapshot_names_no_channel_for_a_sheep_that_has_none() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner =
        ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut talkative = AppConfig::minimal("api", "./srv");
    talkative.channel = true;
    handle
        .start(vec![
            normalize(AppConfig::minimal("web", "./srv")).unwrap(),
            normalize(talkative).unwrap(),
        ])
        .await
        .unwrap();

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let named: Vec<(&str, bool)> = blob
        .sheep()
        .iter()
        .map(|sheep| (sheep.name(), sheep.fds().channel.is_some()))
        .collect();
    assert!(
        named.contains(&("web", false)),
        "a sheep with no channel must carry no channel descriptor: {named:?}"
    );
    assert!(
        named.contains(&("api", true)),
        "a sheep with a live channel must carry its descriptor: {named:?}"
    );
}

/// A pump that never answers has no deadline above it but this one: the
/// SIGHUP path awaits the snapshot before it can fall back, so one stall
/// takes out the handover and the graceful stop with it. An empty
/// `CarriedFds` is what a stopped sheep reports, so collapsing a wedged
/// live pump into one would pass the gate with descriptors dropped. Two
/// sheep, one of each kind, so a gate that refused every flock with a pump
/// in it would pass here for the wrong reason.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_pump_that_never_reports_refuses_the_snapshot_instead_of_hanging() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 2])
        .with_a_pump_that_never_reports(&["wedged"]);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(vec![
            normalize(AppConfig::minimal("answering", "./srv")).unwrap(),
            normalize(AppConfig::minimal("wedged", "./srv")).unwrap(),
        ])
        .await
        .unwrap();

    // Far longer than the deadline under test, and never actually waited:
    // it buys a failure rather than a hung suite if the snapshot has no
    // deadline of its own.
    let snapshot =
        tokio::time::timeout(Duration::from_secs(3600), handle.handover_snapshot(fds))
            .await
            .expect("a snapshot over a wedged pump must answer rather than hang")
            .unwrap();
    let (candidates, _blob, _parked) = snapshot;

    let borrowed: Vec<crate::handover::Candidate<'_>> = candidates
        .iter()
        .map(crate::handover::OwnedCandidate::as_candidate)
        .collect();
    assert_eq!(
        crate::handover::fitness(&borrowed),
        crate::handover::Fitness::Refused(crate::handover::RefusedReason::PumpUnresponsive {
            sheep: "wedged".to_string(),
        }),
        "the gate must refuse, and must name the sheep whose pump went quiet"
    );
    for candidate in &candidates {
        assert_eq!(
            candidate.pump_unresponsive,
            candidate.entry.spec.config().name == "wedged",
            "exactly the wedged sheep is unresponsive, not its neighbour"
        );
    }
}

/// A report parks the pump that answers it, and only that one: a pump that
/// missed the deadline is still reading its streams. An abandoned handover
/// is audited by the resume counter, so a spurious resume reads as a
/// repaired pump forever after. Asserted by sending the resumes, so this
/// is a claim about which pump is told, not how many senders are held.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_pump_that_missed_the_deadline_is_not_in_the_parked_set() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(
        ScriptedRunner::new(vec![ProcScript::never_exits(); 2])
            .with_a_pump_that_never_reports(&["wedged"]),
    );
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    handle
        .start(vec![
            normalize(AppConfig::minimal("answering", "./srv")).unwrap(),
            normalize(AppConfig::minimal("wedged", "./srv")).unwrap(),
        ])
        .await
        .unwrap();

    let (_candidates, _blob, parked) =
        tokio::time::timeout(Duration::from_secs(3600), handle.handover_snapshot(fds))
            .await
            .expect("a snapshot over a wedged pump must answer rather than hang")
            .unwrap();
    parked.resume().await;

    // By name, because spawn order is the supervisor's business and
    // every counter here is indexed by it.
    let answering = runner.spawn_index_of("answering").expect("started above");
    let wedged = runner.spawn_index_of("wedged").expect("started above");
    // A resume carries no acknowledgement, so a send that has returned
    // has only been queued. Bounded, and instant under the paused clock.
    let delivered = async {
        while runner.resumes(answering) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), delivered)
        .await
        .expect("the pump that answered was parked, and is owed a resume");
    assert_eq!(
        runner.resumes(wedged),
        0,
        "a pump that never answered never parked, so nothing may resume it"
    );
}

/// Six wedged pumps is the size this test pins, and the bound it asserts
/// is about the shape rather than about any one budget: a serial sweep
/// scales with N and a concurrent one does not. Under the paused clock a
/// serial sweep reads six [`REPORT_DEADLINE`]s (12s) and a concurrent one
/// about one (2s); the bound below sits in that gap. Six was also where a
/// serial sweep first outlasted `shep-cli`'s `admin::KILL_TEARDOWN_WAIT`
/// while that was 10s, past which the client gives up first, falls back
/// to a predecessor still serving, and exits 0 before the gate refuses
/// for real. At 60s that crossing moved to thirty; the failure it names
/// did not go away, it got further off.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_sweep_of_six_wedged_pumps_costs_one_deadline_not_six() {
    const WEDGED: usize = 6;
    let names: Vec<String> = (0..WEDGED).map(|i| format!("wedged{i}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); WEDGED])
        .with_a_pump_that_never_reports(&name_refs);
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(
            names
                .iter()
                .map(|name| normalize(AppConfig::minimal(name, "./srv")).unwrap())
                .collect(),
        )
        .await
        .unwrap();

    let started_at = tokio::time::Instant::now();
    let (candidates, blob, _parked) =
        tokio::time::timeout(Duration::from_secs(3600), handle.handover_snapshot(fds))
            .await
            .expect("a snapshot over six wedged pumps must answer rather than hang")
            .unwrap();
    let elapsed = started_at.elapsed();

    assert!(
        elapsed < REPORT_DEADLINE * 2,
        "six wedged pumps swept concurrently should cost about one \
         REPORT_DEADLINE, not six; took {elapsed:?}"
    );

    assert_eq!(candidates.len(), WEDGED);
    for candidate in &candidates {
        assert!(
            candidate.pump_unresponsive,
            "{} must still be reported unresponsive; racing the reports \
             must not blur which pump answered",
            candidate.entry.spec.config().name
        );
    }
    // `join_all` returns in input order and `handle_handover_snapshot`
    // sorted `drafts` by id, so `spawn_handover_task` needs no re-sort.
    assert!(
        candidates.windows(2).all(|w| w[0].entry.id < w[1].entry.id),
        "candidates must stay in id order across a concurrent sweep"
    );
    assert!(
        blob.sheep().windows(2).all(|w| w[0].id() < w[1].id()),
        "the blob must stay in id order too: it is a file an operator may read"
    );
}

/// A successor that reissued a live id would collide with a caller still
/// holding it, and a manual command the successor never sees is an
/// operator's `stop` that comes back as a running sheep. The manual marker
/// is asserted whole, since kind and origin decide different things on the
/// far side. The sheep has no live pump, which is also the
/// registered-but-not-running case: no descriptors, and not a refusal.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_snapshot_carries_the_actors_counters_and_slot_state() {
    let dir = tempfile::tempdir().unwrap();
    let (fds, _held) = daemon_fds(&dir);
    let (mut actor, _ctl_rx) = actor_with_stopping_drainee(&dir, 4242, 7);
    actor.sheep.get_mut(&0).unwrap().manual = Some(PendingManual {
        kind: ManualKind::Stop,
        origin: CommandOrigin::Operator,
    });
    actor.sheep.get_mut(&0).unwrap().ready_failed = true;

    let (reply, rx) = oneshot::channel();
    actor.handle_handover_snapshot(fds, reply);
    let (candidates, blob, _parked) = rx.await.unwrap().unwrap();

    assert!(
        !candidates.is_empty(),
        "the flock must still reach the gate at all"
    );
    assert_eq!(
        blob.sheep()[0].manual(),
        Some(PendingManual {
            kind: ManualKind::Stop,
            origin: CommandOrigin::Operator,
        }),
        "a manual stop must reach the successor, kind and origin both"
    );
    assert_eq!(
        blob.sheep()[0].pending_delete(),
        Some(false),
        "nothing asked for this sheep to be deleted"
    );
    assert_eq!(
        blob.sheep()[0].ready_failed(),
        Some(true),
        "an earlier reload's failed verdict must reach the successor, or the rollback that \
         follows it has nothing left to replace"
    );
    assert!(
        blob.next_id() > 0,
        "a successor that reissues a live id collides"
    );
    assert_eq!(blob.sheep()[0].epoch(), 7, "a stale timer must stay stale");
    assert_eq!(
        blob.sheep()[0].fds(),
        CarriedFds::none(),
        "a sheep with no pump has no descriptors to carry"
    );
}
