//! The fixture corpus, and the test that keeps it honest.
//!
//! Each case is serialized and compared byte for byte with the committed
//! file. It is also read back and compared with the original value.
//!
//! Both directions matter. Go, JavaScript and Python libraries are written
//! against these bytes. A drift in either direction would reach them
//! silently.
//!
//! Regenerate with `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel --test fixtures`.

use std::fmt::Debug;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use shep_channel::{ChildMessage, ShepherdMessage};

mod common;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Holds one case against its committed file and against itself.
///
/// Generic over the message type so both corpora run the same check: a
/// round trip that passes for one enum and not the other would otherwise
/// depend on which of the two loops someone remembered to update.
fn check<T: Serialize + DeserializeOwned + PartialEq + Debug>(name: &str, value: &T) {
    let encoded = serde_json::to_string(value).expect("encode");
    common::bless_or_compare(
        &fixtures_dir().join(format!("{name}.json")),
        &encoded,
        "Three other libraries are written against these bytes.",
    );
    let decoded: T = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(&decoded, value, "{name} does not survive a round trip");
}

#[test]
fn child_messages_match_their_fixtures() {
    let cases: Vec<(&str, ChildMessage)> = vec![
        ("child-ready", ChildMessage::Ready),
        (
            "child-metric",
            ChildMessage::Metric {
                name: "rps".into(),
                value: 42.0,
            },
        ),
        (
            "child-metric-zero",
            ChildMessage::Metric {
                name: "idle".into(),
                value: 0.0,
            },
        ),
        (
            "child-action-reply",
            ChildMessage::ActionReply {
                action: "gc".into(),
                body: "ok".into(),
                id: None,
            },
        ),
        (
            "child-action-reply-id",
            ChildMessage::ActionReply {
                action: "gc".into(),
                body: "ok".into(),
                id: Some(7),
            },
        ),
    ];
    for (name, value) in cases {
        check(name, &value);
    }
}

#[test]
fn shepherd_messages_match_their_fixtures() {
    let cases: Vec<(&str, ShepherdMessage)> = vec![
        ("shepherd-shutdown", ShepherdMessage::Shutdown),
        (
            "shepherd-action",
            ShepherdMessage::Action {
                name: "gc".into(),
                params: None,
                id: 7,
            },
        ),
        (
            "shepherd-action-params",
            ShepherdMessage::Action {
                name: "set-log-level".into(),
                params: Some("debug".into()),
                id: 8,
            },
        ),
    ];
    for (name, value) in cases {
        check(name, &value);
    }
}

/// Old apps send this shape without an id. The daemon's name-and-order
/// fallback exists for exactly this case.
#[test]
fn an_action_reply_without_an_id_still_decodes() {
    let decoded: ChildMessage =
        serde_json::from_str(r#"{"kind":"action-reply","action":"gc","body":"ok"}"#)
            .expect("decode");
    assert_eq!(
        decoded,
        ChildMessage::ActionReply {
            action: "gc".into(),
            body: "ok".into(),
            id: None
        }
    );
}
