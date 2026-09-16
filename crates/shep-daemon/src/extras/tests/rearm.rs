//! Tests for [`ExtrasRegistry::rearm_name`]: a reload replaces a group's live
//! tasks with fresh ones, leaves sibling groups and already-healthy instances
//! alone, and rebuilds a multi-instance group exactly once.

use super::*;

/// `arm` keeps a live cron or watch task, which is right for a reload's
/// overlap and wrong for a config change: those tasks read their
/// group-scoped config when they are built.
#[tokio::test(start_paused = true)]
async fn rearm_name_replaces_a_live_group_task() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    let entry = armed_entry(0, 0, 1000, app.clone(), &paths);
    registry.arm(&entry, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let before_cron = registry.groups["web"].cron.as_ref().unwrap().abort_handle();
    let before_watch = registry.groups["web"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();

    registry.rearm_name("web", &[&entry], |_| idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let after_cron = registry.groups["web"].cron.as_ref().unwrap().abort_handle();
    let after_watch = registry.groups["web"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();
    assert_ne!(
        before_cron.id(),
        after_cron.id(),
        "the cron worker survived a rearm"
    );
    assert_ne!(
        before_watch.id(),
        after_watch.id(),
        "the watch task survived a rearm"
    );
}

/// An app left with no watcher at all is worse than one left with a stale
/// watcher.
#[tokio::test(start_paused = true)]
async fn rearm_name_leaves_the_group_armed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    let entry = armed_entry(0, 0, 1000, app.clone(), &paths);
    registry.arm(&entry, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    registry.rearm_name("web", &[&entry], |_| idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let group = &registry.groups["web"];
    assert!(
        group.cron.as_ref().is_some_and(|cron| !cron.is_finished()),
        "the group must still have a live cron worker after a rearm"
    );
    assert!(
        group
            .watch
            .as_ref()
            .is_some_and(|watch| !watch.is_finished()),
        "the group must still have a live watch task after a rearm"
    );
}

#[tokio::test(start_paused = true)]
async fn rearm_name_leaves_another_apps_group_alone() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let web_root = tempfile::tempdir().unwrap();
    let worker_root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let web = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(web_root.path().display().to_string());
    });
    let worker = app_with("worker", |app| {
        app.watch = true;
        app.cwd = Some(worker_root.path().display().to_string());
    });
    handle
        .start(vec![web.clone(), worker.clone()])
        .await
        .unwrap();

    let web_entry = armed_entry(0, 0, 1000, web.clone(), &paths);
    let worker_entry = armed_entry(1, 0, 1001, worker.clone(), &paths);
    registry.arm(&web_entry, idle_prober(), &rig.extras, &handle);
    registry.arm(&worker_entry, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let worker_watch_before = registry.groups["worker"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();

    registry.rearm_name(
        "web",
        &[&web_entry],
        |_| idle_prober(),
        &rig.extras,
        &handle,
    );
    tokio::task::yield_now().await;

    let worker_watch_after = registry.groups["worker"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();
    assert_eq!(
        worker_watch_before.id(),
        worker_watch_after.id(),
        "rearming \"web\" must not touch \"worker\"'s group"
    );
}

/// One group shared across a name's instances is only transitively protected
/// by `arm`'s own idempotency test, since `rearm_name` builds no task
/// itself.
#[tokio::test(start_paused = true)]
async fn rearm_name_rebuilds_a_multi_instance_group_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.instances = 2;
        app.cron_restart = Some("0 * * * *".to_string());
        app.watch = true;
        app.cwd = Some(root.path().display().to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    let entry_a = armed_entry(0, 0, 1000, app.clone(), &paths);
    let entry_b = armed_entry(1, 1, 1001, app.clone(), &paths);
    registry.arm(&entry_a, idle_prober(), &rig.extras, &handle);
    registry.arm(&entry_b, idle_prober(), &rig.extras, &handle);
    tokio::task::yield_now().await;

    let before_cron = registry.groups["web"].cron.as_ref().unwrap().abort_handle();
    let before_watch = registry.groups["web"]
        .watch
        .as_ref()
        .unwrap()
        .abort_handle();

    // The prober closure is the seam that pins one prober per entry:
    // `assemble` bakes `SHEP_INSTANCE` into the environment a prober runs
    // with, so a shared one would probe every instance as one. The call
    // count and order are what is observable here.
    let probed = std::sync::Mutex::new(Vec::new());
    registry.rearm_name(
        "web",
        &[&entry_a, &entry_b],
        |entry| {
            probed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(entry.id);
            idle_prober()
        },
        &rig.extras,
        &handle,
    );
    tokio::task::yield_now().await;

    assert_eq!(
        probed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        [0, 1],
        "each instance must get its own prober, in id order"
    );

    let group = &registry.groups["web"];
    assert_eq!(
        group.members,
        HashSet::from([0, 1]),
        "both instances must still be members after a rearm, not just the last one arm'd"
    );
    assert!(
        group.cron.is_some(),
        "the group must hold exactly one cron task, not zero"
    );
    assert!(
        group.watch.is_some(),
        "the group must hold exactly one watch task, not zero"
    );
    let after_cron = group.cron.as_ref().unwrap().abort_handle();
    let after_watch = group.watch.as_ref().unwrap().abort_handle();
    assert_ne!(
        before_cron.id(),
        after_cron.id(),
        "the cron worker must be rebuilt by the rearm, not left over"
    );
    assert_ne!(
        before_watch.id(),
        after_watch.id(),
        "the watch task must be rebuilt by the rearm, not left over"
    );
}
