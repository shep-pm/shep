//! Answering `shep.validate`: the one call a dog makes to turn a proposed
//! settings table into the reply body it writes back on its channel.
//!
//! A dog that declared [`validate`](shep_core::dogs::manifest::Manifest::validate)
//! in its manifest gets this trigger before shep writes its settings, on the
//! same channel it answers `shep trigger` and its stop message on. The action
//! name is [`VALIDATE_ACTION`](shep_core::dogs::validate::VALIDATE_ACTION), and
//! the reply body is whatever this returns.

use shep_core::dogs::validate::{Proposed, Verdict};

/// The reply body a dog writes back for a `shep.validate` action.
///
/// `params` is the `params` field of the `action` message, verbatim. `check`
/// answers for the table: [`Verdict::Accept`], or
/// [`Verdict::refuse`](Verdict::refuse) with the sentence the operator reads.
/// A `params` shep never sent is refused without calling `check`.
///
/// # Examples
///
/// ```
/// use shep_client::dogs::{Proposed, Verdict, validate_answer};
///
/// fn verdict(proposed: &Proposed) -> Verdict {
///     match proposed.table().get("checkout").and_then(|value| value.as_str()) {
///         Some("/srv/api") => Verdict::Accept,
///         _ => Verdict::refuse("this checkout belongs to another project"),
///     }
/// }
///
/// // The table the dog clears, and the one it turns down.
/// assert_eq!(
///     validate_answer(Some(r#"{"table":{"checkout":"/srv/api"},"sheep":"api"}"#), verdict),
///     r#"{"accept":true}"#,
/// );
/// assert_eq!(
///     validate_answer(Some(r#"{"table":{"checkout":"/other"}}"#), verdict),
///     r#"{"accept":false,"reason":"this checkout belongs to another project"}"#,
/// );
/// ```
#[must_use = "the body is what the dog writes back on its channel"]
pub fn validate_answer(params: Option<&str>, check: impl FnOnce(&Proposed) -> Verdict) -> String {
    let Some(params) = params else {
        return Verdict::refuse("shep sent no table to validate").body();
    };
    let Some(proposed) = Proposed::parse(params) else {
        return Verdict::refuse("shep sent something that is not a proposed table").body();
    };
    check(&proposed).body()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// The dog's own answer, in the words the dog's author writes it in.
    #[test]
    fn a_check_that_accepts_answers_accept() {
        let body = validate_answer(Some(r#"{"table":{"keep":2},"sheep":"api"}"#), |proposed| {
            assert_eq!(proposed.sheep(), Some("api"));
            assert_eq!(proposed.table().get("keep"), Some(&json!(2)));
            Verdict::Accept
        });

        assert_eq!(body, r#"{"accept":true}"#);
    }

    #[test]
    fn a_check_that_refuses_carries_its_reason_to_the_operator() {
        let body = validate_answer(Some(r#"{"table":{"checkout":"/other"}}"#), |_| {
            Verdict::refuse("this checkout belongs to another project")
        });

        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({
                "accept": false,
                "reason": "this checkout belongs to another project"
            })
        );
    }

    /// A dog that cannot read the table has not cleared the write, and says
    /// so rather than answering for a table it guessed at.
    #[test]
    fn a_params_shep_never_sent_is_refused_without_asking_the_dog() {
        for params in [None, Some(""), Some("not a table")] {
            let body = validate_answer(params, |_| {
                panic!("a dog is never asked about a table shep did not send")
            });

            assert_eq!(
                serde_json::from_str::<Value>(&body)
                    .expect("the refusal is a verdict")
                    .get("accept")
                    .and_then(Value::as_bool),
                Some(false),
                "{params:?}"
            );
        }
    }
}
