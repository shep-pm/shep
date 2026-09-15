//! Tests for what a listing says about pending and overridden fields.
//!
//! `to_info` names the fields that are waiting and the fields an operator has
//! overridden, but never an override's value: those can hold secrets. Promoting
//! a user change has to re-resolve credentials.

use super::*;

/// A pending field an operator cannot see is a silent divergence.
#[tokio::test(start_paused = true)]
async fn to_info_reports_the_pending_fields_names_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    assert!(
        entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.pending, Some(vec!["env".to_string()]));
}

/// The daemon's one production construction site converts `MemSize` to
/// raw bytes for the wire. The ceiling is chosen off a round megabyte
/// boundary so a unit mix-up (bytes vs. KiB vs. MiB) could not pass by
/// coincidence.
#[tokio::test(start_paused = true)]
async fn to_info_carries_a_sheep_s_configured_memory_ceiling_in_bytes() {
    const CEILING_BYTES: u64 = 43_000_001;
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(CEILING_BYTES));
        })],
    );

    let entry = &actor.sheep[&0].entry;
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.max_memory, Some(CEILING_BYTES));
}

/// The rules reach a client through the listing and nothing else, so a
/// row that drops them leaves the client reading lines the app already
/// explained. Two rules, since order is the contract and one proves no
/// order.
#[tokio::test(start_paused = true)]
async fn to_info_carries_a_sheep_s_declared_level_rules_in_order() {
    let rules = vec![
        LevelRule {
            pattern: r"\[ERROR\]".to_string(),
            level: LineLevel::Error,
        },
        LevelRule {
            pattern: r"\[WARN\]".to_string(),
            level: LineLevel::Warn,
        },
    ];
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| app.level_rules = rules.clone())],
    );

    let entry = &actor.sheep[&0].entry;
    assert_eq!(to_info(entry, &actor.smits).level_rules, rules);
}

/// A dog's `AppConfig::minimal` sets no ceiling, so its `ProcessInfo`
/// must report `None` rather than inheriting a stray value.
#[tokio::test(start_paused = true)]
async fn to_info_reports_none_for_a_dog_with_no_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(&dir, &[dog_app("watcher")]);

    let entry = &actor.sheep[&0].entry;
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.max_memory, None);
}

/// A scale-up calls `overridden_for` once per new instance, so a cache miss
/// costs one locked file read per slot. The store is seeded with a
/// different answer from the sibling's cache, so the sibling winning is the
/// assertion.
#[tokio::test(start_paused = true)]
async fn overridden_for_prefers_a_live_sibling_over_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    actor.sheep.get_mut(&0).unwrap().entry.overridden = vec!["cwd".to_string()];
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("max_restarts", serde_json::json!(9))],
        ),
    )
    .unwrap();

    assert_eq!(actor.overridden_for("web"), vec!["cwd".to_string()]);
}

/// A muster restore and a handover installation both install one sheep at a
/// time, before there is a sibling to ask.
#[tokio::test(start_paused = true)]
async fn overridden_for_reads_the_store_when_no_sibling_exists() {
    let dir = tempfile::tempdir().unwrap();
    let (actor, _enforcer) = actor_over(&dir, &[]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("max_restarts", serde_json::json!(9))],
        ),
    )
    .unwrap();

    assert_eq!(
        actor.overridden_for("web"),
        vec!["max_restarts".to_string()]
    );
    assert_eq!(
        actor.overridden_for("nobody-has-heard-of-this-app"),
        Vec::<String>::new(),
        "an unreadable-or-empty answer for a name the store has never seen"
    );
}

/// An override with nothing to show it is a silent divergence, the same
/// class as an unreported `pending`.
#[tokio::test(start_paused = true)]
async fn to_info_reports_the_overridden_field_names_the_store_holds() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |app| app.max_restarts = 7)]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("max_restarts", serde_json::json!(7))],
        ),
    )
    .unwrap();

    let file = AppConfig::minimal("web", "./srv");
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.overridden,
        vec!["max_restarts".to_string()],
        "the cache must mirror what this load wrote back to the override store"
    );
    let info = to_info(entry, &actor.smits);
    assert_eq!(info.overridden, Some(vec!["max_restarts".to_string()]));
}

/// `AppOverrides::fields` is a `serde_json::Map` that can hold anything, so
/// the guarantee is that `Actor::apply_one` and `Actor::overridden_for`
/// extract `.keys()` and never a value. Asserted at the producer, over a
/// store seeded with a secret-shaped value the way `env` arrives there.
#[tokio::test(start_paused = true)]
async fn to_info_never_carries_an_override_value() {
    const SENTINEL: &str = "postgres://sentinel-value-that-must-never-appear";

    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.env = BTreeMap::from([("DATABASE_URL".to_string(), SENTINEL.to_string())]);
        })],
    );
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("env", serde_json::json!({ "DATABASE_URL": SENTINEL }))],
        ),
    )
    .unwrap();

    let file = AppConfig::minimal("web", "./srv");
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script"])],
        ResetDepth::None,
    )
    .await;

    let entry = &actor.sheep[&0].entry;
    let info = to_info(entry, &actor.smits);
    assert_eq!(
        info.overridden,
        Some(vec!["env".to_string()]),
        "the name must still reach the operator"
    );
    let json = serde_json::to_string(&info).unwrap();
    assert!(
        !json.contains(SENTINEL),
        "an override value reached the wire: {json}"
    );
}

/// `credentials` is resolved once so a restart does not change a running
/// app's identity by accident; an operator editing `user` is the one case
/// that must re-resolve.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn promoting_a_user_change_re_resolves_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.user = Some(own_user_name());
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "user"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    let wanted = Credentials {
        uid: nix::unistd::geteuid().as_raw(),
        gid: None,
    };
    assert_eq!(
        actor.runner.spawned_as(0),
        Some(wanted),
        "the replacement must carry the identity the promoted `user` resolves to; `None` \
         here is the fixture's stale resolution, which is the change being ignored"
    );
    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id].entry.credentials,
        SpawnIdentity::Resolved(Some(wanted)),
        "and the replacement records it, so the restart after this one reuses it"
    );
    assert_eq!(
        actor.sheep[&0].entry.credentials,
        SpawnIdentity::Resolved(None),
        "while the drainee's own identity is untouched: it is still serving under it, and \
         an abandoned swap must not leave it recorded as never looked up"
    );
}

/// Re-resolving on every promotion would mean a passwd lookup per config
/// change, and would defeat the once-only rule.
#[tokio::test(start_paused = true)]
async fn promoting_an_unrelated_change_keeps_the_resolved_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.user = Some(NO_SUCH_USER.to_string())
        })],
    );
    // An unresolvable name makes reuse observable: this value cannot be
    // re-derived, so a spawn carrying it is a spawn that reused it.
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers one instance")
        .entry
        .credentials = SpawnIdentity::Resolved(Some(settled));

    let mut file = AppConfig::minimal("web", "./srv");
    file.args = vec!["--port=8080".to_string()];
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "args"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    assert_eq!(
        actor.runner.spawn_count(),
        1,
        "the replacement must have been spawned at all: an identity re-resolved here could \
         only refuse, and a refusal would abandon the reload"
    );
    assert_eq!(
        actor.runner.spawned_as(0),
        Some(settled),
        "an `args` change is not an identity change, so the replacement runs as whoever the \
         instance was already running as"
    );
    assert_eq!(
        actor.sheep[&0].entry.credentials,
        SpawnIdentity::Resolved(Some(settled)),
        "and the stored resolution is untouched, so no passwd lookup was spent"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the drainee keeps what it was owed; the replacement is what carries it"
    );
    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id].entry.spec.config().args,
        vec!["--port=8080".to_string()],
        "the promotion itself must still have happened"
    );
}

/// `apply_one` derives one spec from `ids_of_name`'s first id, always
/// instance 0, and writes it onto every sibling. A promotion that diffed
/// `pending` against `spec` would find the `user` change instance 1 has not
/// applied already sitting on instance 1's spec. Three loads, not two:
/// by the third, instance 1's spec is already flattened, so a load that
/// recomputed the flag would clear the first load's decision.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_sibling_that_has_not_promoted_yet_still_re_resolves_after_later_loads() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 3)]);
    // The identity the three instances already run under, and one no lookup
    // could produce, so a spawn carrying it is a spawn that reused it.
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    for id in [0, 1, 2] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers three")
            .entry
            .credentials = SpawnIdentity::Resolved(Some(settled));
    }

    let user_change = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.instances = 3;
        file.user = Some(own_user_name());
        vec![declared_app(file, &["name", "script", "instances", "user"])]
    };
    apply_config(&mut actor, user_change(), ResetDepth::None).await;

    // Instance 0 alone: the shape every automatic restart takes.
    actor.respawn(0, true);

    // The same file twice. Each reads its base config off instance 0, which
    // has now promoted, and writes it over instances 1 and 2.
    apply_config(&mut actor, user_change(), ResetDepth::None).await;
    apply_config(&mut actor, user_change(), ResetDepth::None).await;

    actor.respawn(1, true);

    let wanted = Credentials {
        uid: nix::unistd::geteuid().as_raw(),
        gid: None,
    };
    assert_eq!(
        actor.runner.spawned_as(1),
        Some(wanted),
        "instance 1 has still never applied the `user` change, so its promotion must \
         re-resolve; the settled 4242 here is the change being silently dropped"
    );
    assert_eq!(
        actor.sheep[&1].entry.spec.config().user,
        Some(own_user_name()),
        "and its own spec must record what it came up on"
    );
}

/// The drainee goes back to the child it already had, which was never
/// spawned with the parked config, so an entry claiming it with an empty
/// pending slot leaves the next load seeing no drift while the child runs
/// superseded code.
#[tokio::test(start_paused = true)]
async fn an_abandoned_reload_leaves_the_parked_config_where_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    // A live control sender says this instance's task is still there to
    // go back to; the fixture leaves it `None`.
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    actor.sheep.get_mut(&0).expect("the fixture's sheep").ctl = Some(ctl_tx);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;

    actor.advance_reload("web", VecDeque::from([0]));
    actor.handle_reload_deadline("web", actor.reloads["web"].deadline);

    assert!(actor.reloads.is_empty(), "the swap must really be off");
    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.status,
        ProcStatus::Online,
        "the drainee is serving again, so it is the child spawned before the load"
    );
    assert!(
        entry.spec.config().env.is_empty(),
        "and its spec must still describe what that child was spawned from"
    );
    assert_eq!(
        entry
            .pending
            .as_ref()
            .expect("the config is still owed: no child ever came up on it")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue")
    );
}

/// `SpawnIdentity::Unresolved` makes a later spawn resolve from scratch, so
/// for a `user` that has stopped resolving it is a running app whose next
/// restart is refused over the identity it already runs under.
#[tokio::test(start_paused = true)]
async fn an_abandoned_reload_leaves_the_drainees_identity_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers one instance")
        .entry
        .credentials = SpawnIdentity::Resolved(Some(settled));

    // A `user` that cannot resolve, so the reload is abandoned at the one
    // point that runs before anything else in `spawn_replacement`.
    let mut file = AppConfig::minimal("web", "./srv");
    file.user = Some(NO_SUCH_USER.to_string());
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "user"])],
        ResetDepth::None,
    )
    .await;

    actor.advance_reload("web", VecDeque::from([0]));

    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "the fixture must really refuse the replacement, or this case proves nothing"
    );
    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.credentials,
        SpawnIdentity::Resolved(Some(settled)),
        "the drainee is still serving under this identity, so the abandoned swap must not \
         record it as never looked up"
    );
    assert!(
        entry.pending.is_some(),
        "and it is still owed the config that swap was going to bring"
    );
}

/// `readiness_probe` is `NextSpawn` and lands on the stored spec at once,
/// while `wait_ready` is `NeedsRespawn` and parks, so an app moving from
/// channel readiness to an HTTP probe holds both. `wait_ready` wins in
/// `ReadinessSource::of`, so an ordering read from the stored spec says
/// overlap while the replacement comes up probe-gated: two instances on one
/// address, with a probe the drainee answers.
#[tokio::test(start_paused = true)]
async fn a_reload_orders_itself_by_the_config_its_replacement_will_carry() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&dir, &[app_with("web", |app| app.wait_ready = true)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.wait_ready = false;
    file.readiness_probe = Some(probe_config(ProbeKind::Tcp, "127.0.0.1:9"));
    apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "wait_ready", "readiness_probe"],
        )],
        ResetDepth::None,
    )
    .await;
    let entry = &actor.sheep[&0].entry;
    assert!(
        entry.spec.config().wait_ready && entry.spec.config().readiness_probe.is_some(),
        "the fixture must really hold both at once, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    let job = &actor.reloads["web"];
    assert_eq!(
        job.mode,
        ReloadMode::Serial,
        "the replacement is probe-gated, and a probe cannot say which of two overlapping \
         instances answered it"
    );
    assert_eq!(
        job.swap.phase,
        ReloadPhase::DrainFirst,
        "so the drain runs first and nothing is spawned yet"
    );
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "an overlap here would put a second instance on the drainee's address"
    );
}

/// New instances are spawned from the config the old ones are running, read
/// off instance 0, and `spawn_fresh` registers no pending slot, so a
/// `shep stock` during a parking window would leave them on superseded
/// config with nothing saying a restart is due.
#[tokio::test(start_paused = true)]
async fn a_scale_up_carries_the_parked_config_onto_the_instances_it_creates() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "instances", "env"])],
        ResetDepth::None,
    )
    .await;

    // The standalone verb, not the count inside a load: `apply_one` parks
    // onto every slot after its own scale.
    let (reply, mut answer) = oneshot::channel();
    actor.handle_scale("web", 4, reply);
    answer
        .try_recv()
        .expect("handle_scale answers before it returns")
        .expect("the fixture has scripts enough to scale to four");

    for id in [2, 3] {
        assert_eq!(
            actor.sheep[&id]
                .entry
                .pending
                .as_ref()
                .unwrap_or_else(|| panic!("instance {id} must be owed the parked config"))
                .config()
                .env
                .get("MODE")
                .map(String::as_str),
            Some("blue"),
            "an instance created during a parking window is owed the same config as its \
             siblings"
        );
        assert!(
            actor.sheep[&id].entry.spec.config().env.is_empty(),
            "and its own spec still describes what it was actually spawned from"
        );
    }
}

/// A parked config copied verbatim leaves every slot holding
/// `pending.instances = 2` against a spec of 4, so `drifted_fields` reports
/// `instances` pending forever and the reload that promotes writes the
/// count back down.
#[tokio::test(start_paused = true)]
async fn a_scale_updates_the_count_inside_the_config_it_carries() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "instances", "env"])],
        ResetDepth::None,
    )
    .await;

    let (reply, mut answer) = oneshot::channel();
    actor.handle_scale("web", 4, reply);
    answer
        .try_recv()
        .expect("handle_scale answers before it returns")
        .expect("the fixture has scripts enough to scale to four");

    for id in 0..4 {
        let entry = &actor.sheep[&id].entry;
        assert_eq!(
            entry
                .pending
                .as_ref()
                .unwrap_or_else(|| panic!("instance {id} must be owed the parked config"))
                .config()
                .instances,
            4,
            "the count a scale achieved, not the one an earlier load parked"
        );
        assert!(
            !to_info(entry, &actor.smits)
                .pending
                .unwrap_or_default()
                .contains(&"instances".to_string()),
            "a reload owes this instance nothing about the count"
        );
    }
}

/// The `Live` fields ride across on the carried `AppConfig`; everything
/// parked would vanish, and the next load would compare against a spec that
/// already matched. Asserted through a promotion, since a config that
/// arrives without its flag promotes on the identity the flag exists to
/// replace, and only a spawn shows that.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_parked_config_and_its_reset_decision_survive_a_handover() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[]);

    // The predecessor's entry: registered, not running, owed a `user`
    // change, and settled on an identity no lookup could produce.
    let settled = Credentials {
        uid: 4242,
        gid: None,
    };
    let entry = ProcessEntry {
        id: 7,
        spec: app_with("web", |_| {}),
        pending: Some(app_with("web", |app| app.user = Some(own_user_name()))),
        pending_reidentifies: true,
        overridden: Vec::new(),
        instance: 0,
        status: ProcStatus::Stopped,
        pid: None,
        restarts: 0,
        started_at: None,
        budget: RestartBudget::default(),
        reload: ReloadState::None,
        credentials: SpawnIdentity::Resolved(Some(settled)),
        out_file: PathBuf::new(),
        err_file: PathBuf::new(),
        dog: None,
        last_exit: None,
    };
    let carried =
        CarriedSheep::from_entry(&entry, 0, CarriedFds::none(), false, None, false, None);

    // Through serde, the boundary a handover crosses: an accessor reading
    // the source entry proves nothing about the blob.
    let crossed: CarriedSheep = serde_json::from_value(serde_json::to_value(&carried).unwrap())
        .expect("this daemon reads what it writes");

    actor
        .install_adopted(without_handles(crossed), &Arc::new(AdoptedReaper::new()))
        .expect("a registered-and-stopped sheep installs with nothing to adopt");

    assert!(
        actor.sheep[&7].entry.pending.is_some(),
        "the successor must still owe this sheep the change its predecessor parked"
    );

    actor.respawn(7, true);

    assert_eq!(
        actor.runner.spawned_as(0),
        Some(Credentials {
            uid: nix::unistd::geteuid().as_raw(),
            gid: None,
        }),
        "and promoting it must re-resolve: the settled 4242 here is the reset decision \
         lost in the blob, which is the identity change silently dropped"
    );
    assert_eq!(
        actor.sheep[&7].entry.spec.config().user,
        Some(own_user_name()),
        "and the promoted config is what the successor now records"
    );
}
