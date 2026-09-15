//! `Start` and `Add` registration, re-normalisation, and the batch
//! dependency graph: stage ordering, cycles closing through the
//! registry or the flock, and knots a batch is not part of.

use super::*;

#[tokio::test(start_paused = true)]
async fn start_registers_the_config_and_lists_it() {
    let h = harness(vec![ProcScript::never_exits()]);
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
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].status, ProcStatus::Online);

    // The roll can only be built if Start recorded the config.
    let roll = h.ctx.registry.roll(&infos, 0);
    assert_eq!(roll.apps.len(), 1);
    assert_eq!(roll.apps[0].app.script, "./srv");

    let listed = reply_of(dispatch(envelope(2, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn start_re_normalizes_untrusted_peer_config() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::InvalidConfig);
}

/// The harness is scripted with no processes, which is the forcing
/// mechanism: `ScriptedRunner::spawn` refuses with `script exhausted`
/// once the list is empty, so a build that routed this at `do_start`
/// lands `Errored` rather than `Stopped` with no pid.
#[tokio::test(start_paused = true)]
async fn add_registers_a_stopped_member_and_spawns_nothing() {
    let h = harness(vec![]);
    let added = reply_of(
        dispatch(
            envelope(
                1,
                Request::Add {
                    apps: vec![AppConfig::minimal("web", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Added(infos) = added.result.unwrap() else {
        panic!("expected added")
    };
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].status, ProcStatus::Stopped);
    assert_eq!(infos[0].pid, None, "nothing was spawned");

    // The roll can only be built if `Add` recorded the config, and an app
    // registered and never started is precisely the one a roll would
    // otherwise forget.
    let roll = h.ctx.registry.roll(&infos, 0);
    assert_eq!(roll.apps.len(), 1);
    assert_eq!(roll.apps[0].app.script, "./srv");
    assert_eq!(roll.apps[0].instances_running, 0);

    let listed = reply_of(dispatch(envelope(2, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(
        flock.len(),
        1,
        "it is a member of the flock, just a still one"
    );
}

/// fails if `Add` trusts what a peer sent it. Same rule as `Start`: the
/// socket is the boundary, and an empty name is the shape `normalize`
/// refuses.
#[tokio::test(start_paused = true)]
async fn add_re_normalizes_untrusted_peer_config() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Add {
                    apps: vec![AppConfig::minimal("", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::InvalidConfig);
}

/// The sheep is online, the case that matters: re-running `shep add
/// Flockfile.toml` after editing the file must not stop a service. One
/// script, so a second spawn would fail rather than pass quietly.
#[tokio::test(start_paused = true)]
async fn a_second_add_leaves_a_running_sheep_exactly_as_it_was() {
    let h = harness(vec![ProcScript::never_exits()]);
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
    let Response::Started(before) = started.result.unwrap() else {
        panic!("expected started")
    };

    let added = reply_of(
        dispatch(
            envelope(
                2,
                Request::Add {
                    apps: vec![AppConfig::minimal("web", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Added(after) = added.result.unwrap() else {
        panic!("expected added")
    };
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].id, before[0].id,
        "the same sheep, not a second one"
    );
    assert_eq!(after[0].status, ProcStatus::Online, "still running");
    assert_eq!(after[0].pid, before[0].pid, "the same process");

    let listed = reply_of(dispatch(envelope(3, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1, "one row, not two");
}

/// An app that waits on `depends_on`, for the staged-start cases below.
fn waiting_on(name: &str, depends_on: &[&str]) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.depends_on = depends_on.iter().map(|n| (*n).to_string()).collect();
    app
}

#[tokio::test(start_paused = true)]
async fn a_start_runs_its_batch_in_dependency_order() {
    // fails if `Start` hands the whole batch to one `start` call: the
    // reply would carry the apps in the order the request listed them,
    // which is the reverse of the order they have to come up in.
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![waiting_on("api", &["db"]), waiting_on("db", &[])],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let order: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(order, ["db", "api"], "stage order, not request order");
}

#[tokio::test(start_paused = true)]
async fn a_cycle_closing_through_a_registered_sheep_is_refused_too() {
    // fails if the graph spans only the incoming batch: neither document
    // shows a cycle on its own, and the flock is where the edge back is.
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let first = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![waiting_on("db", &["api"])],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(
        first.result.is_ok(),
        "one app, one edge to a name nobody has"
    );

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Start {
                    apps: vec![waiting_on("api", &["db"])],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(
        err.message.contains("api") && err.message.contains("db"),
        "both ends of the cycle must be named: {}",
        err.message
    );
}

#[tokio::test(start_paused = true)]
async fn a_batch_in_a_knot_the_named_path_leaves_out_is_still_refused() {
    // fails if the cycle check tests the batch against the representative
    // PATH instead of the knot's members: `plan` reports one path per
    // knot, so a knot of three reached through two edges names only two
    // of them, and the third starts into a dependency that can never be
    // satisfied while the operator is told nothing.
    let h = harness(vec![ProcScript::never_exits(); 3]);
    let _started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: ["api", "db", "web"]
                        .iter()
                        .map(|name| AppConfig::minimal(name, "./srv"))
                        .collect(),
                },
            ),
            &h.ctx,
        )
        .await,
    );

    // `api -> {db, web}`, `db -> api`, `web -> api`: one knot holding all
    // three, planted through `ApplyConfig` the way the sibling test does.
    for (name, waits_for) in [
        ("api", vec!["db", "web"]),
        ("db", vec!["api"]),
        ("web", vec!["api"]),
    ] {
        let mut config = AppConfig::minimal(name, "./srv");
        config.depends_on = waits_for.iter().map(|n| (*n).to_string()).collect();
        let reply = reply_of(
            dispatch(
                envelope(
                    2,
                    Request::ApplyConfig {
                        apps: vec![DeclaredApp {
                            config,
                            declared: ["depends_on"].iter().map(|k| (*k).to_string()).collect(),
                            declared_env: BTreeSet::new(),
                        }],
                        reset: ResetDepth::None,
                    },
                ),
                &h.ctx,
            )
            .await,
        );
        assert!(reply.result.is_ok(), "the load itself draws no cycle");
    }

    let mut web = AppConfig::minimal("web", "./srv");
    web.depends_on = vec!["api".to_string()];
    let reply = reply_of(dispatch(envelope(3, Request::Start { apps: vec![web] }), &h.ctx).await);
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(
        err.message.starts_with("dependency cycle:"),
        "web is in the knot even though the reported path omits it: {}",
        err.message
    );
    assert!(
        !err.message.contains("web"),
        "the message still renders the representative path, which is what \
         an operator breaks: {}",
        err.message
    );
}

#[tokio::test(start_paused = true)]
async fn a_knot_no_app_in_the_batch_is_in_does_not_refuse_the_batch() {
    // fails if the cycle check takes the first cycle in the graph: the
    // graph spans the whole registry, so a knot two earlier Flockfile
    // loads left standing elsewhere in the flock would wedge `shep
    // start` and `shep add` for every app, with an error naming apps the
    // operator never mentioned.
    let h = harness(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]);
    let _started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![
                        AppConfig::minimal("api", "./srv"),
                        AppConfig::minimal("db", "./srv"),
                    ],
                },
            ),
            &h.ctx,
        )
        .await,
    );

    // The production door the brief names: `load_one` sends
    // `ApplyConfig` for every app the flock already has, and that arm
    // records the merged config with no cycle check of its own, so two
    // loads neither of which draws a cycle can leave one behind.
    for (name, waits_for) in [("api", "db"), ("db", "api")] {
        let mut config = AppConfig::minimal(name, "./srv");
        config.depends_on = vec![waits_for.to_string()];
        let reply = reply_of(
            dispatch(
                envelope(
                    2,
                    Request::ApplyConfig {
                        apps: vec![DeclaredApp {
                            config,
                            declared: ["depends_on"].iter().map(|k| (*k).to_string()).collect(),
                            declared_env: BTreeSet::new(),
                        }],
                        reset: ResetDepth::None,
                    },
                ),
                &h.ctx,
            )
            .await,
        );
        assert!(reply.result.is_ok(), "the load itself draws no cycle");
    }
    let edges = h.ctx.registry.depends_on_by_name();
    assert_eq!(edges.get("api"), Some(&vec!["db".to_string()]));
    assert_eq!(
        edges.get("db"),
        Some(&vec!["api".to_string()]),
        "the knot is really in the registry, or this test proves nothing"
    );

    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Start {
                    apps: vec![AppConfig::minimal("web", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(
        reply.result.is_ok(),
        "a batch drawing no cycle must start: {:?}",
        reply.result.unwrap_err()
    );
}

#[tokio::test(start_paused = true)]
async fn an_add_whose_cycle_closes_through_the_flock_is_refused_too() {
    // fails if the cycle check rides on the spawning half: `add` and
    // `start` are one path, and a document `add` registered would refuse
    // the moment anything started it. `normalize_all` already catches a
    // cycle drawn inside one document; only the flock closes this one.
    let h = harness(vec![]);
    let first = reply_of(
        dispatch(
            envelope(
                1,
                Request::Add {
                    apps: vec![waiting_on("db", &["api"])],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(
        first.result.is_ok(),
        "one app, one edge to a name nobody has"
    );

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Add {
                    apps: vec![waiting_on("api", &["db"])],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(
        err.message.contains(" -> "),
        "the cycle must be named as a path: {}",
        err.message
    );
}

#[tokio::test(start_paused = true)]
async fn a_stage_never_covers_a_sheep_the_request_did_not_carry() {
    // fails if the stages are taken from the whole graph rather than
    // filtered to the batch: `db` is already up, and a stage naming it
    // would start it a second time.
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let first = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![waiting_on("db", &[])],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(first.result.is_ok(), "db starts on its own");

    let second = reply_of(
        dispatch(
            envelope(
                2,
                Request::Start {
                    apps: vec![waiting_on("api", &["db"])],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Started(infos) = second.result.unwrap() else {
        panic!("expected started")
    };
    let order: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(order, ["api"], "only what this request carried");

    let listed = reply_of(dispatch(envelope(3, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 2, "one row each, not a second db");
}
