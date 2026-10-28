//! `shep delete` on a dog: a built-in refused by this shepherd rather than
//! by whichever CLI typed the verb, an adopted one deregistered for good, and
//! a name a sheep kept still deletable.

use super::*;
use std::collections::BTreeMap;

fn dog(name: &str, source: DogSource) -> crate::dogs::DogSpec {
    crate::dogs::DogSpec {
        name: name.to_string(),
        source,
        env: BTreeMap::new(),
    }
}

fn adopted(name: &str) -> DogSource {
    DogSource::Adopted {
        grants: None,
        path: format!("/usr/local/bin/shep-{name}"),
        channel: false,
    }
}

async fn delete(ctx: &RpcContext, id: u64, selector: SelectorSpec) -> Reply {
    reply_of(dispatch(envelope(id, Request::Delete { selector }), ctx).await)
}

/// The refusal an operator reads, held as one string rather than a shape:
/// the sentence is the actionable half, and it names the verb to use instead.
fn built_in_refusal(name: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidConfig,
        message: crate::dogs::built_in_dog_refusal(name),
        daemon_version: None,
    }
}

/// Refusing here rather than in the CLI is what makes the guard outlive a
/// `cargo install shep` that has not restarted the shepherd yet: an older
/// binary sends the same `Delete` and meets the same answer.
#[tokio::test(start_paused = true)]
async fn deleting_a_built_in_dog_at_rest_is_refused_and_leaves_it_listed() {
    let h = harness(vec![]);
    crate::dogs::register_dogs_at_rest(
        &[dog("metrics", DogSource::BuiltIn { grants: None })],
        &[dog("metrics", DogSource::BuiltIn { grants: None })],
        &h.ctx.paths,
        &h.ctx.supervisor,
    )
    .await;

    let refused = delete(&h.ctx, 1, SelectorSpec::Name("metrics".to_string())).await;

    assert_eq!(refused.result, Err(built_in_refusal("metrics")));
    let flock = list_flock(&h.ctx, 2).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(flock[0].name, "metrics");
    assert_eq!(flock[0].dog, Some(DogSource::BuiltIn { grants: None }));
}

/// A dog on its kill ladder is refused for the same reason as one at rest:
/// the refusal is about what the name is, not about whether a process stands
/// behind it.
#[tokio::test(start_paused = true)]
async fn deleting_a_running_built_in_dog_is_refused() {
    let h = harness(vec![ProcScript::never_exits()]);
    let started = enable_dog(&h.ctx, 1, "bark").await;
    assert_eq!(started.status, ProcStatus::Online);

    let refused = delete(&h.ctx, 2, SelectorSpec::Name("bark".to_string())).await;

    assert_eq!(refused.result, Err(built_in_refusal("bark")));
    let flock = list_flock(&h.ctx, 3).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(flock[0].status, ProcStatus::Online, "{flock:?}");
}

/// The whole flock is one answer, so a mixed command that reaches a built-in
/// is refused before it deregisters anything: half a `delete all` that took
/// the sheep and refused the dog would leave an operator reading a listing
/// rather than a refusal.
#[tokio::test(start_paused = true)]
async fn a_delete_that_reaches_a_built_in_dog_deregisters_nothing_it_also_named() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    start_web(&h).await;
    enable_dog(&h.ctx, 2, "metrics").await;

    let refused = delete(&h.ctx, 3, SelectorSpec::Name("metrics".to_string())).await;

    assert_eq!(refused.result, Err(built_in_refusal("metrics")));
    let flock = list_flock(&h.ctx, 4).await;
    assert_eq!(flock.len(), 2, "{flock:?}");
    assert!(
        flock.iter().any(|row| row.name == "web"),
        "the sheep named beside the dog is untouched: {flock:?}"
    );
}

/// An adopted dog is the one a `delete` removes: the entry goes, and the next
/// boot is the CLI's config edit's job, since this daemon never reads
/// `shep.toml`.
#[tokio::test(start_paused = true)]
async fn deleting_an_adopted_dog_at_rest_deregisters_it() {
    let h = harness(vec![]);
    crate::dogs::register_dogs_at_rest(
        &[dog("log-rotate", adopted("log-rotate"))],
        &[],
        &h.ctx.paths,
        &h.ctx.supervisor,
    )
    .await;
    let at_rest = list_flock(&h.ctx, 1).await;
    assert_eq!(at_rest.len(), 1, "{at_rest:?}");

    let deleted = delete(&h.ctx, 2, SelectorSpec::Name("log-rotate".to_string())).await;

    assert_eq!(deleted.result, Ok(Response::Deleted(vec![at_rest[0].id])));
    let flock = list_flock(&h.ctx, 3).await;
    assert!(flock.is_empty(), "{flock:?}");
}

/// A dog mid-stop still answers `Deleted` once its process is gone, the same
/// reply a sheep's delete answers, so no reader has to know which kind it was.
#[tokio::test(start_paused = true)]
async fn deleting_a_running_adopted_dog_deregisters_it_when_it_stops() {
    let h = harness(vec![ProcScript::never_exits()]);
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::EnableDog {
                    name: "log-rotate".to_string(),
                    env: BTreeMap::default(),
                    source: adopted("log-rotate"),
                },
            ),
            &h.ctx,
        )
        .await,
    )
    .result
    .expect("the dog started");
    let id = match started {
        Response::DogStarted(info) => info.id,
        other => panic!("expected DogStarted, got {other:?}"),
    };

    let deleted = delete(&h.ctx, 2, SelectorSpec::Name("log-rotate".to_string())).await;

    assert_eq!(deleted.result, Ok(Response::Deleted(vec![id])));
    let flock = list_flock(&h.ctx, 3).await;
    assert!(flock.is_empty(), "{flock:?}");
}

/// The guard reads the entry's source, not its name: #770 keeps a saved sheep
/// called `metrics`, and deleting that sheep is an operator's ordinary call.
#[tokio::test(start_paused = true)]
async fn a_sheep_that_kept_a_built_in_dogs_name_is_deletable() {
    let h = harness(vec![ProcScript::never_exits()]);
    reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("metrics", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    )
    .result
    .expect("the sheep started");
    crate::dogs::register_dogs_at_rest(
        &[dog("metrics", DogSource::BuiltIn { grants: None })],
        &[],
        &h.ctx.paths,
        &h.ctx.supervisor,
    )
    .await;
    let before = list_flock(&h.ctx, 2).await;
    assert_eq!(before[0].dog, None, "{before:?}");

    let deleted = delete(&h.ctx, 3, SelectorSpec::Name("metrics".to_string())).await;

    assert_eq!(deleted.result, Ok(Response::Deleted(vec![before[0].id])));
    let flock = list_flock(&h.ctx, 4).await;
    assert!(flock.is_empty(), "{flock:?}");
}

/// `all` is the flock, and a dog is not in it for an acting verb: the sweep
/// that stops every sheep must never be the one that removes a dog.
#[tokio::test(start_paused = true)]
async fn a_wildcard_delete_takes_the_sheep_and_passes_the_dog_by() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;
    crate::dogs::register_dogs_at_rest(
        &[dog("bark", DogSource::BuiltIn { grants: None })],
        &[],
        &h.ctx.paths,
        &h.ctx.supervisor,
    )
    .await;

    let deleted = delete(&h.ctx, 2, SelectorSpec::All).await;

    let ids = match deleted.result {
        Ok(Response::Deleted(ids)) => ids,
        other => panic!("expected Deleted, got {other:?}"),
    };
    assert_eq!(ids.len(), 1, "{ids:?}");
    let flock = list_flock(&h.ctx, 3).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(flock[0].name, "bark");
}
