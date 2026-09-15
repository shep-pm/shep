//! Staged restart and reload: stage ordering, transitive dependencies,
//! refusal collection, stage bounds, and reload swap counting. Also
//! the two selector-validation tests for `Reload` and `Reopen`, and
//! the one that checks `rpc_error` maps a refused reload's `Display`
//! rather than its bare payload.

use super::*;

/// `AppConfig`'s own default `kill_timeout`, which every app built by
/// `AppConfig::minimal` here carries.
const DEFAULT_KILL_TIMEOUT: Duration = Duration::from_millis(1600);

/// A `db` and an `api` that waits for it, started through the ordinary
/// `Start` arm so the registry holds the edge an ordered walk reads.
///
/// `api` is started first, and alone, deliberately: it takes the lower
/// id, and it sorts first by name as well, so neither the id order a
/// batch verb resolves in nor the name order a reload queues in matches
/// the dependency order. Started together, the staged `Start` would give
/// `db` the lower id and an unordered restart would pass by accident.
async fn start_api_before_the_db_it_waits_for(h: &Harness) {
    let mut api = AppConfig::minimal("api", "./api");
    api.depends_on = vec!["db".to_string()];
    for (id, app) in [(1, api), (2, AppConfig::minimal("db", "./db"))] {
        let started =
            reply_of(dispatch(envelope(id, Request::Start { apps: vec![app] }), &h.ctx).await);
        let Response::Started(infos) = started.result.unwrap() else {
            panic!("expected started")
        };
        assert_eq!(infos.len(), 1, "each app comes up before the next");
    }
}

/// The names `kind` was published for, in the order the bus carried them.
fn names_for(
    rx: &mut tokio::sync::broadcast::Receiver<crate::bus::SharedEvent>,
    kind: ProcessEventKind,
) -> Vec<String> {
    let mut names = Vec::new();
    while let Ok(event) = rx.try_recv() {
        let shep_core::protocol::BusEvent::Process {
            event: seen, info, ..
        } = &*event
        else {
            continue;
        };
        if *seen == kind {
            names.push(info.name.clone());
        }
    }
    names
}

/// fails if a fold restarts as one batch, which restarts `api` against a
/// database that has not come back. Four scripts: two for the pair's
/// first spawn and two for their respawns.
#[tokio::test(start_paused = true)]
async fn a_restart_matching_several_walks_the_stages_forward() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_api_before_the_db_it_waits_for(&h).await;

    let mut rx = h.ctx.events.subscribe();
    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Restart {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Restarted {
        accepted: infos, ..
    } = reply.result.unwrap()
    else {
        panic!("expected restarted")
    };
    assert_eq!(infos.len(), 2, "both sheep restart: {infos:?}");

    assert_eq!(names_for(&mut rx, ProcessEventKind::Restart), ["db", "api"]);
}

/// fails if a fold reloads as one batch, which swaps `api` while the
/// database it waits for is still swapping. Four scripts: two for the
/// pair's first spawn and two for the replacements each swap spawns.
#[tokio::test(start_paused = true)]
async fn a_reload_matching_several_walks_the_stages_forward() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_api_before_the_db_it_waits_for(&h).await;

    let mut rx = h.ctx.events.subscribe();
    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Reload {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Reloading { accepted, .. } = reply.result.unwrap() else {
        panic!("expected reloading")
    };
    assert_eq!(accepted.len(), 2, "both sheep are accepted: {accepted:?}");

    assert_eq!(names_for(&mut rx, ProcessEventKind::Reload), ["db", "api"]);
}

/// fails if the ordered walk answers a selector it matched nothing for
/// with an empty table. A restart that moved nothing is `NotFound`, which
/// is what makes `shep restart typo` exit non-zero.
#[tokio::test(start_paused = true)]
async fn a_restart_naming_nothing_the_flock_holds_is_still_not_found() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    start_api_before_the_db_it_waits_for(&h).await;

    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Restart {
                    selector: SelectorSpec::Name("typo".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::NotFound);
}

/// fails if a stage awaits its members in turn, which costs a restart the
/// SUM of their kill ladders where an unordered one costs the longest.
/// Two sheep with no edge between them are one stage, and both ignore
/// SIGTERM, so each burns its whole 1600ms ladder. Virtual time under
/// `start_paused`, which advances only when every task is idle, so the
/// two shapes are exact rather than close.
#[tokio::test(start_paused = true)]
async fn one_stage_restarts_its_members_at_the_same_time() {
    let h = harness(vec![
        ProcScript::ignores_signals(),
        ProcScript::ignores_signals(),
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]);
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![
                        AppConfig::minimal("alpha", "./a"),
                        AppConfig::minimal("zulu", "./z"),
                    ],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(started.result.is_ok(), "both apps come up");

    let began = Instant::now();
    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Restart {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let spent = began.elapsed();

    let Response::Restarted {
        accepted: infos, ..
    } = reply.result.unwrap()
    else {
        panic!("expected restarted")
    };
    assert_eq!(infos.len(), 2, "both sheep restart: {infos:?}");
    assert!(
        spent < DEFAULT_KILL_TIMEOUT * 2,
        "one stage's ladders must overlap; spent {spent:?} on two of them"
    );
}

/// fails if a matched name with no node in the plan is dropped from the
/// walk, which would answer `Ok` for a sheep nothing restarted. The
/// registry is what the stages are built from, and it holds no dog and
/// nothing a `shep dev` teardown cleared.
#[tokio::test(start_paused = true)]
async fn a_matched_sheep_the_registry_does_not_hold_still_restarts() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_api_before_the_db_it_waits_for(&h).await;
    h.ctx.registry.clear();

    let mut rx = h.ctx.events.subscribe();
    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Restart {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Restarted {
        accepted: infos, ..
    } = reply.result.unwrap()
    else {
        panic!("expected restarted")
    };
    assert_eq!(infos.len(), 2, "both sheep restart: {infos:?}");
    assert_eq!(
        names_for(&mut rx, ProcessEventKind::Restart)
            .into_iter()
            .collect::<BTreeSet<_>>(),
        ["api".to_string(), "db".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
    );
}

/// Every process event the receiver holds, as `(kind, name)` in the
/// order the bus carried them.
fn events_in_order(
    rx: &mut tokio::sync::broadcast::Receiver<crate::bus::SharedEvent>,
) -> Vec<(ProcessEventKind, String)> {
    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let shep_core::protocol::BusEvent::Process {
            event: kind, info, ..
        } = &*event
        {
            seen.push((*kind, info.name.clone()));
        }
    }
    seen
}

/// fails if a stage drops its wait for an app whose reload was refused:
/// `ReloadInFlight` is per app now, so a busy dependency contributes
/// nothing to `waiting`, the stage returns at once, and the dependant
/// swaps against a dependency that is mid-swap. Four scripts: two for the
/// pair's first spawn and two for the replacements.
#[tokio::test(start_paused = true)]
async fn a_stage_still_waits_for_an_app_whose_reload_was_refused() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_api_before_the_db_it_waits_for(&h).await;

    // Accepted and still swapping: a single-target reload answers before
    // the replacement is up, which is what leaves `db` in flight.
    let first = reply_of(
        dispatch(
            envelope(
                3,
                Request::Reload {
                    selector: SelectorSpec::Name("db".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(first.result.is_ok(), "the first reload is accepted");

    let mut rx = h.ctx.events.subscribe();
    let reply = reply_of(
        dispatch(
            envelope(
                4,
                Request::Reload {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Reloading { accepted, .. } = reply.result.unwrap() else {
        panic!("expected reloading")
    };
    let names: Vec<&str> = accepted.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(names, ["api"], "db is already reloading and is refused");

    let seen = events_in_order(&mut rx);
    let swapped = seen
        .iter()
        .position(|(kind, name)| *kind == ProcessEventKind::Reloaded && name == "db")
        .unwrap_or_else(|| panic!("db never finished its swap: {seen:?}"));
    let dependant = seen
        .iter()
        .position(|(kind, name)| *kind == ProcessEventKind::Reload && name == "api")
        .unwrap_or_else(|| panic!("api never reloaded: {seen:?}"));
    assert!(
        swapped < dependant,
        "api must wait out the refused stage's swap: {seen:?}"
    );
}

/// fails if a staged restart drops the name of an app it went around: the
/// walk asks per app, so one the flock no longer holds is refused on its
/// own and the reply would otherwise be a success with that app's row
/// silently missing.
///
/// The refusal is planted the way the real one arrives. `walk_for` reads
/// a listing and `restart_in_stages` calls the supervisor per member
/// afterwards, so a sheep that leaves the flock in between is named by
/// the walk and no longer matched by the supervisor; deleting `db`
/// between the two calls is that interleaving, held still. Going through
/// `Request::Restart` instead would take both halves inside one handler
/// with nothing able to run between them. Three scripts: two for the
/// pair's first spawn and one for `api`'s respawn.
#[tokio::test(start_paused = true)]
async fn a_staged_restart_names_the_app_it_could_not_restart() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    start_api_before_the_db_it_waits_for(&h).await;

    let walk = walk_for(&h.ctx, &ProcessSelector::All)
        .await
        .expect("two sheep are an ordered walk");
    let deleted = reply_of(
        dispatch(
            envelope(
                3,
                Request::Delete {
                    selector: SelectorSpec::Name("db".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(deleted.result.is_ok(), "db leaves the flock");

    let (accepted, refused) = restart_in_stages(&h.ctx, &walk)
        .await
        .expect("api still restarted, so the walk answers Ok");
    let names: Vec<&str> = accepted.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(names, ["api"], "the rest of the fold still restarts");
    let refused_names: Vec<&str> = refused.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(refused_names, ["db"], "and the one that did not is named");
    assert!(
        refused[0].reason.contains("no registered sheep"),
        "with the shepherd's own reason: {:?}",
        refused[0].reason
    );
}

/// fails if a staged restart keeps only the first refusal, which is what
/// the walk did with the error before `refused` existed: two apps gone
/// from the flock have to produce two names, not one.
///
/// The same interleaving as the sibling above, over three apps with two
/// of them deleted, so `api` still restarts and the answer is the
/// partial one a client has to render. Four scripts: three for the first
/// spawns and one for `api`'s respawn.
#[tokio::test(start_paused = true)]
async fn a_staged_restart_collects_every_refusal_and_not_just_the_first() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_api_before_the_db_it_waits_for(&h).await;
    let started = reply_of(
        dispatch(
            envelope(
                3,
                Request::Start {
                    apps: vec![AppConfig::minimal("cache", "./cache")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(started.result.is_ok(), "cache comes up beside the pair");

    let walk = walk_for(&h.ctx, &ProcessSelector::All)
        .await
        .expect("three sheep are an ordered walk");
    for (id, name) in [(4, "db"), (5, "cache")] {
        let deleted = reply_of(
            dispatch(
                envelope(
                    id,
                    Request::Delete {
                        selector: SelectorSpec::Name(name.to_string()),
                    },
                ),
                &h.ctx,
            )
            .await,
        );
        assert!(deleted.result.is_ok(), "{name} leaves the flock");
    }

    let (accepted, refused) = restart_in_stages(&h.ctx, &walk)
        .await
        .expect("api still restarted, so the walk answers Ok");
    let names: Vec<&str> = accepted.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(names, ["api"], "the one app still in the flock restarts");
    // A set, not a list: which stage each deleted app fell in is
    // `plan_for_names`' business and not what this pins.
    let refused_names: BTreeSet<&str> = refused.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        refused_names,
        ["cache", "db"].into_iter().collect::<BTreeSet<&str>>(),
        "both are named, not just whichever was refused first"
    );
}

/// fails if a staged reload drops the name of an app it went around: the
/// walk asks per app, so a busy one is refused on its own and the reply
/// would otherwise be a success with that app's row silently missing.
/// Four scripts, as the sibling above.
#[tokio::test(start_paused = true)]
async fn a_staged_reload_names_the_app_it_could_not_reload() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_api_before_the_db_it_waits_for(&h).await;

    let first = reply_of(
        dispatch(
            envelope(
                3,
                Request::Reload {
                    selector: SelectorSpec::Name("db".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(first.result.is_ok(), "the first reload is accepted");

    let reply = reply_of(
        dispatch(
            envelope(
                4,
                Request::Reload {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Reloading { accepted, refused } = reply.result.unwrap() else {
        panic!("expected reloading")
    };
    let names: Vec<&str> = accepted.iter().map(|info| info.name.as_str()).collect();
    assert_eq!(names, ["api"], "the rest of the fold still reloads");
    let refused_names: Vec<&str> = refused.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(refused_names, ["db"], "and the one that did not is named");
    assert!(
        refused[0].reason.contains("already being reloaded"),
        "with the shepherd's own reason: {:?}",
        refused[0].reason
    );
}

/// fails if a reload stage's bound ignores how many instances are still
/// to swap: `advance_reload` replaces one at a time, so a three-instance
/// app costs three drains and three readiness waits, and a per-app bound
/// abandons the stage a third of the way through with the dependant
/// reloading against a half-swapped dependency.
#[tokio::test(start_paused = true)]
async fn a_reload_stage_is_bounded_by_the_swaps_it_is_waiting_for() {
    let h = harness(vec![ProcScript::never_exits()]);
    let mut web = AppConfig::minimal("web", "./web");
    web.listen_timeout = UpDuration::from_millis(4_000);
    web.graceful_timeout = UpDuration::from_millis(6_000);
    let started =
        reply_of(dispatch(envelope(1, Request::Start { apps: vec![web] }), &h.ctx).await);
    assert!(started.result.is_ok(), "web comes up: {started:?}");

    let one = [("web".to_string(), 1)].into_iter().collect();
    let three = [("web".to_string(), 3)].into_iter().collect();
    assert_eq!(
        reload_stage_bound(&h.ctx, &one),
        Duration::from_secs(15),
        "one swap is a drain plus a readiness wait, plus the stage slack"
    );
    assert_eq!(
        reload_stage_bound(&h.ctx, &three),
        Duration::from_secs(35),
        "three swaps are three of each, and the slack is spent once"
    );
}

/// fails if a reload's reply hands every row the same number, or none.
/// The deadline is computed per instance from that instance's own
/// timeouts, so a dog reading it does not have to infer one from a
/// Flockfile copy the shepherd may already have moved past.
#[tokio::test(start_paused = true)]
async fn a_reloads_reply_carries_each_instances_own_swap_deadline() {
    let h = harness(vec![ProcScript::never_exits(); 6]);
    let mut web = AppConfig::minimal("web", "./web");
    web.listen_timeout = UpDuration::from_millis(4_000);
    web.graceful_timeout = UpDuration::from_millis(6_000);
    let mut api = AppConfig::minimal("api", "./api");
    api.listen_timeout = UpDuration::from_millis(1_000);
    api.graceful_timeout = UpDuration::from_millis(2_000);
    for (id, app) in [(1, api), (2, web)] {
        let started =
            reply_of(dispatch(envelope(id, Request::Start { apps: vec![app] }), &h.ctx).await);
        assert!(started.result.is_ok(), "both come up: {started:?}");
    }

    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Reload {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Reloading { accepted, .. } = reply.result.unwrap() else {
        panic!("expected reloading")
    };

    // Sorted by name, so `api` is first. Each is its own two timeouts
    // plus the shepherd's five seconds of slack.
    let deadlines: Vec<(&str, Option<u64>)> = accepted
        .iter()
        .map(|info| (info.name.as_str(), info.reload_deadline_ms))
        .collect();
    assert_eq!(
        deadlines,
        vec![("api", Some(8_000)), ("web", Some(15_000))],
        "each row carries the budget its own swap is bounded by"
    );
}

/// fails if a listing reports a reload deadline. There is no swap
/// coming, so a reader waiting one out would wait for nothing.
#[tokio::test(start_paused = true)]
async fn a_plain_listing_carries_no_reload_deadline() {
    let h = harness(vec![ProcScript::never_exits()]);
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("web", "./web")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(started.result.is_ok(), "web comes up: {started:?}");

    let listed = reply_of(dispatch(envelope(2, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock[0].reload_deadline_ms, None);
}

/// fails if an instance a reload will not replace is handed a deadline
/// anyway. A stopped sheep is matched by the selector and reported in
/// the acceptance, but no swap is queued for it, so a number beside it
/// promises a replacement that is never coming.
#[tokio::test(start_paused = true)]
async fn an_instance_a_reload_skips_carries_no_deadline() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    let mut web = AppConfig::minimal("web", "./web");
    web.listen_timeout = UpDuration::from_millis(4_000);
    web.graceful_timeout = UpDuration::from_millis(6_000);
    let started =
        reply_of(dispatch(envelope(1, Request::Start { apps: vec![web] }), &h.ctx).await);
    assert!(started.result.is_ok(), "web comes up: {started:?}");
    let stopped = reply_of(
        dispatch(
            envelope(
                2,
                Request::Stop {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(stopped.result.is_ok(), "and goes down: {stopped:?}");

    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Reload {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Reloading { accepted, .. } = reply.result.unwrap() else {
        panic!("expected reloading")
    };
    assert_eq!(accepted.len(), 1);
    assert_ne!(
        accepted[0].status,
        ProcStatus::Online,
        "the row is the stopped instance, which is what makes it skippable"
    );
    assert_eq!(
        accepted[0].reload_deadline_ms, None,
        "no swap is queued for it, so there is no deadline to report"
    );
}

/// fails if the walk's wait follows one hop only: with `web -> mid ->
/// db` registered and a selector matching the two ends alone, `mid` is
/// not matched, so a one-hop intersection answers that nothing is
/// depended on and `web` restarts against a `db` no stage ever waited
/// for. That is the failure the ordered walk exists to prevent.
#[tokio::test(start_paused = true)]
async fn a_walk_waits_for_a_dependency_it_reaches_through_an_unmatched_hop() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    let mut web = AppConfig::minimal("web", "./web");
    web.depends_on = vec!["mid".to_string()];
    let mut mid = AppConfig::minimal("mid", "./mid");
    mid.depends_on = vec!["db".to_string()];
    // One request each, the way `start_api_before_the_db_it_waits_for`
    // does: a three-stage batch outlasts the default request budget.
    for (id, app) in [(1, AppConfig::minimal("db", "./db")), (2, mid), (3, web)] {
        let started =
            reply_of(dispatch(envelope(id, Request::Start { apps: vec![app] }), &h.ctx).await);
        assert!(started.result.is_ok(), "the chain comes up: {started:?}");
    }

    let listed = reply_of(dispatch(envelope(4, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    let selector = selector_of(SelectorSpec::Regex("^(web|db)$".to_string())).unwrap();
    let walk = ordered_walk(&h.ctx, &selector, &flock).expect("two names matched");

    assert_eq!(
        walk.stages,
        vec![vec!["db".to_string()], vec!["web".to_string()]],
        "the unmatched hop is not restarted, and the ends keep their order"
    );
    assert_eq!(
        walk.depended_on,
        ["db".to_string()].into_iter().collect::<BTreeSet<_>>(),
        "web reaches db through mid, so db's stage has to be held"
    );
}

/// The daemon's code becomes the CLI's exit status and its message is
/// all that is printed. Fails if either refusal answers a code that is
/// not `Internal`, since neither has one of its own and
/// `SupervisorError`'s `Display` is what tells them apart. Fails too if
/// `ReloadInFlight`'s arm drops the app's name, which says which reload
/// to wait for.
#[test]
fn a_refused_reload_is_internal_and_says_which_refusal_it_was() {
    let in_flight = rpc_error(&SupervisorError::ReloadInFlight("web".to_string()));
    assert_eq!(in_flight.code, RpcErrorCode::Internal);
    assert_eq!(in_flight.message, "web is already being reloaded");

    let shutting_down = rpc_error(&SupervisorError::EngineStopped);
    assert_eq!(shutting_down.code, RpcErrorCode::Internal);
    assert_eq!(shutting_down.message, "the supervisor engine has stopped");
}

/// Fails if `Reload` skips the selector conversion, or converts it
/// without reporting the failure: a peer regex the daemon cannot compile
/// is the client's usage error. `reload_request` converts before it can
/// ask what the selector matched, and an arm that answered `Reloading`
/// off an unconverted selector could still lose it.
#[tokio::test(start_paused = true)]
async fn a_bad_reload_selector_is_invalid_config() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Reload {
                    selector: SelectorSpec::Regex("((".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::InvalidConfig);
}

/// Fails if `Reopen` skips the selector conversion, or converts it
/// without reporting the failure: a peer regex the daemon cannot compile
/// is the client's usage error, not an internal one.
#[tokio::test(start_paused = true)]
async fn a_bad_reopen_selector_is_invalid_config() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Reopen {
                    selector: SelectorSpec::Regex("((".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::InvalidConfig);
}
