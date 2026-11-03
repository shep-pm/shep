//! A dog publishes the kinds its manifest declared under `dog.<name>.<kind>`,
//! and hears another dog's only with an `events:dog.<name>` grant.

use super::*;
use crate::bus::TopicFilter;
use crate::rpc::ConnId;
use shep_core::permissions::{Grants, Permission};
use shep_core::protocol::BusEvent;
use std::time::Duration;

/// Enables `otel` declaring the `exported` kind, the way the CLI sends it.
async fn enable_otel(h: &Harness, events: Vec<String>) {
    let request = Request::EnableDog {
        name: "otel".to_string(),
        env: BTreeMap::default(),
        source: DogSource::Adopted {
            grants: Some(Grants::default()),
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: false,
            events,
        },
    };
    let reply = reply_of(dispatch(envelope(1, request), &h.ctx).await);
    assert!(reply.result.is_ok(), "{reply:?}");
}

fn publish(kind: &str, payload: serde_json::Value) -> Request {
    Request::PublishEvent {
        event: kind.to_string(),
        payload,
    }
}

/// `request` sent on `dog`'s own connection, or on the control socket for
/// `None`.
async fn sent_by(h: &Harness, dog: Option<&str>, request: Request) -> Outcome {
    super::super::dispatch(
        envelope(7, request),
        ConnId::next(),
        dog.map(Link::Dog),
        &h.ctx,
    )
    .await
}

/// The filter a `Subscribe` to `topic` from `dog` is granted, or the refusal.
async fn subscribe(h: &Harness, dog: &str, topic: &str) -> Result<TopicFilter, RpcError> {
    let request = Request::Subscribe {
        topics: vec![topic.to_string()],
    };
    match sent_by(h, Some(dog), request).await {
        Outcome::Subscribe { filter, .. } => Ok(filter),
        other => Err(reply_of(other).result.expect_err("a refusal")),
    }
}

#[tokio::test(start_paused = true)]
async fn a_declared_kind_reaches_a_subscriber_granted_that_dogs_events_unchanged() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_otel(&h, vec!["exported".to_string()]).await;
    h.ctx.known_dogs.insert(
        "watcher",
        Some(Grants::new([Permission::Events("dog.otel".to_string())])),
    );
    let filter = subscribe(&h, "watcher", "dog.otel.*")
        .await
        .expect("granted");
    let mut rx = h.ctx.events.subscribe();
    let payload = serde_json::json!({ "batch": 3, "nested": { "a": [1, null, "x"] } });

    let reply = reply_of(sent_by(&h, Some("otel"), publish("exported", payload.clone())).await);

    assert_eq!(reply.result, Ok(Response::EventPublished));
    let heard = rx.try_recv().expect("the event was published");
    assert!(filter.matches(&heard), "{heard:?}");
    assert_eq!(
        heard.to_event(),
        BusEvent::Dog {
            dog: "otel".to_string(),
            kind: "exported".to_string(),
            payload,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn an_undeclared_kind_is_refused_by_name_and_nothing_is_published() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_otel(&h, vec!["exported".to_string()]).await;
    let mut rx = h.ctx.events.subscribe();

    let reply = reply_of(sent_by(&h, Some("otel"), publish("dropped", serde_json::json!(1))).await);

    let err = reply.result.expect_err("an undeclared kind");
    assert_eq!(err.code, RpcErrorCode::Forbidden);
    assert!(err.message.contains("`dropped`"), "{}", err.message);
    assert!(
        rx.try_recv()
            .map_or(true, |e| !matches!(e.to_event(), BusEvent::Dog { .. })),
        "a refused kind reached the bus"
    );
}

#[tokio::test(start_paused = true)]
async fn a_dog_that_declared_no_events_publishes_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_otel(&h, Vec::new()).await;

    let reply =
        reply_of(sent_by(&h, Some("otel"), publish("exported", serde_json::json!(1))).await);

    assert!(reply.result.is_err(), "{reply:?}");
}

#[tokio::test(start_paused = true)]
async fn the_control_socket_cannot_publish() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_otel(&h, vec!["exported".to_string()]).await;
    let mut rx = h.ctx.events.subscribe();

    let reply = reply_of(sent_by(&h, None, publish("exported", serde_json::json!(1))).await);

    let err = reply.result.expect_err("the control socket is refused");
    assert_eq!(err.code, RpcErrorCode::Forbidden);
    assert!(
        rx.try_recv()
            .map_or(true, |e| !matches!(e.to_event(), BusEvent::Dog { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn a_launch_link_cannot_publish() {
    let h = harness(vec![ProcScript::never_exits()]);
    let link = Link::Launch {
        dog: "otel",
        sheep: "web",
    };
    let outcome = super::super::dispatch(
        envelope(7, publish("exported", serde_json::json!(1))),
        ConnId::next(),
        Some(link),
        &h.ctx,
    )
    .await;

    assert!(reply_of(outcome).result.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_dog_publishes_under_its_own_name_even_when_another_declared_the_kind() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_otel(&h, vec!["exported".to_string()]).await;
    h.ctx.known_dogs.insert("other", Some(Grants::default()));
    let mut rx = h.ctx.events.subscribe();

    let reply =
        reply_of(sent_by(&h, Some("other"), publish("exported", serde_json::json!(1))).await);

    assert!(reply.result.is_err(), "{reply:?}");
    assert!(rx.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn a_subscriber_without_the_grant_is_refused_and_its_own_config_still_reaches_it() {
    let h = harness(vec![ProcScript::never_exits()]);
    h.ctx
        .known_dogs
        .insert("watcher", Some(Grants::new([Permission::FlockRead])));

    let refused = subscribe(&h, "watcher", "dog.otel.*")
        .await
        .expect_err("no grant");
    assert_eq!(refused.code, RpcErrorCode::Forbidden);
    assert!(
        refused.message.contains("events:dog.otel."),
        "{}",
        refused.message
    );

    let own = subscribe(&h, "watcher", "config.dog.watcher")
        .await
        .expect("always");
    let changed = BusEvent::DogConfigChanged {
        dog: "watcher".to_string(),
    };
    assert!(own.matches(&changed));

    let other = subscribe(&h, "watcher", "config.dog.otel")
        .await
        .expect_err("not its own");
    assert!(
        other.message.contains("events:config.dog.otel"),
        "{}",
        other.message
    );
}

#[tokio::test(start_paused = true)]
async fn a_grant_for_one_dog_does_not_cover_a_dog_sharing_its_prefix() {
    let h = harness(vec![ProcScript::never_exits()]);
    h.ctx.known_dogs.insert(
        "watcher",
        Some(Grants::new([Permission::Events("dog.otel".to_string())])),
    );

    let refused = subscribe(&h, "watcher", "dog.otel2.*").await;

    assert!(refused.is_err(), "events:dog.otel covered dog.otel2");
}

/// The declared kinds are the recorded ones: an `EnableDog` sent on a dog's
/// own connection carries a list the shepherd replaces, as it does grants.
#[tokio::test(start_paused = true)]
async fn a_dog_cannot_declare_kinds_for_another_dog_by_enabling_it() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_otel(&h, Vec::new()).await;
    h.ctx.known_dogs.insert("control", Some(Grants::default()));
    let forged = Request::EnableDog {
        name: "otel".to_string(),
        env: BTreeMap::default(),
        source: DogSource::Adopted {
            grants: Some(Grants::default()),
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: false,
            events: vec!["exported".to_string()],
        },
    };
    let _ =
        tokio::time::timeout(Duration::from_secs(1), sent_by(&h, Some("control"), forged)).await;

    assert_eq!(h.ctx.known_dogs.events("otel"), Vec::<String>::new());
}
