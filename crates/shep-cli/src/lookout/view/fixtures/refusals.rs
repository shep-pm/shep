//! The shepherd's refusals, for the tests about what a reply says.

use shep_client::RequestError;
use shep_core::protocol::{RpcError, RpcErrorCode};

/// The daemon's refusal for a write that fails config validation: what a
/// `cwd` the shepherd's user cannot enter comes back as.
pub fn invalid_config() -> RequestError {
    RequestError::Rpc(RpcError {
        code: RpcErrorCode::InvalidConfig,
        message: "cwd: no such directory".to_string(),
        daemon_version: None,
    })
}

/// The shepherd's refusal of one write, for the tests about what a reply
/// says once the pane that asked for it has gone.
pub fn a_refusal() -> RequestError {
    RequestError::Rpc(shep_core::protocol::RpcError {
        code: shep_core::protocol::RpcErrorCode::InvalidConfig,
        message: "the store is locked by another shep".to_owned(),
        daemon_version: None,
    })
}
