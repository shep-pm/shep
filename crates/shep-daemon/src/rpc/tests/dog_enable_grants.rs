//! When `EnableDog` records a dog's grants: before the dog can be served, and
//! only for a dog.

use super::*;
use shep_core::permissions::{Grants, Permission};
use std::time::Duration;

use crate::rpc::ConnId;

fn adopted_with(grants: Grants) -> Request {
    Request::EnableDog {
        name: "otel".to_string(),
        env: BTreeMap::default(),
        source: DogSource::Adopted {
            grants: Some(grants),
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: false,
        },
    }
}

/// The supervisor starts a dog it was queued even when the request's
/// deadline drops the dispatch first, so the grants cannot wait for the
/// start's answer: until they are recorded the dog is undeclared, with
/// full access.
#[tokio::test(start_paused = true)]
async fn a_dispatch_cut_short_after_queueing_the_start_still_records_the_grants() {
    let h = harness(vec![ProcScript::never_exits()]);
    let grants = Grants::new([Permission::FlockRead]);

    let cut = tokio::time::timeout(
        Duration::ZERO,
        dispatch(envelope(1, adopted_with(grants.clone())), &h.ctx),
    )
    .await;

    assert!(cut.is_err(), "the dispatch was meant to be cut short");
    assert_eq!(h.ctx.known_dogs.grants("otel"), Some(grants));
}

/// A sheep holding the name is refused, and that is not a dog to remember:
/// the name stays unknown and keeps no grants.
#[tokio::test(start_paused = true)]
async fn an_enable_refused_for_a_sheeps_name_remembers_no_dog() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;
    let request = Request::EnableDog {
        name: "web".to_string(),
        env: BTreeMap::default(),
        source: DogSource::Adopted {
            grants: Some(Grants::default()),
            path: "/usr/local/bin/shep-web".to_string(),
            channel: false,
        },
    };

    let refused = reply_of(dispatch(envelope(2, request), &h.ctx).await).result;

    let Err(err) = refused else {
        panic!("a sheep's name must be refused, got {refused:?}")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{err:?}");
    assert!(!h.ctx.known_dogs.contains("web"));
}

/// A name that was already a dog keeps what it was granted when a later
/// enable of it is refused.
#[tokio::test(start_paused = true)]
async fn a_refused_enable_leaves_the_grants_the_name_already_held() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;
    let before = Grants::new([Permission::LogsRead]);
    h.ctx.known_dogs.insert("web", Some(before.clone()));
    let request = Request::EnableDog {
        name: "web".to_string(),
        env: BTreeMap::default(),
        source: DogSource::Adopted {
            grants: Some(Grants::default()),
            path: "/usr/local/bin/shep-web".to_string(),
            channel: false,
        },
    };

    let _ = dispatch(envelope(2, request), &h.ctx).await;

    assert_eq!(h.ctx.known_dogs.grants("web"), Some(before));
}

/// Sends `request` on `dog`'s own connection.
async fn as_dog(h: &crate::testing::Harness, dog: &str, request: Request) -> Reply {
    reply_of(super::super::dispatch(envelope(1, request), ConnId::next(), Some(dog), &h.ctx).await)
}

fn enabling(name: &str, grants: Option<Grants>) -> Request {
    Request::EnableDog {
        name: name.to_string(),
        env: BTreeMap::default(),
        source: DogSource::Adopted {
            grants,
            path: format!("/usr/local/bin/shep-{name}"),
            channel: false,
        },
    }
}

/// The decision log's rule, that a dog never grants itself anything: the
/// grants an `EnableDog` carries come from the operator's CLI, and a dog
/// holding `dogs.control` can send one of its own.
#[tokio::test(start_paused = true)]
async fn a_dog_cannot_widen_its_own_grants_by_enabling_itself() {
    let h = harness(vec![ProcScript::never_exits()]);
    let held = Grants::new([Permission::DogsControl]);
    h.ctx.known_dogs.insert("otel", Some(held.clone()));

    let reply = as_dog(&h, "otel", enabling("otel", None)).await;

    assert!(reply.result.is_ok(), "{reply:?}");
    assert_eq!(h.ctx.known_dogs.recorded("otel"), Some(Some(held)));
}

/// The same door for a second dog: the enabler's word is not the operator's.
/// A name it has never been told of is granted nothing, not left undeclared.
#[tokio::test(start_paused = true)]
async fn a_dog_enabling_another_dog_cannot_choose_that_dogs_grants() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    h.ctx
        .known_dogs
        .insert("otel", Some(Grants::new([Permission::DogsControl])));
    let kept = Grants::new([Permission::LogsRead]);
    h.ctx.known_dogs.insert("bark", Some(kept.clone()));

    as_dog(&h, "otel", enabling("bark", None)).await;
    as_dog(&h, "otel", enabling("fresh", None)).await;

    assert_eq!(h.ctx.known_dogs.recorded("bark"), Some(Some(kept)));
    assert_eq!(
        h.ctx.known_dogs.recorded("fresh"),
        Some(Some(Grants::default()))
    );
}

/// The control socket is the operator, and its `EnableDog` is how grants are
/// recorded at all.
#[tokio::test(start_paused = true)]
async fn the_control_socket_still_sets_the_grants_it_carries() {
    let h = harness(vec![ProcScript::never_exits()]);
    let grants = Grants::new([Permission::FlockRead]);

    let reply =
        reply_of(dispatch(envelope(1, enabling("otel", Some(grants.clone()))), &h.ctx).await);

    assert!(reply.result.is_ok(), "{reply:?}");
    assert_eq!(h.ctx.known_dogs.recorded("otel"), Some(Some(grants)));
}

/// A spawn that fails still registers the dog, as `Errored`, and a restart
/// brings it up: its link must still find the grants.
#[tokio::test(start_paused = true)]
async fn a_dog_whose_spawn_failed_keeps_the_grants_it_was_enabled_with() {
    let h = harness(vec![]);
    let grants = Grants::new([Permission::FlockRead]);

    let reply =
        reply_of(dispatch(envelope(1, enabling("otel", Some(grants.clone()))), &h.ctx).await);

    assert_eq!(
        reply.result.as_ref().err().map(|e| e.code),
        Some(RpcErrorCode::SpawnFailed),
        "{reply:?}"
    );
    assert_eq!(h.ctx.known_dogs.recorded("otel"), Some(Some(grants)));
}
