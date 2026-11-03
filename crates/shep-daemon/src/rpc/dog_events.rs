//! `Request::PublishEvent`: a dog puts one of the event kinds its manifest
//! declared on the bus, under its own name.

use shep_core::protocol::{BusEvent, Response, RpcError, RpcErrorCode};

use super::context::RpcContext;
use super::gate::Link;

/// Publishes `BusEvent::Dog { dog, kind, payload }` for the dog on `link`.
///
/// The name comes from the connection, never from the request, so a dog can
/// publish only under its own. The shepherd checks the kind was declared and
/// nothing about the payload.
///
/// # Errors
/// [`RpcErrorCode::Forbidden`] when `link` is the control socket or a launch
/// link, or when the dog did not declare `kind`.
pub(super) fn publish_dog_event(
    link: Option<Link<'_>>,
    kind: &str,
    payload: serde_json::Value,
    ctx: &RpcContext,
) -> Result<Response, RpcError> {
    let Some(Link::Dog(dog)) = link else {
        return Err(forbidden(
            "only a dog's own connection publishes events: this connection is not one",
        ));
    };
    if !ctx.known_dogs.declares(dog, kind) {
        return Err(forbidden(&format!(
            "dog `{dog}` did not declare the event kind `{kind}` in its manifest, so it may not publish it"
        )));
    }
    // No subscriber is not a failure: the event was published.
    let _ = ctx.events.send(
        BusEvent::Dog {
            dog: dog.to_owned(),
            kind: kind.to_owned(),
            payload,
        }
        .into(),
    );
    Ok(Response::EventPublished)
}

fn forbidden(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Forbidden,
        message: message.to_owned(),
        daemon_version: None,
    }
}
