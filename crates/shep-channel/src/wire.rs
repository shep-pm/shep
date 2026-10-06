//! The shepherd channel: newline-JSON wire between the shepherd and each
//! spawned child. Unix carries it on fd 3, Windows on a named pipe.
//! [`ChildMessage`] flows child to shepherd; [`ShepherdMessage`] flows
//! shepherd to child.
//!
//! Both enums are exhaustive on purpose. The channel has no handshake, so
//! a new variant has to be announced out of band. An exhaustive match
//! forces every call site to react to it.
//!
//! Pins the wire shapes only. See `docs/shepherd-channel.md` for reply and
//! correlation semantics.

use serde::{Deserialize, Serialize};

use crate::question::{Answer, QuestionId, QuestionText, Takes};

/// The value the shepherd exports as `SHEP_CHANNEL_VERSION` to every child
/// it opens a channel for.
///
/// Not a negotiation: a way for an app to notice a wire it has never
/// seen. `docs/shepherd-channel.md` defines what `"1"` means.
pub const CHANNEL_VERSION: &str = "1";

/// Child -> daemon shepherd-channel message (spec §7, kebab-case kinds)
// wire format: changing these strings is a breaking change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ChildMessage {
    /// `{"kind":"ready"}`: readiness signal (`wait_ready` gate)
    Ready,
    /// Custom metric sample
    Metric {
        /// Metric name
        name: String,
        /// Metric value
        value: f64,
    },
    /// Reply to a daemon-initiated action
    ActionReply {
        /// The action name this replies to
        action: String,
        /// Free-form reply body
        body: String,
        /// The `id` of the [`ShepherdMessage::Action`] this answers, echoed
        /// back verbatim. `None` when the app did not echo it. Then the
        /// daemon falls back to matching by name and order.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        id: Option<u64>,
    },
    /// Names one of this sheep's lambs for `shep describe` and lookout.
    ///
    /// Shown only while `pid` is in the sheep's own process tree. An empty
    /// `label` clears the one `pid` had.
    LambLabel {
        /// The lamb's pid, as the app's own spawn call reported it
        pid: u32,
        /// What `describe` shows beside the lamb's executable name
        label: LambLabel,
    },
    /// Puts a question to the operator.
    Ask {
        /// The name the answer will carry back
        question: QuestionId,
        /// What the operator is asked
        text: QuestionText,
        /// The kind of answer the question takes
        takes: Takes,
    },
    /// Takes back a question the app asked and no longer needs answered.
    Withdraw {
        /// The question to take back
        question: QuestionId,
    },
}

/// A sheep's own name for one of its lambs
///
/// Holds at most [`LambLabel::MAX_CHARS`] characters and no control
/// character, whether built with [`LambLabel::new`] or read off the wire.
/// Empty is a real value: it clears a label rather than setting one.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LambLabel(String);

impl LambLabel {
    /// The most characters a label may hold, counted as Unicode scalar
    /// values.
    // Room for `worker 12 of 16` or a queue name, and still one table column.
    pub const MAX_CHARS: usize = 64;

    /// A label, checked against the channel's grammar.
    ///
    /// # Errors
    ///
    /// - [`LambLabelError::TooLong`] when `label` has more than
    ///   [`Self::MAX_CHARS`] characters.
    /// - [`LambLabelError::ControlCharacter`] when `label` holds one, a
    ///   newline included.
    pub fn new(label: impl Into<String>) -> Result<Self, LambLabelError> {
        let label = label.into();
        let chars = label.chars().count();
        if chars > Self::MAX_CHARS {
            return Err(LambLabelError::TooLong { chars });
        }
        if label.chars().any(char::is_control) {
            return Err(LambLabelError::ControlCharacter);
        }
        Ok(Self(label))
    }

    /// The label's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this clears a label rather than setting one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<String> for LambLabel {
    type Error = LambLabelError;

    fn try_from(label: String) -> Result<Self, Self::Error> {
        Self::new(label)
    }
}

impl From<LambLabel> for String {
    fn from(label: LambLabel) -> Self {
        label.0
    }
}

/// Why a string is not a [`LambLabel`].
// Library crate: a new rule in the label grammar is a new variant.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LambLabelError {
    /// The label has more than [`LambLabel::MAX_CHARS`] characters.
    TooLong {
        /// How many characters it has.
        chars: usize,
    },
    /// The label holds a control character, which could drive the
    /// terminal it is shown on.
    ControlCharacter,
}

impl core::fmt::Display for LambLabelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLong { chars } => write!(
                f,
                "a lamb label holds at most {} characters, and this one has {chars}",
                LambLabel::MAX_CHARS
            ),
            Self::ControlCharacter => f.write_str("a lamb label cannot hold a control character"),
        }
    }
}

impl core::error::Error for LambLabelError {}

/// Daemon -> child message
// wire format: changing these strings is a breaking change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ShepherdMessage {
    /// Graceful-stop request (`shutdown_with_message`)
    Shutdown,
    /// Custom action dispatch
    Action {
        /// The action name
        name: String,
        /// Argument text for the action, passed through to the child
        /// verbatim; `None` when triggered without any. Omitted from the
        /// wire when `None`, so a message with no arguments round-trips
        /// byte-identical.
        ///
        /// One opaque string the daemon never reads, so an app parses it
        /// in its own grammar.
        // `skip_serializing_if` is load-bearing: without it, an empty
        // message serializes `"params":null` instead of omitting the key.
        // `default` guards a future type change on a channel with no
        // version to announce one.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        params: Option<String>,
        /// This dispatch's correlation id, unique for the life of the
        /// daemon. Echo it back as `id` on your
        /// [`ChildMessage::ActionReply`]. The daemon then matches your
        /// answer to this request, not to its name.
        ///
        /// Always present, unlike `params`. Treat `u64` and increasing as
        /// implementation details, not a promise.
        id: u64,
    },
    /// The operator's answer to one question.
    Answer(Answer),
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures pinned from spec §7. Round-tripped both ways so a silent
    // drift fails loudly.

    #[test]
    fn ready_wire_fixture_round_trips() {
        let fixture = r#"{"kind":"ready"}"#;
        assert_eq!(
            serde_json::from_str::<ChildMessage>(fixture).unwrap(),
            ChildMessage::Ready
        );
        assert_eq!(
            serde_json::to_string(&ChildMessage::Ready).unwrap(),
            fixture
        );
    }

    #[test]
    fn metric_wire_fixture_round_trips() {
        let fixture = r#"{"kind":"metric","name":"rps","value":42.0}"#;
        let msg = ChildMessage::Metric {
            name: "rps".to_string(),
            value: 42.0,
        };
        assert_eq!(serde_json::from_str::<ChildMessage>(fixture).unwrap(), msg);
        assert_eq!(serde_json::to_string(&msg).unwrap(), fixture);
    }

    #[test]
    fn an_action_reply_without_an_id_round_trips() {
        let fixture = r#"{"kind":"action-reply","action":"gc","body":"ok"}"#;
        let msg = ChildMessage::ActionReply {
            action: "gc".to_string(),
            body: "ok".to_string(),
            id: None,
        };
        assert_eq!(serde_json::from_str::<ChildMessage>(fixture).unwrap(), msg);
        assert_eq!(serde_json::to_string(&msg).unwrap(), fixture);
    }

    #[test]
    fn an_action_reply_with_an_echoed_id_round_trips() {
        let fixture = r#"{"kind":"action-reply","action":"gc","body":"ok","id":7}"#;
        let msg = ChildMessage::ActionReply {
            action: "gc".to_string(),
            body: "ok".to_string(),
            id: Some(7),
        };
        assert_eq!(serde_json::from_str::<ChildMessage>(fixture).unwrap(), msg);
        assert_eq!(serde_json::to_string(&msg).unwrap(), fixture);
    }

    #[test]
    fn shutdown_wire_fixture_round_trips() {
        let fixture = r#"{"kind":"shutdown"}"#;
        assert_eq!(
            serde_json::from_str::<ShepherdMessage>(fixture).unwrap(),
            ShepherdMessage::Shutdown
        );
        assert_eq!(
            serde_json::to_string(&ShepherdMessage::Shutdown).unwrap(),
            fixture
        );
    }

    /// Checks both directions: serialize and deserialize.
    #[test]
    fn an_action_carries_its_id_with_or_without_params() {
        let bare = r#"{"kind":"action","name":"gc","id":7}"#;
        let bare_msg = ShepherdMessage::Action {
            name: "gc".to_string(),
            params: None,
            id: 7,
        };
        assert_eq!(serde_json::to_string(&bare_msg).unwrap(), bare);
        assert_eq!(
            serde_json::from_str::<ShepherdMessage>(bare).unwrap(),
            bare_msg
        );

        let with_params = r#"{"kind":"action","name":"set-log-level","params":"debug","id":8}"#;
        let with_params_msg = ShepherdMessage::Action {
            name: "set-log-level".to_string(),
            params: Some("debug".to_string()),
            id: 8,
        };
        assert_eq!(
            serde_json::to_string(&with_params_msg).unwrap(),
            with_params
        );
        assert_eq!(
            serde_json::from_str::<ShepherdMessage>(with_params).unwrap(),
            with_params_msg
        );
    }

    #[test]
    fn a_lamb_label_round_trips() {
        let fixture = r#"{"kind":"lamb-label","pid":4312,"label":"worker 1"}"#;
        let msg = ChildMessage::LambLabel {
            pid: 4312,
            label: LambLabel::new("worker 1").unwrap(),
        };
        assert_eq!(serde_json::from_str::<ChildMessage>(fixture).unwrap(), msg);
        assert_eq!(serde_json::to_string(&msg).unwrap(), fixture);
    }

    /// The clearing form: an absent key would be a malformed frame instead.
    #[test]
    fn an_empty_lamb_label_is_on_the_wire_and_clears() {
        let fixture = r#"{"kind":"lamb-label","pid":4312,"label":""}"#;
        let ChildMessage::LambLabel { label, .. } =
            serde_json::from_str::<ChildMessage>(fixture).unwrap()
        else {
            panic!("{fixture} decoded as another kind");
        };
        assert!(label.is_empty());
        assert!(
            serde_json::from_str::<ChildMessage>(r#"{"kind":"lamb-label","pid":4312}"#).is_err()
        );
    }

    #[test]
    fn a_label_is_counted_in_characters_up_to_the_limit() {
        let at_limit = "é".repeat(LambLabel::MAX_CHARS);
        assert_eq!(LambLabel::new(at_limit.clone()).unwrap().as_str(), at_limit);
        assert_eq!(
            LambLabel::new(format!("{at_limit}e")),
            Err(LambLabelError::TooLong {
                chars: LambLabel::MAX_CHARS + 1
            })
        );
    }

    #[test]
    fn a_label_with_a_control_character_is_refused_on_the_wire_too() {
        for label in ["a\nb", "\u{1b}[2J", "tab\there", "\u{9b}31m"] {
            assert_eq!(
                LambLabel::new(label),
                Err(LambLabelError::ControlCharacter),
                "{label:?}"
            );
            let frame = serde_json::json!({"kind": "lamb-label", "pid": 1, "label": label});
            assert!(
                serde_json::from_value::<ChildMessage>(frame).is_err(),
                "{label:?} decoded"
            );
        }
    }

    #[test]
    fn ask_withdraw_and_answer_wire_fixtures_round_trip() {
        let ask = r#"{"kind":"ask","question":"koji-3","text":"Merge #12?","takes":"yes-no"}"#;
        let withdraw = r#"{"kind":"withdraw","question":"koji-3"}"#;
        let answer = r#"{"kind":"answer","question":"koji-3","answer":"no","note":"rebase first","via":"discord","who":"<@81234>"}"#;
        let bare = r#"{"kind":"answer","question":"koji-3","answer":"yes"}"#;
        for line in [ask, withdraw] {
            let parsed: ChildMessage = serde_json::from_str(line).unwrap();
            assert_eq!(serde_json::to_string(&parsed).unwrap(), line);
        }
        for line in [answer, bare] {
            let parsed: ShepherdMessage = serde_json::from_str(line).unwrap();
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                line,
                "absent options stay absent"
            );
        }
    }

    #[test]
    fn a_label_error_says_the_limit_and_the_length() {
        assert_eq!(
            LambLabelError::TooLong { chars: 70 }.to_string(),
            "a lamb label holds at most 64 characters, and this one has 70"
        );
    }
}
