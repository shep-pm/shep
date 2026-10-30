//! The wire-level [`RpcError`] a reply carries: the mapping from a
//! [`SupervisorError`], and the constructors for the refusals a handler
//! builds itself.
//!
//! Every refusal here leaves `daemon_version` `None`: that field is reserved
//! for a [`RpcErrorCode::ProtocolMismatch`] refusal, the only reply where
//! [`shep_core::protocol::HelloAck::daemon_version`] never reaches the
//! client.

use shep_core::protocol::{RpcError, RpcErrorCode};

use crate::supervisor::SupervisorError;

use super::selector_verbs::not_found;

/// An `InvalidConfig` refusal carrying `message`'s text
///
/// For a config-shaped refusal a handler builds itself rather than mapping
/// from a [`SupervisorError`]: something the caller can ask differently, not
/// a daemon-side failure.
pub(super) fn invalid_config(message: impl core::fmt::Display) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidConfig,
        message: message.to_string(),
        daemon_version: None,
    }
}

/// A `NotFound` refusal naming the sheep `name`
///
/// For a verb that aims at one sheep by name, where an unknown name is a
/// refusal rather than an empty selector match ([`not_found`]).
pub(super) fn no_sheep(name: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::NotFound,
        message: format!("no sheep named {name}"),
        daemon_version: None,
    }
}

pub(super) fn rpc_error(err: &SupervisorError) -> RpcError {
    match err {
        SupervisorError::NotFound => not_found(),
        SupervisorError::NoSuchSheep(name) => no_sheep(name),
        SupervisorError::SpawnFailed(msg) => RpcError {
            code: RpcErrorCode::SpawnFailed,
            message: msg.clone(),
            daemon_version: None,
        },
        // The same code as `SpawnFailed`: `RpcErrorCode` is versioned, and a
        // client predating a new code cannot decode the reply at all. The
        // bare payload rather than `err.to_string()`, since this message
        // already opens with "nothing in this batch was registered".
        SupervisorError::CannotStart(msg) => RpcError {
            code: RpcErrorCode::SpawnFailed,
            message: msg.clone(),
            daemon_version: None,
        },
        // `Internal`, an unexpected daemon-side failure, and no code of its
        // own since a client predating a new one could not decode the reply.
        // `err.to_string()` rather than the bare payload: `Display` is the
        // only thing distinguishing the two once they share a code.
        SupervisorError::ReopenFailed(_) | SupervisorError::FlushFailed(_) => RpcError {
            code: RpcErrorCode::Internal,
            message: err.to_string(),
            daemon_version: None,
        },
        // `Internal` under protest: an app already being reloaded is a
        // conflict the caller can act on, and the wire has no code for one.
        // `Display` names the app, which is the part that says what to do.
        SupervisorError::ReloadInFlight(_) => RpcError {
            code: RpcErrorCode::Internal,
            message: err.to_string(),
            daemon_version: None,
        },
        // Every `InvalidScale` is something the caller can ask differently: a
        // count of `0`, a dog, or an app whose earlier scale is still
        // shutting instances down. That last one is a conflict, like
        // `ReloadInFlight`, and the wire has no code for one.
        SupervisorError::InvalidScale(msg) => invalid_config(msg),
        // A request aimed at a dog is one the caller can aim elsewhere. The
        // message is the same sentence `apply_one` puts in front of an
        // operator whose Flockfile named a dog, so the two doors read alike.
        SupervisorError::IsADog(msg) => invalid_config(msg),
        // Something the caller asked for that it can ask differently: telling
        // an operator "unexpected daemon-side failure" about their own env key
        // would send them to the wrong place entirely. The message is
        // `normalize`'s own refusal, which already names the key.
        SupervisorError::InvalidEnv(msg) => invalid_config(msg),
        SupervisorError::EnvChanged(_) => invalid_config(err),
        // Beside `InvalidEnv` above and for its reason: every shape that
        // reaches it is the caller's own key or value, and the message names
        // the field.
        SupervisorError::InvalidField(msg) => invalid_config(msg),
        // `Internal`, on the same rule the log-maintenance pair above states:
        // an override store that cannot be read or written is an unexpected
        // daemon-side failure, and there is no code for it that a client
        // predating this build could decode.
        SupervisorError::Overrides(_) | SupervisorError::DogDefaults(_) => RpcError {
            code: RpcErrorCode::Internal,
            message: err.to_string(),
            daemon_version: None,
        },
        // Its own code: the fix is another verb, and `Display` names it.
        SupervisorError::Disabled(_) => RpcError {
            code: RpcErrorCode::Disabled,
            message: err.to_string(),
            daemon_version: None,
        },
        SupervisorError::EngineStopped => RpcError {
            code: RpcErrorCode::Internal,
            message: "the supervisor engine has stopped".to_string(),
            daemon_version: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shep_core::config::NormalizeError;

    #[test]
    fn invalid_config_carries_the_displayed_text_and_no_daemon_version() {
        let from_text = invalid_config("a line may not contain a newline");
        assert_eq!(from_text.code, RpcErrorCode::InvalidConfig);
        assert_eq!(from_text.message, "a line may not contain a newline");
        assert_eq!(from_text.daemon_version, None);

        let from_error = invalid_config(NormalizeError::DuplicateName("web".to_string()));
        assert_eq!(from_error.code, RpcErrorCode::InvalidConfig);
        assert_eq!(from_error.message, "duplicate sheep name `web`");
        assert_eq!(from_error.daemon_version, None);
    }

    #[test]
    fn no_sheep_names_the_sheep_under_not_found() {
        let err = no_sheep("web");
        assert_eq!(err.code, RpcErrorCode::NotFound);
        assert_eq!(err.message, "no sheep named web");
        assert_eq!(err.daemon_version, None);
    }
}
