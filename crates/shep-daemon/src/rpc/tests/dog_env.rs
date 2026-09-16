//! Dog env overrides: refused where a plain config edit would be
//! accepted, the sheep-config pane, set/remove, parked-env vs the
//! live roll, and an unreadable override store.

use super::*;

/// A dog runs at the daemon's own trust level and its binary is what
/// `shep adopt` vetted, so a parked `PATH`, `LD_PRELOAD` or
/// `DYLD_INSERT_LIBRARIES` for its next respawn would run arbitrary
/// code at that level. Refused at the daemon, not at a caller, because
/// the socket is already live. Asserts the store as well as the code:
/// a refusal that still wrote would be the same hole.
#[tokio::test(start_paused = true)]
async fn a_dog_is_refused_an_env_override_rather_than_given_one() {
    let h = harness(vec![ProcScript::never_exits()]);
    let dog = enable_dog(&h.ctx, 1, "bark").await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "bark".to_string(),
                    key: "PATH".to_string(),
                    value: Some("/tmp/evil".to_string().into()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("a dog was given an env override")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("bark is a dog"), "{}", err.message);

    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "bark")
            .unwrap()
            .is_none(),
        "the refusal still wrote the store"
    );
    let described = reply_of(
        dispatch(
            envelope(
                3,
                Request::Describe {
                    selector: SelectorSpec::Name("bark".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Described(infos)) = described.result else {
        panic!("expected Described")
    };
    assert_eq!(infos[0].id, dog.id);
    assert_eq!(infos[0].pending, None, "the refusal still parked a config");
}

/// No other request hands a client a config at all, so this would be a
/// read surface that exists for dogs and nothing else. A dog's config
/// is what `shep adopt` vetted, not something an operator edits here.
#[tokio::test(start_paused = true)]
async fn a_dogs_config_is_not_readable_through_the_sheep_config_pane() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_dog(&h.ctx, 1, "bark").await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SheepConfig {
                    name: "bark".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("a dog's config was served to a pane")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("bark is a dog"), "{}", err.message);
}

/// Both halves matter: a pane that cannot name the keys cannot offer
/// to edit them, and one handed the values has put a secret on a
/// socket for nothing (IR-41).
#[tokio::test(start_paused = true)]
async fn sheep_config_answers_with_env_emptied_and_its_keys_listed() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SheepConfig {
                    name: "web".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::SheepConfig(view)) = reply.result else {
        panic!("expected SheepConfig")
    };
    assert_eq!(view.name, "web");
    assert!(view.config.env.is_empty());
    assert_eq!(view.env_keys, ["DB_PASS"]);
}

/// A pane asking about a sheep deleted out from under it is normal,
/// not a daemon fault. `Internal` would send an operator looking for
/// a bug in the shepherd.
#[tokio::test(start_paused = true)]
async fn sheep_config_for_an_unknown_name_is_not_found_not_internal() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::SheepConfig {
                    name: "ghost".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("expected a refusal")
    };
    assert_eq!(err.code, RpcErrorCode::NotFound);
}

/// The running process was handed its environment at spawn and cannot
/// be handed another, so an edit reported as applied would be one the
/// operator believes is in force when it is not.
#[tokio::test(start_paused = true)]
async fn set_sheep_env_writes_the_store_and_parks_env_until_a_respawn() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "web".to_string(),
                    key: "NEW".to_string(),
                    value: Some("1".to_string().into()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(
        matches!(reply.result, Ok(Response::SheepEnvSet { .. })),
        "{:?}",
        reply.result
    );

    let stored = shep_core::overrides::get(&h.ctx.paths.overrides, "web")
        .unwrap()
        .unwrap();
    assert_eq!(stored.fields["env"]["NEW"], "1");

    let described = reply_of(
        dispatch(
            envelope(
                3,
                Request::Describe {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Described(infos)) = described.result else {
        panic!("expected Described")
    };
    assert_eq!(
        infos[0].pending.as_deref(),
        Some(["env".to_string()].as_slice())
    );
}

/// The CFG column reads `ProcessEntry::overridden`, so an `env` key
/// left in the store's field set after its last value is gone marks a
/// sheep that no longer differs from its Flockfile.
#[tokio::test(start_paused = true)]
async fn removing_the_last_env_override_stops_marking_the_sheep() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    for (id, value) in [(2, Some("1".to_string().into())), (3, None)] {
        let reply = reply_of(
            dispatch(
                envelope(
                    id,
                    Request::SetSheepEnv {
                        name: "web".to_string(),
                        key: "NEW".to_string(),
                        value,
                    },
                ),
                &h.ctx,
            )
            .await,
        );
        assert!(reply.result.is_ok(), "{:?}", reply.result);
    }

    let stored = shep_core::overrides::get(&h.ctx.paths.overrides, "web")
        .unwrap()
        .unwrap();
    assert!(!stored.fields.contains_key("env"), "{stored:?}");

    let reply = reply_of(
        dispatch(
            envelope(
                4,
                Request::SheepConfig {
                    name: "web".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::SheepConfig(view)) = reply.result else {
        panic!("expected SheepConfig")
    };
    assert!(view.overridden.is_empty(), "{view:?}");
}

/// The muster roll is written from the `FlockRegistry`, and nothing on
/// the restore path reads the override store, so a handler that parks
/// a config without recording it looks correct in every live test and
/// forgets the edit on the next cold start. Asserts the roll rather
/// than the registry's own accessor, since the roll is what `shep
/// muster` actually restores from.
#[tokio::test(start_paused = true)]
async fn a_set_env_reaches_the_roll_a_cold_restart_would_come_back_on() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "web".to_string(),
                    key: "NEW".to_string(),
                    value: Some("1".to_string().into()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(reply.result.is_ok(), "{:?}", reply.result);

    let infos = list_flock(&h.ctx, 3).await;
    let roll = h.ctx.registry.roll(&infos, 0);
    let web = roll
        .apps
        .iter()
        .find(|entry| entry.app.name == "web")
        .expect("web is in the roll");
    assert_eq!(web.app.env.get("NEW").map(String::as_str), Some("1"));
}

/// The sibling above asserts the registry; this asserts the file, which
/// is what a cold boot actually reads. A parked edit moves no process, so
/// the bus says nothing and the roll's only other schedule is the
/// graceful shutdown a `SIGKILL` never reaches. The store keeps the edit
/// either way and `overridden_for` reads it back, so a stale roll is not
/// a lost edit but a divergent one: the sheep comes back on the old value
/// under a CFG cell claiming the operator set a new one.
#[tokio::test(start_paused = true)]
async fn a_parked_env_edit_reaches_the_roll_before_a_hard_stop_can_lose_it() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    // The roll as a `shep save` left it, so the baseline is on disk
    // whatever the writer does, and the writer subscribing after the
    // start's own events so the edit below is all it has left to react
    // to.
    h.ctx.save_roll_now().await.unwrap();
    let baseline = crate::snapshot::read(&h.ctx.snapshot_path).unwrap();
    assert_eq!(baseline.apps[0].app.env.get("NEW"), None);
    let writer = crate::snapshot::spawn_snapshot_writer(
        h.ctx.snapshot_path.clone(),
        h.ctx.supervisor.clone(),
        h.ctx.registry.clone(),
        h.ctx.events.subscribe(),
    );
    let settle = std::time::Duration::from_millis(crate::snapshot::SNAPSHOT_DEBOUNCE_MS * 2);
    tokio::time::sleep(settle).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "web".to_string(),
                    key: "NEW".to_string(),
                    value: Some("1".to_string().into()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(reply.result.is_ok(), "{:?}", reply.result);
    tokio::time::sleep(settle).await;

    // The hard stop: the writer goes where a `SIGKILL` would have taken
    // it, and no graceful `save_roll_now` runs after this line.
    writer.stop().await;

    let (events, _rx) = crate::bus::test_bus(64);
    let cold = crate::supervisor::spawn_supervisor(
        crate::fake::ScriptedRunner::new(vec![ProcScript::never_exits()]),
        h.ctx.paths.clone(),
        events.clone(),
    );
    // The cold registry, not `sheep_config`: that view clears `env` on
    // its way out, and the registry is what the restored sheep was
    // started from.
    let cold_registry = crate::snapshot::FlockRegistry::new();
    let restored = crate::snapshot::muster(
        &h.ctx.snapshot_path,
        &cold_registry,
        &cold,
        &events,
        &[],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(restored, vec!["web".to_string()]);

    let listed = cold.list().await;
    let came_back = cold_registry.roll(&listed, 0);
    assert_eq!(
        came_back.apps[0].app.env.get("NEW").map(String::as_str),
        Some("1"),
        "the sheep must come back on the edit its CFG cell claims: {:?}",
        listed[0].overridden
    );
    cold.shutdown().await;
}

/// `map.remove` is a no-op for a key the app's own config supplied, so
/// without a tombstone the store comes back empty and the removal
/// lives only in `ProcessEntry::pending`, a change the operator just
/// made.
#[tokio::test(start_paused = true)]
async fn removing_a_key_the_operator_never_set_is_still_recorded() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "web".to_string(),
                    key: "DB_PASS".to_string(),
                    value: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(reply.result.is_ok(), "{:?}", reply.result);

    let stored = shep_core::overrides::get(&h.ctx.paths.overrides, "web")
        .unwrap()
        .expect("the removal is recorded");
    assert_eq!(
        stored.fields["env"]["DB_PASS"],
        serde_json::Value::Null,
        "a removal is a tombstone, not an absence"
    );

    let view = sheep_config_view(&h.ctx, 3, "web").await;
    assert_eq!(view.overridden, ["env"]);
    assert!(!view.env_keys.contains(&"DB_PASS".to_string()));
}

/// Two things must compose: `merge_declared`'s env loop must skip a
/// key held in `overridden_env` so the file's value does not come
/// back, and `establish_env`, which runs after, must not spend the
/// tombstone the way it spends a valued override, or `overridden`
/// stops naming `env` while the sheep still differs from its file.
/// Loaded twice on purpose: a single load would pass against a build
/// that spent the tombstone and leaned on `declared_env` alone.
#[tokio::test(start_paused = true)]
async fn a_removed_key_stays_removed_and_stays_reported_across_reloads() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let removed = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "web".to_string(),
                    key: "DB_PASS".to_string(),
                    value: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(removed.result.is_ok(), "{:?}", removed.result);

    // The Flockfile still declares the key the operator removed, which
    // is the whole point: a deploy re-runs the same file.
    for id in [3, 4] {
        let loaded = reply_of(
            dispatch(
                envelope(
                    id,
                    Request::ApplyConfig {
                        apps: vec![DeclaredApp {
                            config: {
                                let mut app = AppConfig::minimal("web", "./srv");
                                app.env
                                    .insert("DB_PASS".to_string(), "fromfile".to_string());
                                app
                            },
                            declared: ["name", "script"]
                                .iter()
                                .map(|k| (*k).to_string())
                                .collect(),
                            declared_env: ["DB_PASS"].iter().map(|k| (*k).to_string()).collect(),
                        }],
                        reset: ResetDepth::None,
                    },
                ),
                &h.ctx,
            )
            .await,
        );
        let Ok(Response::Applied(report)) = loaded.result else {
            panic!("expected Applied")
        };
        assert_eq!(report[0].refused, None);

        let stored = shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .expect("the removal is still recorded");
        assert_eq!(
            stored.fields["env"]["DB_PASS"],
            serde_json::Value::Null,
            "load {id} spent the tombstone"
        );

        let view = sheep_config_view(&h.ctx, id + 10, "web").await;
        assert!(
            !view.env_keys.contains(&"DB_PASS".to_string()),
            "load {id} put the file's value back"
        );
        assert_eq!(view.overridden, ["env"], "load {id} stopped reporting it");
    }
}

/// Two halves, and the second is the one a refactor breaks:
/// `SHEP_NAME` is injected per instance and refused in a hand-written
/// env, so this is a real refusal an operator can meet from a
/// free-text pane, and a handler that wrote first would leave a
/// stored override for a config the daemon will not accept.
#[tokio::test(start_paused = true)]
async fn an_env_key_normalize_refuses_is_invalid_config_and_writes_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "web".to_string(),
                    key: "SHEP_NAME".to_string(),
                    value: Some("impostor".to_string().into()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("a reserved env key was accepted")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "the refusal still wrote the store"
    );
}

/// Neither the caller's request nor a refusal they can act on by
/// asking differently, which is why it gets a variant of its own
/// rather than sharing `InvalidEnv`'s.
#[tokio::test(start_paused = true)]
async fn an_unreadable_override_store_is_internal_not_a_bad_request() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_with_a_secret(&h.ctx).await;
    std::fs::write(&h.ctx.paths.overrides, "{ this is not json").unwrap();

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepEnv {
                    name: "web".to_string(),
                    key: "NEW".to_string(),
                    value: Some("1".to_string().into()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("an unreadable store was reported as success")
    };
    assert_eq!(err.code, RpcErrorCode::Internal);
    assert!(
        err.message.contains("overrides store unusable"),
        "{}",
        err.message
    );
}
