//! Tests for a reload replacing one instance.
//!
//! The replacement gets a new id in the drainee's slot, and the two halves of
//! the swap sit on the entries that own them. What a replacement does before it
//! is ready decides whether the old instance keeps serving.

use super::*;

/// A [`ScriptedRunner`] that refuses one spawn by ordinal and forwards
/// every other one.
///
/// `ScriptedRunner` can only fail by running out of scripts, which fails
/// every spawn from then on. Refusing exactly one tells a correct reload,
/// which stops there, from a broken one, which spawns a second replacement.
struct RefusesOneSpawn {
    inner: ScriptedRunner,
    /// Which spawn, counting from the engine's first, is refused.
    refuse: usize,
    /// Every spawn attempted, refused ones included, which
    /// `ScriptedRunner`'s own counters cannot report.
    attempts: Arc<AtomicUsize>,
}

impl fmt::Debug for RefusesOneSpawn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusesOneSpawn").finish_non_exhaustive()
    }
}

impl ProcessRunner for RefusesOneSpawn {
    type Proc = crate::fake::FakeProc;

    fn spawn(
        &self,
        spec: &SpawnSpec,
    ) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
        let nth = self.attempts.fetch_add(1, AtomicOrdering::SeqCst);
        if nth == self.refuse {
            return Err(crate::runner::RunnerError::SpawnFailed(
                "refused by the fixture".to_string(),
            ));
        }
        self.inner.spawn(spec)
    }
}

// `Drainee` names the replacement and belongs on the instance being
// replaced; `Replacement` belongs on the replacement and names nothing;
// `Stopping` is the drainee's status alone. `ProcessEntry::reload` never
// reaches the wire, so this is the only tier that can read it back.
#[tokio::test(start_paused = true)]
async fn a_swap_puts_each_half_of_a_reload_on_the_entry_that_owns_it() {
    let dir = tempfile::tempdir().unwrap();
    // One script for the one spawn a correct `SpawnNew` performs.
    let (mut actor, _mailbox) =
        actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);

    let new_id = actor
        .spawn_replacement(0, ReloadMode::Overlap)
        .expect("the fixture's one script covers this spawn");

    assert_ne!(new_id, 0, "a replacement never reuses the drainee's id");
    let drainee = &actor.sheep[&0].entry;
    let replacement = &actor.sheep[&new_id].entry;

    assert_eq!(drainee.status, ProcStatus::Stopping);
    assert_eq!(
        drainee.reload,
        ReloadState::Drainee {
            new_id: Some(new_id)
        }
    );
    assert_ne!(
        replacement.status,
        ProcStatus::Stopping,
        "`Stopping` belongs to the instance going away, not the one arriving"
    );
    assert_eq!(replacement.status, ProcStatus::Starting);
    assert_eq!(replacement.reload, ReloadState::Replacement);
    assert_eq!(
        replacement.instance, drainee.instance,
        "a replacement takes the drainee's instance slot, or an app deriving \
         its port from it binds a different one and nothing overlaps"
    );
}

// A reload's replacement would be a child outside the shutdown
// aggregation's `online` snapshot, and so orphaned when the actor exits.
// Two guards stand between a shutdown and that child: the reply witnesses
// `Command::Reload`'s, and the `advance_reload` call below reaches the other.
#[tokio::test(start_paused = true)]
async fn a_reload_is_refused_once_a_shutdown_has_begun() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) =
        actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    actor.shutting_down = true;

    let (reply, rx) = oneshot::channel();
    actor.handle_command(Command::Reload {
        selector: ProcessSelector::All,
        reply,
    });
    assert_eq!(rx.await, Ok(Err(SupervisorError::EngineStopped)));

    // `advance_reload` is the one door into `SpawnNew`.
    actor.advance_reload("web", VecDeque::from([0]));

    assert_eq!(actor.sheep.len(), 1, "nothing new was registered");
    assert_eq!(actor.sheep[&0].entry.status, ProcStatus::Online);
    assert_eq!(actor.sheep[&0].entry.reload, ReloadState::None);
    assert!(actor.reloads.is_empty(), "no job was started");
}

// A replacement takes a new id in the drainee's instance slot, of which the
// log paths are the observable half. The drainee's registration goes with
// it: nothing else removes it, so the flock would grow a dead row per
// instance per reload.
#[tokio::test(start_paused = true)]
async fn a_reload_gives_the_replacement_a_new_id_in_the_drainees_slot() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;
    let before = handle.list().await;

    let accepted = handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    assert_eq!(
        accepted.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0],
        "the answer is the flock as it stood when the reload was accepted"
    );

    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;

    let after = handle.list().await;
    assert_eq!(after.len(), 1, "the drainee's registration goes with it");
    assert_eq!(after[0].id, 1);
    assert_eq!(after[0].status, ProcStatus::Online);
    assert_eq!(after[0].out_file, before[0].out_file);
    assert_eq!(after[0].err_file, before[0].err_file);
    assert_eq!(
        runner.kill_counts().len(),
        2,
        "one original and one replacement, and nothing else"
    );
}

// Marking the drainee `Stopping` only when its drain starts leaves the app
// two entries that `snapshot.rs`'s `is_running` counts for the whole
// `AwaitReady` window, so a muster roll written during a reload records an
// instance count the flock does not have.
#[tokio::test(start_paused = true)]
async fn a_reload_stops_counting_the_drainee_as_running_before_its_replacement_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    let mid = handle.list().await;
    assert_eq!(mid.len(), 2, "both entries are registered mid-swap");
    assert_eq!(mid[0].status, ProcStatus::Stopping, "the drainee");
    assert_eq!(mid[1].status, ProcStatus::Starting, "its replacement");
    let running = mid
        .iter()
        .filter(|info| {
            matches!(
                info.status,
                ProcStatus::Online | ProcStatus::Starting | ProcStatus::WaitingRestart
            )
        })
        .count();
    assert_eq!(
        running, 1,
        "a one-instance app must never count as two running instances"
    );
}

// `await_ready`'s `Heuristic` arm returns `Ready` at the deadline, since for
// an app configuring neither `wait_ready` nor `readiness_probe` the elapse
// is the signal. An implementation keyed on the deadline instead abandons
// every reload of every such app.
#[tokio::test(start_paused = true)]
async fn a_reload_of_an_app_with_no_readiness_signal_completes_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    // Not the 3000ms default: a distinctive value says which wait ran.
    app.listen_timeout = UpDuration::from_millis(2_500);
    let (handle, _runner, mut rx) = started(
        &dir,
        app,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    let start = tokio::time::Instant::now();
    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    assert_eq!(
        tokio::time::Instant::now() - start,
        Duration::from_millis(2_500)
    );

    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;
    let after = handle.list().await;
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, 1);
    assert_eq!(after[0].status, ProcStatus::Online);
}

// A swap that ignored the shepherd channel's ready signal would sit out the
// whole `listen_timeout` before committing.
#[tokio::test(start_paused = true)]
async fn a_reload_commits_the_moment_the_replacement_signals_ready() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true;
    let (handle, _runner, mut rx) = started(
        &dir,
        app,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;
    // The original is gated too, so it needs its own signal before it is
    // `Online` and therefore reloadable.
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    expect_event(&mut rx, 0, ProcessEventKind::Online).await;

    let start = tokio::time::Instant::now();
    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;
    handle.tx.send(Msg::Ready { id: 1 }).await.unwrap();
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;

    assert!(
        tokio::time::Instant::now() - start < Duration::from_millis(3_000),
        "the swap committed at the signal, not at `listen_timeout`"
    );
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;
    assert_eq!(handle.list().await[0].id, 1);
}

// The ordinary readiness rule takes a slow app online rather than looping
// it, and a reload must not inherit that: committing to an instance that has
// not proved it can serve means killing the one that can. The abandoned
// replacement got far enough to fork lambs, so it goes through the ladder.
#[tokio::test(start_paused = true)]
async fn a_replacement_that_never_becomes_ready_leaves_the_old_instance_serving() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals the replacement
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
    expect_event(&mut rx, 1, ProcessEventKind::Delete).await;

    let after = handle.list().await;
    assert_eq!(after.len(), 1, "the abandoned replacement is deregistered");
    assert_eq!(after[0].id, 0);
    assert_eq!(
        after[0].status,
        ProcStatus::Online,
        "the instance that can serve keeps serving"
    );
    assert_eq!(
        runner.signals(1),
        vec![15],
        "the replacement went through the stop ladder rather than being dropped"
    );
    assert_eq!(runner.kill_counts(), vec![0, 0], "neither needed SIGKILL");
}

// The drain runs under `graceful_timeout`, not `kill_timeout`: 8000ms
// against 1600ms by default.
#[tokio::test(start_paused = true)]
async fn a_reload_drains_the_old_instance_under_graceful_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        // The drainee ignores its stop signal: the elapsed time is the cap.
        vec![ProcScript::ignores_signals(), ProcScript::never_exits()],
    )
    .await;

    let start = tokio::time::Instant::now();
    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Online).await;
    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;

    assert_eq!(
        tokio::time::Instant::now() - start,
        // listen_timeout (3000) + graceful_timeout (8000)
        Duration::from_millis(11_000)
    );
    assert_eq!(
        runner.kill_counts(),
        vec![1, 0],
        "only the defiant drainee reached the SIGKILL rung"
    );
}

// Starting every swap at once would leave a clustered app entirely
// `Stopping` for the whole window, with nothing holding the old listeners.
#[tokio::test(start_paused = true)]
async fn a_reload_replaces_a_clustered_apps_instances_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    // Four spawns: two originals and two replacements. Starting both swaps
    // together performs the same four, so only the timing tells them apart.
    let (handle, runner, mut rx) = started(&dir, app, vec![ProcScript::never_exits(); 4]).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");

    expect_event(&mut rx, 2, ProcessEventKind::Start).await;
    assert_eq!(
        runner.kill_counts().len(),
        3,
        "only the first instance's replacement exists yet"
    );
    assert_eq!(
        handle
            .list()
            .await
            .iter()
            .filter(|info| info.status == ProcStatus::Stopping)
            .count(),
        1,
        "one instance is being replaced, and the other is untouched"
    );

    expect_event(&mut rx, 0, ProcessEventKind::Delete).await;
    expect_event(&mut rx, 3, ProcessEventKind::Start).await;
    assert_eq!(runner.kill_counts().len(), 4);
    expect_event(&mut rx, 1, ProcessEventKind::Delete).await;

    let after = handle.list().await;
    assert_eq!(
        after.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(after.iter().all(|info| info.status == ProcStatus::Online));
}

// Failure of the new instance aborts the rest and keeps the old instances
// running. The drainee must not be left `Stopping` after the spawn that was
// to replace it never happened, which takes it out of the muster roll and
// out of reach of a liveness restart.
#[tokio::test(start_paused = true)]
async fn a_replacement_that_cannot_be_spawned_leaves_every_instance_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    // Four scripts, of which a correct run uses two. The fixture refuses the
    // third; the fourth is for the spawn a reload carrying on would take.
    let attempts = Arc::new(AtomicUsize::new(0));
    let runner = RefusesOneSpawn {
        inner: ScriptedRunner::new(vec![ProcScript::never_exits(); 4]),
        refuse: 2,
        attempts: Arc::clone(&attempts),
    };
    let (events, mut rx) = crate::bus::test_bus(256);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted before anything is spawned");

    // A reload that carried on would spawn instance 1's replacement under
    // id 3.
    assert_no_event_within(&mut rx, 3, ProcessEventKind::Start, Duration::from_secs(10)).await;

    let after = handle.list().await;
    assert_eq!(
        after.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(
        after.iter().all(|info| info.status == ProcStatus::Online),
        "both old instances keep serving: {after:?}"
    );
    assert_eq!(
        attempts.load(AtomicOrdering::SeqCst),
        3,
        "two originals and one refused replacement, and no second attempt"
    );
}

// The app is clustered so an acceptance has somewhere to go: with instance 0
// mid-swap, a second reload finds instance 1 still `Online` and
// `advance_reload`'s insert overwrites the first job, whose drainee is never
// reaped. A single-instance fixture shows none of that.
#[tokio::test(start_paused = true)]
async fn a_second_reload_of_an_app_already_reloading_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    // Four scripts, of which a correct run uses three. The fourth lets a
    // wrongly-accepted second reload succeed into a live entry rather than
    // hide behind an exhausted pool.
    let (handle, runner, mut rx) = started(&dir, app, vec![ProcScript::never_exits(); 4]).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the first reload is accepted");
    expect_event(&mut rx, 2, ProcessEventKind::Start).await;

    let refused = handle.reload(ProcessSelector::All).await;

    assert_eq!(
        refused,
        Err(SupervisorError::ReloadInFlight("web".to_string()))
    );
    assert_eq!(
        runner.kill_counts().len(),
        3,
        "no second replacement was spawned"
    );
    assert_eq!(
        handle.list().await.len(),
        3,
        "one drainee, its replacement, and the instance untouched so far"
    );
}

// Reported as a success, so one stopped sheep does not fail a reload of
// the rest.
#[tokio::test(start_paused = true)]
async fn a_reload_of_a_sheep_that_is_not_online_is_a_no_op_success() {
    let dir = tempfile::tempdir().unwrap();
    // One script: a correct reload spawns nothing at all here.
    let (handle, runner, _rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits()],
    )
    .await;
    handle.stop(ProcessSelector::All).await.unwrap();

    let reloaded = handle
        .reload(ProcessSelector::All)
        .await
        .expect("a reload that has nothing to replace still succeeds");

    assert_eq!(reloaded.len(), 1);
    assert_eq!(reloaded[0].status, ProcStatus::Stopped);
    assert_eq!(handle.list().await.len(), 1, "nothing was registered");
    assert_eq!(runner.kill_counts().len(), 1, "nothing was spawned");
}

// A `claim_manual` ladder leaves the status `Online`, so a swap started
// against one is abandoned by that ladder's exit, which kills the
// replacement and warns of an operator command nobody issued.
#[tokio::test(start_paused = true)]
async fn a_reload_skips_an_instance_whose_kill_ladder_is_already_running() {
    let dir = tempfile::tempdir().unwrap();
    // The original defies its signal, so the breach's ladder is still
    // running when the reload lands.
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::ignores_signals(), ProcScript::never_exits()],
    )
    .await;
    let pid = handle.list().await[0].pid.expect("a live sheep has a pid");
    handle.extra_restart(0, pid, None, None).await;

    let reloaded = handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("a reload with nothing left to replace still succeeds");

    assert_eq!(reloaded.len(), 1, "the reload still answers for the match");
    assert_eq!(
        handle.list().await.len(),
        1,
        "no replacement was registered against an instance on its way out"
    );
    assert_eq!(runner.kill_counts().len(), 1, "nothing was spawned");

    expect_event(&mut rx, 0, ProcessEventKind::Restart).await;
    let after = handle.list().await;
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, 0);
    assert_eq!(
        runner.kill_counts().len(),
        2,
        "the original and its respawn"
    );
}
