//! Anything the daemon writes to a connected client
//!
//! The server sends two kinds of frames on one socket: [`Reply`] (answers to
//! requests) and [`BusEvent`] (broadcast events). This type decodes either,
//! untagged on the wire, because their JSON key sets are disjoint
//! (`id`/`result` vs `event`/`data`), at zero cost to the wire.
//!
//! Decoding reads keys until one names a frame kind, then hands the rest of
//! the object to that kind's own decoder. Serde's `untagged` would buffer every
//! payload into an owned tree first, which this avoids.

use core::fmt;

use serde::de::value::MapAccessDeserializer;
use serde::de::{DeserializeSeed, Error as _, IgnoredAny, IntoDeserializer, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::protocol::{BusEvent, Reply};

/// Anything the daemon writes to a connected client
///
/// Round-trips to byte-identical output, since the daemon serializes
/// `Reply`/`BusEvent` directly (pinned by `server_frame_is_byte_identical`).
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
// Growth is anticipated: a future frame kind (progress, flow control) is
// additive here and stays additive on the wire.
#[non_exhaustive]
pub enum ServerFrame {
    /// An answer to one request (from [`Envelope`](crate::protocol::Envelope))
    Reply(Reply),
    /// One subscribed bus event
    Event(BusEvent),
}

/// The frame kind a JSON key belongs to
///
/// A new frame kind adds a variant here, a row in [`Kind::of`] and an arm in
/// [`FrameVisitor::visit_map`], which the compiler flags.
enum Kind {
    Reply,
    Event,
}

impl Kind {
    /// The kind owning `key`, with the key as a static string to replay
    fn of(key: &str) -> Option<(Self, &'static str)> {
        match key {
            "id" => Some((Self::Reply, "id")),
            "result" => Some((Self::Reply, "result")),
            "event" => Some((Self::Event, "event")),
            "data" => Some((Self::Event, "data")),
            _ => None,
        }
    }
}

/// One top-level key: a known one costs no allocation, an unknown one owns its text
enum Key {
    Known(Kind, &'static str),
    Other(String),
}

struct KeySeed;

impl<'de> DeserializeSeed<'de> for KeySeed {
    type Value = Key;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Key, D::Error> {
        struct KeyVisitor;

        impl Visitor<'_> for KeyVisitor {
            type Value = Key;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string key")
            }

            fn visit_str<E>(self, key: &str) -> Result<Key, E> {
                Ok(match Kind::of(key) {
                    Some((kind, name)) => Key::Known(kind, name),
                    None => Key::Other(key.to_owned()),
                })
            }
        }

        deserializer.deserialize_identifier(KeyVisitor)
    }
}

/// A map handed to one kind's decoder: yields the key already read first, then
/// the rest, and notes whether a reply key went by
///
/// A frame with a reply key after an event key is neither kind, and decoding
/// it as an event would leave the request that `id` answers waiting.
struct Keys<A> {
    first: Option<&'static str>,
    rest: A,
    reply_key_seen: bool,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for Keys<A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        if let Some(key) = self.first.take() {
            return seed.deserialize(key.into_deserializer()).map(Some);
        }
        match self.rest.next_key_seed(KeySeed)? {
            None => Ok(None),
            Some(Key::Known(kind, name)) => {
                self.reply_key_seen |= matches!(kind, Kind::Reply);
                seed.deserialize(name.into_deserializer()).map(Some)
            }
            Some(Key::Other(name)) => seed.deserialize(name.into_deserializer()).map(Some),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        self.rest.next_value_seed(seed)
    }
}

struct FrameVisitor;

impl<'de> Visitor<'de> for FrameVisitor {
    type Value = ServerFrame;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a reply (`id`, `result`) or a bus event (`event`, `data`)")
    }

    // A key no frame kind owns is skipped: a Reply has always tolerated those.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ServerFrame, A::Error> {
        let (kind, key) = loop {
            match map.next_key_seed(KeySeed)? {
                Some(Key::Known(kind, name)) => break (kind, name),
                Some(Key::Other(_)) => {
                    map.next_value::<IgnoredAny>()?;
                }
                None => return Err(A::Error::custom("missing `id`/`result` or `event`/`data`")),
            }
        };
        let mut keys = Keys {
            first: Some(key),
            rest: map,
            reply_key_seen: false,
        };
        match kind {
            Kind::Reply => {
                Reply::deserialize(MapAccessDeserializer::new(keys)).map(ServerFrame::Reply)
            }
            Kind::Event => {
                let event = BusEvent::deserialize(MapAccessDeserializer::new(&mut keys))?;
                if keys.reply_key_seen {
                    return Err(A::Error::custom("an event frame carries `id` or `result`"));
                }
                Ok(ServerFrame::Event(event))
            }
        }
    }
}

impl<'de> Deserialize<'de> for ServerFrame {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(FrameVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        BusEvent, ProcessEventKind, ProcessInfo, Reply, Response, RpcError, RpcErrorCode,
        encode_frame,
    };
    use crate::status::ProcStatus;

    fn sample_reply() -> Reply {
        Reply {
            id: 7,
            result: Ok(Response::Pong),
        }
    }

    fn sample_reply_with_id(id: u64) -> Reply {
        Reply {
            id,
            ..sample_reply()
        }
    }

    fn sample_event() -> BusEvent {
        BusEvent::Process {
            event: ProcessEventKind::Online,
            info: ProcessInfo {
                id: 3,
                name: "web".to_string(),
                status: ProcStatus::Online,
                pid: Some(4242),
                restarts: 0,
                uptime_ms: 0,
                fold: None,
                depends_on: Vec::new(),
                out_file: Some("/home/ada/.shep/logs/web-0-out.log".to_string()),
                err_file: Some("/home/ada/.shep/logs/web-0-err.log".to_string()),
                cpu_percent: None,
                memory_bytes: None,
                cpu_ms: None,
                dog: None,
                lambs: None,
                last_exit: None,
                smit: None,
                instance: None,
                handshook: None,
                dog_stale: None,
                pending: None,
                overridden: None,
                max_memory: None,
                level_rules: Vec::new(),
                reload_deadline_ms: None,
            },
            manually: false,
            at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn server_frame_decodes_both_directions_of_the_stream() {
        // The two shapes are disjoint: a Reply has no `event` key and an
        // event has no `id`/`result` pair, so untagged never guesses wrong.
        let reply = r#"{"id":1,"result":{"Ok":{"kind":"pong"}}}"#;
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(reply).unwrap(),
            ServerFrame::Reply(Reply { id: 1, .. })
        ));
        let event = r#"{"event":"log_out","data":{"id":3,"line":"ready"}}"#;
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(event).unwrap(),
            ServerFrame::Event(BusEvent::LogOut { id: 3, .. })
        ));
    }

    #[test]
    fn a_whole_payload_survives_the_trip_through_the_frame() {
        let event = serde_json::to_string(&sample_event()).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerFrame>(&event).unwrap(),
            ServerFrame::Event(sample_event())
        );
        let reply = serde_json::to_string(&sample_reply()).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerFrame>(&reply).unwrap(),
            ServerFrame::Reply(sample_reply())
        );
    }

    #[test]
    fn a_malformed_reply_reports_its_own_failure() {
        // Untagged buffering would swallow this into "did not match any variant".
        let err =
            serde_json::from_str::<ServerFrame>(r#"{"id":1,"result":{"Ok":{"kind":"bogus"}}}"#)
                .unwrap_err()
                .to_string();
        assert!(!err.contains("untagged"), "{err}");
        assert!(err.contains("bogus"), "{err}");
    }

    #[test]
    fn a_frame_with_no_known_key_is_refused() {
        let err = serde_json::from_str::<ServerFrame>(r#"{"unrelated":1}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("event"), "{err}");
        assert!(err.contains("result"), "{err}");
    }

    #[test]
    fn key_order_does_not_change_which_frame_decodes() {
        let reply = r#"{"result":{"Ok":{"kind":"pong"}},"id":1}"#;
        assert_eq!(
            serde_json::from_str::<ServerFrame>(reply).unwrap(),
            ServerFrame::Reply(sample_reply_with_id(1))
        );
        let event = r#"{"data":{"id":3,"line":"ready"},"event":"log_out"}"#;
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(event).unwrap(),
            ServerFrame::Event(BusEvent::LogOut { id: 3, .. })
        ));
    }

    #[test]
    fn a_frame_carrying_both_kinds_of_key_is_never_taken_for_an_event() {
        // Untagged tried Reply first. The first key decides here, so an event
        // key leading a reply key fails instead of routing the reply as an event.
        let event_first = r#"{"event":"log_out","data":{"id":3,"line":"ready"},"id":1,"result":{"Ok":{"kind":"pong"}}}"#;
        let decoded = serde_json::from_str::<ServerFrame>(event_first);
        assert!(decoded.is_err(), "{decoded:?}");

        let reply_first = r#"{"id":1,"result":{"Ok":{"kind":"pong"}},"event":"log_out","data":{"id":3,"line":"ready"}}"#;
        assert_eq!(
            serde_json::from_str::<ServerFrame>(reply_first).unwrap(),
            ServerFrame::Reply(sample_reply_with_id(1))
        );
    }

    #[test]
    fn an_unknown_leading_key_does_not_hide_a_reply() {
        // A Reply has always tolerated keys it does not know.
        let reply = r#"{"extra":{"a":[1,2]},"id":1,"result":{"Ok":{"kind":"pong"}}}"#;
        assert_eq!(
            serde_json::from_str::<ServerFrame>(reply).unwrap(),
            ServerFrame::Reply(sample_reply_with_id(1))
        );
    }

    #[test]
    fn server_frame_is_byte_identical_to_its_payload() {
        // The daemon encodes Reply/BusEvent directly; if wrapping ever
        // started adding bytes, every client would break at once.
        let reply = sample_reply();
        assert_eq!(
            encode_frame(&ServerFrame::Reply(reply.clone())).unwrap(),
            encode_frame(&reply).unwrap()
        );
        let event = sample_event();
        assert_eq!(
            encode_frame(&ServerFrame::Event(event.clone())).unwrap(),
            encode_frame(&event).unwrap()
        );
    }

    #[test]
    fn an_error_reply_still_decodes_as_a_reply_frame() {
        let err = Reply {
            id: 2,
            result: Err(RpcError {
                code: RpcErrorCode::DeadlineExceeded,
                message: "request deadline of 5000 ms expired".to_string(),
                daemon_version: None,
            }),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerFrame>(&json).unwrap(),
            ServerFrame::Reply(err)
        );
    }
}
