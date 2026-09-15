//! A property test over interleaved commands.
//!
//! The supervisor's invariants have to hold whatever order commands and exits
//! arrive in, which is more orderings than a hand-written test can cover. This
//! generates them.

use super::*;

#[derive(Debug, Clone, Copy)]
enum Step {
    List,
    StopAll,
    RestartAll,
    DeleteFirst,
    StartOne,
    /// A memory breach or a liveness failure raised against the pid the
    /// first listed sheep is running right now.
    Report,
    /// The same report, raised against a pid that sheep does not have.
    StaleReport,
}

fn step_strategy() -> impl proptest::strategy::Strategy<Value = Step> {
    proptest::prop_oneof![
        proptest::strategy::Just(Step::List),
        proptest::strategy::Just(Step::StopAll),
        proptest::strategy::Just(Step::RestartAll),
        proptest::strategy::Just(Step::DeleteFirst),
        proptest::strategy::Just(Step::StartOne),
        proptest::strategy::Just(Step::Report),
        proptest::strategy::Just(Step::StaleReport),
    ]
}

/// How a generated app gates its own `starting -> online` transition.
#[derive(Debug, Clone, Copy)]
enum Gate {
    /// Neither `wait_ready` nor `readiness_probe`: `spawn_fresh` marks the
    /// sheep `Online` inline.
    Ungated,
    /// `wait_ready = true` with this `listen_timeout` in milliseconds. No
    /// scripted child writes `{"kind":"ready"}`, so every wait ends at its
    /// deadline, and the deadline decides which later step lands under it.
    Channel(u64),
}

fn gate_strategy() -> impl proptest::strategy::Strategy<Value = Gate> {
    use proptest::strategy::Strategy as _; // `prop_map` below
    proptest::prop_oneof![
        2 => proptest::strategy::Just(Gate::Ungated),
        // Spans the driver's own durations, a 1600ms kill ladder and a
        // 2000ms `stable_then_exit`, so a deadline drawn here lands
        // before, during and after them.
        1 => (1u64..4_000u64).prop_map(Gate::Channel),
    ]
}

fn script_strategy() -> impl proptest::strategy::Strategy<Value = ProcScript> {
    // Weighted toward long-lived children so a run explores command
    // handling rather than only exhausting the restart budget.
    proptest::prop_oneof![
        6 => proptest::strategy::Just(ProcScript::never_exits()),
        2 => proptest::strategy::Just(ProcScript::const_exit(1)),
        1 => proptest::strategy::Just(ProcScript::stable_then_exit(2_000, 0)),
        1 => proptest::strategy::Just(ProcScript::ignores_signals()),
    ]
}

/// How many scripted procs one generated case may spawn.
///
/// A 9-command run reaches at most 30 command-driven spawns plus
/// crash-loop respawns capped at 16 per sheep: under 200 all told. An
/// exhausted `ScriptedRunner` answers `SpawnFailed`, which the actor turns
/// into `Errored`, so a claim about a restart that must not happen would
/// pass for the wrong reason. Still finite: a `stable_then_exit` script
/// resets the restart budget, and the pool running dry ends the chain.
const SCRIPT_POOL: usize = 512;

/// How long the steady-state drain waits for one more transition before
/// concluding there are none left.
///
/// Longer than every deadline a run can leave pending (a 4000ms readiness
/// wait, a 1600ms kill ladder, a 2000ms `stable_then_exit` script) and far
/// shorter than `fake::NEVER_MS`, so a `never_exits` proc stays alive
/// across it.
const QUIET_WINDOW: Duration = Duration::from_secs(60);

/// Ceiling on transitions observed after the last command. Each spawn
/// produces at most a start/restart, an online and a terminal event, so
/// `3 * SCRIPT_POOL` bounds a correct run; anything past this ceiling is
/// a flock that never settles.
const EVENT_BUDGET: usize = 3 * SCRIPT_POOL;

proptest::proptest! {
    // 128 cases: 24 misses the 3-step `[StartOne, StopAll, DeleteFirst]`
    // sequence that equally-weighted draws rarely land. ~0.6s under the
    // paused clock. `PROPTEST_CASES` overrides.
    #![proptest_config(crate::testing::proptest_config(128))]

    #[test]
    fn supervisor_upholds_its_invariants_under_any_interleaving(
        steps in proptest::collection::vec(step_strategy(), 1..10),
        gates in proptest::collection::vec(gate_strategy(), 1..10),
        scripts in proptest::collection::vec(script_strategy(), SCRIPT_POOL..SCRIPT_POOL + 1),
    ) {
        // Paused clock: every backoff, kill ladder and readiness delay is
        // virtual, so a 128-case run stays cheap whatever it draws.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        runtime.block_on(async move {
            // Capacity above `EVENT_BUDGET`: the drain below treats a
            // `Lagged` as a failure rather than skipping past it, since a
            // hole in the stream is a hole in every claim read off it.
            let (events, mut rx) = crate::bus::test_bus(8192);
            let handle = spawn_supervisor(
                ScriptedRunner::new(scripts),
                test_paths(&dir),
                events,
            );
            let mut started = 0u32;
            let mut highest_restarts = std::collections::HashMap::<u32, u32>::new();
            // `extra_restart` is the one command with no reply, so it is
            // the only way a restart is still mid-kill-ladder when the next
            // step is issued. `Step::StopAll` keeps its claim across that:
            // `claim_manual` takes the `manual` marker off it.

            for step in steps {
                match step {
                    Step::StartOne => {
                        let gate = gates[started as usize % gates.len()];
                        started += 1;
                        let mut app = AppConfig::minimal(&format!("sheep-{started}"), "./s");
                        if let Gate::Channel(ms) = gate {
                            app.wait_ready = true;
                            app.listen_timeout = UpDuration::from_millis(ms);
                        }
                        let _ = handle.start(vec![normalize(app).unwrap()]).await;
                    }
                    Step::StopAll => {
                        if let Ok(stopped) = handle.stop(ProcessSelector::All).await {
                            for info in stopped {
                                // A deferred reply means every match is
                                // terminal.
                                proptest::prop_assert_eq!(info.status, ProcStatus::Stopped);
                            }
                        }
                    }
                    Step::RestartAll => {
                        let _ = handle.restart(ProcessSelector::All).await;
                    }
                    Step::DeleteFirst => {
                        if let Some(first) = handle.list().await.first() {
                            let id = first.id;
                            if let Ok(deleted) = handle.delete(ProcessSelector::Id(id)).await {
                                proptest::prop_assert_eq!(deleted, vec![id]);
                            }
                            proptest::prop_assert!(
                                handle.list().await.iter().all(|i| i.id != id)
                            );
                        }
                    }
                    Step::Report => {
                        if let Some(first) = handle.list().await.first()
                            && let Some(pid) = first.pid
                        {
                            handle.extra_restart(first.id, pid, None, None).await;
                        }
                    }
                    Step::StaleReport => {
                        if let Some(first) = handle.list().await.first() {
                            // Never this sheep's own pid. A pid belonging
                            // to another sheep is just as stale: the guard
                            // compares against this id's entry.
                            let stale = first.pid.unwrap_or(0).wrapping_add(1);
                            handle.extra_restart(first.id, stale, None, None).await;
                        }
                    }
                    Step::List => {}
                }

                let listed = handle.list().await;
                // (1) ids are unique and the listing is sorted by id.
                let ids: Vec<u32> = listed.iter().map(|i| i.id).collect();
                let mut sorted = ids.clone();
                sorted.sort_unstable();
                sorted.dedup();
                proptest::prop_assert_eq!(&ids, &sorted);
                for info in &listed {
                    // (2) restart counts never decrease for a given id.
                    let seen = highest_restarts.entry(info.id).or_default();
                    proptest::prop_assert!(info.restarts >= *seen);
                    *seen = info.restarts;
                    // (3) no status outside the spec's set ever surfaces.
                    proptest::prop_assert!(matches!(
                        info.status,
                        ProcStatus::Starting | ProcStatus::Online | ProcStatus::Stopping
                            | ProcStatus::Stopped | ProcStatus::Errored | ProcStatus::WaitingRestart
                    ));
                }
            }

            // (4) steady state: with no further commands, the flock stops
            // transitioning. A bounded window, not `try_recv`: a run ends
            // with deadlines still pending, and the window walks the
            // paused clock over them.
            let mut observed = Vec::new();
            loop {
                match tokio::time::timeout(QUIET_WINDOW, rx.recv()).await {
                    Ok(Ok(event)) => {
                        observed.push(event.to_event());
                        proptest::prop_assert!(
                            observed.len() <= EVENT_BUDGET,
                            "the flock never reached steady state: {} transitions after \
                             the last command",
                            observed.len()
                        );
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped))) => {
                        return Err(proptest::test_runner::TestCaseError::fail(format!(
                            "event stream lagged by {skipped}: the invariants below cannot \
                             be read off a stream with holes in it"
                        )));
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                    Err(_elapsed) => break, // nothing left to transition
                }
            }

            // (5) never two live processes for one id, and never an
            // `Online` for an id with no live process. `spawn_fresh` never
            // reuses an id, and `handle_ready_result` resolves long after
            // the spawn it belongs to.
            let mut live = std::collections::HashSet::<u32>::new();
            let mut event_restarts = std::collections::HashMap::<u32, u32>::new();
            for event in observed {
                let BusEvent::Process { event, info, .. } = event else {
                    // LogOut/LogErr carry no lifecycle transition.
                    continue;
                };
                match event {
                    ProcessEventKind::Start => {
                        proptest::prop_assert!(
                            live.insert(info.id),
                            "two live spawns for id {}",
                            info.id
                        );
                    }
                    // One out and one in: the predecessor's `Msg::Exited`
                    // is what caused the respawn, so the id stays live.
                    ProcessEventKind::Restart => {
                        live.insert(info.id);
                    }
                    ProcessEventKind::Online => {
                        proptest::prop_assert!(
                            live.contains(&info.id),
                            "id {} was marked online with no live process: a readiness \
                             wait resolved onto a sheep that had already gone terminal",
                            info.id
                        );
                    }
                    ProcessEventKind::Exit
                    | ProcessEventKind::Stop
                    | ProcessEventKind::Errored
                    | ProcessEventKind::Delete => {
                        live.remove(&info.id);
                    }
                    // `ProcessEventKind` is `#[non_exhaustive]`, so E0004
                    // never fires here. Leaving `live` untouched for a
                    // later variant only makes the assertions stricter.
                    _ => {}
                }
                // (2) again, off the event stream rather than `list()`: a
                // snapshot only sees the counter between two commands.
                let seen = event_restarts.entry(info.id).or_default();
                proptest::prop_assert!(
                    info.restarts >= *seen,
                    "restart count for id {} went backwards: {} after {}",
                    info.id,
                    info.restarts,
                    *seen
                );
                *seen = info.restarts;
            }
            // The async block's error type is proptest's, so `?` above and
            // this tail agree.
            Ok::<(), proptest::test_runner::TestCaseError>(())
        })?;
    }
}
