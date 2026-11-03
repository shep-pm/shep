//! Who a trigger names as its caller on the action the app receives.

use super::*;
use shep_core::protocol::ShepherdMessage;

/// Starts `web` with a channel, triggers `gc` on `link`, and returns the
/// action message that reached the app.
async fn action_seen_on(link: Option<Link<'_>>) -> ShepherdMessage {
    let h = harness(vec![ProcScript::never_exits()]);
    let mut web = AppConfig::minimal("web", "./srv");
    web.channel = true;
    reply_of(dispatch(envelope(1, Request::Start { apps: vec![web] }), &h.ctx).await);
    let mut io = h.runner.io_handles(0);

    let trigger = envelope(
        2,
        Request::Trigger {
            selector: SelectorSpec::Name("web".to_string()),
            action: "gc".to_string(),
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
    let seen = action_seen_on(Some(Link::Dog("janitor"))).await;
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
    let seen = action_seen_on(None).await;
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
