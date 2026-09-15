//! Tests that wait on real filesystem events or real elapsed time. The inner
//! loop skips them with `--skip ::slow::`; the full suite runs them.

use super::*;

// The overlap a reload runs on: the replacement arms before the
// drainee's exit disarms the old id, so `disarm` finds a member still
// standing. Task identity tells a surviving group from a rebuilt one, and
// the two `AbortHandle`s are held unfired so tokio cannot reuse the id.
#[tokio::test(start_paused = true)]
async fn a_replacement_arming_before_the_drainee_disarms_keeps_the_groups_own_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    // Both per-name extras, because the overlap has to hold for both.
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    // The drainee: id 0, holding instance slot 0.
    registry.arm(
        &armed_entry(0, 0, 1000, app.clone(), &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    // Lets the cron worker reach its first poll, so the reading below
    // is settled rather than racing it.
    tokio::task::yield_now().await;

    let group = &registry.groups["web"];
    let cron = group
        .cron
        .as_ref()
        .expect("fixture check: the cron worker must have armed")
        .abort_handle();
    let watch = group
        .watch
        .as_ref()
        .expect("fixture check: the watch must have armed")
        .abort_handle();
    let reads_before = rig.clock.reads();

    // The overlap, in the order a swap performs it: the replacement
    // takes a new id in the drainee's slot and goes `Online` first.
    registry.arm(
        &armed_entry(1, 0, 2000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    registry.disarm(0, "web");
    // Lets a rebuilt worker reach its own first poll, so an unchanged
    // count below means there is none rather than that it had not run.
    tokio::task::yield_now().await;

    let group = &registry.groups["web"];
    assert_eq!(
        group.members,
        HashSet::from([1]),
        "fixture check: the drainee must really have left a group the \
     replacement had already joined — this reads the same under either \
     ordering, which is why it cannot be the claim that matters"
    );
    assert_eq!(
        group.cron.as_ref().map(JoinHandle::id),
        Some(cron.id()),
        "the group must still hold the cron worker it was armed with, not \
     an identical one put back in its place"
    );
    assert_eq!(
        group.watch.as_ref().map(JoinHandle::id),
        Some(watch.id()),
        "and the watch it was armed with, whose rebuild means re-registering \
     the OS watcher"
    );
    assert_eq!(
        rig.clock.reads(),
        reads_before,
        "a surviving cron worker performs no startup work; a rebuilt one \
     reads the clock again to derive its next occurrence"
    );
}

// A name group with zero online instances, armed and disarmed before it
// ever fires. An app whose first spawn is stopped straight away passes
// through this shape, and a worker leaked there restarts a flock nobody
// is running.
#[tokio::test(start_paused = true)]
async fn a_group_disarmed_before_its_first_occurrence_leaves_no_worker_behind() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    // Both per-name extras and one per-pid extra, so a single disarm
    // has to reach all three.
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
        app.max_memory = Some(MemSize::from_bytes(1024));
    });
    handle.start(vec![app.clone()]).await.unwrap();

    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    let group = &registry.groups["web"];
    assert_eq!(group.members, HashSet::from([0]));
    assert!(group.cron.is_some(), "the cron worker must have armed");
    assert!(group.watch.is_some(), "the watch must have armed");
    assert_eq!(rig.enforcer.arms().len(), 1);

    registry.disarm(0, "web");

    assert!(
        registry.groups.is_empty(),
        "a group whose only member left before its first occurrence must go with it"
    );
    assert!(
        registry.instances.is_empty(),
        "the same disarm must take the instance's own extras too"
    );
    assert_eq!(rig.enforcer.disarms(), vec![0]);
    assert_no_restart_within(&mut rx, "web", PAST_THE_NEXT_OCCURRENCE).await;
}

// The watch twin of the case above, and separate because the gate is two
// independent conditions. The ending is forced with `abort` rather than
// by killing the `WatchSource` the loop really returns on, which dies
// with an OS thread no test reaches; both leave a finished handle.
#[tokio::test]
async fn a_watch_that_ended_on_its_own_is_rebuilt_on_the_next_arm() {
    let home = tempfile::tempdir().unwrap();
    let paths = test_paths(&home);
    let root = tempfile::tempdir().unwrap();
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
        app.watch_delay = Some(UpDuration::from_millis(
            real_time::TEST_DELAY.as_millis() as u64
        ));
    });
    handle.start(vec![app.clone()]).await.unwrap();

    registry.arm(
        &armed_entry(0, 0, 1000, app.clone(), &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    let armed = registry.groups["web"]
        .watch
        .as_ref()
        .expect("the first arm registers a watcher");
    armed.abort();
    settle_finished(armed).await;

    registry.arm(
        &armed_entry(0, 0, 2000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    touch(root.path(), "trigger.txt").unwrap();
    let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
    assert_eq!(info.restarts, 1);
}

// An unresolved root fires never: on macOS a tempdir under `/var/...` is
// delivered as `/private/var/...` and every `strip_prefix` fails.
#[tokio::test]
async fn a_watched_app_restarts_on_a_save_and_goes_quiet_once_disarmed() {
    let home = tempfile::tempdir().unwrap();
    let paths = test_paths(&home);
    let root = tempfile::tempdir().unwrap();
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
        // From `watch::real_time`, the owner of this subsystem's
        // real-time constants.
        app.watch_delay = Some(UpDuration::from_millis(
            real_time::TEST_DELAY.as_millis() as u64
        ));
    });
    handle.start(vec![app.clone()]).await.unwrap();
    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    touch(root.path(), "trigger.txt").unwrap();
    let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
    assert_eq!(info.restarts, 1);

    registry.disarm(0, "web");
    touch(root.path(), "after-disarm.txt").unwrap();
    assert_no_restart_within(&mut rx, "web", real_time::NO_EVENT_WINDOW).await;
}

// `DEFAULT_WATCH_DELAY` is 500ms, so any longer fallback leaves the save
// below with no restart inside the deadline.
#[tokio::test]
async fn a_watched_app_naming_no_delay_still_restarts_on_a_save() {
    let home = tempfile::tempdir().unwrap();
    let paths = test_paths(&home);
    let root = tempfile::tempdir().unwrap();
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
        // No `watch_delay`: this case exists for the default.
    });
    handle.start(vec![app.clone()]).await.unwrap();
    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    touch(root.path(), "trigger.txt").unwrap();
    let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
    assert_eq!(info.restarts, 1);
}

// The watch's own door into the claim the cron case makes, and separate
// because the two subsystems pick their `SupervisorHandle` method
// independently: through `restart`, an autosave is reported as a deploy.
#[tokio::test]
async fn a_watch_restart_is_not_reported_as_a_user_action() {
    let home = tempfile::tempdir().unwrap();
    let paths = test_paths(&home);
    let root = tempfile::tempdir().unwrap();
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
        app.watch_delay = Some(UpDuration::from_millis(
            real_time::TEST_DELAY.as_millis() as u64
        ));
    });
    handle.start(vec![app.clone()]).await.unwrap();
    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    touch(root.path(), "trigger.txt").unwrap();

    let (info, manually) = expect_restart_event(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
    assert_eq!(info.restarts, 1);
    assert!(
        !manually,
        "a file changing under a watched tree is not a user action"
    );
}

// A filter built from empty slices discards every ignore rule the user
// wrote.
#[tokio::test]
async fn a_watched_app_ignores_the_paths_its_ignore_watch_names() {
    let home = tempfile::tempdir().unwrap();
    let paths = test_paths(&home);
    let root = tempfile::tempdir().unwrap();
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
        app.ignore_watch = vec!["ignored.txt".to_string()];
        app.watch_delay = Some(UpDuration::from_millis(
            real_time::TEST_DELAY.as_millis() as u64
        ));
    });
    handle.start(vec![app.clone()]).await.unwrap();
    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    touch(root.path(), "ignored.txt").unwrap();
    assert_no_restart_within(&mut rx, "web", real_time::NO_EVENT_WINDOW).await;

    touch(root.path(), "trigger.txt").unwrap();
    let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
    assert_eq!(info.restarts, 1);
}

// The default globs plus `ignore_watch` alone let an app naming an
// explicit `out_file`/`err_file` under its own `cwd` restart on its own
// log writes forever: the default log glob covers only a directory named
// `logs`, and an automatic restart resets the budget.
#[tokio::test]
async fn a_watched_app_ignores_its_own_log_writes() {
    let home = tempfile::tempdir().unwrap();
    let paths = test_paths(&home);
    let root = tempfile::tempdir().unwrap();
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
        // Absolute, under the watched tree, and named nothing like
        // `logs`: a shep write really does land inside the tree.
        app.out_file = Some(root.path().join("app-out.txt").display().to_string());
        app.err_file = Some(root.path().join("app-err.txt").display().to_string());
        app.watch_delay = Some(UpDuration::from_millis(
            real_time::TEST_DELAY.as_millis() as u64
        ));
    });
    handle.start(vec![app.clone()]).await.unwrap();
    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    touch(root.path(), "app-out.txt").unwrap();
    touch(root.path(), "app-err.txt").unwrap();
    assert_no_restart_within(&mut rx, "web", real_time::NO_EVENT_WINDOW).await;

    touch(root.path(), "trigger.txt").unwrap();
    let info = expect_restart(&mut rx, "web", real_time::SMOKE_DEADLINE).await;
    assert_eq!(info.restarts, 1);
}

// `probe_exec` runs `env_clear().envs(&self.env)` and `SHEP_INSTANCE` is
// written by `assemble` alone, so a prober built from `config.env`, or
// built once and shared, expands it to nothing and both instances report.
// A file, not a port: `test -f` needs no listener and cannot race.
#[cfg(unix)]
#[tokio::test]
async fn each_instances_liveness_probe_runs_with_its_own_assembled_env() {
    let markers = tempfile::tempdir().unwrap();
    // Instance 0's marker exists; instance 1's never will.
    std::fs::write(markers.path().join("live-0"), b"").unwrap();

    let mut h = harness(vec![ProcScript::never_exits(); 4]);
    h.ctx
        .supervisor
        .start(vec![app_with("web", |app| {
            app.instances = 2;
            app.liveness_probe = Some(ProbeConfig {
                failure_threshold: 1,
                interval: PROBE_INTERVAL,
                timeout: UpDuration::from_millis(5_000),
                ..probe_config(
                    ProbeKind::Exec,
                    &format!(
                        r#"test -f "{}/live-$SHEP_INSTANCE""#,
                        markers.path().display()
                    ),
                )
            });
        })])
        .await
        .unwrap();

    let listing = h.ctx.supervisor.list().await;
    // `ProcessInfo` carries no instance number, but the assembler's log
    // path does, from the same `assemble` call, so this pins which
    // instance id 1 is rather than assuming the allocation order.
    assert!(
        listing[1]
            .out_file
            .as_ref()
            .is_some_and(|path| path.ends_with("web-1-out.log")),
        "id 1 must be instance 1: {:?}",
        listing[1].out_file
    );
    let instance_one_pid = listing[1].pid.expect("a live sheep has a pid");

    let failure = expect_liveness(&mut h.liveness, LIVENESS_DEADLINE).await;
    assert_eq!(
        failure,
        LivenessReport {
            id: 1,
            pid: instance_one_pid,
            epoch: 1,
        },
        "only the instance whose own marker is missing may report"
    );
    // Both instances report under the bugs above and which arrives
    // first is a race, so the window catching the other is not optional.
    assert_no_liveness_within(&mut h.liveness, PROBE_INTERVAL.as_duration() * 3).await;
}
