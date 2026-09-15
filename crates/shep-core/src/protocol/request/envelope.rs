//! Wire framing around a request and its reply, and the structured error a reply can hold.

use serde::{Deserialize, Serialize};

use super::{HelloAck, Request, Response};

/// A request frame
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Per-connection request id
    pub id: u64,
    /// Client-imposed deadline (daemon aborts work past it)
    pub deadline_ms: Option<u64>,
    /// The request
    pub body: Request,
}

/// A reply frame
///
/// `result` uses serde's stock `Result` representation: the wire carries
/// `{"Ok": ...}` / `{"Err": ...}`, with capitalized keys, pinned by snapshot.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    /// Echoes [`Envelope::id`]
    pub id: u64,
    /// The outcome
    pub result: Result<Response, RpcError>,
}

/// Handshake outcome: `HelloAck` or a typed refusal, since version skew is
/// an error rather than silence. Same `Ok`/`Err` wire shape as
/// [`Reply::result`]; refusals use [`RpcErrorCode::ProtocolMismatch`].
pub type HelloReply = Result<HelloAck, RpcError>;

/// Structured RPC failure
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// Machine-readable code
    pub code: RpcErrorCode,
    /// Human-readable message (plain English, no theme)
    pub message: String,
    /// The daemon's own crate version, when it chose to name it.
    ///
    /// Set on a [`RpcErrorCode::ProtocolMismatch`] refusal, the only place a
    /// client can learn it, since [`HelloAck::daemon_version`] never arrives
    /// there. `None` on every other error, and on a refusal from a daemon
    /// built before the field existed, so a reader treats `None` as unknown
    /// and takes the conservative path.
    ///
    /// Absent on the wire rather than `null`, so
    /// [`crate::protocol::PROTOCOL_VERSION`] does not move for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
}

/// Machine-readable RPC error codes
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RpcErrorCode {
    /// Selector matched nothing
    NotFound,
    /// Config failed validation daemon-side
    InvalidConfig,
    /// Spawn failed (exec error, permissions)
    SpawnFailed,
    /// Handshake protocol version mismatch
    ProtocolMismatch,
    /// Unexpected daemon-side failure
    Internal,
    /// The request's deadline expired before the daemon finished it
    DeadlineExceeded,
    /// The peer asked for something this build does not implement.
    ///
    /// Distinct from `NotFound`, which means a selector matched nothing.
    /// This means the verb itself is unknown here, and the remedy is a
    /// newer shepherd rather than a different selector.
    Unsupported,
    /// A code this build has not been taught.
    ///
    /// Only ever produced by decoding: an unrecognized string falls through
    /// to this variant via `#[serde(other)]` instead of failing the whole
    /// frame. Nothing constructs one to send, which is a call-site
    /// invariant rather than a type-level one: `#[serde(other)]` governs
    /// decoding only, so serializing this would emit `"unrecognized"`.
    #[serde(other)]
    Unrecognized,
}

impl RpcErrorCode {
    /// Every variant, for code that needs to iterate them all.
    ///
    /// `#[non_exhaustive]` forces a `_` arm on any match written outside this
    /// crate, which would swallow a variant added here and never updated
    /// there (`crates/shep-cli/src/exit.rs` maps every code to an exit
    /// status).
    pub const ALL: [Self; 7] = [
        Self::NotFound,
        Self::InvalidConfig,
        Self::SpawnFailed,
        Self::ProtocolMismatch,
        Self::Internal,
        Self::DeadlineExceeded,
        Self::Unsupported,
    ];

    /// Never called; exists so this crate fails to build if a variant is
    /// added to [`RpcErrorCode`] without also being added to [`Self::ALL`].
    ///
    /// A match here is still checked for exhaustiveness, and each arm indexes
    /// a fixed literal position into [`Self::ALL`], so growing the enum
    /// without growing the array is an out-of-bounds constant index.
    #[allow(dead_code)]
    const fn assert_all_lists_every_variant(code: Self) -> Self {
        match code {
            Self::NotFound => Self::ALL[0],
            Self::InvalidConfig => Self::ALL[1],
            Self::SpawnFailed => Self::ALL[2],
            Self::ProtocolMismatch => Self::ALL[3],
            Self::Internal => Self::ALL[4],
            Self::DeadlineExceeded => Self::ALL[5],
            Self::Unsupported => Self::ALL[6],
            // `Unrecognized` is decode-only and never appears in `ALL`;
            // treat it as `Internal` would be treated.
            Self::Unrecognized => Self::ALL[4],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A code this build has never heard of must decode, not fail. Without
    /// this, adding any error code is a breaking change for every peer.
    #[test]
    fn an_unknown_error_code_decodes_as_unrecognized() {
        assert_eq!(
            serde_json::from_str::<RpcErrorCode>(r#""invented_next_year""#).unwrap(),
            RpcErrorCode::Unrecognized
        );
    }

    #[test]
    fn every_known_error_code_still_round_trips() {
        for code in [
            RpcErrorCode::NotFound,
            RpcErrorCode::InvalidConfig,
            RpcErrorCode::SpawnFailed,
            RpcErrorCode::ProtocolMismatch,
            RpcErrorCode::Internal,
            RpcErrorCode::DeadlineExceeded,
            RpcErrorCode::Unsupported,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(serde_json::from_str::<RpcErrorCode>(&json).unwrap(), code);
        }
    }

    /// The fallback absorbs an unknown STRING, not an unknown TYPE. A number
    /// where a code belongs is still a defect worth reporting.
    #[test]
    fn a_non_string_error_code_is_still_an_error() {
        assert!(serde_json::from_str::<RpcErrorCode>("42").is_err());
    }

    /// The id has to survive a body this build cannot name, or the daemon
    /// has nothing to address a refusal to.
    #[test]
    fn an_unknown_request_kind_keeps_the_envelope_id() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"id":42,"deadline_ms":null,"body":{"kind":"from_the_future","extra":{"a":1}}}"#,
        )
        .unwrap();
        assert_eq!(envelope.id, 42);
        assert_eq!(envelope.body, Request::Unrecognized);
    }

    #[test]
    fn v1_reply_fixture_still_deserializes() {
        // Committed byte fixture, protocol v1.
        let ok = r#"{"id":1,"result":{"Ok":{"kind":"pong"}}}"#;
        let reply: Reply = serde_json::from_str(ok).unwrap();
        assert!(matches!(reply.result, Ok(Response::Pong)));
        let err = r#"{"id":2,"result":{"Err":{"code":"not_found","message":"no sheep"}}}"#;
        let reply: Reply = serde_json::from_str(err).unwrap();
        assert_eq!(reply.result.unwrap_err().code, RpcErrorCode::NotFound);
    }

    #[test]
    fn an_rpc_error_without_a_daemon_version_serializes_exactly_as_before() {
        // `skip_serializing_if` is what makes the field free: no
        // `"daemon_version":null` key for an older client to ignore.
        let plain = RpcError {
            code: RpcErrorCode::NotFound,
            message: "no sheep".to_string(),
            daemon_version: None,
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"code":"not_found","message":"no sheep"}"#
        );
    }

    #[test]
    fn a_v1_rpc_error_fixture_deserializes_with_no_daemon_version() {
        let fixture =
            r#"{"code":"protocol_mismatch","message":"daemon speaks protocol 1, client sent 2"}"#;
        let err: RpcError = serde_json::from_str(fixture).unwrap();
        assert_eq!(err.code, RpcErrorCode::ProtocolMismatch);
        assert_eq!(err.daemon_version, None);
    }

    #[test]
    fn an_old_client_ignores_an_rpc_error_field_it_has_never_seen() {
        // `RpcError` carries no `deny_unknown_fields`, so an optional field
        // may be added without moving `PROTOCOL_VERSION`.
        #[derive(Deserialize)]
        struct OldRpcError {
            code: RpcErrorCode,
            message: String,
        }

        let current = serde_json::to_string(&RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: "daemon speaks protocol 1, client sent 2".to_string(),
            daemon_version: Some("0.1.16".to_string()),
        })
        .unwrap();
        let old: OldRpcError = serde_json::from_str(&current).expect("must tolerate");
        assert_eq!(old.code, RpcErrorCode::ProtocolMismatch);
        assert_eq!(old.message, "daemon speaks protocol 1, client sent 2");
    }

    #[test]
    fn deadline_exceeded_code_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&RpcErrorCode::DeadlineExceeded).unwrap(),
            "\"deadline_exceeded\""
        );
        assert_eq!(
            serde_json::from_str::<RpcErrorCode>("\"deadline_exceeded\"").unwrap(),
            RpcErrorCode::DeadlineExceeded
        );
    }
}
