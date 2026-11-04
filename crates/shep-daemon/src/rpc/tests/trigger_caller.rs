//! Who a trigger names as its caller on the action the app receives.

use super::*;
use shep_core::permissions::{Grants, Permission};
use shep_core::protocol::ShepherdMessage;

/// Starts `web` with a channel, triggers `action` on `link`, and returns the
/// action message that reached the app.
async fn action_seen_on(link: Option<Link<'_>>, action: &str) -> ShepherdMessage {
    let h = harness(vec![ProcScript::never_exits()]);
    let mut web = AppConfig::minimal("web", "./srv");
    web.channel = true;
    reply_of(dispatch(envelope(1, Request::Start { apps: vec![web] }), &h.ctx).await);
    let mut io = h.runner.io_handles(0);

    let trigger = envelope(
        2,
        Request::Trigger {
            selector: SelectorSpec::Name("web".to_string()),
            action: action.to_string(),
            params: None,
        },
    );
    // The app never answers, so the dispatch ends on the action timeout.
    let (_, seen) = tokio::join!(
        super::super::dispatch(trigger, ConnId::next(), link, &h.ctx),
        tokio::time::timeout(Duration::from_secs(1), io.to_child_rx.recv()),
    );
    seen.expect("the action reached the app in time")
        .expect("the app's channel stayed open")
}

#[tokio::test(start_paused = true)]
async fn a_trigger_on_a_dogs_own_connection_names_the_dog_as_caller() {
    let seen = action_seen_on(Some(Link::Dog("janitor")), "gc").await;
    assert_eq!(
        seen,
        ShepherdMessage::Action {
            name: "gc".to_string(),
            params: None,
            caller: Some("janitor".to_string()),
            id: 0,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_trigger_from_the_control_socket_names_no_caller() {
    let seen = action_seen_on(None, "gc").await;
    assert_eq!(
        seen,
        ShepherdMessage::Action {
            name: "gc".to_string(),
            params: None,
            caller: None,
            id: 0,
        }
    );
}

/// The operator's own link sends a `shep.` action: that is how `shep set` asks
/// a dog to validate a settings table, and how lookout will ask for a pane.
#[tokio::test(start_paused = true)]
async fn the_operator_link_still_sends_a_reserved_action() {
    let seen = action_seen_on(None, shep_core::dogs::validate::VALIDATE_ACTION).await;
    assert_eq!(
        seen,
        ShepherdMessage::Action {
            name: "shep.validate".to_string(),
            params: None,
            caller: None,
            id: 0,
        }
    );
}

fn adopted(name: &str, grants: Grants, channel: bool) -> Request {
    Request::EnableDog {
        name: name.to_string(),
        env: BTreeMap::default(),
        source: DogSource::Adopted {
            grants: Some(grants),
            path: format!("/usr/local/bin/shep-{name}"),
            channel,
            events: Vec::new(),
        },
    }
}

/// Adopts `janitor` with a channel and `scheduler` holding `granted`, has
/// `scheduler` trigger `action` at `janitor` on its own connection, and returns
/// the reply with the action message that reached `janitor`, if any.
async fn dog_to_dog(granted: Grants, action: &str) -> (Reply, Option<ShepherdMessage>) {
    let h = harness(vec![ProcScript::never_exits()]);
    reply_of(
        dispatch(
            envelope(1, adopted("janitor", Grants::default(), true)),
            &h.ctx,
        )
        .await,
    );
    h.ctx.known_dogs.insert("scheduler", Some(granted));
    let mut io = h.runner.io_handles(0);

    let trigger = envelope(
        2,
        Request::Trigger {
            selector: SelectorSpec::Name("janitor".to_string()),
            action: action.to_string(),
            params: None,
        },
    );
    let (outcome, seen) = tokio::join!(
        super::super::dispatch(
            trigger,
            ConnId::next(),
            Some(Link::Dog("scheduler")),
            &h.ctx
        ),
        tokio::time::timeout(Duration::from_secs(1), io.to_child_rx.recv()),
    );
    (reply_of(outcome), seen.ok().flatten())
}

#[tokio::test(start_paused = true)]
async fn a_dog_granted_the_trigger_reaches_another_dog_and_is_named_as_caller() {
    let granted = Grants::new([Permission::DogsTrigger("janitor".to_string())]);

    let (_, seen) = dog_to_dog(granted, "gc").await;

    assert_eq!(
        seen,
        Some(ShepherdMessage::Action {
            name: "gc".to_string(),
            params: None,
            caller: Some("scheduler".to_string()),
            id: 0,
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_dog_without_the_grant_is_refused_and_the_callee_hears_nothing() {
    let granted = Grants::new([Permission::FlockRead]);

    let (reply, seen) = dog_to_dog(granted, "gc").await;

    assert_eq!(seen, None);
    let Err(err) = reply.result else {
        panic!("a dog without `dogs.trigger:janitor` must be refused")
    };
    assert_eq!(err.code, RpcErrorCode::Forbidden);
}

/// A grant is not the wall here: a granted dog is refused too, because the
/// action name is shep's to send.
#[tokio::test(start_paused = true)]
async fn a_dog_cannot_trigger_a_reserved_action() {
    let granted = Grants::new([Permission::DogsTrigger("janitor".to_string())]);

    let (reply, seen) = dog_to_dog(granted, shep_core::dogs::pane::PANE_ACTION).await;

    assert_eq!(seen, None, "janitor must hear nothing");
    let Err(err) = reply.result else {
        panic!("a dog must not send the operator's `shep.pane`")
    };
    assert_eq!(err.code, RpcErrorCode::Forbidden);
    assert!(err.message.contains("shep.pane"), "{err:?}");
    assert!(err.message.contains("scheduler"), "{err:?}");
}
