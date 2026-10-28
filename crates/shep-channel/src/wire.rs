//! The shepherd channel: newline-JSON wire between the shepherd and each
//! spawned child. Unix carries it on fd 3, Windows on a named pipe.
//! [`ChildMessage`] flows child to shepherd; [`ShepherdMessage`] flows
//! shepherd to child.
//!
//! Both enums are exhaustive on purpose. The channel has no handshake, so
//! a new variant has to be announced out of band. An exhaustive match
//! forces every call site to react to it.
//!
//! Pins the wire shapes only. `docs/shepherd-channel.md` has the semantics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The value the shepherd exports as `SHEP_CHANNEL_VERSION` to every child
/// it opens a channel for.
///
/// Not a negotiation: a way for an app to notice a wire it has never
/// seen. `docs/shepherd-channel.md` defines what `"1"` means.
pub const CHANNEL_VERSION: &str = "1";

/// What kind of number a [`ChildMessage::Metric`] sample is
///
/// Wire spellings are `"gauge"` and `"counter"`. Absent on the wire means
/// gauge. Exhaustive on purpose, like the two message enums: a new kind
/// is a wire change announced out of band, and an exhaustive match
/// forces every call site to react to it.
// wire format: changing these strings is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricType {
    /// A value that can go up and down
    Gauge,
    /// A value that only accumulates
    Counter,
}

/// Child -> daemon shepherd-channel message (spec §7, kebab-case kinds)
// wire format: changing these strings is a breaking change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ChildMessage {
    /// `{"kind":"ready"}`: readiness signal (`wait_ready` gate)
    Ready,
    /// Custom metric sample
    ///
    /// Names and labels carry no grammar check here: the metrics dog
    /// enforces that when it reads them. Do not add a half check to this
    /// crate.
    Metric {
        /// Metric name
        name: String,
        /// Metric value
        value: f64,
        /// What kind of number this is. Absent on the wire means gauge.
        ///
        /// Wire key `type`: a Rust field named `kind` would collide with
        /// the tag. An unknown string fails to decode, so the shepherd
        /// drops the frame as malformed rather than guessing a kind.
        #[serde(rename = "type", skip_serializing_if = "Option::is_none", default)]
        metric_type: Option<MetricType>,
        /// Labels for the sample. An absent key and an empty map are the
        /// same message.
        #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
        labels: BTreeMap<String, String>,
        /// Free-text help for the sample
        #[serde(skip_serializing_if = "Option::is_none", default)]
        help: Option<String>,
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
    /// A name and a number for a dog, published as `channel.note`
    ///
    /// Not a metric sample: the metrics dog never reads one. No grammar
    /// check on `name`, no labels and no help.
    Note {
        /// Whatever the app and the dog agree on
        name: String,
        /// The number that goes with `name`
        value: f64,
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
}

#[cfg(test)]
mod tests {
    use core::fmt::Debug;

    use serde::de::DeserializeOwned;

    use super::*;

    // Fixtures pinned from spec §7. Round-tripped both ways so a silent
    // drift fails loudly.

    #[track_caller]
    fn assert_round_trip<T>(fixture: &str, value: &T)
    where
        T: Serialize + DeserializeOwned + PartialEq + Debug,
    {
        assert_eq!(&serde_json::from_str::<T>(fixture).unwrap(), value);
        assert_eq!(serde_json::to_string(value).unwrap(), fixture);
    }

    #[test]
    fn ready_wire_fixture_round_trips() {
        let fixture = r#"{"kind":"ready"}"#;
        assert_round_trip(fixture, &ChildMessage::Ready);
    }

    #[test]
    fn metric_wire_fixture_round_trips() {
        let fixture = r#"{"kind":"metric","name":"rps","value":42.0}"#;
        let msg = ChildMessage::Metric {
            name: "rps".to_string(),
            value: 42.0,
            metric_type: None,
            labels: BTreeMap::new(),
            help: None,
        };
        assert_round_trip(fixture, &msg);
    }

    /// Field order on the wire is `name, value, type, labels, help`.
    #[test]
    fn a_metric_with_type_labels_and_help_round_trips() {
        let fixture = concat!(
            r#"{"kind":"metric","name":"queue_depth","value":3.0,"type":"counter","#,
            r#""labels":{"model":"qwen"},"help":"Requests waiting for the lease."}"#,
        );
        let msg = ChildMessage::Metric {
            name: "queue_depth".to_string(),
            value: 3.0,
            metric_type: Some(MetricType::Counter),
            labels: [("model".to_string(), "qwen".to_string())].into(),
            help: Some("Requests waiting for the lease.".to_string()),
        };
        assert_round_trip(fixture, &msg);
    }

    #[test]
    fn a_gauge_type_round_trips_as_its_own_spelling() {
        let fixture = r#"{"kind":"metric","name":"rps","value":1.0,"type":"gauge"}"#;
        let msg = ChildMessage::Metric {
            name: "rps".to_string(),
            value: 1.0,
            metric_type: Some(MetricType::Gauge),
            labels: BTreeMap::new(),
            help: None,
        };
        assert_round_trip(fixture, &msg);
    }

    /// No fallback to gauge: the daemon drops the line as malformed.
    #[test]
    fn an_unknown_metric_type_fails_to_decode() {
        for frame in [
            r#"{"kind":"metric","name":"rps","value":1.0,"type":"histogram"}"#,
            r#"{"kind":"metric","name":"rps","value":1.0,"type":"Counter"}"#,
        ] {
            assert!(
                serde_json::from_str::<ChildMessage>(frame).is_err(),
                "{frame} decoded"
            );
        }
    }

    #[test]
    fn an_action_reply_without_an_id_round_trips() {
        let fixture = r#"{"kind":"action-reply","action":"gc","body":"ok"}"#;
        let msg = ChildMessage::ActionReply {
            action: "gc".to_string(),
            body: "ok".to_string(),
            id: None,
        };
        assert_round_trip(fixture, &msg);
    }

    #[test]
    fn an_action_reply_with_an_echoed_id_round_trips() {
        let fixture = r#"{"kind":"action-reply","action":"gc","body":"ok","id":7}"#;
        let msg = ChildMessage::ActionReply {
            action: "gc".to_string(),
            body: "ok".to_string(),
            id: Some(7),
        };
        assert_round_trip(fixture, &msg);
    }

    #[test]
    fn shutdown_wire_fixture_round_trips() {
        let fixture = r#"{"kind":"shutdown"}"#;
        assert_round_trip(fixture, &ShepherdMessage::Shutdown);
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
        assert_round_trip(bare, &bare_msg);

        let with_params = r#"{"kind":"action","name":"set-log-level","params":"debug","id":8}"#;
        let with_params_msg = ShepherdMessage::Action {
            name: "set-log-level".to_string(),
            params: Some("debug".to_string()),
            id: 8,
        };
        assert_round_trip(with_params, &with_params_msg);
    }

    #[test]
    fn a_lamb_label_round_trips() {
        let fixture = r#"{"kind":"lamb-label","pid":4312,"label":"worker 1"}"#;
        let msg = ChildMessage::LambLabel {
            pid: 4312,
            label: LambLabel::new("worker 1").unwrap(),
        };
        assert_round_trip(fixture, &msg);
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

    /// A name the metrics grammar would refuse goes through untouched.
    #[test]
    fn a_note_round_trips_with_any_name() {
        let fixture = r#"{"kind":"note","name":"lease.web.want.1760500000","value":1.0}"#;
        let msg = ChildMessage::Note {
            name: "lease.web.want.1760500000".to_string(),
            value: 1.0,
        };
        assert_round_trip(fixture, &msg);
    }

    #[test]
    fn a_note_without_a_value_fails_to_decode() {
        assert!(serde_json::from_str::<ChildMessage>(r#"{"kind":"note","name":"x"}"#).is_err());
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
    fn a_label_error_says_the_limit_and_the_length() {
        assert_eq!(
            LambLabelError::TooLong { chars: 70 }.to_string(),
            "a lamb label holds at most 64 characters, and this one has 70"
        );
    }
}
