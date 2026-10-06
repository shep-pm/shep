//! A question a sheep has open, and how one that is no longer open ended.

use serde::{Deserialize, Serialize};

use crate::protocol::{QuestionId, QuestionText, Takes};

/// One question a sheep has put to the operator and nobody has settled yet.
// wire format: changing this is a breaking change
// A wire struct that will grow: a new `takes` kind can bring a field.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenQuestion {
    /// The id the sheep chose. Unique among that sheep's open questions.
    pub question: QuestionId,
    /// What the sheep asked.
    pub text: QuestionText,
    /// The kind of answer the sheep accepts.
    pub takes: Takes,
    /// When the shepherd received the question, in Unix milliseconds.
    pub asked_at_ms: u64,
}

impl OpenQuestion {
    /// A question asked at `asked_at_ms` Unix milliseconds.
    #[must_use]
    pub fn new(question: QuestionId, text: QuestionText, takes: Takes, asked_at_ms: u64) -> Self {
        Self {
            question,
            text,
            takes,
            asked_at_ms,
        }
    }
}

/// How an open question stopped being open.
// wire format: changing this is a breaking change
// A wire enum: a new way to close a question must not break a match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Settled {
    /// An operator's answer was delivered to the sheep.
    Answered {
        /// The channel the answer came through, when the answerer named one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
        /// Who answered, when the answerer said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        who: Option<String>,
    },
    /// The sheep took the question back before anyone answered.
    Withdrawn,
    /// The sheep exited or restarted with the question still open.
    Gone,
    /// A settlement kind this build has not been taught.
    ///
    /// Only ever produced by decoding: an unknown `kind` falls through to
    /// this variant via `#[serde(other)]` instead of failing the whole
    /// frame. Nothing constructs one to send. That is a call-site invariant
    /// rather than a type-level one: `#[serde(other)]` governs decoding
    /// only, so serializing this would emit `{"kind":"unrecognized"}`.
    #[serde(other)]
    Unrecognized,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_wire_shapes_are_pinned() {
        let answered = Settled::Answered {
            via: Some("discord".into()),
            who: None,
        };
        assert_eq!(
            serde_json::to_string(&answered).unwrap(),
            r#"{"kind":"answered","via":"discord"}"#
        );
        assert_eq!(
            serde_json::to_string(&Settled::Withdrawn).unwrap(),
            r#"{"kind":"withdrawn"}"#
        );
        assert_eq!(
            serde_json::to_string(&Settled::Gone).unwrap(),
            r#"{"kind":"gone"}"#
        );
    }

    #[test]
    fn an_unknown_settlement_decodes_as_unrecognized() {
        assert_eq!(
            serde_json::from_str::<Settled>(r#"{"kind":"expired","after_ms":5}"#).unwrap(),
            Settled::Unrecognized
        );
    }

    #[test]
    fn an_answered_settlement_without_via_or_who_decodes() {
        assert_eq!(
            serde_json::from_str::<Settled>(r#"{"kind":"answered"}"#).unwrap(),
            Settled::Answered {
                via: None,
                who: None
            }
        );
    }
}
