//! The opening frame pair: a client says hello, the daemon answers.

use serde::{Deserialize, Serialize};

/// Client's opening frame
///
/// No `deny_unknown_fields`: refusing an unknown field here would refuse a
/// newer client before `protocol` is read.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Client crate version (semver string)
    pub client_version: String,
    /// [`crate::protocol::PROTOCOL_VERSION`] the client speaks
    pub protocol: u32,
    /// The name this client was registered under as a dog, when it is one.
    ///
    /// `None` for every other client; a bare `Client` cannot set it. The
    /// daemon needs it to name a dog it refuses at the handshake, which never
    /// reaches `Request::DogConfig`. A dog reads its own name from
    /// `$SHEP_DOG_NAME`.
    ///
    /// Absent on the wire rather than `null`, so
    /// [`crate::protocol::PROTOCOL_VERSION`] does not move for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dog_name: Option<String>,
}

/// Daemon's handshake answer
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAck {
    /// Daemon crate version
    pub daemon_version: String,
    /// Protocol version the daemon speaks
    pub protocol: u32,
    /// Daemon pid
    pub pid: u32,
    /// The oldest protocol this daemon accepts, or `None` from a daemon
    /// predating the floor.
    ///
    /// Absent rather than `null` on the wire, so it does not move
    /// [`crate::protocol::PROTOCOL_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::super::{HelloReply, RpcError, RpcErrorCode};
    use super::*;
    use crate::protocol::PROTOCOL_VERSION;

    #[test]
    fn hello_handshake_shape() {
        let hello = Hello {
            client_version: "0.1.0".to_string(),
            protocol: PROTOCOL_VERSION,
            dog_name: None,
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(json, r#"{"client_version":"0.1.0","protocol":9}"#);
    }

    #[test]
    fn a_dogs_hello_names_the_dog_and_nothing_elses_does() {
        let dog = Hello {
            client_version: "0.1.0".to_string(),
            protocol: PROTOCOL_VERSION,
            dog_name: Some("metrics".to_string()),
        };
        let json = serde_json::to_string(&dog).unwrap();
        assert_eq!(
            json,
            r#"{"client_version":"0.1.0","protocol":9,"dog_name":"metrics"}"#
        );
        assert_eq!(serde_json::from_str::<Hello>(&json).unwrap(), dog);
    }

    /// `Hello` is the version-negotiation frame, so `deny_unknown_fields`
    /// here would refuse a newer client before `protocol` is read, leaving
    /// neither peer able to report the skew.
    #[test]
    fn a_hello_without_a_dog_name_still_parses() {
        let fixture = r#"{"client_version":"0.1.14","protocol":2}"#;
        let hello: Hello = serde_json::from_str(fixture).unwrap();
        assert_eq!(hello.protocol, 2);
        assert_eq!(hello.dog_name, None);

        // The other direction: an older daemon ignores a key it does not
        // know. `unknown_to_an_older_daemon` stands in for `dog_name`.
        let newer = r#"{"client_version":"9.9.9","protocol":2,"dog_name":"metrics","unknown_to_an_older_daemon":true}"#;
        let hello: Hello = serde_json::from_str(newer).unwrap();
        assert_eq!(hello.protocol, 2);
        assert_eq!(hello.dog_name.as_deref(), Some("metrics"));
    }

    #[test]
    fn hello_ack_handshake_shape() {
        let ack = HelloAck {
            daemon_version: "0.5.0".to_string(),
            protocol: PROTOCOL_VERSION,
            pid: 1234,
            min_supported: Some(crate::protocol::MIN_SUPPORTED),
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert_eq!(
            json,
            r#"{"daemon_version":"0.5.0","protocol":9,"pid":1234,"min_supported":8}"#
        );
        assert_eq!(serde_json::from_str::<HelloAck>(&json).unwrap(), ack);
    }

    /// `min_supported` is `None` from a daemon predating the floor, and the
    /// omission has to be a missing key rather than `null`, or it would move
    /// `PROTOCOL_VERSION` for every daemon that already ships one.
    #[test]
    fn hello_ack_without_min_supported_omits_the_key_not_nulls_it() {
        let ack = HelloAck {
            daemon_version: "0.5.0".to_string(),
            protocol: PROTOCOL_VERSION,
            pid: 1234,
            min_supported: None,
        };
        let json = serde_json::to_string(&ack).unwrap();
        assert_eq!(
            json,
            r#"{"daemon_version":"0.5.0","protocol":9,"pid":1234}"#
        );
        assert!(!json.contains("min_supported"));
    }

    /// An old daemon fixture, from before the floor existed, still decodes.
    #[test]
    fn an_old_hello_ack_without_min_supported_still_parses() {
        let fixture = r#"{"daemon_version":"0.1.14","protocol":2,"pid":9}"#;
        let ack: HelloAck = serde_json::from_str(fixture).unwrap();
        assert_eq!(ack.protocol, 2);
        assert_eq!(ack.min_supported, None);
    }

    #[test]
    fn hello_reply_carries_typed_skew_error() {
        let refusal: HelloReply = Err(RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: "daemon speaks protocol 1, client sent 2".to_string(),
            daemon_version: None,
        });
        let json = serde_json::to_string(&refusal).unwrap();
        assert_eq!(
            json,
            r#"{"Err":{"code":"protocol_mismatch","message":"daemon speaks protocol 1, client sent 2"}}"#
        );
        let back: HelloReply = serde_json::from_str(&json).unwrap();
        assert_eq!(back, refusal);
    }

    #[test]
    fn v1_hello_ack_fixture_still_deserializes() {
        let fixture = r#"{"Ok":{"daemon_version":"0.1.0","protocol":1,"pid":4242}}"#;
        let ack: HelloReply = serde_json::from_str(fixture).unwrap();
        assert_eq!(ack.unwrap().pid, 4242);
    }
}
