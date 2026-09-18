//! Maps a [`SupervisorError`] to the wire-level [`RpcError`] a reply
//! carries.

use shep_core::protocol::{RpcError, RpcErrorCode};

use crate::supervisor::SupervisorError;

use super::selector_verbs::not_found;

pub(super) fn rpc_error(err: &SupervisorError) -> RpcError {
    match err {
        SupervisorError::NotFound => not_found(),
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
        SupervisorError::InvalidScale(msg) => RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: msg.clone(),
            daemon_version: None,
        },
        // `InvalidConfig`, like `InvalidScale` above and for its reason: a
        // request aimed at a dog is one the caller can aim elsewhere. The
        // bare payload is the same sentence `apply_one` puts in front of an
        // operator whose Flockfile named a dog, so the two doors read alike.
        SupervisorError::IsADog(msg) => RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: msg.clone(),
            daemon_version: None,
        },
        // `InvalidConfig`, like `InvalidScale` above and for its reason:
        // this is something the caller asked for that it can ask
        // differently, and telling an operator "unexpected daemon-side
        // failure" about their own env key would send them to the wrong
        // place entirely. The bare payload is `normalize`'s own refusal,
        // which already names the key.
        SupervisorError::InvalidEnv(msg) => RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: msg.clone(),
            daemon_version: None,
        },
        // `InvalidConfig`, beside `InvalidEnv` above and for its reason:
        // every shape that reaches it is the caller's own key or value,
        // which it can ask differently. The bare payload rather than
        // `err.to_string()`, again like `InvalidEnv`: the message already
        // names the field.
        SupervisorError::InvalidField(msg) => RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: msg.clone(),
            daemon_version: None,
        },
        // `Internal`, on the same rule the log-maintenance pair above
        // states: an override store that cannot be read or written is an
        // unexpected daemon-side failure, and there is no code for it that
        // a client predating this build could decode. `err.to_string()`
        // rather than the bare payload, so the reader is told the store was
        // the thing that failed and not the request.
        SupervisorError::Overrides(_) => RpcError {
            code: RpcErrorCode::Internal,
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
