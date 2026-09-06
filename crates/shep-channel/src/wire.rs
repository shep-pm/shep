//! The shepherd channel: newline-JSON wire on fd 3 between the shepherd
//! and each spawned child. [`ChildMessage`] flows child to shepherd;
//! [`ShepherdMessage`] flows shepherd to child.
//!
//! Both enums are exhaustive on purpose. The channel has no handshake, so
//! a new variant has to be announced out of band. An exhaustive match
//! forces every call site to react to it.
//!
//! Pins the wire shapes only. See `docs/shepherd-channel.md` for reply and
//! correlation semantics.

use serde::{Deserialize, Serialize};

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
}

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
}
