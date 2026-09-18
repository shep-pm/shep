//! `Trigger` and `Signal`: budget, grammar, the newline refusal, an
//! oversized action timeout, a bad selector, and an empty match.

use super::*;

/// `TimedOut` rather than `NoChannel` is the assertion that matters: the
/// action was really delivered and waited on, and `web`'s 3s
/// `action_timeout` elapsed inside the request's own budget. Raise it
/// past [`DEFAULT_DEADLINE_MS`] and the reply becomes `DeadlineExceeded`
/// instead, which names no sheep; that ordering is pinned right below in
/// `an_oversized_action_timeout_loses_the_race`.
///
/// Nothing answers, because the harness keeps no handle on its runner.
#[tokio::test(start_paused = true)]
async fn trigger_routes_to_the_flock_and_reports_each_match_within_the_budget() {
    // Two apps, not one: ids start at 0, so a single-app harness would
    // give `web` id 0, indistinguishable from a row-mapping bug that
    // leaves the field's default. `other` first pushes `web` to id 1.
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let mut web = AppConfig::minimal("web", "./srv");
    web.channel = true;
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("other", "./o"), web],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Started(started) = started.result.unwrap() else {
        panic!("expected started")
    };
    let web_id = started
        .iter()
        .find(|i| i.name == "web")
        .expect("web registered")
        .id;
    assert_ne!(web_id, 0, "the test's own premise: web must not be id 0");

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Trigger {
                    selector: SelectorSpec::Name("web".to_string()),
                    action: "gc".to_string(),
                    params: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Triggered(rows) = reply.result.unwrap() else {
        panic!("expected triggered")
    };
    assert_eq!(
        rows,
        vec![ActionReply {
            id: web_id,
            name: "web".to_string(),
            outcome: ActionOutcome::TimedOut,
        }]
    );
}

/// A bad signal name must be refused at the dispatch boundary with
/// `InvalidConfig`: an operator who typed `SIGHUPP` needs the accepted
/// list, and only this arm has it.
#[tokio::test]
async fn a_signal_name_outside_the_grammar_is_refused_with_the_accepted_list() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Signal {
                    selector: SelectorSpec::All,
                    signal: "SIGHUPP".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("SIGHUPP"), "{}", err.message);
    assert!(err.message.contains("SIGHUP"), "{}", err.message);
    assert!(err.message.contains("SIGUSR2"), "{}", err.message);
}

/// Refused at the dispatch boundary, so it never reaches `send_line`.
/// There is no sheep in this fixture to answer it, so a `NotFound` here
/// would mean the refusal was skipped rather than that it fired.
#[tokio::test]
async fn a_line_carrying_a_newline_is_refused_before_it_reaches_the_supervisor() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::SendLine {
                    selector: SelectorSpec::All,
                    line: "reload\nrm -rf /".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("newline"), "{}", err.message);
}

/// `web`'s `action_timeout` is set past the 5s default budget and the
/// request carries no deadline of its own, so under the paused clock
/// `dispatch`'s own `with_deadline` wins and the reply is
/// `DeadlineExceeded` rather than a `Triggered` row.
/// `shep_core::config::normalize` refuses only a timeout no caller could
/// ever satisfy, so anything under that has to lose this race.
#[tokio::test(start_paused = true)]
async fn an_oversized_action_timeout_loses_the_race() {
    let h = harness(vec![ProcScript::never_exits()]);
    let mut web = AppConfig::minimal("web", "./srv");
    web.channel = true;
    web.action_timeout = UpDuration::from_millis(9_000); // > DEFAULT_DEADLINE_MS (5s)
    reply_of(dispatch(envelope(1, Request::Start { apps: vec![web] }), &h.ctx).await);

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Trigger {
                    selector: SelectorSpec::Name("web".to_string()),
                    action: "gc".to_string(),
                    params: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(
        reply.result.unwrap_err().code,
        RpcErrorCode::DeadlineExceeded,
        "an action_timeout past the caller's default budget must lose that race, not \
         report an honest TimedOut row nobody can reach"
    );
}

/// Fails if `Trigger` skips the selector conversion, or converts it
/// without reporting the failure: a peer regex the daemon cannot compile
/// is the client's usage error, not an internal one.
#[tokio::test(start_paused = true)]
async fn a_bad_trigger_selector_is_invalid_config() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Trigger {
                    selector: SelectorSpec::Regex("((".to_string()),
                    action: "gc".to_string(),
                    params: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::InvalidConfig);
}

/// A selector matching no registered sheep is a whole-request
/// `NotFound`, kept separate from a per-row `NoChannel`, which only
/// appears inside a non-empty match.
#[tokio::test(start_paused = true)]
async fn a_trigger_matching_nothing_is_not_found() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::Trigger {
                    selector: SelectorSpec::Name("ghost".to_string()),
                    action: "gc".to_string(),
                    params: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::NotFound);
}
