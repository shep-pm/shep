//! Frame encoding: u32 length prefix + JSON payload
//!
//! One codec constructor + encode/decode helpers shared by daemon and
//! client so framing parameters can never drift between the two.

use core::fmt;

use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_util::codec::LengthDelimitedCodec;

/// Hard ceiling per frame; larger is a protocol violation
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Builds the shared length-delimited codec (u32 BE prefix, 16 MiB cap)
#[must_use]
pub fn codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .length_field_type::<u32>()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_codec()
}

/// Serializes one value to a frame payload
///
/// # Errors
///
/// - [`WireError::Json`]: serialization failed (carries serde's message).
/// - [`WireError::FrameTooLarge`]: payload exceeds [`MAX_FRAME_BYTES`].
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Bytes, WireError> {
    let vec = serde_json::to_vec(value).map_err(|e| WireError::Json(e.to_string()))?;
    if vec.len() > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge(vec.len()));
    }
    Ok(Bytes::from(vec)) // zero-copy: Bytes takes the Vec's buffer
}

/// Deserializes one frame payload
///
/// # Errors
///
/// - [`WireError::Json`]: the payload is not valid JSON for `T`.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, WireError> {
    serde_json::from_slice(frame).map_err(|e| WireError::Json(e.to_string()))
}

/// Enough of a reply to recover its id when the reply's own type does not
/// decode.
///
/// `result` is required but its value is never looked at: its only job is to
/// keep this struct from matching a frame that merely happens to carry an
/// `id`, such as a future progress or flow-control frame. Without it,
/// `reply_id` would misidentify that frame as an undecodable reply and fail
/// a caller whose real reply is still in flight.
#[derive(serde::Deserialize)]
struct ReplyIdOnly {
    id: u64,
    #[allow(dead_code, reason = "present only to narrow the match; never read")]
    result: serde::de::IgnoredAny,
}

/// If `frame` is a reply, the id it answers; `None` for anything else,
/// including an event and a frame that is not JSON at all.
///
/// A reply whose `Response` this build cannot decode still has a caller
/// waiting on it. The id is the only thing needed to fail that caller by
/// name instead of leaving it to wait out its deadline for an answer that
/// already arrived.
#[must_use]
pub fn reply_id(frame: &[u8]) -> Option<u64> {
    decode_frame::<ReplyIdOnly>(frame).ok().map(|r| r.id)
}

/// Error type returned from [`encode_frame`] and [`decode_frame`]
///
/// `#[non_exhaustive]`: this type is on the peer-facing surface, and the
/// protocol will grow past a JSON payload and a size cap eventually.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// JSON (de)serialization failed (carries the serde message)
    Json(String),
    /// Encoded payload exceeds [`MAX_FRAME_BYTES`] (carries actual size)
    FrameTooLarge(usize),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(m) => write!(f, "wire frame JSON error: {m}"),
            Self::FrameTooLarge(n) => {
                write!(
                    f,
                    "frame of {n} bytes exceeds the {MAX_FRAME_BYTES}-byte limit"
                )
            }
        }
    }
}

impl core::error::Error for WireError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Envelope, Request};

    #[test]
    fn encode_decode_round_trip() {
        let env = Envelope {
            id: 9,
            deadline_ms: Some(5000),
            body: Request::Ping,
        };
        let bytes = encode_frame(&env).unwrap();
        let back: Envelope = decode_frame(&bytes).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn decode_rejects_garbage_with_json_error() {
        assert!(matches!(
            decode_frame::<Envelope>(b"not json"),
            Err(WireError::Json(_))
        ));
    }

    #[test]
    fn reply_id_reads_the_id_off_a_real_reply() {
        assert_eq!(
            reply_id(br#"{"id":7,"result":{"Ok":{"kind":"Pong"}}}"#),
            Some(7)
        );
    }

    #[test]
    fn reply_id_is_none_for_an_event() {
        assert_eq!(reply_id(br#"{"event":"from_the_future","data":{}}"#), None);
    }

    #[test]
    fn reply_id_is_none_for_non_json() {
        assert_eq!(reply_id(b"not json"), None);
    }

    #[test]
    fn reply_id_is_none_for_a_future_progress_frame() {
        // A frame that carries an `id` but is not shaped like a reply (no
        // `result`) must not be mistaken for an undecodable reply: the
        // caller it would falsely fail is still waiting on the real one.
        assert_eq!(
            reply_id(br#"{"kind":"progress","id":7,"percent":40}"#),
            None
        );
    }

    #[test]
    fn codec_uses_u32_prefix_and_max_frame() {
        let c = codec();
        assert_eq!(c.max_frame_length(), MAX_FRAME_BYTES);
    }

    #[tokio::test]
    async fn framed_stream_round_trip() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_util::codec::{FramedRead, FramedWrite};

        let (client, server) = tokio::io::duplex(64 * 1024);
        let mut writer = FramedWrite::new(client, codec());
        let mut reader = FramedRead::new(server, codec());

        let env = Envelope {
            id: 1,
            deadline_ms: None,
            body: Request::ListFlock,
        };
        writer.send(encode_frame(&env).unwrap()).await.unwrap();

        let frame = reader.next().await.unwrap().unwrap();
        let back: Envelope = decode_frame(&frame).unwrap();
        assert_eq!(back, env);
    }
}
