//! Tests for what a reload reports on the bus.
//!
//! A completed swap announces itself, an abandoned one says so, and a
//! replacement that is no longer serving announces nothing. The deadline cases
//! cover a stale timer that must not end the reload that followed it.

use super::*;

// The reply is an acceptance, so these frames are the whole of what a
// client learns. `Reload` names the drainee before the replacement's
// `Start` and carries `Stopping`; `Reloaded` lands only once the drainee's
// `Delete` has.
#[tokio::test(start_paused = true)]
async fn a_completed_swap_reports_itself_on_the_bus() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(); 3],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");

    let seen = events_through(&mut rx, 1, ProcessEventKind::Reloaded).await;

    assert!(
        at(&seen, 0, ProcessEventKind::Reload) < at(&seen, 1, ProcessEventKind::Start),
        "the instance being replaced is named before its replacement starts: {seen:?}"
    );
    assert_eq!(
        seen[at(&seen, 0, ProcessEventKind::Reload)],
        Seen {
            id: 0,
            kind: ProcessEventKind::Reload,
            status: ProcStatus::Stopping,
            manually: true,
        }
    );
    assert!(
        at(&seen, 0, ProcessEventKind::Delete) < at(&seen, 1, ProcessEventKind::Reloaded),
        "a swap is not over until the instance it replaced is gone: {seen:?}"
    );
    assert_eq!(
        seen[at(&seen, 1, ProcessEventKind::Reloaded)],
        Seen {
            id: 1,
            kind: ProcessEventKind::Reloaded,
            status: ProcStatus::Online,
            manually: true,
        }
    );
}

// A replacement that goes down inside the drain window keeps its row in the
// map, so a registration test passes and `Reloaded` names a process that is
// down. The drainee ignores its stop signal, so its drain runs the full
// 8000ms `graceful_timeout`; the replacement exits 5000ms in.
#[tokio::test(start_paused = true)]
async fn a_swap_is_not_announced_for_a_replacement_that_is_no_longer_serving() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.autorestart = false; // the replacement's exit is terminal, and registered
    let (handle, _runner, mut rx) = started(
        &dir,
        app,
        vec![
            ProcScript::ignores_signals(),
            ProcScript::stable_then_exit(5_000, 1),
        ],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");

    let seen = events_through(&mut rx, 1, ProcessEventKind::ReloadAbandoned).await;
    assert_eq!(
        seen[at(&seen, 1, ProcessEventKind::ReloadAbandoned)],
        Seen {
            id: 1,
            kind: ProcessEventKind::ReloadAbandoned,
            status: ProcStatus::Stopped,
            manually: true,
        },
        "a replacement that died inside the drain window is what the \
         abandonment names, carrying the status it actually reached"
    );
    assert!(
        !seen.iter().any(|e| e.kind == ProcessEventKind::Reloaded),
        "no swap succeeded, so nothing may say one did: {seen:?}"
    );
}

// Every transition out of a `ReloadJob` is driven by a `Msg::Exited` or a
// `Msg::ReadyResult`, and neither is guaranteed: `kill_process`'s
// post-`SIGKILL` `wait` is unbounded. `never_reports_its_exit` delivers and
// counts the `SIGKILL` and withholds only the exit.
#[tokio::test(start_paused = true)]
async fn a_swap_whose_drainee_never_reports_its_exit_gives_up_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![
            ProcScript::never_reports_its_exit(),
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");

    // The swap commits, the replacement is serving, and then stalls: the
    // drain's ladder runs to its `SIGKILL` and no exit ever follows it.
    let seen = events_through(&mut rx, 1, ProcessEventKind::ReloadAbandoned).await;
    assert_eq!(
        seen[at(&seen, 1, ProcessEventKind::ReloadAbandoned)],
        Seen {
            id: 1,
            kind: ProcessEventKind::ReloadAbandoned,
            status: ProcStatus::Online,
            manually: true,
        },
        "the replacement took the slot over and is what is left holding it"
    );
    assert_eq!(
        runner.kill_counts(),
        vec![1, 0],
        "the drain did reach `SIGKILL`; what never came back was the exit"
    );

    // The point of giving up: the verb works again without a daemon restart.
    let again = handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .map(|infos| infos.iter().map(|info| info.id).collect::<Vec<_>>());
    assert_eq!(
        again,
        Ok(vec![0, 1]),
        "a wedged instance must not refuse the app's next reload"
    );
}

// Staleness is read off the swap's `new_id`, since ids are never reused. A
// clustered app puts two swaps in one window: each drainee ignores its stop
// signal, so a swap runs its full 3000 + 8000ms, the first ends at 11000
// and its deadline comes home at 16000, by when the job is the second swap.
#[tokio::test(start_paused = true)]
async fn a_deadline_from_a_finished_swap_never_ends_the_one_that_followed_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    let (handle, _runner, mut rx) = started(
        &dir,
        app,
        vec![
            ProcScript::ignores_signals(),
            ProcScript::ignores_signals(),
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");

    expect_event(&mut rx, 2, ProcessEventKind::Reloaded).await;
    expect_event(&mut rx, 3, ProcessEventKind::Reloaded).await;

    let after = handle.list().await;
    assert_eq!(
        after.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![2, 3],
        "both instances were replaced: {after:?}"
    );
    assert!(after.iter().all(|info| info.status == ProcStatus::Online));
}

// Taking the committed ending instead drops the job and leaves the instance
// being replaced `Stopping` under a drain nothing started. Driven directly:
// every readiness task carries its own `listen_timeout` and always sends.
#[tokio::test(start_paused = true)]
async fn a_deadline_before_the_commit_puts_the_instance_being_replaced_back() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    // A live control sender says this instance's task is still there.
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    actor.sheep.get_mut(&0).expect("the fixture's sheep").ctl = Some(ctl_tx);

    let new_id = actor
        .spawn_replacement(0, ReloadMode::Overlap)
        .expect("the fixture's one script covers this spawn");
    actor.reloads.insert(
        "web".to_string(),
        ReloadJob {
            queue: VecDeque::new(),
            mode: ReloadMode::Overlap,
            deadline: 0,
            swap: ReloadSwap {
                old_id: 0,
                new_id: Some(new_id),
                phase: ReloadPhase::AwaitReady,
            },
        },
    );

    actor.handle_reload_deadline("web", actor.reloads["web"].deadline);

    assert!(
        actor.reloads.is_empty(),
        "the job is gone, so the app is reloadable again"
    );
    let drainee = &actor.sheep[&0];
    assert_eq!(
        drainee.entry.status,
        ProcStatus::Online,
        "nothing was ever killed, so the instance being replaced goes back to serving"
    );
    assert_eq!(drainee.entry.reload, ReloadState::None);
    assert_eq!(
        actor.sheep[&new_id].manual.map(|pending| pending.kind),
        Some(ManualKind::Delete),
        "the replacement that never proved itself is taken back down"
    );
}

// The reply was an acceptance, so a subscriber that hears a `Reload` and
// never hears again cannot tell a reload still running from one that gave
// up. `wait_ready` with nothing signalling the replacement is the
// abandonment reached here.
#[tokio::test(start_paused = true)]
async fn an_abandoned_reload_says_so_on_the_bus() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.wait_ready = true; // nobody ever signals the replacement
    let (handle, _runner, mut rx) = started(&dir, app, vec![ProcScript::never_exits(); 2]).await;
    handle.tx.send(Msg::Ready { id: 0 }).await.unwrap();
    expect_event(&mut rx, 0, ProcessEventKind::Online).await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");

    let seen = events_through(&mut rx, 0, ProcessEventKind::ReloadAbandoned).await;
    assert_eq!(
        seen[at(&seen, 0, ProcessEventKind::ReloadAbandoned)],
        Seen {
            id: 0,
            kind: ProcessEventKind::ReloadAbandoned,
            status: ProcStatus::Online,
            manually: true,
        },
        "the abandoned reload's own instance is still the one serving"
    );
}

// No replacement is registered, so there is no `Start` and no `Delete`
// unless `advance_reload`'s own failure arm says so. The exhausted pool is
// the injected failure.
#[tokio::test(start_paused = true)]
async fn a_reload_whose_replacement_cannot_spawn_says_so_on_the_bus() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits()],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted before anything is spawned");

    let seen = events_through(&mut rx, 0, ProcessEventKind::ReloadAbandoned).await;
    assert_eq!(
        seen[at(&seen, 0, ProcessEventKind::ReloadAbandoned)],
        Seen {
            id: 0,
            kind: ProcessEventKind::ReloadAbandoned,
            status: ProcStatus::Online,
            manually: true,
        },
        "a failed spawn leaves the instance it was replacing serving"
    );
    let after = handle.list().await;
    assert_eq!(after.len(), 1, "no replacement was registered: {after:?}");
    assert_eq!(after[0].id, 0);
}

/// Fails if `run_sheep` lets go of `ProcIo::log_ctl` while its sheep is
/// still running. The real runner's log pump ends with that sender, and the
/// read ends of the child's stdout and stderr close with the pump. Reads
/// the fake's control task, which ends exactly when a real pump would.
#[tokio::test(start_paused = true)]
async fn a_live_sheep_keeps_holding_its_log_control_sender() {
    // One script, and it must not exit: an exiting proc closes its own
    // control channel, which is indistinguishable from the dropped sender.
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let (proc, io) = runner.spawn(&log_ctl_spec()).unwrap();
    assert!(runner.log_ctl_live(0), "sanity: the fake starts it live");

    let (events, _rx) = crate::bus::test_bus(64);
    let (_ctl_tx, ctl_rx) = mpsc::channel(8);
    let (_signal_tx, signal_rx) = mpsc::channel(8);
    let (actor_tx, _actor_rx) = mpsc::channel(8);
    let app = normalize(AppConfig::minimal("svc", "./svc")).unwrap();
    tokio::spawn(run_sheep(
        7, proc, io, app, ctl_rx, signal_rx, events, actor_tx,
    ));

    // Yields rather than a clock advance: the failing path is ready work,
    // not a timer, and the proc under it must stay unexited.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(
        runner.log_ctl_live(0),
        "run_sheep dropped ProcIo::log_ctl while its sheep was still \
         running: against the real runner that closes the read ends of \
         the child's stdout and stderr"
    );
}

/// Fails if a sheep's log pump outlives its sheep task, in the one case the
/// sheep's own exit cannot end it: a lamb inheriting the child's pipes
/// holds both streams open, and [`SheepSlot::log_ctl`] keeps a control
/// sender. What reaps the pump is its `logs` receiver going away.
#[tokio::test(start_paused = true)]
async fn a_pump_is_reaped_when_its_sheep_ends_even_with_a_lamb_on_the_pipe() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::stable_then_exit(1_000, 0).with_a_lamb_holding_the_pipe(),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.autorestart = false;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert!(
        runner.log_ctl_live(0),
        "sanity: a running sheep has a live pump"
    );

    // `Stop`, not `Exit`: with `autorestart` off a clean exit is a clean
    // stop.
    await_event(&mut rx, 0, ProcessEventKind::Stop).await;
    assert_eq!(
        handle.list().await.len(),
        1,
        "sanity: the sheep is still registered, so its slot still holds a \
         clone of the control sender"
    );

    // A bounded poll: the pump ends on its own task's schedule.
    let reaped = tokio::time::timeout(Duration::from_secs(5), async {
        while runner.log_ctl_live(0) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        reaped.is_ok(),
        "the pump outlived its sheep task, holding both log files and \
         both pipe read ends open"
    );
}

/// Fails if [`SheepSlot::to_child`] outlives the process it was cloned for:
/// delete the clearing line in `handle_exited` and this reddens. The far
/// end is a writer task parked on `recv()`, so every sender being dropped
/// is the only thing that retires it. Asserts the task ending rather than
/// the field being clear.
#[tokio::test(start_paused = true)]
async fn a_writer_task_is_reaped_when_its_sheep_exits() {
    let dir = tempfile::tempdir().unwrap();
    // `autorestart` off so the exit is terminal and the slot stays registered.
    let mut app = AppConfig::minimal("web", "./srv");
    app.autorestart = false;
    // The writer task exists only for a sheep with a channel.
    app.channel = true;
    // Exits on its own rather than under a kill: a `Kill` can put a
    // `Shutdown` on this very channel, and the read below wants its end.
    let (handle, runner, mut rx) =
        started(&dir, app, vec![ProcScript::stable_then_exit(1_000, 0)]).await;
    let mut io = runner.io_handles(0);
    assert_eq!(
        io.to_child_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty),
        "sanity: a running sheep's channel is open and quiet, so the \
         `None` below is a close and not a channel that never opened"
    );

    // `Stop`, not `Exit`: with `autorestart` off a clean exit is a clean
    // stop.
    await_event(&mut rx, 0, ProcessEventKind::Stop).await;
    assert_eq!(
        handle.list().await.len(),
        1,
        "sanity: the sheep is still registered, so nothing but the \
         clearing can have let go of the slot's clone"
    );

    // Bounded, so a leak fails the case instead of hanging it.
    let reaped = tokio::time::timeout(Duration::from_secs(5), io.to_child_rx.recv()).await;
    assert_eq!(
        reaped.ok(),
        Some(None),
        "the writer task outlived its sheep, parked on `recv()` and \
         holding the daemon's end of the shepherd channel"
    );
}

/// Fails if a spawn failure goes back to naming neither the sheep nor the
/// path. An exact string: the message is the whole product here.
///
/// The `cwd` half is left to the end-to-end tier, which has a real one.
#[tokio::test(start_paused = true)]
async fn a_failed_spawn_names_the_sheep_and_the_path_it_tried() {
    let dir = tempfile::tempdir().unwrap();
    // An empty script pool, so `ScriptedRunner` refuses the first spawn.
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, Vec::new());

    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Start {
        apps: vec![normalize(AppConfig::minimal("api", "./api")).unwrap()],
        policy: BatchPolicy::AllOrNothing,
        gate: BTreeSet::new(),
        reply,
    });

    let err = answer
        .await
        .expect("the actor answers every Start")
        .expect_err("an empty script pool cannot spawn");
    assert_eq!(
        err.to_string(),
        "spawn failed: api: process spawn failed: script exhausted; tried `./api`"
    );
}

/// Fails if `config_drift` stops naming an edited field, names a field
/// nobody edited, reports an app the flock has never heard of, or lets a
/// value out. The last assertion is the other half of the contract: asking
/// must not apply the edit.
#[tokio::test(start_paused = true)]
async fn config_drift_names_an_edited_sheeps_fields_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);

    // Two fields edited, so a comparator stopping at the first difference
    // fails here. `env` is one of them, reported by name only.
    let mut edited = AppConfig::minimal("web", "./srv");
    edited.cwd = Some("/srv/new".to_string());
    edited
        .env
        .insert("DATABASE_URL".to_string(), "postgres://hunter2".to_string());
    // A name the flock does not have: `start` will register it, so it must
    // not appear in the answer.
    let unknown = AppConfig::minimal("api", "./api");

    let drift = actor.config_drift(&[normalize(edited).unwrap(), normalize(unknown).unwrap()]);

    assert_eq!(
        drift,
        vec![SheepDrift::new(
            "web",
            vec!["cwd".to_string(), "env".to_string()]
        )]
    );
    assert!(
        !format!("{drift:?}").contains("hunter2"),
        "a value must never travel with the field name that changed: {drift:?}"
    );
    assert_eq!(
        actor.sheep[&0].entry.spec.config().cwd,
        None,
        "asking which fields differ must not apply them"
    );
}

/// Fails if any of the three spawns stops putting its [`ProcIo::to_child`]
/// clone on the slot, the handle the actor reaches a live child through.
/// Nothing yet reads the field, so a spawn that stopped taking its clone
/// changes no behaviour any other case can see.
#[tokio::test(start_paused = true)]
async fn every_spawn_leaves_the_daemons_end_of_the_channel_on_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    // Three scripts, none of which exits: a proc that went on its own would
    // put a second, unasked-for `Msg::Exited` in play.
    let (mut actor, _mailbox) = actor_with_one_online_sheep(
        &dir,
        vec![
            ProcScript::never_exits(),
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ],
    );

    // `spawn_fresh`, the door every sheep arrives by, registering id 1
    // beside the fixture's own. `autorestart` off so the entry stays.
    let mut app = AppConfig::minimal("api", "./api");
    app.autorestart = false;
    let (reply, _answer) = oneshot::channel();
    actor.handle_command(Command::Start {
        apps: vec![normalize(app).unwrap()],
        policy: BatchPolicy::AllOrNothing,
        gate: BTreeSet::new(),
        reply,
    });
    assert!(
        actor.sheep[&1].to_child.is_some(),
        "a fresh spawn's slot holds the daemon's end of its shepherd channel"
    );

    actor.handle_exited(
        1,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );
    assert!(
        actor.sheep[&1].to_child.is_none(),
        "the clone goes with the process it was cloned for"
    );

    // `respawn`, the door a crash loop and a manual restart come back
    // through. A new process under the same id needs a new handle.
    actor.respawn(1, false);
    assert!(
        actor.sheep[&1].to_child.is_some(),
        "a respawn hands the slot the new process's channel, not the dead \
         one's"
    );

    // `spawn_replacement`, the reload's door: a new id in the drainee's
    // instance slot needs a handle of its own.
    actor.advance_reload("web", VecDeque::from([0]));
    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert!(
        actor.sheep[&new_id].to_child.is_some(),
        "a reload's replacement holds the daemon's end of its own channel"
    );
}
