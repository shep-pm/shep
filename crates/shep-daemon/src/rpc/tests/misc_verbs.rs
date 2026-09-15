//! Everything that is not big enough for its own file: `Ping`,
//! `HostUsage`, a bad selector or peer regex, `Describe`'s fold
//! filter, `Reopen`, single-target `Reload`, `Subscribe`, `KillDaemon`,
//! `SaveRoll`, and the deadline/budget plumbing every verb rides on.

use super::*;

#[tokio::test(start_paused = true)]
async fn ping_answers_pong_on_the_same_envelope_id() {
    let h = harness(vec![]);
    let reply = reply_of(dispatch(envelope(9, Request::Ping), &h.ctx).await);
    assert_eq!(reply.id, 9);
    assert_eq!(reply.result.unwrap(), Response::Pong);
}

/// The door a listing actually knocks on. `HostState`'s own tests prove
/// the tick writes and the reader does not sample; this one proves the
/// request reaches that reader at all, which no test of the state alone
/// can see.
#[tokio::test(start_paused = true)]
async fn host_usage_serves_the_reading_the_tick_left_behind() {
    let mut h = harness(vec![]);
    h.ctx.host = crate::host::HostState::fixed(Some(HostUsage {
        cpu_percent: Some(11.5),
        memory_used_bytes: 39_963_869_184,
        memory_total_bytes: 51_539_607_552,
        disk_bytes_per_second: Some((1_258_291, 491_520)),
        network_bytes_per_second: Some((24_594, 9_260)),
    }));

    let reply = reply_of(dispatch(envelope(1, Request::HostUsage), &h.ctx).await);

    let Response::HostUsage(Some(usage)) = reply.result.unwrap() else {
        panic!("expected a host reading")
    };
    assert_eq!(usage.cpu_percent, Some(11.5));
    assert_eq!(usage.memory_used_bytes, 39_963_869_184);
    assert_eq!(usage.disk_bytes_per_second, Some((1_258_291, 491_520)));
    assert_eq!(usage.network_bytes_per_second, Some((24_594, 9_260)));
}

/// A platform `sysinfo` cannot read is not an error, and answering one
/// with `RpcErrorCode::Internal` would fail a listing over a
/// decoration. The harness default is exactly this state.
#[tokio::test(start_paused = true)]
async fn a_host_that_cannot_be_read_answers_none_rather_than_failing() {
    let h = harness(vec![]);

    let reply = reply_of(dispatch(envelope(1, Request::HostUsage), &h.ctx).await);

    assert_eq!(reply.result.unwrap(), Response::HostUsage(None));
}
#[tokio::test(start_paused = true)]
async fn a_selector_matching_nothing_is_not_found() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Stop {
                    selector: SelectorSpec::Name("ghost".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::NotFound);
}

#[tokio::test(start_paused = true)]
async fn a_bad_peer_regex_is_invalid_config_not_a_panic() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Describe {
                    selector: SelectorSpec::Regex("((".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::InvalidConfig);
}

#[tokio::test(start_paused = true)]
async fn describe_filters_by_fold() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let mut api = AppConfig::minimal("api", "./a");
    api.fold = Some("backend".to_string());
    dispatch(
        envelope(
            1,
            Request::Start {
                apps: vec![api, AppConfig::minimal("web", "./w")],
            },
        ),
        &h.ctx,
    )
    .await;
    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Describe {
                    selector: SelectorSpec::Fold("backend".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Described(hits) = reply.result.unwrap() else {
        panic!("expected described")
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "api");
}

/// Fails if `Reopen` is left to `run`'s catch-all arm, which answers
/// `Internal` for a request this daemon implements, or if it is routed
/// to another verb's supervisor call: `Stop` would stop the sheep.
#[tokio::test(start_paused = true)]
async fn reopen_routes_to_the_supervisor_and_leaves_the_sheep_running() {
    let h = harness(vec![ProcScript::never_exits()]);
    dispatch(
        envelope(
            1,
            Request::Start {
                apps: vec![AppConfig::minimal("web", "./srv")],
            },
        ),
        &h.ctx,
    )
    .await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Reopen {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Reopened(infos) = reply.result.unwrap() else {
        panic!("expected reopened")
    };
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].status, ProcStatus::Online);

    let listed = reply_of(dispatch(envelope(3, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(
        flock[0].status,
        ProcStatus::Online,
        "a reopen must not disturb the sheep it reopens"
    );
}

/// Fails if the arm is routed to another verb's supervisor call while
/// keeping `Response::Reloading`, which no assertion on the reply alone
/// can see. What separates a reload is the flock it leaves behind: two
/// entries in one instance slot, the drainee `Stopping` under its
/// original id and a replacement `Starting` under a new one.
///
/// The mid-swap state is not a race: nothing advances the clock, and
/// `ListFlock` is queued to an actor that runs `handle_reload` to
/// completion before it takes another message. Three scripts, of which a
/// correct run uses two; the third is sized for the spawn a broken arm
/// performs, so it lands as a live entry rather than as `Errored`.
#[tokio::test(start_paused = true)]
async fn reload_routes_to_the_supervisor_and_starts_a_swap() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    dispatch(
        envelope(
            1,
            Request::Start {
                apps: vec![AppConfig::minimal("web", "./srv")],
            },
        ),
        &h.ctx,
    )
    .await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
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
    assert_eq!(
        accepted[0].status,
        ProcStatus::Online,
        "the answer is the flock as it stood when the reload was accepted"
    );

    let listed = reply_of(dispatch(envelope(3, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(
        flock.len(),
        2,
        "a swap in progress is two entries in one instance slot, not one: {flock:?}"
    );
    assert_eq!(flock[0].id, accepted[0].id);
    assert_eq!(flock[0].status, ProcStatus::Stopping);
    assert_ne!(
        flock[1].id, accepted[0].id,
        "the replacement takes a new id"
    );
    assert_eq!(flock[1].status, ProcStatus::Starting);
}
/// Fails if the `ReopenFailed | FlushFailed` arm answers any other code:
/// `SpawnFailed` exits 7 and reads as "could not start it". Fails too if
/// it sends the bare payload instead of `err.to_string()`, since once the
/// two share a wire code `Display` is all that tells a reader which half
/// of the log plane failed.
#[test]
fn a_log_plane_failure_is_internal_and_says_which_half_failed() {
    let reopen = rpc_error(&SupervisorError::ReopenFailed(
        "web (id 0): could not reopen /logs/web-out.log: Permission denied".to_string(),
    ));
    assert_eq!(reopen.code, RpcErrorCode::Internal);
    assert_eq!(
        reopen.message,
        "log reopen failed: web (id 0): could not reopen \
         /logs/web-out.log: Permission denied"
    );

    let flush = rpc_error(&SupervisorError::FlushFailed(
        "/logs/web-out.log: Permission denied".to_string(),
    ));
    assert_eq!(flush.code, RpcErrorCode::Internal);
    assert_eq!(
        flush.message,
        "log flush failed: /logs/web-out.log: Permission denied"
    );
}

#[tokio::test(start_paused = true)]
async fn subscribe_hands_back_a_compiled_filter() {
    let h = harness(vec![]);
    let outcome = dispatch(
        envelope(
            1,
            Request::Subscribe {
                topics: vec!["process.*".to_string()],
            },
        ),
        &h.ctx,
    )
    .await;
    let Outcome::Subscribe { reply, filter } = outcome else {
        panic!("expected subscribe")
    };
    assert_eq!(reply.result.unwrap(), Response::Subscribed);
    assert_eq!(filter.patterns(), ["process.*"]);

    let bad = reply_of(
        dispatch(
            envelope(
                2,
                Request::Subscribe {
                    topics: vec!["[".to_string()],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(bad.result.unwrap_err().code, RpcErrorCode::InvalidConfig);
}

#[tokio::test(start_paused = true)]
async fn kill_daemon_asks_for_shutdown_without_taking_the_engine_down_itself() {
    let mut h = harness(vec![]);
    let Outcome::Shutdown(reply) = dispatch(envelope(1, Request::KillDaemon), &h.ctx).await
    else {
        panic!("expected a shutdown outcome")
    };
    assert_eq!(reply.result.unwrap(), Response::ShuttingDown);
    // Dispatch only reports the intent; the connection layer triggers it.
    assert!(!*h.shutdown_rx.borrow_and_update());
    h.ctx.shutdown();
    assert!(h.shutdown_rx.changed().await.is_ok());
    assert!(*h.shutdown_rx.borrow());
}

#[test]
fn budgets_default_and_clamp() {
    assert_eq!(budget(None), Duration::from_millis(DEFAULT_DEADLINE_MS));
    assert_eq!(budget(Some(250)), Duration::from_millis(250));
    assert_eq!(budget(Some(0)), Duration::from_millis(1));
    assert_eq!(
        budget(Some(u64::MAX)),
        Duration::from_millis(MAX_DEADLINE_MS)
    );
}

#[tokio::test(start_paused = true)]
async fn envelope_deadline_ms_actually_bounds_the_reply() {
    // Drives a real envelope's `deadline_ms` through `dispatch` into
    // `budget`. `Stop` on an `ignores_signals()` sheep waits the full
    // 1600ms `kill_timeout` ladder, far past this 1ms deadline, while a
    // build passing `budget(None)` would take the 5s default and pass.
    let h = harness(vec![ProcScript::ignores_signals()]);
    dispatch(
        envelope(
            1,
            Request::Start {
                apps: vec![AppConfig::minimal("web", "./srv")],
            },
        ),
        &h.ctx,
    )
    .await;

    let reply = reply_of(
        dispatch(
            Envelope {
                id: 2,
                deadline_ms: Some(1),
                body: Request::Stop {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            },
            &h.ctx,
        )
        .await,
    );
    let err = reply.result.unwrap_err();
    assert_eq!(
        err.code,
        RpcErrorCode::DeadlineExceeded,
        "a 1ms client deadline against a 1600ms kill ladder must expire, not {err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn work_past_its_deadline_answers_deadline_exceeded() {
    // Driven at the deadline seam with a future that never finishes: the
    // paused clock auto-advances the moment the test parks, so this is
    // instant and exact.
    let outcome = with_deadline(
        5,
        Duration::from_millis(250),
        std::future::pending::<Outcome>(),
    )
    .await;
    let reply = reply_of(outcome);
    assert_eq!(reply.id, 5);
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::DeadlineExceeded);
    assert!(err.message.contains("250 ms"), "{}", err.message);
}

/// The assertion reads the file the reply named and compares its app
/// count against the number the reply claimed, so a handler answering
/// `apps: 0` for a two-app flock reddens here.
#[tokio::test]
async fn save_roll_writes_the_file_it_names_and_counts_what_it_recorded() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![
                        AppConfig::minimal("web", "./srv"),
                        AppConfig::minimal("worker", "./work"),
                    ],
                },
            ),
            &h.ctx,
        )
        .await,
    );

    let reply = reply_of(dispatch(envelope(2, Request::SaveRoll), &h.ctx).await);
    let Ok(Response::RollSaved { path, apps }) = reply.result else {
        panic!("expected RollSaved, got {:?}", reply.result)
    };
    assert_eq!(apps, 2);

    let roll = crate::snapshot::read(std::path::Path::new(&path)).unwrap();
    assert_eq!(roll.apps.len(), 2, "the reply's count must match the file");
    assert_eq!(path, h.ctx.snapshot_path.display().to_string());
}

/// fails if the muster roll keeps the pre-scale count. This is the test for the
/// bug that is invisible until a reboot: the roll is what `shep muster` reads,
/// so a scale missing from it is a scale that silently reverts.
#[tokio::test]
async fn a_scale_is_recorded_in_the_roll_the_next_muster_reads() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    reply_of(dispatch(envelope(1, Request::Start { apps: vec![app] }), &h.ctx).await);

    reply_of(
        dispatch(
            envelope(
                2,
                Request::Scale {
                    name: "web".to_string(),
                    count: 4,
                },
            ),
            &h.ctx,
        )
        .await,
    );

    let reply = reply_of(dispatch(envelope(3, Request::SaveRoll), &h.ctx).await);
    let Ok(Response::RollSaved { path, .. }) = reply.result else {
        panic!("expected RollSaved, got {:?}", reply.result)
    };
    let roll = crate::snapshot::read(std::path::Path::new(&path)).unwrap();
    assert_eq!(roll.apps[0].app.instances, 4);
}

/// `web` at two instances, scaled to four, with one script left so the
/// first new spawn succeeds and the second fails. Three instances are
/// then running: a roll saying `2` stops one at the next muster, a roll
/// saying `4` brings up a count that never ran. Only `3` is the truth,
/// and it gets there only if the handler records off the `Err` path too.
///
/// The reply is asserted as well as the roll: recording what the daemon
/// did must not turn "three of four" into a success.
#[tokio::test]
async fn a_partial_scale_is_recorded_in_the_roll_and_still_reported_short() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;
    reply_of(dispatch(envelope(1, Request::Start { apps: vec![app] }), &h.ctx).await);

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Scale {
                    name: "web".to_string(),
                    count: 4,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::SpawnFailed);
    assert!(
        err.message.contains("3 of 4"),
        "the operator has to be told both numbers: {}",
        err.message
    );

    let saved = reply_of(dispatch(envelope(3, Request::SaveRoll), &h.ctx).await);
    let Ok(Response::RollSaved { path, .. }) = saved.result else {
        panic!("expected RollSaved, got {:?}", saved.result)
    };
    let roll = crate::snapshot::read(std::path::Path::new(&path)).unwrap();
    assert_eq!(
        roll.apps[0].app.instances, 3,
        "the roll must hold the three instances really running — not the \
         pre-scale two, and not the four that were asked for"
    );
}

/// Fails if the handler forwards `snapshot_now`'s engine-stopped `Ok(())`
/// as a success. A save that wrote nothing and said "saved" is the
/// failure mode an operator reboots into.
#[tokio::test]
async fn save_roll_against_a_stopped_engine_is_an_error_not_a_silent_success() {
    let h = harness(vec![]);
    h.ctx.supervisor.shutdown().await;

    let reply = reply_of(dispatch(envelope(1, Request::SaveRoll), &h.ctx).await);
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::Internal);
    assert!(
        err.message.contains("engine"),
        "the operator must be told why nothing was written: {}",
        err.message
    );
}

/// Assembling a flock that is already assembled starts nothing, so a
/// reply naming only what this call spawned cannot be told from "the roll
/// was empty".
///
/// One script: `web`'s first start consumes it, so a muster that started
/// the roll's apps unconditionally would exhaust the pool and land a
/// second, `Errored` `web` in the listing. The count and the name
/// assertion are what catch it.
#[tokio::test]
async fn a_second_muster_still_reports_the_flock_the_roll_restored() {
    let h = harness(vec![ProcScript::never_exits()]);
    reply_of(
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
    reply_of(dispatch(envelope(2, Request::SaveRoll), &h.ctx).await);

    let reply = reply_of(dispatch(envelope(3, Request::Muster), &h.ctx).await);
    let Ok(Response::Mustered(infos)) = reply.result else {
        panic!("expected Mustered, got {:?}", reply.result)
    };
    assert_eq!(
        infos.len(),
        1,
        "the sheep the roll restores, not the ones this call spawned"
    );
    assert_eq!(infos[0].name, "web");
    assert_eq!(infos[0].status, ProcStatus::Online);
}
