//! The actor tier: that the engine really arms and disarms at the
//! transitions, not just that the registry can.

use super::*;

// `ProcessSelector::Name` matches a stopped sheep too, so a cron worker that
// keeps its schedule brings back a sheep the user stopped.
#[tokio::test(start_paused = true)]
async fn stopping_the_last_instance_stops_its_cron_worker() {
    let clock = Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z")));
    let h = harness_with_extras(vec![ProcScript::never_exits(); 12], |reports| Extras {
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        enforcer: Arc::new(RecordingEnforcer::default()),
        max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
        reports,
        stats: idle_stats(),
    });
    let mut rx = h.ctx.events.subscribe();
    h.ctx
        .supervisor
        .start(vec![app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })])
        .await
        .unwrap();

    cross_one_hour().await;
    expect_restart(&mut rx, "web", EVENT_WAIT).await;

    h.ctx
        .supervisor
        .stop(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the sheep stops");
    assert_no_restart_within(&mut rx, "web", PAST_THE_NEXT_OCCURRENCE).await;
    assert_eq!(h.ctx.supervisor.list().await[0].status, ProcStatus::Stopped);
}

/// Keyed by id and kind rather than by name: a swap puts two entries under
/// one name.
async fn expect_process_event(
    rx: &mut broadcast::Receiver<SharedEvent>,
    id: u32,
    kind: ProcessEventKind,
    window: Duration,
) {
    let wanted = async {
        loop {
            match rx.recv().await.map(|event| event.to_event()) {
                Ok(BusEvent::Process { event, info, .. }) if event == kind && info.id == id => {
                    return;
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(err) => panic!("event stream closed before {kind:?} for id {id}: {err}"),
            }
        }
    };
    if tokio::time::timeout(window, wanted).await.is_err() {
        panic!("timed out waiting for {kind:?} for id {id}");
    }
}

// The clock reading is the only observation out here: a rebuild re-spawns
// the cron worker, which reads the wall clock on its first poll.
// `max_cron_sleep` is 600s against a swap costing at most 11s of virtual
// time, so a surviving worker cannot wake inside the window and read too.
#[tokio::test(start_paused = true)]
async fn a_reload_leaves_the_name_groups_cron_worker_where_it_was() {
    let clock = Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z")));
    // Two procs, counted: the original and the one replacement a reload of
    // a one-instance app performs. A third is answered "script exhausted".
    let h = harness_with_extras(vec![ProcScript::never_exits(); 2], |reports| Extras {
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        enforcer: Arc::new(RecordingEnforcer::default()),
        max_cron_sleep: Duration::from_secs(600),
        reports,
        stats: idle_stats(),
    });
    let mut rx = h.ctx.events.subscribe();
    h.ctx
        .supervisor
        .start(vec![app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })])
        .await
        .unwrap();
    expect_process_event(&mut rx, 0, ProcessEventKind::Online, EVENT_WAIT).await;
    // Lets the armed worker reach its first poll before the count is taken.
    tokio::task::yield_now().await;

    let reads_before = clock.reads();
    assert_eq!(
        reads_before, 1,
        "fixture check: one armed cron worker takes exactly one reading, \
         so a rebuild's second one is a countable difference rather than \
         noise — and a count of 0 here would mean the worker had not run \
         yet, which would make the claim below vacuous"
    );

    h.ctx
        .supervisor
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_process_event(&mut rx, 1, ProcessEventKind::Online, EVENT_WAIT).await;
    expect_process_event(&mut rx, 0, ProcessEventKind::Delete, EVENT_WAIT).await;

    let listed = h.ctx.supervisor.list().await;
    assert_eq!(
        listed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![1],
        "fixture check: the swap must have run to completion, or there was \
         never an overlap for the ordering to matter in"
    );
    assert_eq!(
        clock.reads(),
        reads_before,
        "the name group's cron worker must have been left where it was: a \
         rebuilt one reads the clock again to derive its next occurrence"
    );
}

/// A harness whose runner hands out one proc that exits at once and then
/// plenty that never do, plus a cron-restarting app that parks in a long
/// backoff after any exit.
///
/// The two cases below need a sheep in `WaitingRestart`, the one state that
/// reaches a terminal transition through `apply_immediate`.
fn backoff_harness(clock: &Arc<TestClock>) -> Harness {
    harness_with_extras(
        {
            let mut scripts = vec![ProcScript::never_exits()];
            scripts.push(ProcScript::const_exit(1));
            // Spare procs so a broken implementation has something to
            // respawn from: without them the supervisor emits `Errored` and
            // the negative assertions below pass vacuously.
            scripts.extend([ProcScript::never_exits(); 8]);
            scripts
        },
        |reports| Extras {
            clock: Arc::clone(clock) as Arc<dyn Clock>,
            enforcer: Arc::new(RecordingEnforcer::default()),
            max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
            reports,
            stats: idle_stats(),
        },
    )
}

/// The cron-restarting app the two backoff cases start, parked in a backoff
/// far longer than either case's own window, so its pending `RestartDue` can
/// never be what a restart came from.
fn backoff_app() -> shep_core::config::ResolvedApp {
    app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.restart_delay = Some(UpDuration::from_millis(3 * 60 * 60 * 1_000));
    })
}

/// Each `list()` is a full round trip through the actor's mailbox, so this
/// makes progress rather than merely observing it.
async fn settle_into(supervisor: &SupervisorHandle, id: u32, status: ProcStatus) {
    for _ in 0..200 {
        tokio::task::yield_now().await;
        let listing = supervisor.list().await;
        if listing
            .iter()
            .any(|info| info.id == id && info.status == status)
        {
            return;
        }
    }
    panic!("id {id} never reached {status:?}");
}

// A sheep waiting out its restart backoff has no live task, so its stop
// never reaches `handle_exited`'s terminal branches, and
// `ProcessSelector::Name` matches a stopped sheep just as happily.
#[tokio::test(start_paused = true)]
async fn stopping_a_sheep_mid_backoff_still_stops_its_cron_worker() {
    let clock = Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z")));
    let h = backoff_harness(&clock);
    let mut rx = h.ctx.events.subscribe();
    h.ctx.supervisor.start(vec![backoff_app()]).await.unwrap();

    // The cron occurrence restarts it onto the immediately-exiting proc,
    // landing it in its three-hour backoff.
    cross_one_hour().await;
    expect_restart(&mut rx, "web", EVENT_WAIT).await;
    settle_into(&h.ctx.supervisor, 0, ProcStatus::WaitingRestart).await;

    h.ctx
        .supervisor
        .stop(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the sheep stops");
    assert_eq!(h.ctx.supervisor.list().await[0].status, ProcStatus::Stopped);
    // Spans the next occurrence, and stays well inside the three-hour
    // backoff, so a restart arriving here could only be the cron worker.
    assert_no_restart_within(&mut rx, "web", PAST_THE_NEXT_OCCURRENCE).await;
}

// Without a disarm the slot is deregistered while the name-group's cron
// worker keeps firing at a name nothing answers to.
#[tokio::test(start_paused = true)]
async fn deleting_a_sheep_mid_backoff_takes_its_cron_worker_with_it() {
    let clock = Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z")));
    let h = backoff_harness(&clock);
    let mut rx = h.ctx.events.subscribe();
    h.ctx.supervisor.start(vec![backoff_app()]).await.unwrap();

    cross_one_hour().await;
    expect_restart(&mut rx, "web", EVENT_WAIT).await;
    settle_into(&h.ctx.supervisor, 0, ProcStatus::WaitingRestart).await;

    h.ctx
        .supervisor
        .delete(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the sheep is deleted");
    assert!(h.ctx.supervisor.list().await.is_empty());
    // A surviving worker would `restart(Name("web"))`, find nothing, and log
    // at debug, emitting no bus event. The clock is the observable claim.
    let reads_after_delete = clock.reads();
    assert_no_restart_within(&mut rx, "web", PAST_THE_NEXT_OCCURRENCE).await;
    assert_eq!(
        clock.reads(),
        reads_after_delete,
        "a deleted sheep's cron worker must stop reading the clock, not merely stop finding sheep"
    );
}

// A cron occurrence reaches a group's instances through two doors: a running
// instance from `handle_exited`'s forced-restart branch, one sitting out its
// backoff from `apply_immediate`. Both must report `manually: false`.
#[tokio::test(start_paused = true)]
async fn a_cron_restart_is_never_reported_as_a_user_action() {
    let clock = Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z")));
    // The script pool makes the second half reachable: the first occurrence
    // respawns onto a proc that exits at once, and the spares behind it are
    // what the second respawns from.
    let h = backoff_harness(&clock);
    let mut rx = h.ctx.events.subscribe();
    h.ctx.supervisor.start(vec![backoff_app()]).await.unwrap();

    cross_one_hour().await;
    let (running, manually) = expect_restart_event(&mut rx, "web", EVENT_WAIT).await;
    assert_eq!(running.restarts, 1);
    assert!(
        !manually,
        "a cron occurrence is nobody typing `shep restart`"
    );

    // That respawn exited into a three-hour backoff, so the next occurrence
    // is well inside it and finds no live task.
    settle_into(&h.ctx.supervisor, 0, ProcStatus::WaitingRestart).await;
    cross_one_hour().await;
    let (backing_off, manually) = expect_restart_event(&mut rx, "web", EVENT_WAIT).await;
    assert_eq!(backing_off.restarts, 2);
    assert!(
        !manually,
        "the same occurrence must answer the same way through `apply_immediate`"
    );
}

// `respawn`'s Err arm is reachable in an ordinary deploy: a binary replaced
// mid-deploy, or a cwd that is gone. A failed respawn emits `Errored` and
// never `Restart`, so a surviving worker leaves only its clock readings.
#[tokio::test(start_paused = true)]
async fn a_respawn_that_cannot_spawn_stops_the_name_groups_cron_worker() {
    let clock = Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z")));
    // Exactly one script: the initial start consumes it, so the cron
    // occurrence's respawn finds it exhausted and `respawn` takes its Err
    // arm.
    let h = harness_with_extras(vec![ProcScript::never_exits()], |reports| Extras {
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        enforcer: Arc::new(RecordingEnforcer::default()),
        max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
        reports,
        stats: idle_stats(),
    });
    let mut rx = h.ctx.events.subscribe();
    h.ctx
        .supervisor
        .start(vec![app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })])
        .await
        .unwrap();

    cross_one_hour().await;
    settle_into(&h.ctx.supervisor, 0, ProcStatus::Errored).await;

    let reads_after_error = clock.reads();
    assert_no_restart_within(&mut rx, "web", PAST_THE_NEXT_OCCURRENCE).await;
    assert_eq!(
        clock.reads(),
        reads_after_error,
        "an errored sheep's cron worker must stop reading the clock"
    );
}

// The scripted table holds exactly one process, the pid the runner hands the
// first spawn, so an arming against any other number never breaches.
#[tokio::test(start_paused = true)]
async fn the_actor_arms_the_memory_limit_against_the_spawned_pid() {
    let mut h = harness_with_extras(vec![ProcScript::never_exits(); 4], |reports| {
        let sampler: Arc<dyn MemorySampler> =
            Arc::new(ScriptedSampler::new(vec![vec![ProcessRss {
                pid: 1000,
                parent: None,
                bytes: 900,
                cpu_ms: 0,
            }]]));
        let stats = Arc::new(StatsState::new(Arc::clone(&sampler)));
        Extras {
            clock: Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z"))),
            enforcer: Arc::new(PollingEnforcer::start(
                sampler,
                reports.breaches.clone(),
                Arc::clone(&stats),
            )),
            max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
            reports,
            stats,
        }
    });
    h.ctx
        .supervisor
        .start(vec![app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(500));
        })])
        .await
        .unwrap();
    let pid = h.ctx.supervisor.list().await[0]
        .pid
        .expect("a live sheep has a pid");

    let breach = match tokio::time::timeout(EVENT_WAIT, h.breaches.recv()).await {
        Ok(Some(breach)) => breach,
        Ok(None) => panic!("the breach channel closed before a breach arrived"),
        Err(_) => panic!("timed out waiting for a breach"),
    };

    assert_eq!(breach.id, 0);
    assert_eq!(
        breach.root_pid, pid,
        "the enforcer must be armed against the pid the sheep is actually running as"
    );
    assert_eq!(breach.observed.bytes(), 900);
}

// The gated one of `arm_extras`'s three transitions:
// `handle_ready_result`'s `went_online` reverted to a plain `emit` leaves
// every other case in this file green while every readiness-gated app loses
// all four extras. The readiness wait ends in a timeout, the same site.
#[tokio::test(start_paused = true)]
async fn the_actor_arms_a_readiness_gated_app_once_it_comes_online() {
    let mut h = harness_with_extras(vec![ProcScript::never_exits(); 4], |reports| {
        let sampler: Arc<dyn MemorySampler> =
            Arc::new(ScriptedSampler::new(vec![vec![ProcessRss {
                pid: 1000,
                parent: None,
                bytes: 900,
                cpu_ms: 0,
            }]]));
        let stats = Arc::new(StatsState::new(Arc::clone(&sampler)));
        Extras {
            clock: Arc::new(TestClock::starting_at(dt("2026-01-01T00:00:00Z"))),
            enforcer: Arc::new(PollingEnforcer::start(
                sampler,
                reports.breaches.clone(),
                Arc::clone(&stats),
            )),
            max_cron_sleep: DEFAULT_MAX_CRON_SLEEP,
            reports,
            stats,
        }
    });
    h.ctx
        .supervisor
        .start(vec![app_with("web", |app| {
            app.wait_ready = true;
            app.max_memory = Some(MemSize::from_bytes(500));
        })])
        .await
        .unwrap();
    let listing = h.ctx.supervisor.list().await;
    assert_eq!(
        listing[0].status,
        ProcStatus::Starting,
        "a gated app must not be Online when its Start reply lands"
    );
    let pid = listing[0].pid.expect("a spawned sheep has a pid");

    let breach = match tokio::time::timeout(EVENT_WAIT, h.breaches.recv()).await {
        Ok(Some(breach)) => breach,
        Ok(None) => panic!("the breach channel closed before a breach arrived"),
        Err(_) => panic!("timed out waiting for a breach"),
    };
    assert_eq!(breach.id, 0);
    assert_eq!(
        breach.root_pid, pid,
        "the gated path must arm against the pid the sheep is running as"
    );
}

// Real time and a real `OsProber`, because that is what the actor builds:
// the paused clock does not move a real TCP connect.
#[tokio::test]
async fn the_actor_arms_the_liveness_loop_against_the_spawned_pid() {
    // Reserve a port, then release it: nothing ever listens there, so every
    // probe fails with a connection refusal.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserved.local_addr().unwrap();
    drop(reserved);

    let mut h = harness(vec![ProcScript::never_exits(); 4]);
    h.ctx
        .supervisor
        .start(vec![app_with("web", |app| {
            app.liveness_probe = Some(ProbeConfig {
                failure_threshold: 1,
                interval: PROBE_INTERVAL,
                timeout: UpDuration::from_millis(500),
                ..probe_config(ProbeKind::Tcp, &addr.to_string())
            });
        })])
        .await
        .unwrap();
    let pid = h.ctx.supervisor.list().await[0]
        .pid
        .expect("a live sheep has a pid");

    let failure = expect_liveness(&mut h.liveness, LIVENESS_DEADLINE).await;
    assert_eq!(
        failure,
        LivenessReport {
            id: 0,
            pid,
            epoch: 1
        }
    );
}
