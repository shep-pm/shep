//! `only_describe`'s lamb tree, and dog staleness: pending vs stale,
//! the handshake facts a listing carries, and which silent dogs this
//! shepherd has given up on.

use super::*;

/// The split is a cost decision (`with_lambs`) and nothing else enforces
/// it: both arms build their rows from the same `snapshot_all`, so a
/// helper applied in the wrong place looks correct at every other level.
#[tokio::test(start_paused = true)]
async fn only_describe_carries_a_lamb_tree() {
    // A process table where FIRST_SCRIPTED_PID really has a child, so a
    // walk that runs finds something and a walk that does not is
    // distinguishable from one that found nothing.
    let h = harness_identifying(
        vec![ProcScript::never_exits()],
        vec![
            identity(FIRST_SCRIPTED_PID, None, "srv"),
            identity(FIRST_SCRIPTED_PID + 1, Some(FIRST_SCRIPTED_PID), "node"),
        ],
    );
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

    let listed = reply_of(dispatch(envelope(2, Request::ListFlock), &h.ctx).await);
    let Ok(Response::Flock(rows)) = listed.result else {
        panic!("expected a flock listing");
    };
    assert!(
        rows.iter().all(|row| row.lambs.is_none()),
        "ListFlock must not walk the process table"
    );

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
    let Ok(Response::Described(rows)) = described.result else {
        panic!("expected a describe listing");
    };
    assert_eq!(
        rows[0].lambs,
        Some(vec![Lamb::new(FIRST_SCRIPTED_PID + 1, "node")])
    );
}

/// Registers one built-in dog on `ctx`'s supervisor, the same way
/// `spawn_enabled_dogs` does at boot.
async fn start_dog(ctx: &RpcContext, name: &str) -> ProcessInfo {
    let spec = DogSpec {
        name: name.to_string(),
        source: DogSource::BuiltIn,
    };
    let app = crate::dogs::dog_app(&spec, &ctx.paths).expect("the dog fixture must assemble");
    ctx.supervisor
        .start_dog(app, DogSource::BuiltIn)
        .await
        .expect("the dog fixture must start")
}

/// The two lists `Request::DogStaleness` answers with.
async fn staleness(ctx: &RpcContext) -> (Vec<String>, Vec<String>) {
    let reply = reply_of(dispatch(envelope(1, Request::DogStaleness), ctx).await);
    let Ok(Response::DogStaleness { stale, pending }) = reply.result else {
        panic!("expected a dog staleness answer");
    };
    (stale, pending)
}

/// The sheep is the point, not scenery: a reader that walked every row
/// instead of every dog row would hold an operator's reload open waiting
/// for `web` to handshake, which a sheep never does.
#[tokio::test]
async fn a_flock_of_ordinary_sheep_has_nothing_stale_and_nothing_pending() {
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
    started.result.expect("the sheep must start");

    assert_eq!(staleness(&h.ctx).await, (Vec::new(), Vec::new()));
}

/// `shep flock` printed `(o.o) online`, restarts 0, for a dog whose own
/// log was filling with protocol refusals: `status` answers a question
/// the operator was not asking. Both halves are asserted, since losing
/// the liveness would be the same defect pointed the other way.
#[tokio::test]
async fn a_listing_says_which_dogs_have_answered_this_shepherd() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;

    let silent = list_flock(&h.ctx, 1).await;
    let dog = silent
        .iter()
        .find(|info| info.name == "metrics")
        .expect("the dog must be listed");
    assert_eq!(dog.handshook, Some(false));
    assert_eq!(
        dog.status,
        ProcStatus::Online,
        "the process is up, and the listing still says so"
    );

    h.ctx.dog_refusals.handshook("metrics");
    let talking = list_flock(&h.ctx, 2).await;
    assert_eq!(
        talking
            .iter()
            .find(|info| info.name == "metrics")
            .expect("still listed")
            .handshook,
        Some(true)
    );
}

/// A sheep does not speak this protocol at all, so it has no handshake to
/// report and `None` is the only honest answer. `Some(false)` here would
/// paint every sheep in the flock as broken.
#[tokio::test]
async fn a_sheep_carries_no_handshake_fact_at_all() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;

    let infos = list_flock(&h.ctx, 1).await;
    assert_eq!(infos[0].name, "web");
    assert_eq!(infos[0].handshook, None);
    assert_eq!(
        infos[0].dog_stale, None,
        "a sheep is never given up on, because it was never asked to answer"
    );
}

/// Both rows are `handshook: Some(false)` with a live process. One needs
/// nothing done about it, the dog having been spawned a moment ago; the
/// other is a dog this shepherd will never restart again.
#[tokio::test]
async fn a_listing_says_which_silent_dogs_this_shepherd_gave_up_on() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;

    let waiting = list_flock(&h.ctx, 1).await;
    let dog = waiting
        .iter()
        .find(|info| info.name == "metrics")
        .expect("the dog must be listed");
    assert_eq!(dog.handshook, Some(false));
    assert_eq!(
        dog.dog_stale,
        Some(false),
        "a dog that has not answered YET is not one this shepherd gave up on"
    );

    // The ladder, driven the same way `a_dog_being_restarted_is_pending_
    // and_then_stale` drives it: one refusal buys the restart, the second
    // is the give-up.
    h.ctx.dog_refusals.refused("metrics");
    h.ctx.dog_refusals.refused("metrics");

    let given_up = list_flock(&h.ctx, 2).await;
    let dog = given_up
        .iter()
        .find(|info| info.name == "metrics")
        .expect("still listed");
    assert_eq!(dog.dog_stale, Some(true));
    assert_eq!(
        dog.status,
        ProcStatus::Online,
        "the process is still up, and the listing still says so"
    );

    // And it heals: a dog that gets in clears everything held against
    // it, so the listing must stop reporting the give-up.
    h.ctx.dog_refusals.handshook("metrics");
    let talking = list_flock(&h.ctx, 3).await;
    let dog = talking
        .iter()
        .find(|info| info.name == "metrics")
        .expect("still listed");
    assert_eq!(dog.handshook, Some(true));
    assert_eq!(dog.dog_stale, Some(false));
}

/// fails if `describe` answers a different question from `flock` about
/// the same dog. It is the other verb an operator reads a listing from,
/// and the one `shep describe <dog>` reaches by name.
#[tokio::test]
async fn describe_carries_the_handshake_fact_too() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;

    let described = reply_of(
        dispatch(
            envelope(
                1,
                Request::Describe {
                    selector: SelectorSpec::Name("metrics".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Described(rows)) = described.result else {
        panic!("expected a describe listing");
    };
    assert_eq!(rows[0].handshook, Some(false));
}

/// A carried dog holds that state for the whole gap between the exec and
/// its reconnect, so a report taken while it holds would read "nothing
/// stale" as "every dog came back".
#[tokio::test]
async fn a_dog_that_has_not_handshaken_is_pending() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;

    assert_eq!(
        staleness(&h.ctx).await,
        (Vec::new(), vec!["metrics".to_string()])
    );

    h.ctx.dog_refusals.handshook("metrics");
    assert_eq!(
        staleness(&h.ctx).await,
        (Vec::new(), Vec::new()),
        "a dog talking to this shepherd is settled and is not worth reporting"
    );
}

/// A refused dog passes through this state on its way to being stale, so
/// a reader that treated it as settled would report every stale dog as
/// healthy. Drives the ladder rather than asserting on the record,
/// because the claim is about what a caller over the wire sees.
#[tokio::test]
async fn a_dog_being_restarted_is_pending_and_then_stale() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    h.ctx.dog_refusals.handshook("metrics");

    h.ctx.dog_refusals.refused("metrics");
    assert_eq!(
        staleness(&h.ctx).await,
        (Vec::new(), vec!["metrics".to_string()]),
        "one refusal buys a restart; it does not condemn the dog"
    );

    h.ctx.dog_refusals.refused("metrics");
    assert_eq!(
        staleness(&h.ctx).await,
        (vec!["metrics".to_string()], Vec::new()),
        "a stale dog is a finding, not something still to wait on"
    );
}

/// A dog with no process, out of its restart budget, parked in a backoff
/// or stopped by an operator, cannot handshake, so waiting on one would
/// make every later reload pay the whole budget for a dog already
/// reported broken everywhere else.
#[tokio::test]
async fn a_dog_that_has_stopped_running_is_not_waited_on() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    assert_eq!(staleness(&h.ctx).await.1, vec!["metrics".to_string()]);

    h.ctx
        .supervisor
        .stop(ProcessSelector::Name("metrics".to_string()))
        .await
        .expect("the dog must stop");

    assert_eq!(
        staleness(&h.ctx).await,
        (Vec::new(), Vec::new()),
        "a dog that is not running has nothing to answer with"
    );
}
