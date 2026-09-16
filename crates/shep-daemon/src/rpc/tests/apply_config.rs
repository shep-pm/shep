//! `ApplyConfig`: a duplicate name refused whole, what it records in
//! the registry, and an unregistered app named back in the reply.

use super::*;

/// One app as a Flockfile would declare it: the config, plus the keys
/// the document literally wrote. `declared` is what an apply keys on, so
/// a fixture that left it empty would declare nothing and apply nothing.
fn declared(name: &str, script: &str, keys: &[&str]) -> DeclaredApp {
    DeclaredApp {
        config: AppConfig::minimal(name, script),
        declared: keys.iter().map(|k| (*k).to_string()).collect(),
        declared_env: BTreeSet::new(),
    }
}

/// `handle_apply_config` reads the override store once for the whole
/// file, so a second entry of the same name merges against the store as
/// the first entry found it and the first entry's record is lost.
/// `normalize_all` refuses a duplicate on the `Start` path and is not on
/// this one.
#[tokio::test(start_paused = true)]
async fn apply_config_refuses_a_request_naming_one_app_twice() {
    let h = harness(vec![ProcScript::never_exits()]);
    let _started = reply_of(
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

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::ApplyConfig {
                    apps: vec![
                        declared("web", "./one", &["script"]),
                        declared("web", "./two", &["script"]),
                    ],
                    reset: ResetDepth::None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("web"), "the name is named: {err:?}");

    // Refused BEFORE anything was touched, which is the half an error
    // code alone does not prove: the flock still runs what `Start`
    // registered, not either of the two scripts the request carried.
    let listed = reply_of(dispatch(envelope(3, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    let roll = h.ctx.registry.roll(&flock, 0);
    assert_eq!(roll.apps[0].app.script, "./srv");
}

/// The `Scale` arm's reasoning, applied to this one: a change that
/// reached the stored spec and not the roll is undone by the next
/// reboot.
#[tokio::test(start_paused = true)]
async fn apply_config_records_what_it_applied_in_the_registry() {
    let h = harness(vec![ProcScript::never_exits()]);
    let _started = reply_of(
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

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::ApplyConfig {
                    apps: vec![DeclaredApp {
                        config: {
                            let mut app = AppConfig::minimal("web", "./srv");
                            app.max_restarts = 99;
                            app
                        },
                        declared: ["max_restarts"].iter().map(|k| (*k).to_string()).collect(),
                        declared_env: BTreeSet::new(),
                    }],
                    reset: ResetDepth::None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Applied(report) = reply.result.unwrap() else {
        panic!("expected applied")
    };
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].name, "web");
    assert_eq!(report[0].applied, vec!["max_restarts".to_string()]);
    assert_eq!(report[0].refused, None);

    let listed = reply_of(dispatch(envelope(3, Request::ListFlock), &h.ctx).await);
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    let roll = h.ctx.registry.roll(&flock, 0);
    assert_eq!(roll.apps[0].app.max_restarts, 99);
}

/// One app that cannot be applied must not cost the rest of the file its
/// load, so a miss is a per-app refusal inside an `Ok`, never an `Err`.
#[tokio::test(start_paused = true)]
async fn apply_config_refuses_an_unregistered_app_inside_the_reply() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::ApplyConfig {
                    apps: vec![declared("ghost", "./srv", &["script"])],
                    reset: ResetDepth::None,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Response::Applied(report) = reply.result.unwrap() else {
        panic!("expected applied")
    };
    assert_eq!(report.len(), 1);
    let refused = report[0].refused.as_deref().unwrap_or_default();
    assert!(
        refused.contains("ghost"),
        "the refusal names the app: {refused}"
    );
}
