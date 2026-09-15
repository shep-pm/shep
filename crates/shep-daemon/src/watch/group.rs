use super::filter::{RootedFilter, WatchFilter};
use crate::supervisor::{SupervisorError, SupervisorHandle};
use crate::watch::source::{WatchBatch, WatchError, watch_tree};
use core::time::Duration;
use shep_core::selector::ProcessSelector;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Debounce window when an app sets no `watch_delay`.
///
/// Long enough to coalesce the multi-event burst a single editor save
/// produces (write to a temp file, rename over the target, chmod), short
/// enough that a save-to-restart round trip still feels immediate.
pub(crate) const DEFAULT_WATCH_DELAY: Duration = Duration::from_millis(500);

/// Floor the watch arming enforces on an app's own `watch_delay`.
///
/// Nothing upstream stops a caller from handing [`spawn_watch_group`] a
/// zero, and `notify-debouncer-full` derives its poll tick as `delay / 4`:
/// at zero that spins the debouncer thread in a tight sleep loop.
///
/// One millisecond, not the full second `cron::MIN_MAX_SLEEP`
/// uses: this is a debounce, not a polling period, so a floor that high
/// would noticeably lengthen a save-to-restart round trip.
pub(crate) const MIN_WATCH_DELAY: Duration = Duration::from_millis(1);

/// Runs one name-group's watch until the returned handle is aborted.
///
/// `root` must already be canonicalized, from the app's own `cwd`. Must run
/// inside a Tokio runtime: it spawns the group loop immediately.
///
/// A triggering change restarts every instance of the name; the last instance
/// stopping disarms the group. A rescan restarts regardless of `watch_options`/`ignore_watch`.
///
/// # Errors
///
/// - [`WatchError::Backend`]: notify could not create a watcher.
/// - [`WatchError::Watch`]: notify could not watch `root`.
pub fn spawn_watch_group(
    name: String,
    root: PathBuf,
    filter: WatchFilter,
    delay: Duration,
    supervisor: SupervisorHandle,
) -> Result<tokio::task::JoinHandle<()>, WatchError> {
    let (source, rx) = watch_tree(&root, delay)?;
    let filter = RootedFilter { root, filter };
    let handle = tokio::spawn(async move {
        // The guard lives in the task, not in this function: aborting the
        // handle drops the future and therefore the guard, which is what
        // stops the OS watch, not just the loop.
        let _source = source;
        run_group(name, filter, rx, supervisor).await;
    });
    Ok(handle)
}

/// The group loop: filters each debounced batch and single-flights a
/// group-wide restart through [`SupervisorHandle::restart_automatic`](crate::supervisor::SupervisorHandle::restart_automatic), so
/// an operator's `stop` racing a watch-triggered restart wins rather than
/// being converted into it.
///
/// No dirty flag: the channel's own buffering is the re-check mechanism.
/// A batch arriving mid-restart stays queued, and the next iteration
/// drains whatever accumulated into one combined check, so a backlog
/// produces one restart, not one per queued send.
pub(super) async fn run_group(
    name: String,
    filter: RootedFilter,
    mut rx: mpsc::UnboundedReceiver<WatchBatch>,
    supervisor: SupervisorHandle,
) {
    loop {
        let Some(mut batch) = rx.recv().await else {
            return; // the source is gone: WatchSource dropped, or its debouncer thread exited
        };
        // Drain whatever else is already queued, batches that arrived
        // while the previous restart was in flight, into this same check.
        while let Ok(more) = rx.try_recv() {
            batch.paths.extend(more.paths);
            batch.rescan |= more.rescan;
        }
        // A rescan is checked ahead of the glob sets: it is not a path, so
        // there is nothing for either list to match against. Restarting is
        // the conservative reading; the alternative is a watch that goes
        // quiet precisely when it knows least.
        if !batch.rescan && !batch.paths.iter().any(|path| filter.triggers(path)) {
            continue;
        }
        match supervisor
            .restart_automatic(ProcessSelector::Name(name.clone()))
            .await
        {
            Ok(_) => {}
            Err(SupervisorError::NotFound) => {
                // The sheep is gone but the registry has not disarmed this
                // group yet: a race with disarm, not a fault, and the
                // disarm is moments away.
                tracing::debug!(name, "watch fired but no sheep by this name is registered");
            }
            Err(err @ SupervisorError::SpawnFailed(_)) => {
                tracing::warn!(name, %err, "watch-triggered restart failed to spawn");
            }
            Err(
                err @ (SupervisorError::ReopenFailed(_)
                | SupervisorError::FlushFailed(_)
                | SupervisorError::ReloadInFlight(_)
                | SupervisorError::InvalidScale(_)
                | SupervisorError::CannotStart(_)
                | SupervisorError::IsADog(_)
                | SupervisorError::InvalidEnv(_)
                | SupervisorError::InvalidField(_)
                | SupervisorError::Overrides(_)),
            ) => {
                // A restart touches no log files, starts no reload, scales
                // nothing and registers no batch, names no dog, field or
                // override, so none of these nine can arrive here. Named
                // rather than swept into a catch-all, so a variant this path
                // can produce still fails to compile.
                tracing::warn!(name, %err, "watch-triggered restart reported an unrelated failure");
            }
            Err(err @ SupervisorError::EngineStopped) => {
                tracing::warn!(name, %err, "supervisor engine has shut down; watch worker ending");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::filter::{RootedFilter, WatchFilter};
    use crate::watch::real_time;

    use core::time::Duration;
    use std::path::PathBuf;

    use super::*;
    use crate::supervisor::SupervisorError;
    use crate::watch::source::WatchError;
    use shep_core::selector::ProcessSelector;
    use tokio::sync::broadcast;
    use tokio::sync::mpsc;

    use super::super::testing::*;
    use crate::fake::{ProcScript, ScriptedRunner};
    use crate::supervisor::spawn_supervisor;
    use crate::testing::test_paths;
    use shep_core::config::{AppConfig, normalize};
    use shep_core::protocol::{BusEvent, ProcessEventKind};
    use shep_core::status::ProcStatus;
    use shep_core::values::UpDuration;

    #[tokio::test(start_paused = true)]
    async fn a_batch_with_one_triggering_path_produces_exactly_one_restart() {
        // Three scripts: one for `start_app`, one for the expected restart,
        // and a third so a double-firing implementation can spawn and emit
        // a second `Restart`. With only two, it would report `Errored`
        // instead, and the trailing negative below could not fail.
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 3]);
        let name = "web";
        start_app(&handle, name, 1).await;
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        tx.send(changed(vec![root.join("src/main.rs")])).unwrap();
        let info = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(info.restarts, 1);
        assert_no_restart_within(&mut rx, name, Duration::from_secs(5)).await;

        group.abort();
    }

    // fails if a rescan is filtered like an ordinary path: on Linux it
    // carries no path at all, so consulting the glob sets leaves the watch
    // deaf exactly when notify has already lost events. Two scripts: one
    // for `start_app`, one for the restart this expects.
    #[tokio::test(start_paused = true)]
    async fn a_rescan_restarts_under_a_non_matching_watch_options() {
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 2]);
        let name = "web";
        start_app(&handle, name, 1).await;
        let root = PathBuf::from("/watched");
        let filter = RootedFilter {
            root,
            filter: WatchFilter::new(&["src/**/*.rs".to_string()], &[]).unwrap(),
        };
        let (tx, group_rx) = mpsc::unbounded_channel();
        let group = tokio::spawn(run_group(
            name.to_string(),
            filter,
            group_rx,
            handle.clone(),
        ));

        tx.send(rescan_marker()).unwrap();
        let info = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(info.restarts, 1);

        group.abort();
    }

    // The loop's rescan check runs before the filter, so a loop that read
    // the root itself as a rescan signal would restart here while every
    // filter-tier assertion stayed green.
    #[tokio::test(start_paused = true)]
    async fn an_ordinary_event_on_the_root_itself_produces_no_restart() {
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 2]);
        let name = "web";
        start_app(&handle, name, 1).await;
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        // The root, with no rescan flag on it: a `chmod` of that inode, or
        // FSEvents' arm-time `Create(Folder)`.
        tx.send(changed(vec![root.clone()])).unwrap();
        assert_no_restart_within(&mut rx, name, Duration::from_secs(5)).await;

        // Control: the same loop, the same watch, one level in, so the
        // silence above is the root being filtered rather than the loop
        // having simply stopped restarting.
        tx.send(changed(vec![root.join("src/main.rs")])).unwrap();
        let info = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(info.restarts, 1);

        group.abort();
    }

    // The two sends are made back to back with no `settle`, so they are
    // genuinely queued together when the loop next looks.
    #[tokio::test(start_paused = true)]
    async fn a_rescan_queued_behind_an_ignored_batch_survives_the_drain() {
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 2]);
        let name = "web";
        start_app(&handle, name, 1).await;
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        tx.send(changed(vec![root.join(".git/index")])).unwrap();
        tx.send(rescan_marker()).unwrap();
        let info = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(info.restarts, 1);

        group.abort();
    }

    // fails two ways: a loop that drops a batch queued during an in-flight
    // restart (only 1 restart total, not 2, no re-check), and a loop that
    // processes each queued send as its own `recv`/restart cycle instead of
    // draining them into one check (3 restarts total, not 2).
    #[tokio::test(start_paused = true)]
    async fn a_batch_queued_during_a_restart_is_rechecked_and_drained_as_one() {
        // Four scripts, not three: a broken implementation that processes
        // `b.rs` and `c.rs` as two separate restarts needs a fourth spawn.
        // With only three, the third attempt would report `Errored` and
        // the mutation would pass by accident.
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::ignores_signals(); 4]);
        let name = "web";
        start_app(&handle, name, 1).await;
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        // Batch 1 kicks off a restart. The scripted process ignores its
        // graceful signal, so the kill ladder is stuck on the full
        // `kill_timeout` until the paused clock actually moves; `settle`
        // only yields, it never advances time.
        tx.send(changed(vec![root.join("a.rs")])).unwrap();
        settle().await;

        // Two more sends land in the queue while restart 1 is still
        // pending.
        tx.send(changed(vec![root.join("b.rs")])).unwrap();
        tx.send(changed(vec![root.join("c.rs")])).unwrap();

        let first = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(first.restarts, 1);
        let second = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(second.restarts, 2);
        assert_no_restart_within(&mut rx, name, Duration::from_secs(5)).await;

        group.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_sender_ends_the_group_task() {
        let (handle, _rx, _dir) = spawn_test_fixture(vec![]);
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, "ghost", &handle);

        drop(tx);

        tokio::time::timeout(EVENT_WAIT, group)
            .await
            .expect("group task did not end after its sender was dropped")
            .expect("group task panicked");
    }

    // fails if the group loop filters by status: the reach is the whole
    // name-group, not just its running instances, pinning this against a
    // reimplementation of the withdrawn per-instance filter.
    #[tokio::test(start_paused = true)]
    async fn a_triggering_batch_restarts_a_stopped_instance_in_the_same_group() {
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 4]);
        let name = "web";
        let infos = start_app(&handle, name, 2).await;
        let stopped_id = infos[1].id;
        handle.stop(ProcessSelector::Id(stopped_id)).await.unwrap();

        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        tx.send(changed(vec![root.join("src/main.rs")])).unwrap();

        let first = expect_restart(&mut rx, name, EVENT_WAIT).await;
        let second = expect_restart(&mut rx, name, EVENT_WAIT).await;
        let stopped_info = [first, second]
            .into_iter()
            .find(|info| info.id == stopped_id)
            .expect("the previously-stopped instance never restarted");
        // `Online` alone would pass against a group that never touched the
        // stopped instance if something else had started it; `restarts`
        // is what makes the claim about this restart.
        assert_eq!(stopped_info.status, ProcStatus::Online);
        assert_eq!(stopped_info.restarts, 1);

        group.abort();
    }

    // Two instances so the restart is provably observed before the stop
    // lands. fails if the loop calls `restart` instead of
    // `restart_automatic`.
    #[tokio::test(start_paused = true)]
    async fn an_operators_stop_beats_a_watch_triggered_restart_mid_ladder() {
        // Four procs is the most this test can demand: both instances'
        // initial ones, the untouched instance's legitimate respawn, and
        // the respawn a broken implementation performs behind the stop's
        // back. Three would report `Errored` instead of showing `Online`.
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![
            ProcScript::ignores_signals(), // held for the whole 1600ms ladder
            ProcScript::never_exits(),     // exits the moment the ladder signals it
            ProcScript::never_exits(),     // the untouched instance's respawn
            ProcScript::never_exits(),     // the respawn a broken implementation performs
        ]);
        let name = "web";
        let infos = start_app(&handle, name, 2).await;
        let (held, released) = (infos[0].id, infos[1].id);
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        // The batch claims BOTH instances' next exit and starts both kill
        // ladders. Only the second sheep's ladder can finish without the clock
        // moving, so its restart lands while the first is still mid-ladder.
        tx.send(changed(vec![root.join("src/main.rs")])).unwrap();
        let restarted = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(
            (restarted.id, restarted.restarts),
            (released, 1),
            "the batch never reached the actor, so the stop below would race \
                 nothing -- got {restarted:?}"
        );
        // Aborted before the stop so no later batch can reach the assertions
        // below. The restart is already in the actor's hands; the dropped
        // reply receiver only means nobody reads the answer.
        group.abort();

        let stopped = handle.stop(ProcessSelector::Id(held)).await.unwrap();
        assert_eq!(stopped.len(), 1);
        assert_eq!(
            (stopped[0].id, stopped[0].status, stopped[0].restarts),
            (held, ProcStatus::Stopped, 0),
            "an operator's stop was silently converted into the watch-triggered \
                 restart it raced -- got {stopped:?}"
        );
        let listed = handle.list().await;
        assert_eq!(
            (listed[0].id, listed[0].status, listed[0].pid),
            (held, ProcStatus::Stopped, None),
            "the sheep an operator stopped is running again -- got {listed:?}"
        );
        assert_eq!(
            (listed[1].id, listed[1].status),
            (released, ProcStatus::Online),
            "the instance the operator did not name must still be up, \
                 restarted by the batch -- got {listed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn not_found_leaves_the_loop_alive_for_the_next_batch() {
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 2]);
        let name = "ghost";
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        // `name` matches nothing yet: the restart resolves `NotFound`, and
        // the loop must stay alive rather than returning.
        tx.send(changed(vec![root.join("a.rs")])).unwrap();
        assert_no_restart_within(&mut rx, name, Duration::from_millis(200)).await;
        assert!(!group.is_finished(), "the loop must not exit on NotFound");

        // Registering the name for real and sending a second batch: if the
        // earlier `NotFound` had ended the loop, this would time out.
        start_app(&handle, name, 1).await;
        tx.send(changed(vec![root.join("b.rs")])).unwrap();
        let info = expect_restart(&mut rx, name, EVENT_WAIT).await;
        assert_eq!(info.restarts, 1);

        group.abort();
    }

    // The loop's other exit: the engine it restarts through, rather than
    // its source going away. fails if the `EngineStopped` arm falls through
    // instead of returning, leaving the group watching forever. No scripts
    // in the fixture: the engine is shut down before the batch is sent.
    #[tokio::test(start_paused = true)]
    async fn the_group_task_ends_when_the_supervisor_engine_has_stopped() {
        let (handle, _rx, _dir) = spawn_test_fixture(Vec::new());
        let name = "web";
        handle.shutdown().await;
        // The premise, stated rather than assumed: with the actor gone, the
        // restart this batch is about to trigger answers `EngineStopped`.
        assert_eq!(
            handle
                .restart_automatic(ProcessSelector::Name(name.to_string()))
                .await
                .unwrap_err(),
            SupervisorError::EngineStopped
        );

        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        tx.send(changed(vec![root.join("src/main.rs")])).unwrap();
        tokio::time::timeout(EVENT_WAIT, group)
            .await
            .expect("the group task did not end after the engine shut down")
            .expect("group task panicked");
        drop(tx); // kept alive until here: a dropped sender ends the loop too
    }

    // The only case that constructs a real `WatchSource`, exercised by
    // `spawn_watch_group_restarts_on_a_real_touch_and_stops_on_abort`. The
    // touch half catches a guard dropped before the loop sees an event; the
    // abort half only proves the loop stops, not that a leaked guard is caught.
    // A watch root that does not exist, seen from the arming entry point:
    // fails if the arming swallows the failure, or reports `Backend`
    // instead of `Watch` carrying the exact path handed to it. Real time,
    // like every other case here that constructs a watcher.
    #[tokio::test]
    async fn a_watch_root_that_does_not_exist_names_the_path_it_could_not_watch() {
        let (handle, _rx, _dir) = spawn_test_fixture(vec![]);
        // Inside a live tempdir so the parent exists and only the leaf does
        // not: a root whose whole prefix is missing would leave "which
        // component did notify object to" ambiguous.
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("no-such-directory");

        let err = spawn_watch_group(
            "web".to_string(),
            missing.clone(),
            WatchFilter::new(&[], &[]).unwrap(),
            real_time::TEST_DELAY,
            handle,
        )
        .unwrap_err();

        let WatchError::Watch { path, reason } = err else {
            panic!("a root that does not exist must report `Watch`, got {err:?}");
        };
        assert_eq!(path, missing, "`Watch` must carry the root it was handed");
        assert!(!reason.is_empty(), "`Watch` must carry notify's own reason");
    }

    proptest::proptest! {
        // 64 rather than the supervisor proptest's 128: each case here boots
        // a runtime, a supervisor and a group loop and walks virtual time
        // across every generated gap, so a case costs more.
        // `PROPTEST_CASES` still overrides it.
        #![proptest_config(crate::testing::proptest_config(64))]

        // `run_group` awaits each restart before its next `recv`, so single
        // flight falls out of the shape. Every scripted proc ignores its
        // graceful signal, so a restart takes exactly the generated
        // `kill_timeout`: two restarts less than that apart overlapped.
        #[test]
        fn a_watch_group_never_has_two_restarts_in_flight(
            batches in proptest::collection::vec(batch_strategy(), 1..=MAX_BATCHES),
            kill_timeout_ms in 200u64..2_000u64,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .start_paused(true)
                .build()
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            runtime.block_on(async move {
                let kill_timeout = Duration::from_millis(kill_timeout_ms);
                let (events, mut rx) = crate::bus::test_bus(1024);
                let runner =
                    ScriptedRunner::new(vec![ProcScript::ignores_signals(); SINGLE_FLIGHT_SCRIPTS]);
                let handle = spawn_supervisor(runner, test_paths(&dir), events);
                let name = "web";
                let mut app = AppConfig::minimal(name, "./srv");
                app.kill_timeout = UpDuration::from_millis(kill_timeout_ms);
                handle.start(vec![normalize(app).unwrap()]).await.unwrap();

                let root = PathBuf::from("/watched");
                let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

                let start = tokio::time::Instant::now();
                // Drained by its own task, started before the first send:
                // a broadcast send wakes it without moving the paused
                // clock, so the recorded instant is the instant the actor
                // emitted, not the end of some later sleep.
                let watched = name.to_string();
                let collector = tokio::spawn(async move {
                    let mut observed = Vec::new();
                    loop {
                        match tokio::time::timeout(EVENT_WAIT, rx.recv()).await.map(|received| received.map(|event| event.to_event())) {
                            Ok(Ok(BusEvent::Process {
                                event: ProcessEventKind::Restart,
                                info,
                                ..
                            })) if info.name == watched => {
                                observed
                                    .push((tokio::time::Instant::now() - start, info.restarts));
                            }
                            Ok(Ok(_)) => continue,
                            // A claim about overlap cannot skip events: a
                            // dropped one may be the very restart that
                            // overlapped.
                            Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                                return Err(skipped);
                            }
                            Ok(Err(broadcast::error::RecvError::Closed)) => break,
                            Err(_elapsed) => break, // the group has gone quiet
                        }
                    }
                    Ok(observed)
                });

                for (i, batch) in batches.iter().enumerate() {
                    if batch.gap > Duration::ZERO {
                        tokio::time::sleep(batch.gap).await;
                    }
                    // `.git/` is in `DEFAULT_IGNORE_GLOBS`, so a non-
                    // triggering batch is a real delivered path the filter
                    // rejects rather than an empty send the loop would never
                    // see.
                    let path = if batch.triggers {
                        root.join(format!("src/f{i}.rs"))
                    } else {
                        root.join(format!(".git/o{i}"))
                    };
                    tx.send(changed(vec![path])).unwrap();
                }

                let observed = match collector.await.expect("collector task panicked") {
                    Ok(observed) => observed,
                    Err(skipped) => {
                        return Err(proptest::test_runner::TestCaseError::fail(format!(
                            "event stream lagged by {skipped}"
                        )));
                    }
                };
                group.abort();

                // The invariant itself, read off the bus: consecutive
                // restarts of one group are never closer together than one
                // restart takes.
                for pair in observed.windows(2) {
                    proptest::prop_assert!(
                        pair[1].0 - pair[0].0 >= kill_timeout,
                        "two restarts of {} finished {:?} apart, less than the {:?} one takes: \
                         they overlapped",
                        name,
                        pair[1].0 - pair[0].0,
                        kill_timeout
                    );
                }

                // ...and the same claim stated positively, against the
                // sequential model: a loop that overlaps restarts, drops a
                // batch, or gives each queued send its own cycle disagrees
                // with it on when, and how many times, it restarted.
                let expected = expected_restart_instants(&batches, kill_timeout);
                let counted: Vec<u32> = (1..=expected.len() as u32).collect();
                proptest::prop_assert_eq!(
                    observed,
                    expected.into_iter().zip(counted).collect::<Vec<_>>()
                );
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }
    }

    /// Tests that wait on real filesystem events or real elapsed time.
    ///
    /// The inner loop skips this module with `--skip ::slow::`; the full
    /// suite still runs them because nothing here is `#[ignore]`d.
    mod slow {
        use super::*;

        // fails if the debouncer guard is dropped before the loop ever sees
        // an event: the watch dies inside `spawn_watch_group` and the touch
        // below produces no restart. The abort half only proves the loop
        // stops, not that a leak is caught.
        #[tokio::test]
        async fn spawn_watch_group_restarts_on_a_real_touch_and_stops_on_abort() {
            let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 2]);
            let name = "web";
            start_app(&handle, name, 1).await;

            let watch_dir = tempfile::tempdir().unwrap();
            let root = watch_dir.path().canonicalize().unwrap();
            let filter = WatchFilter::new(&[], &[]).unwrap();
            let group = spawn_watch_group(
                name.to_string(),
                root.clone(),
                filter,
                real_time::TEST_DELAY,
                handle.clone(),
            )
            .unwrap();

            crate::testing::touch(&root, "trigger.txt").unwrap();
            let info = expect_restart(&mut rx, name, real_time::SMOKE_DEADLINE).await;
            assert_eq!(info.restarts, 1);

            group.abort();
            crate::testing::touch(&root, "after-abort.txt").unwrap();
            assert_no_restart_within(&mut rx, name, real_time::NO_EVENT_WINDOW).await;
        }
    }
}
