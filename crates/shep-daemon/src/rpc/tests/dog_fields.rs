//! Config-field edits: `autostart` and `depends_on` live vs parked,
//! a dog refused this door, and every `SetSheepField` variant reaching
//! its own arm rather than falling through to the wildcard.

use super::*;

/// Sends one `SetSheepField` and hands back the reply.
async fn set_field(
    ctx: &RpcContext,
    id: u64,
    name: &str,
    key: &str,
    value: serde_json::Value,
) -> Result<Response, RpcError> {
    reply_of(
        dispatch(
            envelope(
                id,
                Request::SetSheepField {
                    name: name.to_string(),
                    key: key.to_string(),
                    value,
                },
            ),
            ctx,
        )
        .await,
    )
    .result
}

/// The same edit through `ApplyConfig` at `ResetDepth::File` moves the
/// field and spends the override in `merge_declared`, so the key drops
/// from `overridden` and the pane's `*` marker never appears for a
/// value the operator just set. Asserted through both `SheepConfig`,
/// which the pane reads, and `ListFlock`, which the CFG column reads,
/// because the request alone can be correct while both derived views
/// are wrong.
#[tokio::test(start_paused = true)]
async fn a_field_edit_is_reported_as_an_operator_override() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let view = sheep_config_view(&h.ctx, 2, "web").await;
    assert!(
        !view.overridden.contains(&"max_restarts".to_string()),
        "nothing is overridden before the edit: {:?}",
        view.overridden
    );

    let reply = set_field(&h.ctx, 3, "web", "max_restarts", serde_json::json!(40)).await;
    assert!(
        matches!(reply, Ok(Response::SheepFieldSet { .. })),
        "{reply:?}"
    );

    let stored = shep_core::overrides::get(&h.ctx.paths.overrides, "web")
        .unwrap()
        .expect("the edit is recorded");
    assert_eq!(stored.fields["max_restarts"], 40);

    let view = sheep_config_view(&h.ctx, 4, "web").await;
    assert_eq!(view.config.max_restarts, 40, "the pane shows the new value");
    assert!(
        view.overridden.contains(&"max_restarts".to_string()),
        "the `*` marker reads this: {:?}",
        view.overridden
    );

    // The CFG column's own source, which is a different code path from
    // the pane's and is the half that was silently wrong.
    let infos = list_flock(&h.ctx, 5).await;
    let web = infos
        .iter()
        .find(|info| info.name == "web")
        .expect("web is in the flock");
    assert!(
        web.overridden
            .as_deref()
            .is_some_and(|fields| fields.contains(&"max_restarts".to_string())),
        "{:?}",
        web.overridden
    );
}

/// The four-way apply classification governs this door: a `Live`
/// field is in force now, a `NeedsRespawn` field parks and says so,
/// and the pane's cost column promises exactly this.
#[tokio::test(start_paused = true)]
async fn a_live_field_applies_now_and_a_respawn_field_parks() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = set_field(&h.ctx, 2, "web", "max_restarts", serde_json::json!(40)).await;
    let Ok(Response::SheepFieldSet { pending, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert!(!pending, "max_restarts is Live and is in force now");
    let view = sheep_config_view(&h.ctx, 3, "web").await;
    assert!(
        !view.pending.contains(&"max_restarts".to_string()),
        "{:?}",
        view.pending
    );

    let reply = set_field(&h.ctx, 4, "web", "script", serde_json::json!("./next")).await;
    let Ok(Response::SheepFieldSet { pending, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert!(pending, "script needs a respawn");
    let view = sheep_config_view(&h.ctx, 5, "web").await;
    assert!(
        view.pending.contains(&"script".to_string()),
        "{:?}",
        view.pending
    );
    // And the Live edit is still in force beside the parked one.
    assert_eq!(view.config.max_restarts, 40);
    assert_eq!(
        view.overridden,
        ["max_restarts", "script"],
        "both are the operator's"
    );
}

/// The write still lands on a `cwd` that does not exist: `warning` is
/// advisory, never a second way to refuse. `harness`'s own `ShepPaths`
/// point into a real tempdir but skip `boot`'s `mkdir`s, so a directory
/// under it that this test never created is a real absence, not a
/// fixture quirk.
#[tokio::test(start_paused = true)]
async fn a_cwd_that_does_not_exist_writes_and_warns() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let missing = h.ctx.paths.home.join("not-created-yet");
    let reply = set_field(
        &h.ctx,
        2,
        "web",
        "cwd",
        serde_json::json!(missing.display().to_string()),
    )
    .await;
    let Ok(Response::SheepFieldSet { warning, .. }) = reply else {
        panic!("{reply:?}")
    };
    let warning = warning.expect("a cwd that is not there warns");
    assert!(warning.contains("does not exist"), "{warning}");
    assert!(
        warning.contains(&missing.display().to_string()),
        "{warning}"
    );

    // Advisory: the value is still on the sheep's parked config.
    let view = sheep_config_view(&h.ctx, 3, "web").await;
    assert_eq!(view.config.cwd.as_deref(), Some(missing.to_str().unwrap()));
}

/// The negative case beside the one above: a `cwd` that is really
/// there warns nothing, and neither does an unrelated field share a
/// stale complaint about a `cwd` nobody touched this time.
#[tokio::test(start_paused = true)]
async fn an_existing_cwd_and_an_unrelated_field_warn_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = set_field(
        &h.ctx,
        2,
        "web",
        "cwd",
        serde_json::json!(h.ctx.paths.home.display().to_string()),
    )
    .await;
    let Ok(Response::SheepFieldSet { warning, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(warning, None, "the shepherd's own home really exists");

    let reply = set_field(&h.ctx, 3, "web", "max_restarts", serde_json::json!(40)).await;
    let Ok(Response::SheepFieldSet { warning, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(warning, None, "max_restarts carries no path to check");
}

/// `log_path_advisory`'s wiring through this door: an explicit
/// `out_file` whose directory does not exist warns, and the same path
/// once its directory is real does not.
#[tokio::test(start_paused = true)]
async fn an_out_file_whose_directory_is_missing_warns() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let missing = h.ctx.paths.home.join("not-created-yet").join("web.log");
    let reply = set_field(
        &h.ctx,
        2,
        "web",
        "out_file",
        serde_json::json!(missing.display().to_string()),
    )
    .await;
    let Ok(Response::SheepFieldSet { warning, .. }) = reply else {
        panic!("{reply:?}")
    };
    let warning = warning.expect("a missing log directory warns");
    assert!(warning.contains("does not exist"), "{warning}");

    std::fs::create_dir_all(missing.parent().unwrap()).unwrap();
    let reply = set_field(
        &h.ctx,
        3,
        "web",
        "out_file",
        serde_json::json!(missing.display().to_string()),
    )
    .await;
    let Ok(Response::SheepFieldSet { warning, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(warning, None, "the directory is real now");
}

/// One of two cases where `pending` carries information `apply_group`
/// alone cannot: `reached_spec` builds a subset of the config, running
/// plus the one field that reaches, and `normalize` checks fields
/// against each other. `watch` needs a `cwd`, and a `cwd` still parked
/// is not on that subset, so a `Live` field parks anyway.
#[tokio::test(start_paused = true)]
async fn a_live_field_whose_subset_will_not_normalize_parks_instead() {
    let h = harness(vec![ProcScript::never_exits()]);
    // No `cwd`, which is what makes `watch` refusable on its own.
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("web", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(started.result.is_ok(), "{:?}", started.result);

    // `cwd` is NeedsRespawn, so this parks and the running spec still
    // has none.
    let reply = set_field(&h.ctx, 2, "web", "cwd", serde_json::json!("/srv")).await;
    let Ok(Response::SheepFieldSet { pending, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert!(pending, "cwd needs a respawn");

    // `watch` is Live, so `apply_group` alone predicts "in force now".
    // The merge is valid (it carries the parked `cwd`) but the
    // subset is `running + watch`, which is a watch with no directory.
    let reply = set_field(&h.ctx, 3, "web", "watch", serde_json::json!(true)).await;
    let Ok(Response::SheepFieldSet { pending, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(
        apply_group("watch"),
        ApplyGroup::Live,
        "the premise: apply_group predicts this one applies now"
    );
    assert!(
        pending,
        "a Live field the running child cannot be given still parks"
    );

    // And the pane's own durable marker agrees, so an operator who
    // misses the status line still sees it on the row.
    let view = sheep_config_view(&h.ctx, 4, "web").await;
    assert!(
        view.pending.contains(&"watch".to_string()),
        "{:?}",
        view.pending
    );
}

/// `autostart` is `ApplyGroup::NextSpawn`, so `apply_group` predicts a
/// respawn is needed, but `snapshot::restorable` reads it at muster or
/// boot rather than at spawn, so it is in force the moment it lands on
/// the stored spec. `kill_signal` is its group-mate and is genuinely
/// read at spawn, asserted alongside it to pin the carve-out rather
/// than the whole group.
#[tokio::test(start_paused = true)]
async fn autostart_reports_in_force_and_its_group_mate_reports_pending() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    for key in ["autostart", "kill_signal"] {
        assert_eq!(
            apply_group(key),
            ApplyGroup::NextSpawn,
            "the premise: both are the same group"
        );
    }

    let reply = set_field(&h.ctx, 2, "web", "autostart", serde_json::json!(false)).await;
    let Ok(Response::SheepFieldSet { pending, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert!(
        !pending,
        "autostart is read at muster, so a restart would do nothing"
    );

    let reply = set_field(&h.ctx, 3, "web", "kill_signal", serde_json::json!("SIGINT")).await;
    let Ok(Response::SheepFieldSet { pending, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert!(pending, "kill_signal really is read at a spawn");
}

/// `depends_on` shares `autostart`'s carve-out above, and the pane is
/// the door that shows it: a field reported pending puts a `!` on the
/// row and sends the operator to `shep reload` for a value the next
/// ordered walk reads off the stored spec regardless.
#[tokio::test(start_paused = true)]
async fn depends_on_reports_in_force_the_way_autostart_does() {
    // fails if `depends_on` is left in the ordinary NextSpawn arm.
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    assert_eq!(
        apply_group("depends_on"),
        ApplyGroup::NextSpawn,
        "the premise: the carve-out is against its own group"
    );

    let reply = set_field(&h.ctx, 2, "web", "depends_on", serde_json::json!(["db"])).await;
    let Ok(Response::SheepFieldSet { pending, .. }) = reply else {
        panic!("{reply:?}")
    };
    assert!(
        !pending,
        "an ordered walk reads depends_on off the stored spec, so a restart would do nothing"
    );
}

/// The same hole `a_dog_is_refused_an_env_override_rather_than_given_one`
/// closes for `env`, sharper here since this door reaches `script` and
/// `args` directly and a dog runs at the daemon's own trust level.
/// Asserts the store as well as the code: a refusal that still wrote
/// would be the same hole with a better error.
#[tokio::test(start_paused = true)]
async fn a_dog_is_refused_a_config_field_rather_than_given_one() {
    let h = harness(vec![ProcScript::never_exits()]);
    let dog = enable_dog(&h.ctx, 1, "bark").await;

    let reply = set_field(&h.ctx, 2, "bark", "script", serde_json::json!("/tmp/evil")).await;
    let Err(err) = reply else {
        panic!("a dog was given a config field")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("bark is a dog"), "{}", err.message);
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "bark")
            .unwrap()
            .is_none(),
        "the refusal still wrote the store"
    );
    drop(dog);
}

/// `env` would be replaced wholesale by a request carrying one value,
/// wiping every other key; `instances` and `name` are Structural, and
/// the count moves through `shep stock`.
#[tokio::test(start_paused = true)]
async fn env_and_the_structural_fields_are_refused_by_this_door() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    for (id, key, value) in [
        (2, "env", serde_json::json!({ "A": "1" })),
        (3, "instances", serde_json::json!(4)),
        (4, "name", serde_json::json!("other")),
    ] {
        let reply = set_field(&h.ctx, id, "web", key, value).await;
        let Err(err) = reply else {
            panic!("{key} was accepted")
        };
        assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{key}");
        assert!(err.message.contains(key), "{key}: {}", err.message);
    }
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "a refusal still wrote the store"
    );
}

/// Three shapes, and each is the caller's: a key `AppConfig` has no
/// field for, a value that will not deserialize into the field it
/// names, and a value that deserializes and then fails `normalize`.
#[tokio::test(start_paused = true)]
async fn a_field_this_build_refuses_is_invalid_config_and_writes_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    for (id, key, value) in [
        (2, "no_such_field", serde_json::json!(1)),
        (3, "max_restarts", serde_json::json!("forty")),
        (
            4,
            "cron_restart",
            serde_json::json!("not a cron expression"),
        ),
    ] {
        let reply = set_field(&h.ctx, id, "web", key, value).await;
        let Err(err) = reply else {
            panic!("{key} was accepted")
        };
        assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{key}");
        assert!(
            shep_core::overrides::get(&h.ctx.paths.overrides, "web")
                .unwrap()
                .is_none(),
            "{key}: the refusal still wrote the store"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_store_is_internal_for_a_field_edit_too() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;
    std::fs::write(&h.ctx.paths.overrides, "{ this is not json").unwrap();

    let reply = set_field(&h.ctx, 2, "web", "max_restarts", serde_json::json!(40)).await;
    let Err(err) = reply else {
        panic!("an unreadable store was reported as success")
    };
    assert_eq!(err.code, RpcErrorCode::Internal);
    assert!(
        err.message.contains("overrides store unusable"),
        "{}",
        err.message
    );
}

/// The muster roll is a registry record `rpc.rs` writes, not
/// something the supervisor does. Nothing on the restore path reads
/// the override store, so an edit that skipped it would survive a
/// `shep daemon reload` and vanish on a cold restart.
#[tokio::test(start_paused = true)]
async fn a_field_edit_reaches_the_muster_roll() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = set_field(&h.ctx, 2, "web", "max_restarts", serde_json::json!(40)).await;
    assert!(reply.is_ok(), "{reply:?}");

    let infos = list_flock(&h.ctx, 3).await;
    let roll = h.ctx.registry.roll(&infos, 0);
    let web = roll
        .apps
        .iter()
        .find(|entry| entry.app.name == "web")
        .expect("web is in the roll");
    assert_eq!(web.app.max_restarts, 40);
}

/// `Request` is `#[non_exhaustive]` with dispatch ending in a
/// wildcard, so a variant missing its arm compiles and passes every
/// other test, silently refused at runtime instead. The list below is
/// hand-written and nothing makes it exhaustive: a new variant is
/// covered only if its author adds it here.
#[tokio::test(start_paused = true)]
async fn every_new_variant_reaches_an_arm_and_not_the_wildcard() {
    let h = harness(vec![]);
    let requests = [
        Request::SheepConfig {
            name: "ghost".to_string(),
        },
        Request::SetSheepEnv {
            name: "ghost".to_string(),
            key: "K".to_string(),
            value: None,
        },
        Request::SetSheepField {
            name: "ghost".to_string(),
            key: "max_restarts".to_string(),
            value: serde_json::json!(1),
        },
        Request::SetDogConfig {
            name: "ghost".to_string(),
            toml: String::new().into(),
        },
        Request::PutSecrets {
            namespace: "ghost".to_string(),
            environment: "production".to_string(),
            entries: BTreeMap::new(),
        },
        Request::SetSheepEnvBatch {
            name: "ghost".to_string(),
            entries: BTreeMap::new(),
            force: false,
            dry_run: true,
        },
    ];
    for (id, request) in requests.into_iter().enumerate() {
        let named = format!("{request:?}");
        let reply = reply_of(
            dispatch(
                envelope(
                    u64::try_from(id).expect("an index into `requests` fits a u64"),
                    request,
                ),
                &h.ctx,
            )
            .await,
        );
        let Err(err) = reply.result else {
            continue;
        };
        assert_ne!(
            err.message, "this daemon does not implement that request",
            "{named} fell through to the wildcard"
        );
    }
}
