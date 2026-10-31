//! A wildcard passes a disabled app by and a name still reaches it, and
//! `enable` and `disable` take any selector.

use super::*;

fn by_name(name: &str) -> SelectorSpec {
    SelectorSpec::Name(name.to_string())
}

fn in_fold(name: &str, fold: &str) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.fold = Some(fold.to_string());
    app
}

async fn start(h: &Harness, id: u64, apps: Vec<AppConfig>) {
    let reply = reply_of(dispatch(envelope(id, Request::Start { apps }), &h.ctx).await);
    assert!(reply.result.is_ok(), "fixture start failed: {reply:?}");
}

async fn send(h: &Harness, id: u64, body: Request) -> Reply {
    reply_of(dispatch(envelope(id, body), &h.ctx).await)
}

async fn disable(h: &Harness, id: u64, selector: SelectorSpec) -> Vec<ProcessInfo> {
    match send(h, id, Request::Disable { selector }).await.result {
        Ok(Response::Disabled(infos)) => infos,
        other => panic!("expected Disabled, got {other:?}"),
    }
}

async fn enable(h: &Harness, id: u64, selector: SelectorSpec) -> Reply {
    send(h, id, Request::Enable { selector }).await
}

fn names(infos: &[ProcessInfo]) -> Vec<&str> {
    let mut names: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// A flock of `api` (disabled), `web` and `worker`, all started.
async fn flock_with_api_disabled(h: &Harness) {
    start(
        h,
        1,
        vec![
            AppConfig::minimal("api", "./srv"),
            AppConfig::minimal("web", "./srv"),
            AppConfig::minimal("worker", "./srv"),
        ],
    )
    .await;
    disable(h, 2, by_name("api")).await;
}

/// Fails if the supervisor's matching stops passing a disabled app by: `stop
/// all` would answer a row for `api` too.
#[tokio::test(start_paused = true)]
async fn stop_all_skips_a_disabled_app() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    flock_with_api_disabled(&h).await;

    let Ok(Response::Stopped(infos)) = send(
        &h,
        3,
        Request::Stop {
            selector: SelectorSpec::All,
        },
    )
    .await
    .result
    else {
        panic!("expected Stopped")
    };

    assert_eq!(names(&infos), ["web", "worker"]);
}

#[tokio::test(start_paused = true)]
async fn delete_all_skips_a_disabled_app_and_leaves_it_in_the_flock() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    flock_with_api_disabled(&h).await;

    let reply = send(
        &h,
        3,
        Request::Delete {
            selector: SelectorSpec::All,
        },
    )
    .await;
    let Ok(Response::Deleted(ids)) = reply.result else {
        panic!("expected Deleted, got {reply:?}")
    };

    assert_eq!(ids.len(), 2, "{ids:?}");
    let flock = list_flock(&h.ctx, 4).await;
    assert_eq!(names(&flock), ["api"]);
    assert_eq!(flock[0].disabled, Some(true));
}

#[tokio::test(start_paused = true)]
async fn a_regex_and_a_fold_pass_a_disabled_app_by() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    start(
        &h,
        1,
        vec![
            in_fold("api", "web"),
            in_fold("site", "web"),
            AppConfig::minimal("worker", "./srv"),
        ],
    )
    .await;
    disable(&h, 2, by_name("api")).await;

    let Ok(Response::Stopped(by_fold)) = send(
        &h,
        3,
        Request::Stop {
            selector: SelectorSpec::Fold("web".to_string()),
        },
    )
    .await
    .result
    else {
        panic!("expected Stopped")
    };
    assert_eq!(names(&by_fold), ["site"]);

    let regex = SelectorSpec::Regex("^a".to_string());
    let missed = send(&h, 4, Request::Stop { selector: regex }).await;
    assert_eq!(
        missed.result.unwrap_err().code,
        RpcErrorCode::NotFound,
        "`^a` only matches the disabled `api`"
    );
}

/// Fails if the sweep's condition also gates an exact selector: naming
/// `api` must still reach it for every verb that is not a spawn.
#[tokio::test(start_paused = true)]
async fn naming_a_disabled_app_still_reaches_stop_signal_describe_and_delete() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    flock_with_api_disabled(&h).await;

    let Ok(Response::Stopped(stopped)) = send(
        &h,
        3,
        Request::Stop {
            selector: by_name("api"),
        },
    )
    .await
    .result
    else {
        panic!("expected Stopped")
    };
    assert_eq!(names(&stopped), ["api"]);

    let Ok(Response::Signalled(rows)) = send(
        &h,
        4,
        Request::Signal {
            selector: by_name("api"),
            signal: "TERM".to_string(),
            lamb: None,
        },
    )
    .await
    .result
    else {
        panic!("expected Signalled")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "api");

    let described = describe(&h.ctx, 5, SelectorSpec::All).await;
    assert_eq!(names(&described), ["api", "web", "worker"]);

    let Ok(Response::Deleted(deleted)) = send(
        &h,
        6,
        Request::Delete {
            selector: by_name("api"),
        },
    )
    .await
    .result
    else {
        panic!("expected Deleted")
    };
    assert_eq!(deleted.len(), 1);
    assert_eq!(names(&list_flock(&h.ctx, 7).await), ["web", "worker"]);
}

#[tokio::test(start_paused = true)]
async fn disable_takes_a_fold_and_disables_every_app_in_it() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    start(
        &h,
        1,
        vec![
            in_fold("api", "web"),
            in_fold("site", "web"),
            AppConfig::minimal("worker", "./srv"),
        ],
    )
    .await;

    let disabled = disable(&h, 2, SelectorSpec::Fold("web".to_string())).await;

    assert_eq!(names(&disabled), ["api", "site"]);
    let flock = list_flock(&h.ctx, 3).await;
    for info in &flock {
        let parked = info.name != "worker";
        assert_eq!(info.disabled, parked.then_some(true), "{info:?}");
        assert_eq!(info.status == ProcStatus::Stopped, parked, "{info:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn disable_all_takes_every_enabled_sheep_and_no_dog() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    start(&h, 1, vec![AppConfig::minimal("api", "./srv")]).await;
    let dog = enable_dog(&h.ctx, 2, "metrics").await;

    let disabled = disable(&h, 3, SelectorSpec::All).await;

    assert_eq!(names(&disabled), ["api"]);
    let flock = list_flock(&h.ctx, 4).await;
    let dog_row = flock.iter().find(|info| info.id == dog.id).unwrap();
    assert_eq!(dog_row.status, ProcStatus::Online, "{dog_row:?}");
    assert_eq!(dog_row.disabled, None);
}

/// Fails if `enable`'s sweep uses the ordinary condition: it would find no
/// disabled app at all.
#[tokio::test(start_paused = true)]
async fn enable_all_reaches_every_disabled_sheep_and_no_dog() {
    let h = harness(vec![ProcScript::never_exits(); 6]);
    start(
        &h,
        1,
        vec![
            AppConfig::minimal("api", "./srv"),
            AppConfig::minimal("web", "./srv"),
            AppConfig::minimal("worker", "./srv"),
        ],
    )
    .await;
    let dog = enable_dog(&h.ctx, 2, "metrics").await;
    disable(&h, 3, by_name("api")).await;
    disable(&h, 4, by_name("web")).await;

    let Ok(Response::Enabled(enabled)) = enable(&h, 5, SelectorSpec::All).await.result else {
        panic!("expected Enabled")
    };

    assert_eq!(names(&enabled), ["api", "web"]);
    let flock = list_flock(&h.ctx, 6).await;
    assert!(
        flock.iter().all(|info| info.disabled.is_none()),
        "{flock:?}"
    );
    assert!(
        flock.iter().all(|info| info.status == ProcStatus::Online),
        "{flock:?}"
    );
    assert_eq!(
        flock.iter().filter(|info| info.id == dog.id).count(),
        1,
        "the dog is neither restarted nor re-registered"
    );
}

#[tokio::test(start_paused = true)]
async fn enable_by_fold_and_by_regex_reaches_only_disabled_members() {
    let h = harness(vec![ProcScript::never_exits(); 5]);
    start(
        &h,
        1,
        vec![
            in_fold("api", "web"),
            in_fold("site", "web"),
            AppConfig::minimal("worker", "./srv"),
        ],
    )
    .await;
    disable(&h, 2, by_name("api")).await;

    let Ok(Response::Enabled(by_fold)) = enable(&h, 3, SelectorSpec::Fold("web".to_string()))
        .await
        .result
    else {
        panic!("expected Enabled")
    };
    assert_eq!(names(&by_fold), ["api"]);

    disable(&h, 4, by_name("worker")).await;
    let regex = SelectorSpec::Regex("^w".to_string());
    let Ok(Response::Enabled(by_regex)) = enable(&h, 5, regex).await.result else {
        panic!("expected Enabled")
    };
    assert_eq!(names(&by_regex), ["worker"]);
}

#[tokio::test(start_paused = true)]
async fn enable_all_with_nothing_disabled_changes_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    start(&h, 1, vec![AppConfig::minimal("api", "./srv")]).await;

    let Ok(Response::Enabled(enabled)) = enable(&h, 2, SelectorSpec::All).await.result else {
        panic!("expected Enabled")
    };

    assert!(enabled.is_empty(), "{enabled:?}");
}

/// Fails if a disable sweep over apps that are all disabled already answers
/// `NotFound`: the fold exists and every app in it is parked as asked.
#[tokio::test(start_paused = true)]
async fn disabling_a_fold_again_changes_nothing_and_succeeds() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    start(&h, 1, vec![in_fold("api", "web"), in_fold("site", "web")]).await;
    disable(&h, 2, SelectorSpec::Fold("web".to_string())).await;

    let again = disable(&h, 3, SelectorSpec::Fold("web".to_string())).await;
    assert!(again.is_empty(), "{again:?}");
    let all = disable(&h, 4, SelectorSpec::All).await;
    assert!(all.is_empty(), "{all:?}");

    let missed = send(
        &h,
        5,
        Request::Disable {
            selector: SelectorSpec::Fold("nope".to_string()),
        },
    )
    .await;
    assert_eq!(missed.result.unwrap_err().code, RpcErrorCode::NotFound);
}

/// Fails if the log verbs resolve through the ordinary sweep: `flush all` and
/// `reopen all` would skip `api`, or answer `NotFound` with every app parked.
#[tokio::test(start_paused = true)]
async fn flush_and_reopen_all_still_reach_a_disabled_app() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    start(
        &h,
        1,
        vec![
            AppConfig::minimal("api", "./srv"),
            AppConfig::minimal("web", "./srv"),
        ],
    )
    .await;
    disable(&h, 2, by_name("api")).await;

    let Ok(Response::Flushed(flushed)) = send(
        &h,
        3,
        Request::Flush {
            selector: SelectorSpec::All,
        },
    )
    .await
    .result
    else {
        panic!("expected Flushed")
    };
    assert_eq!(names(&flushed), ["api", "web"]);

    disable(&h, 4, by_name("web")).await;
    let Ok(Response::Reopened(reopened)) = send(
        &h,
        5,
        Request::Reopen {
            selector: SelectorSpec::All,
        },
    )
    .await
    .result
    else {
        panic!("expected Reopened")
    };
    assert_eq!(names(&reopened), ["api", "web"]);
}
