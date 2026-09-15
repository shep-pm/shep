//! Tests for loading a Flockfile onto a running flock.
//!
//! A load may not overwrite what an operator established, and a field that
//! cannot take effect underneath a running process parks as pending rather than
//! being applied.

use super::*;

/// Additive is the default because a Flockfile arrives from the app's own
/// repository: a merged pull request must not change a running flock.
#[tokio::test(start_paused = true)]
async fn a_file_load_does_not_overwrite_an_established_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |app| app.max_restarts = 3)]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script", "max_restarts"],
            vec![("max_restarts", serde_json::json!(3))],
        ),
    )
    .unwrap();

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 99;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().max_restarts,
        3,
        "the file overwrote a key the operator had set"
    );
    assert!(
        reply[0].applied.is_empty(),
        "nothing applied, so nothing may be reported as applied: {reply:?}"
    );
}

/// Appending an unestablished key is what makes a template update reach an
/// app at all.
#[tokio::test(start_paused = true)]
async fn a_file_load_appends_a_key_nobody_had_established() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(&["name", "script"], Vec::new()),
    )
    .unwrap();

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_memory = Some(MemSize::from_bytes(512 << 20));
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_memory"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().max_memory,
        Some(MemSize::from_bytes(512 << 20)),
        "a key nobody had established must be appended"
    );
    assert_eq!(reply[0].applied, vec!["max_memory".to_string()]);
}

#[tokio::test(start_paused = true)]
async fn a_live_field_lands_on_the_stored_spec() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 42;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(actor.sheep[&0].entry.spec.config().max_restarts, 42);
    assert_eq!(reply[0].applied, vec!["max_restarts".to_string()]);
    assert!(reply[0].pending.is_empty(), "{reply:?}");
}

/// `depends_on` is `ApplyGroup::NextSpawn` and reports as in force
/// anyway, the carve-out `autostart` already had and for the same reason:
/// nothing reads either at a spawn. `plan_for_names` reads `depends_on`
/// when a batch is ordered, off the stored spec, so the new value already
/// governs the next restart, the next shutdown and the next boot.
/// Reporting it pending would send an operator to `shep reload` to
/// promote a value that is already promoted.
#[tokio::test(start_paused = true)]
async fn a_loaded_depends_on_is_in_force_and_never_reports_as_pending() {
    // fails if `depends_on` is left in the ordinary NextSpawn arm, which
    // shows the sheep as `!1` in `shep flock` and tells `shep describe`
    // to reload for a value the next ordered walk is already reading.
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.depends_on = vec!["db".to_string()];
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "depends_on"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().depends_on,
        vec!["db".to_string()],
        "the edge has to be on the stored spec for the walk to read it"
    );
    assert_eq!(reply[0].applied, vec!["depends_on".to_string()]);
    assert!(
        reply[0].pending.is_empty(),
        "an edge already being read is not pending: {reply:?}"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_none(),
        "nothing may be parked for a respawn to promote"
    );
}

/// A load must never kill a process.
#[tokio::test(start_paused = true)]
async fn a_needs_respawn_field_parks_as_pending_and_leaves_the_child_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.pid,
        Some(APPLY_FIRST_PID),
        "the running child must not have been replaced"
    );
    assert!(
        entry.spec.config().env.is_empty(),
        "the running child's own config must keep describing what it was spawned from"
    );
    assert_eq!(
        entry
            .pending
            .as_ref()
            .expect("a NeedsRespawn change parks as pending")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue")
    );
    assert_eq!(reply[0].pending, vec!["env".to_string()]);
    assert!(reply[0].applied.is_empty(), "{reply:?}");
    assert_eq!(
        reply[0]
            .app
            .as_ref()
            .expect("an applied app is recorded")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "a reboot spawns everything afresh, so what it comes up on is the \
         full merge and not what the running child is on"
    );
}

/// An app whose merge is invalid refuses whole; the rest of the flock
/// still applies.
#[tokio::test(start_paused = true)]
async fn an_unnormalizable_merge_refuses_one_app_and_applies_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |_| {}), app_with("worker", |_| {})]);

    // Two instances sharing one explicit log path, with no `{{instance}}`
    // in it and no `merge_logs`: the one refusal a merge can produce out
    // of two individually-legal keys.
    let mut broken = AppConfig::minimal("web", "./srv");
    broken.instances = 2;
    broken.out_file = Some("/tmp/web.log".to_string());
    let mut worker = AppConfig::minimal("worker", "./srv");
    worker.max_restarts = 7;

    // `Policy`, not `None`: a plain load holds `instances` out of the
    // merge, so the two keys could not meet and the merge would be valid.
    let reply = apply_config(
        &mut actor,
        vec![
            declared_app(broken, &["name", "script", "instances", "out_file"]),
            declared_app(worker, &["name", "script", "max_restarts"]),
        ],
        ResetDepth::Policy,
    )
    .await;

    assert!(
        reply[0].refused.is_some(),
        "an unnormalizable merge must refuse: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0].entry.spec.config().instances,
        1,
        "a refused app's stored config must be untouched"
    );
    assert!(actor.sheep[&0].entry.spec.config().out_file.is_none());
    assert_eq!(actor.sheep[&1].entry.spec.config().max_restarts, 7);
    assert_eq!(reply[1].applied, vec!["max_restarts".to_string()]);
}

/// A load never prunes: the daemon has no record of which Flockfile an app
/// came from, so `shep start ./a/Flockfile.toml` followed by
/// `./b/Flockfile.toml` would have the second wipe the first's flock.
#[tokio::test(start_paused = true)]
async fn an_app_absent_from_the_file_is_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |_| {}), app_with("worker", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 5;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    let worker = &actor.sheep[&1].entry;
    assert_eq!(worker.spec.config().name, "worker");
    assert_eq!(worker.status, ProcStatus::Online);
    assert_eq!(worker.pid, Some(APPLY_FIRST_PID + 1));
    assert!(
        reply.iter().all(|applied| applied.name != "worker"),
        "a load must not claim to have touched an app the file never named: {reply:?}"
    );
}

/// The drainee holds the lower id, so `ids.first()` reaches the instance on
/// its way out, and a spec derived from it lands on the live replacement.
#[tokio::test(start_paused = true)]
async fn a_load_during_a_reload_reads_the_replacement_and_not_the_drainee() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.instances = 2;
            app.cwd = Some("/srv/new".to_string());
        })],
    );
    // Instance 0 is the drainee: the lower id, still on the config the
    // reload is replacing, and already `Stopping`.
    {
        let slot = actor
            .sheep
            .get_mut(&0)
            .expect("the fixture registered two slots");
        slot.entry.status = ProcStatus::Stopping;
        slot.entry.reload = ReloadState::Drainee { new_id: Some(1) };
        slot.entry.spec = app_with("web", |app| {
            app.instances = 2;
            app.cwd = Some("/srv/old".to_string());
        });
    }

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 7;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.sheep[&1].entry.spec.config().cwd.as_deref(),
        Some("/srv/new"),
        "the replacement's spec must keep describing what the replacement \
         was spawned from: {reply:?}"
    );
    assert_eq!(actor.sheep[&1].entry.spec.config().max_restarts, 7);
    assert_eq!(reply[0].applied, vec!["max_restarts".to_string()]);
    assert!(reply[0].pending.is_empty(), "{reply:?}");
}

/// A dog runs at the daemon's own trust level, so a file naming one and
/// carrying a `script` would replace an adopted binary without adopting
/// anything, while `shep dogs` went on reporting the previous dog.
#[tokio::test(start_paused = true)]
async fn a_file_naming_a_dog_is_refused_rather_than_merged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("metrics", |_| {})]);
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registered one slot")
        .entry
        .dog = Some(DogSource::BuiltIn);

    let mut file = AppConfig::minimal("metrics", "/opt/evil");
    file.max_restarts = 42;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        reply[0].refused.as_deref(),
        Some(
            "metrics is a dog, and a dog's config comes from `shep adopt` rather than \
             from a Flockfile"
        ),
        "{reply:?}"
    );
    let entry = &actor.sheep[&0].entry;
    assert_eq!(entry.spec.config().script, "./srv");
    assert_eq!(
        entry.spec.config().max_restarts,
        AppConfig::default().max_restarts
    );
    assert!(entry.pending.is_none(), "a refused app parks nothing");
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "metrics")
            .unwrap()
            .is_none(),
        "a refused app establishes nothing"
    );
}

/// A reset resolves an undeclared key to the file as loaded, not to the
/// compiled default. The CLI defaults `cwd` to the Flockfile's own
/// directory without the document declaring it, so the compiled default
/// would park `cwd: None` and the next restart could not find the script.
/// `fold` and `interpreter` arrive the same way.
#[tokio::test(start_paused = true)]
async fn a_reset_against_an_unchanged_file_keeps_a_defaulted_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.cwd = Some("/srv/web".to_string());
            app.fold = Some("edge".to_string());
        })],
    );

    // What `shep start Flockfile.toml --reset` sends for an unmodified
    // two-line file: the resolved config carries the defaulted `cwd` and
    // the `--fold`, and the document declared neither.
    let mut file = AppConfig::minimal("web", "./srv");
    file.cwd = Some("/srv/web".to_string());
    file.fold = Some("edge".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script"])],
        ResetDepth::Policy,
    )
    .await;

    // `cwd` is `NeedsRespawn`, so the damage lands in `pending` rather
    // than on the running spec.
    assert!(
        actor.sheep[&0].entry.pending.is_none(),
        "an unchanged file has nothing to park: {reply:?}"
    );
    assert!(reply[0].pending.is_empty(), "{reply:?}");
    let config = actor.sheep[&0].entry.spec.config().clone();
    assert_eq!(config.cwd.as_deref(), Some("/srv/web"));
    assert_eq!(config.fold.as_deref(), Some("edge"));
}

/// `--reset=policy` restores settings, declared or not, and leaves env;
/// `--reset=all` takes env with it and drops the record.
#[tokio::test(start_paused = true)]
async fn reset_restores_every_setting_and_only_reset_all_takes_env() {
    // The operator's three edits since the file established name, script
    // and max_restarts: one over a key the file declares, one env key and
    // one field the file has never mentioned.
    let stored = |name: &str| {
        app_with(name, |app| {
            app.max_restarts = 3;
            app.env = BTreeMap::from([("OPERATOR".to_string(), "1".to_string())]);
            app.min_uptime = UpDuration::from_millis(9000);
        })
    };
    let record = || {
        established(
            &["name", "script", "max_restarts"],
            vec![
                ("max_restarts", serde_json::json!(3)),
                ("env", serde_json::json!({ "OPERATOR": "1" })),
                ("min_uptime", serde_json::json!("9000ms")),
            ],
        )
    };
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.max_restarts = 10;
        declared_app(file, &["name", "script", "max_restarts"])
    };

    let settings_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&settings_dir, &[stored("web")]);
    shep_core::overrides::put(&actor.paths.overrides, "web", &record()).unwrap();
    apply_config(&mut actor, vec![file()], ResetDepth::Policy).await;
    let settings = actor.sheep[&0].entry.spec.config().clone();
    let settings_pending = actor.sheep[&0].entry.pending.clone();
    assert_eq!(
        settings.max_restarts, 10,
        "--reset=policy puts a declared setting back to the file's"
    );
    assert_eq!(
        settings.min_uptime,
        AppConfig::default().min_uptime,
        "--reset=policy puts a field the file never declared back to the \
         file's own value, which for an undeclared key is the compiled \
         default"
    );
    assert_eq!(
        settings_pending
            .as_ref()
            .map_or(&settings.env, |app| &app.config().env)
            .get("OPERATOR")
            .map(String::as_str),
        Some("1"),
        "--reset=policy keeps env"
    );

    let all_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&all_dir, &[stored("web")]);
    shep_core::overrides::put(&actor.paths.overrides, "web", &record()).unwrap();
    apply_config(&mut actor, vec![file()], ResetDepth::All).await;
    let all = actor.sheep[&0].entry.spec.config().clone();
    let all_pending = actor.sheep[&0].entry.pending.clone();
    assert_eq!(all.max_restarts, 10);
    assert_eq!(
        all.min_uptime,
        AppConfig::default().min_uptime,
        "--reset=all drops a field the operator added"
    );
    assert!(
        all_pending
            .as_ref()
            .expect("dropping an env key needs a respawn")
            .config()
            .env
            .is_empty(),
        "--reset=all drops an env key the operator added"
    );
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "--reset=all removes the override record"
    );
}
