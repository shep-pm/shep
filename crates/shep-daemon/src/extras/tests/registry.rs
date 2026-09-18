//! The registry: what gets armed, and what stops.

use super::*;

#[tokio::test(start_paused = true)]
async fn extras_debug_names_the_seams_by_role_and_the_sleep_bound_by_value() {
    let rig = rig(Duration::from_secs(300));
    assert_eq!(
        format!("{:?}", rig.extras),
        r#"Extras { clock: "<dyn Clock>", enforcer: "<dyn LimitEnforcer>", max_cron_sleep: 300s, .. }"#
    );
}

#[tokio::test(start_paused = true)]
async fn instance_extras_debug_reports_an_arming_without_naming_the_enforcer() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let capped = app_with("web", |app| app.max_memory = Some(MemSize::from_bytes(500)));
    let uncapped = app_with("api", |app| app.max_memory = None);

    registry.arm(
        &armed_entry(0, 0, 1000, capped, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    registry.arm(
        &armed_entry(1, 0, 1001, uncapped, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    assert_eq!(
        format!("{:?}", registry.instances[&0]),
        "InstanceExtras { limit_armed: true, liveness: None, .. }"
    );
    assert_eq!(
        format!("{:?}", registry.instances[&1]),
        "InstanceExtras { limit_armed: false, liveness: None, .. }"
    );
}

// Real sysinfo over this very test process, whose RSS is comfortably over
// one byte, on the paused clock the polling loop sleeps on.
#[tokio::test(start_paused = true)]
async fn real_extras_wire_the_enforcer_to_the_reports_channel() {
    let (breach_tx, mut breaches) = mpsc::channel(4);
    let (live_tx, _liveness) = mpsc::channel(4);
    let extras = Extras::real(
        ExtrasReports {
            breaches: breach_tx,
            liveness: live_tx,
        },
        Duration::from_secs(300),
    );
    assert_eq!(
        format!("{extras:?}"),
        r#"Extras { clock: "<dyn Clock>", enforcer: "<dyn LimitEnforcer>", max_cron_sleep: 300s, .. }"#,
        "`real` must carry the sleep bound it was handed, not re-derive one"
    );

    let limit = MemSize::from_bytes(1);
    extras.enforcer.arm(3, std::process::id(), limit);
    let breach = match tokio::time::timeout(EVENT_WAIT, breaches.recv()).await {
        Ok(Some(breach)) => breach,
        Ok(None) => panic!("the breach channel closed before a breach arrived"),
        Err(_) => panic!("timed out waiting for a breach from the real enforcer"),
    };
    assert_eq!(breach.id, 3);
    assert_eq!(breach.root_pid, std::process::id());
    assert_eq!(breach.limit, limit);
}

// The cwd is a real directory, so this app is one a watcher could really
// have been registered on.
#[tokio::test(start_paused = true)]
async fn an_app_configuring_no_extras_arms_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();

    let app = app_with("web", |app| {
        app.cwd = Some(root.path().display().to_string());
    });
    let entry = armed_entry(0, 0, 1000, app, &paths);
    registry.arm(&entry, idle_prober(), &rig.extras, &handle);

    assert!(
        registry.groups.is_empty(),
        "an app with neither cron_restart nor watch must arm no name-group tasks"
    );
    // Sampling is the exception: it is armed for every sheep with a pid.
    assert_eq!(
        format!("{:?}", registry.instances[&0]),
        "InstanceExtras { limit_armed: false, liveness: None, .. }",
        "an app with neither max_memory nor liveness_probe must arm nothing beyond sampling"
    );
    assert!(
        rig.enforcer.arms().is_empty(),
        "an app with no max_memory must not reach the enforcer at all"
    );
}

// The disarm at the end is the other half: a watch never dropped samples a
// dead pid forever, and hands its CPU baseline to whatever gets that number.
#[tokio::test(start_paused = true)]
async fn every_sheep_with_a_pid_is_watched_even_with_no_limit_and_no_probe() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.max_memory = None;
        app.liveness_probe = None;
    });

    registry.arm(
        &armed_entry(7, 0, 4242, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    assert_eq!(
        rig.extras.stats.watched_for_test(),
        vec![(7, 4242)],
        "an app with neither max_memory nor a liveness_probe is the ORDINARY case, and a \
         listing reporting `-` for every one of them is what this split exists to fix"
    );
    assert!(
        rig.enforcer.arms().is_empty(),
        "sampling is not enforcement: an app with no ceiling must still not be armed"
    );

    registry.disarm(7, "web");
    assert!(rig.extras.stats.watched_for_test().is_empty());
}

// A cron-restarting app is a member of its name group whatever it thinks of
// watching, so it is the one shape that reaches `arm_watch` unwatched. Its
// cwd is real, so a watcher really would register.
#[tokio::test(start_paused = true)]
async fn a_cron_only_app_with_a_real_cwd_arms_no_watch() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.cwd = Some(root.path().display().to_string());
    });

    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    let group = &registry.groups["web"];
    assert!(group.cron.is_some(), "the cron worker is what armed here");
    assert!(
        group.watch.is_none(),
        "an app that did not ask to be watched must get no watcher on its cwd"
    );
}

// `Etc/GMT+5` is UTC minus five, POSIX inverting the sign, so 05:00 local is
// 10:00Z. Read as UTC it fires five hours inside the silent window below.
#[tokio::test(start_paused = true)]
async fn a_cron_pattern_is_resolved_in_the_apps_own_timezone() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 5 * * *".to_string());
        app.cron_timezone = Some("Etc/GMT+5".to_string());
    });
    handle.start(vec![app.clone()]).await.unwrap();

    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    tokio::task::yield_now().await;

    // Six hours from midnight UTC: past a UTC reading of the pattern,
    // still four hours short of the app's own.
    assert_no_restart_within(&mut rx, "web", Duration::from_secs(6 * 3_600)).await;
    expect_restart(&mut rx, "web", Duration::from_secs(6 * 3_600)).await;
}

// An unresolvable cwd is the one config-shaped watch failure that survives
// normalization: `normalize` compiles both glob lists.
#[tokio::test(start_paused = true)]
async fn a_watch_root_that_will_not_resolve_costs_the_app_its_watch_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(root.path().join("no-such-directory").display().to_string());
        app.cron_restart = Some("0 * * * *".to_string());
    });

    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    let group = &registry.groups["web"];
    assert!(group.watch.is_none(), "the watch root cannot have resolved");
    assert!(
        group.cron.is_some(),
        "an unresolvable watch root must not cost this app its cron worker too"
    );
}

#[tokio::test(start_paused = true)]
async fn a_watch_root_that_will_not_resolve_says_in_the_log_which_app_lost_its_watch() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let root = tempfile::tempdir().unwrap();
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("unwatchable", |app| {
        app.watch = true;
        app.cwd = Some(root.path().join("no-such-directory").display().to_string());
    });
    let entry = armed_entry(0, 0, 1000, app, &paths);

    let records = capture_logs(|| {
        registry.arm(&entry, idle_prober(), &rig.extras, &handle);
    });

    assert!(
        registry.groups["unwatchable"].watch.is_none(),
        "precondition: the watch root cannot have resolved"
    );
    assert!(
        records.contains("watch root could not be resolved"),
        "arming no watch must be reported, not swallowed: {records:?}"
    );
    assert!(
        records.contains("WARN"),
        "an app silently losing its watch is a warning, not a debug detail: {records:?}"
    );
    assert!(
        records.contains(r#"name="unwatchable""#),
        "the record must name the app that lost its watch: {records:?}"
    );
}

// The clock count is the only trace a second worker leaves: two
// `restart(Name)` commands racing the same sheep are collapsed by the
// actor's first-command-wins dedupe.
#[tokio::test(start_paused = true)]
async fn a_second_instance_of_a_name_arms_no_second_cron_worker() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(Duration::from_secs(600));
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.instances = 2;
    });
    handle.start(vec![app.clone()]).await.unwrap();

    for (id, instance, pid) in [(0, 0, 1000), (1, 1, 1001)] {
        let entry = armed_entry(id, instance, pid, app.clone(), &paths);
        registry.arm(&entry, idle_prober(), &rig.extras, &handle);
        // Lets the worker commit to its first `next` while the clock still
        // reads close to now: `advance` jumps first and polls after.
        tokio::task::yield_now().await;
    }

    assert_eq!(registry.groups.len(), 1, "one group, not one per instance");
    assert_eq!(
        registry.groups["web"].members,
        HashSet::from([0, 1]),
        "both instances must be recorded as keeping the group alive"
    );

    cross_one_hour().await;
    expect_restart(&mut rx, "web", EVENT_WAIT).await;
    assert!(
        rig.clock.reads() < 20,
        "one cron worker reads the clock ~13 times over this hour; two read ~26 (got {})",
        rig.clock.reads()
    );
}

#[tokio::test(start_paused = true)]
async fn a_respawn_re_arms_the_enforcer_with_the_new_pid() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let limit = MemSize::from_bytes(500);
    let app = app_with("web", |app| app.max_memory = Some(limit));

    registry.arm(
        &armed_entry(4, 0, 1000, app.clone(), &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    registry.arm(
        &armed_entry(4, 0, 2000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    assert_eq!(
        rig.enforcer.arms(),
        vec![
            ArmCall {
                id: 4,
                root_pid: 1000,
                limit,
            },
            ArmCall {
                id: 4,
                root_pid: 2000,
                limit,
            },
        ]
    );
    assert_eq!(
        rig.enforcer.disarms(),
        vec![4],
        "a re-arm must undo the previous arming rather than leaking it"
    );
}

#[tokio::test(start_paused = true)]
async fn only_the_last_instance_leaving_stops_the_name_groups_cron_worker() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string());
        app.instances = 2;
    });
    handle.start(vec![app.clone()]).await.unwrap();
    for (id, instance, pid) in [(0, 0, 1000), (1, 1, 1001)] {
        let entry = armed_entry(id, instance, pid, app.clone(), &paths);
        registry.arm(&entry, idle_prober(), &rig.extras, &handle);
        tokio::task::yield_now().await;
    }

    registry.disarm(0, "web");
    assert_eq!(
        registry.groups["web"].members,
        HashSet::from([1]),
        "a non-last disarm drops only its own membership"
    );
    cross_one_hour().await;
    // `ProcessSelector::Name` reaches both online instances, so one
    // occurrence produces two `Restart` events. A leftover would read as a
    // worker that outlived its disarm.
    expect_restart(&mut rx, "web", EVENT_WAIT).await;
    expect_restart(&mut rx, "web", EVENT_WAIT).await;

    registry.disarm(1, "web");
    assert!(
        !registry.groups.contains_key("web"),
        "the last instance leaving must take the group with it"
    );
    assert_no_restart_within(&mut rx, "web", PAST_THE_NEXT_OCCURRENCE).await;
}

#[tokio::test(start_paused = true)]
async fn disarming_an_id_that_was_never_armed_leaves_the_group_alone() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 * * * *".to_string())
    });
    handle.start(vec![app.clone()]).await.unwrap();
    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    tokio::task::yield_now().await;

    registry.disarm(99, "web"); // a name that exists, an id that does not
    registry.disarm(0, "other"); // an id that exists, a name that does not

    assert_eq!(registry.groups["web"].members, HashSet::from([0]));
    cross_one_hour().await;
    expect_restart(&mut rx, "web", EVENT_WAIT).await;
}

// The clock makes the claim: a fresh worker on this pattern reads it once
// before returning, so a second reading means a second worker.
#[tokio::test(start_paused = true)]
async fn a_cron_worker_that_ended_on_its_own_is_rebuilt_on_the_next_arm() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    // 30 February: a pattern croner parses and finds no occurrence for,
    // which is the `Ok(None)` arm `spawn_cron_worker` returns on.
    let app = app_with("web", |app| {
        app.cron_restart = Some("0 0 30 2 *".to_string());
    });

    registry.arm(
        &armed_entry(0, 0, 1000, app.clone(), &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    settle_finished(
        registry.groups["web"]
            .cron
            .as_ref()
            .expect("the first arm spawns a worker"),
    )
    .await;
    assert_eq!(
        rig.clock.reads(),
        1,
        "a worker on a pattern with no occurrence reads the clock once and ends"
    );

    registry.arm(
        &armed_entry(0, 0, 2000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    settle_finished(
        registry.groups["web"]
            .cron
            .as_ref()
            .expect("the re-arm must leave a worker behind"),
    )
    .await;
    assert_eq!(
        rig.clock.reads(),
        2,
        "a re-arm must rebuild a name-group task that ended on its own"
    );
}

// This app asks for a watch and gets none. Under a build-keyed membership it
// would be in no group at all, so stopping a later instance whose watch did
// arm would tear the watch down with this one still online.
#[tokio::test(start_paused = true)]
async fn an_instance_whose_watch_could_not_be_armed_still_joins_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let limit = MemSize::from_bytes(500);
    // `canonicalize` failing is the one watch-arming failure `normalize`
    // lets through: it never checks that the cwd resolves.
    let missing = dir.path().join("no-such-directory");
    let app = app_with("web", |app| {
        app.watch = true;
        app.cwd = Some(missing.display().to_string());
        app.max_memory = Some(limit);
    });

    registry.arm(
        &armed_entry(0, 0, 1000, app, &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );

    let group = registry
        .groups
        .get("web")
        .expect("a watched app is a member of its group whether or not the watch armed");
    assert!(group.watch.is_none(), "this app's watch cannot have armed");
    assert_eq!(group.members, HashSet::from([0]));
    assert_eq!(
        rig.enforcer.arms(),
        vec![ArmCall {
            id: 0,
            root_pid: 1000,
            limit,
        }],
        "a watch that could not be armed must not take the memory limit with it"
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_the_registry_stops_the_liveness_loop_it_armed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let mut rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let app = app_with("web", |app| {
        app.liveness_probe = Some(ProbeConfig {
            failure_threshold: 1,
            ..probe_config(ProbeKind::Tcp, "localhost:5432")
        });
    });
    let interval = app
        .config()
        .liveness_probe
        .as_ref()
        .expect("the fixture just set one")
        .interval
        .as_duration();

    let mut kept = ExtrasRegistry::default();
    kept.arm(
        &armed_entry(8, 1, 5678, app.clone(), &paths),
        failing_prober(),
        &rig.extras,
        &handle,
    );
    let mut discarded = ExtrasRegistry::default();
    discarded.arm(
        &armed_entry(7, 0, 1234, app, &paths),
        failing_prober(),
        &rig.extras,
        &handle,
    );
    drop(discarded);

    let failure = expect_liveness(&mut rig.liveness, EVENT_WAIT).await;
    assert_eq!(
        failure,
        LivenessReport {
            id: 8,
            pid: 5678,
            epoch: 1
        },
        "only the registry that is still alive may report"
    );
    assert_no_liveness_within(&mut rig.liveness, interval * 3).await;
}

// Two names, not two instances of one: the bus attributes a restart to a
// name, so the control and the subject have to be tellable apart on the
// wire. `kept` is that control.
#[tokio::test(start_paused = true)]
async fn dropping_the_registry_stops_the_name_group_worker_it_armed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, mut rx, _fixture) = spawn_test_fixture();
    let rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let hourly = |name: &str| {
        app_with(name, |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })
    };
    handle
        .start(vec![hourly("kept"), hourly("dropped")])
        .await
        .unwrap();

    let mut kept = ExtrasRegistry::default();
    kept.arm(
        &armed_entry(0, 0, 1000, hourly("kept"), &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    let mut discarded = ExtrasRegistry::default();
    discarded.arm(
        &armed_entry(1, 0, 1001, hourly("dropped"), &paths),
        idle_prober(),
        &rig.extras,
        &handle,
    );
    // Lets both workers commit to their first `next` while the clock still
    // reads close to now.
    tokio::task::yield_now().await;
    drop(discarded);

    cross_one_hour().await;
    expect_restart(&mut rx, "kept", EVENT_WAIT).await;
    assert_no_restart_within(&mut rx, "dropped", PAST_THE_NEXT_OCCURRENCE).await;
}

// A healthy liveness loop never ends on its own, so a stopped sheep would
// leak a task probing a pid that is gone.
#[tokio::test(start_paused = true)]
async fn disarming_an_id_stops_its_liveness_loop() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let mut rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.instances = 2;
        app.liveness_probe = Some(ProbeConfig {
            failure_threshold: 1,
            ..probe_config(ProbeKind::Tcp, "localhost:5432")
        });
    });
    let interval = app
        .config()
        .liveness_probe
        .as_ref()
        .expect("the fixture just set one")
        .interval
        .as_duration();

    for (id, instance, pid) in [(0, 0, 1000), (1, 1, 1001)] {
        registry.arm(
            &armed_entry(id, instance, pid, app.clone(), &paths),
            failing_prober(),
            &rig.extras,
            &handle,
        );
    }
    registry.disarm(0, "web");

    let failure = expect_liveness(&mut rig.liveness, EVENT_WAIT).await;
    assert_eq!(
        failure,
        LivenessReport {
            id: 1,
            pid: 1001,
            epoch: 1
        },
        "only the instance that is still armed may report"
    );
    assert_no_liveness_within(&mut rig.liveness, interval * 3).await;
}

#[tokio::test(start_paused = true)]
async fn a_liveness_threshold_reports_this_instances_id_and_pid() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let (handle, _rx, _fixture) = spawn_test_fixture();
    let mut rig = rig(DEFAULT_MAX_CRON_SLEEP);
    let mut registry = ExtrasRegistry::default();
    let app = app_with("web", |app| {
        app.liveness_probe = Some(ProbeConfig {
            failure_threshold: 2,
            ..probe_config(ProbeKind::Tcp, "localhost:5432")
        });
    });
    let interval = app
        .config()
        .liveness_probe
        .as_ref()
        .expect("the fixture just set one")
        .interval
        .as_duration();

    registry.arm(
        &armed_entry(7, 0, 1234, app, &paths),
        Arc::new(ScriptedProber::new(vec![Err(ProbeFailure::Timeout)])),
        &rig.extras,
        &handle,
    );

    let failure = expect_liveness(&mut rig.liveness, EVENT_WAIT).await;
    assert_eq!(
        failure,
        LivenessReport {
            id: 7,
            pid: 1234,
            epoch: 1
        }
    );
    // The scripted prober repeats its last outcome forever, so a loop still
    // probing after its report would report again inside this window.
    assert_no_liveness_within(&mut rig.liveness, interval * 3).await;
}

// `notify-debouncer-full` derives its poll tick as `delay / 4` and sleeps it
// on a dedicated OS thread, so a zero makes that thread spin. A direct call,
// since `normalize` refuses `watch_delay = "0"` and no fixture can carry one
// as far as `ExtrasRegistry::arm`.
#[test]
fn a_zero_watch_delay_is_floored_before_it_reaches_the_debouncer() {
    let mut app = AppConfig::minimal("web", "./srv");
    app.watch_delay = Some(UpDuration::from_millis(0));
    assert_eq!(watch_delay_for(&app), MIN_WATCH_DELAY);
}

// `normalize` accepts every non-zero `watch_delay`, so a floor above one
// millisecond would lengthen a round trip the user shortened on purpose.
#[test]
fn a_watch_delay_the_config_layer_accepts_is_never_clamped() {
    let mut app = AppConfig::minimal("web", "./srv");
    app.watch_delay = Some(UpDuration::from_millis(1));
    assert_eq!(watch_delay_for(&app), Duration::from_millis(1));

    app.watch_delay = Some(UpDuration::from_millis(20));
    assert_eq!(watch_delay_for(&app), Duration::from_millis(20));

    app.watch_delay = None;
    assert_eq!(watch_delay_for(&app), DEFAULT_WATCH_DELAY);
}
