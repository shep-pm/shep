//! Work longer than a trigger: answer at once with an id, report the
//! result later as one of the dog's own events.
//!
//! A trigger's reply is bounded by the callee's `action_timeout`, so the
//! callee answers with [`ActionId::accept`]'s body and publishes
//! [`DONE_KIND`] with [`DogRuntime::finish_action`] when the work ends.
//! The caller filters by id. The shepherd keeps no state for it.
//!
//! The kind is fixed so [`DogRuntime::trigger_and_wait`] works against any
//! dog. The callee declares it with [`Probe::event`](super::Probe::event).

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use shep_core::protocol::{ActionOutcome, BusEvent, Request, Response, RpcErrorCode, SelectorSpec};

use crate::{EventStream, Lagged, ReconnectingClient, RequestError};

#[cfg(doc)]
use super::DogRuntime;

/// The kind a callee publishes when a long action ends, heard as
/// `dog.<callee>.action.done`.
pub const DONE_KIND: &str = "action.done";

/// Which long action one trigger started.
///
/// Every holder of `events:dog.<callee>` sees it in the result event, so it
/// correlates and authenticates nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionId(String);

impl ActionId {
    /// A fresh id and the reply body that hands it to the caller.
    ///
    /// Return the body from the action's handler, then run the work
    /// elsewhere. A handler that ran the work inline would hold the reply
    /// back until `action_timeout`.
    #[must_use]
    pub fn accept() -> (Self, String) {
        let id = Self::mint();
        let body = json!({ "id": id.0 }).to_string();
        (id, body)
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Process start, pid and a counter: unique across this dog's
    /// concurrent actions and across its restarts.
    fn mint() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        static START: OnceLock<u128> = OnceLock::new();
        let start = START.get_or_init(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
        });
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("{start:x}-{:x}-{count}", std::process::id()))
    }

    /// The id a reply body carries, `None` for a body that is not one.
    fn from_reply(body: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(body).ok()?;
        value.get("id")?.as_str().map(|id| Self(id.to_owned()))
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A long action's result, as the callee published it.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionDone {
    /// The id the callee answered the trigger with.
    pub id: ActionId,
    /// What the work produced.
    pub result: Value,
}

/// Why [`DogRuntime::trigger_and_wait`] returned no result.
#[derive(Debug)]
#[non_exhaustive]
pub enum LongActionError {
    /// The shepherd refused a request for want of a grant. Names the grant
    /// to add to the caller's `granted` list.
    NotGranted {
        /// `events:dog.<callee>` for the subscription, or
        /// `dogs.trigger:<callee>` for the trigger.
        grant: String,
        /// The shepherd's refusal.
        message: String,
    },
    /// A request failed for any other reason.
    Request(RequestError),
    /// The trigger reached no reply: the callee is not running, has no
    /// channel, skipped the action or did not answer in time.
    NotDelivered(String),
    /// The callee answered the trigger, but not with an id from
    /// [`ActionId::accept`].
    NoId {
        /// The reply body as the callee sent it.
        body: String,
    },
    /// The callee published an error for this action.
    Failed {
        /// The action that failed.
        id: ActionId,
        /// What the callee said went wrong.
        error: String,
    },
    /// No result arrived within the caller's timeout. The work may still
    /// be running.
    TimedOut {
        /// The action still outstanding.
        id: ActionId,
        /// The timeout that ran out.
        after: Duration,
    },
    /// The subscription ended or fell behind before the result arrived, so
    /// the result may have been missed.
    EventsLost(String),
}

impl fmt::Display for LongActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotGranted { grant, message } => {
                write!(f, "the caller needs the grant `{grant}`: {message}")
            }
            Self::Request(error) => write!(f, "a request to the shepherd failed: {error}"),
            Self::NotDelivered(why) => write!(f, "the action was not delivered: {why}"),
            Self::NoId { body } => write!(f, "the callee did not answer with an action id: {body}"),
            Self::Failed { id, error } => write!(f, "action {id} failed: {error}"),
            Self::TimedOut { id, after } => {
                write!(f, "action {id} sent no result within {after:?}")
            }
            Self::EventsLost(why) => write!(f, "the result may have been missed: {why}"),
        }
    }
}

impl core::error::Error for LongActionError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            _ => None,
        }
    }
}

/// The payload [`DONE_KIND`] carries.
pub(super) fn done_payload(id: &ActionId, outcome: Result<Value, String>) -> Value {
    match outcome {
        Ok(result) => json!({ "id": id.0, "result": result }),
        Err(error) => json!({ "id": id.0, "error": error }),
    }
}

/// Subscribes to `callee`'s results, triggers `action`, and waits for the
/// result carrying the id it answered with.
///
/// Subscribing first is what stops a result published before the trigger's
/// reply arrives from passing unheard.
pub(super) async fn trigger_and_wait(
    client: &ReconnectingClient,
    callee: &str,
    action: &str,
    params: Option<&str>,
    timeout: Duration,
) -> Result<ActionDone, LongActionError> {
    let topic = format!("dog.{callee}.{DONE_KIND}");
    let mut events = client
        .subscribe(vec![topic])
        .await
        .map_err(|error| refused(error, format!("events:dog.{callee}")))?;

    let id = trigger(client, callee, action, params).await?;
    match tokio::time::timeout(timeout, result_for(&mut events, callee, &id)).await {
        Ok(Ok(result)) => Ok(ActionDone { id, result }),
        Ok(Err(error)) => Err(error),
        Err(_elapsed) => Err(LongActionError::TimedOut { id, after: timeout }),
    }
}

/// Sends the trigger and reads the id out of the callee's reply.
async fn trigger(
    client: &ReconnectingClient,
    callee: &str,
    action: &str,
    params: Option<&str>,
) -> Result<ActionId, LongActionError> {
    let reply = client
        .request(Request::Trigger {
            selector: SelectorSpec::Name(callee.to_owned()),
            action: action.to_owned(),
            params: params.map(str::to_owned),
        })
        .await
        .map_err(|error| refused(error, format!("dogs.trigger:{callee}")))?;
    let Response::Triggered(rows) = reply else {
        return Err(LongActionError::Request(RequestError::UnexpectedReply {
            asked: "Trigger",
            answered: reply.name(),
        }));
    };
    let Some(row) = rows.into_iter().find(|row| row.name == callee) else {
        return Err(LongActionError::NotDelivered(format!(
            "no dog named `{callee}`"
        )));
    };
    match row.outcome {
        ActionOutcome::Replied { body } => {
            ActionId::from_reply(&body).ok_or(LongActionError::NoId { body })
        }
        other => Err(LongActionError::NotDelivered(format!("{other:?}"))),
    }
}

/// Reads `events` until the result for `id` arrives.
async fn result_for(
    events: &mut EventStream,
    callee: &str,
    id: &ActionId,
) -> Result<Value, LongActionError> {
    loop {
        match events.next().await {
            None => return Err(LongActionError::EventsLost("the connection closed".into())),
            Some(Err(Lagged { count })) => {
                return Err(LongActionError::EventsLost(format!(
                    "{count} events dropped locally"
                )));
            }
            Some(Ok(BusEvent::Dropped { .. })) => {
                return Err(LongActionError::EventsLost(
                    "the shepherd dropped events".into(),
                ));
            }
            Some(Ok(BusEvent::Dog { dog, kind, payload }))
                if dog == callee && kind == DONE_KIND =>
            {
                if payload.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                    continue;
                }
                return match payload.get("error").and_then(Value::as_str) {
                    Some(error) => Err(LongActionError::Failed {
                        id: id.clone(),
                        error: error.to_owned(),
                    }),
                    None => Ok(payload.get("result").cloned().unwrap_or(Value::Null)),
                };
            }
            Some(Ok(_other)) => {}
        }
    }
}

/// A `Forbidden` refusal as the grant that would have let it through, any
/// other failure as itself.
fn refused(error: RequestError, grant: String) -> LongActionError {
    match error {
        RequestError::Rpc(rpc) if rpc.code == RpcErrorCode::Forbidden => {
            LongActionError::NotGranted {
                grant,
                message: rpc.message,
            }
        }
        other => LongActionError::Request(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_accepted_action_gets_its_own_id_and_the_body_carries_it() {
        let (first, body) = ActionId::accept();
        let (second, _) = ActionId::accept();

        assert_ne!(first, second);
        assert_eq!(ActionId::from_reply(&body), Some(first));
    }

    #[test]
    fn a_reply_that_is_not_an_id_is_not_taken_for_one() {
        for body in ["swept", "{}", r#"{"id":7}"#, ""] {
            assert_eq!(ActionId::from_reply(body), None, "{body:?}");
        }
    }

    #[test]
    fn the_done_payload_carries_the_id_and_either_a_result_or_an_error() {
        let id = ActionId("abc".to_owned());
        assert_eq!(
            done_payload(&id, Ok(json!({ "built": 3 }))),
            json!({ "id": "abc", "result": { "built": 3 } })
        );
        assert_eq!(
            done_payload(&id, Err("disk full".to_owned())),
            json!({ "id": "abc", "error": "disk full" })
        );
    }
}
