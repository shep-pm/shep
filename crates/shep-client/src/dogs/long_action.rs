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
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use shep_core::protocol::{ActionOutcome, BusEvent, Request, Response, RpcErrorCode, SelectorSpec};

use crate::reconnect::Covered;
use crate::{EventStream, Lagged, ReconnectingClient, RequestError, TRIGGER_DEADLINE};

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

    /// The id a trigger's reply body carries, `None` for a body that is not
    /// an object with a non-empty text `id`.
    ///
    /// A caller that gets `None` has a plain answer, not a long action.
    ///
    /// # Example
    ///
    /// ```
    /// use shep_client::dogs::ActionId;
    ///
    /// let (id, body) = ActionId::accept();
    /// assert_eq!(ActionId::from_reply(&body), Some(id));
    /// assert_eq!(ActionId::from_reply("swept 3 files"), None);
    /// ```
    #[must_use]
    pub fn from_reply(body: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(body).ok()?;
        let id = value.get("id")?.as_str().filter(|id| !id.is_empty())?;
        Some(Self(id.to_owned()))
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

impl ActionDone {
    /// The id and outcome a [`DONE_KIND`] event's payload carries, `None`
    /// for a payload that is not what the callee's `finish_action` writes.
    ///
    /// The outcome is the payload's `error` when it has one, else its
    /// `result`. A payload with an empty id, a non-text `error` or neither
    /// field is `None`, so a callee's slip is not read as success.
    ///
    /// # Example
    ///
    /// ```
    /// use serde_json::json;
    /// use shep_client::dogs::ActionDone;
    ///
    /// let payload = json!({ "id": "abc", "error": "disk full" });
    /// let (id, outcome) = ActionDone::read(&payload).unwrap();
    /// assert_eq!(id.as_str(), "abc");
    /// assert_eq!(outcome, Err("disk full".to_owned()));
    /// assert!(ActionDone::read(&json!({ "id": "abc" })).is_none());
    /// ```
    #[must_use]
    pub fn read(payload: &Value) -> Option<(ActionId, Result<Value, String>)> {
        let id = payload.get("id")?.as_str().filter(|id| !id.is_empty())?;
        let outcome = match payload.get("error") {
            Some(Value::String(error)) => Err(error.clone()),
            Some(_) => return None,
            None => Ok(payload.get("result")?.clone()),
        };
        Some((ActionId(id.to_owned()), outcome))
    }
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
    /// The callee published an event for this action that is neither a
    /// result nor an error.
    Malformed {
        /// The action the event names.
        id: ActionId,
        /// The event's payload as the callee sent it.
        payload: String,
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
            Self::Malformed { id, payload } => {
                write!(f, "action {id} ended with an unreadable event: {payload}")
            }
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

/// The result topics a dog's waits are listening on.
///
/// A connection holds one topic filter and a `Subscribe` replaces it, so
/// waits on different callees must send the union of their topics. A
/// `Subscribe` also costs the shepherd's old forwarder the event it holds in
/// flight, so one is sent only when the union grows.
#[derive(Debug, Default)]
pub(super) struct Waits {
    /// What the connection's filter holds. Held across a `Subscribe`, so the
    /// filters reach the shepherd in the order their unions were made.
    covered: tokio::sync::Mutex<Covered>,
    /// Waiting count per topic. Never held across an await.
    topics: Mutex<BTreeMap<String, usize>>,
}

/// One wait's claim on a topic, released when the wait ends.
struct Listening<'a> {
    waits: &'a Waits,
    topic: String,
}

impl Waits {
    /// Starts hearing `topic` alongside every other topic being waited on.
    async fn listen<'a>(
        &'a self,
        client: &ReconnectingClient,
        topic: String,
    ) -> Result<(EventStream, Listening<'a>), RequestError> {
        let mut covered = self.covered.lock().await;
        let listening = self.claim(topic);
        let events = client
            .subscribe_covered(self.topics(), &mut covered)
            .await?;
        Ok((events, listening))
    }

    fn claim(&self, topic: String) -> Listening<'_> {
        *self.lock().entry(topic.clone()).or_insert(0) += 1;
        Listening { waits: self, topic }
    }

    fn topics(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, usize>> {
        self.topics.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Listening<'_> {
    fn drop(&mut self) {
        let mut topics = self.waits.lock();
        if let Some(count) = topics.get_mut(&self.topic) {
            *count -= 1;
            if *count == 0 {
                topics.remove(&self.topic);
            }
        }
    }
}

/// Subscribes to `callee`'s results, triggers `action`, and waits for the
/// result carrying the id it answered with.
///
/// Subscribing first is what stops a result published before the trigger's
/// reply arrives from passing unheard.
pub(super) async fn trigger_and_wait(
    waits: &Waits,
    client: &ReconnectingClient,
    callee: &str,
    action: &str,
    params: Option<&str>,
    timeout: Duration,
) -> Result<ActionDone, LongActionError> {
    let topic = format!("dog.{callee}.{DONE_KIND}");
    let (mut events, _listening) = waits
        .listen(client, topic)
        .await
        .map_err(|error| refused(error, format!("events:dog.{callee}")))?;

    let id = trigger(client, callee, action, params).await?;
    match tokio::time::timeout(timeout, result_for(&mut events, callee, &id)).await {
        Ok(Ok(result)) => Ok(ActionDone { id, result }),
        Ok(Err(error)) => Err(error),
        Err(_elapsed) => Err(LongActionError::TimedOut { id, after: timeout }),
    }
}

/// Sends the trigger, waiting as long as any trigger does for a callee that
/// is slow to answer, and reads the id out of the callee's reply.
async fn trigger(
    client: &ReconnectingClient,
    callee: &str,
    action: &str,
    params: Option<&str>,
) -> Result<ActionId, LongActionError> {
    let request = Request::Trigger {
        selector: SelectorSpec::Name(callee.to_owned()),
        action: action.to_owned(),
        params: params.map(str::to_owned),
    };
    let reply = client
        .request_with_deadline(request, Some(TRIGGER_DEADLINE))
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
                return outcome_of(id, &payload);
            }
            Some(Ok(_other)) => {}
        }
    }
}

/// What a result event for `id` says: its `error` when it has one, else its
/// `result`. An `error` that is not text, or an event with neither, is
/// [`LongActionError::Malformed`], so a callee's slip is not read as success.
fn outcome_of(id: &ActionId, payload: &Value) -> Result<Value, LongActionError> {
    let malformed = || LongActionError::Malformed {
        id: id.clone(),
        payload: payload.to_string(),
    };
    match payload.get("error") {
        Some(Value::String(error)) => Err(LongActionError::Failed {
            id: id.clone(),
            error: error.clone(),
        }),
        Some(_) => Err(malformed()),
        None => payload.get("result").cloned().ok_or_else(malformed),
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
        for body in ["swept", "{}", r#"{"id":7}"#, r#"{"id":""}"#, ""] {
            assert_eq!(ActionId::from_reply(body), None, "{body:?}");
        }
    }

    #[test]
    fn from_reply_refuses_an_empty_id_a_non_object_and_a_body_with_no_id() {
        let (id, body) = ActionId::accept();
        assert_eq!(ActionId::from_reply(&body), Some(id));
        for body in [r#"{"id":""}"#, "[]", r#""abc""#, r#"{"name":"abc"}"#] {
            assert_eq!(ActionId::from_reply(body), None, "{body:?}");
        }
    }

    #[test]
    fn done_read_reads_both_payload_shapes_done_payload_writes() {
        let id = ActionId("abc".to_owned());
        for outcome in [
            Ok(json!({ "built": 3 })),
            Ok(Value::Null),
            Err("disk full".to_owned()),
        ] {
            let payload = done_payload(&id, outcome.clone());
            assert_eq!(ActionDone::read(&payload), Some((id.clone(), outcome)));
        }
    }

    #[test]
    fn done_read_refuses_a_payload_with_neither_result_nor_error() {
        for payload in [
            json!({ "id": "abc" }),
            json!({ "id": "abc", "error": 7 }),
            json!({ "result": 1 }),
            json!({ "id": "", "result": 1 }),
            json!("abc"),
        ] {
            assert_eq!(ActionDone::read(&payload), None, "{payload}");
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

    #[test]
    fn an_event_that_is_neither_a_result_nor_an_error_is_not_a_success() {
        let id = ActionId("abc".to_owned());
        assert_eq!(
            outcome_of(&id, &json!({ "id": "abc", "result": null })).unwrap(),
            Value::Null
        );
        for payload in [
            json!({ "id": "abc" }),
            json!({ "id": "abc", "error": 7 }),
            json!({ "id": "abc", "error": null, "result": 1 }),
        ] {
            let error = outcome_of(&id, &payload).unwrap_err();
            assert!(
                matches!(error, LongActionError::Malformed { .. }),
                "{payload}"
            );
        }
        let error = outcome_of(&id, &json!({ "id": "abc", "error": "disk full" })).unwrap_err();
        assert!(matches!(error, LongActionError::Failed { error, .. } if error == "disk full"));
    }

    #[test]
    fn waits_on_two_topics_listen_to_both_until_each_ends() {
        let waits = Waits::default();

        let first = waits.claim("dog.a.action.done".to_owned());
        let second = waits.claim("dog.b.action.done".to_owned());
        let again = waits.claim("dog.a.action.done".to_owned());
        assert_eq!(waits.topics(), ["dog.a.action.done", "dog.b.action.done"]);

        drop(first);
        assert_eq!(waits.topics(), ["dog.a.action.done", "dog.b.action.done"]);
        drop(again);
        drop(second);
        assert!(waits.topics().is_empty());
    }
}
