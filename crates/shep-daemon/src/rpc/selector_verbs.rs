//! Selector-in, flock-out verbs that share the resolve-call-map
//! pattern ([`selector_call`]), plus [`trigger`] and [`signal_request`],
//! which need their own resolve path.

use core::future::Future;

use shep_core::protocol::{ProcessInfo, Reply, Response, RpcError, RpcErrorCode, SelectorSpec};
use shep_core::selector::ProcessSelector;
use shep_core::signals::OperatorSignal;

use crate::supervisor::SupervisorError;

use super::context::{Outcome, RpcContext};
use super::error::rpc_error;

pub(super) fn not_found() -> RpcError {
    RpcError {
        code: RpcErrorCode::NotFound,
        message: "selector matched no registered sheep".to_string(),
        daemon_version: None,
    }
}

pub(super) fn selector_of(spec: SelectorSpec) -> Result<ProcessSelector, RpcError> {
    ProcessSelector::try_from(spec).map_err(|err| RpcError {
        code: RpcErrorCode::InvalidConfig,
        message: err.to_string(),
        daemon_version: None,
    })
}

/// The helper every selector-in, flock-out verb shares: convert the selector,
/// call the supervisor, map the hits through the passed `Response`
/// constructor.
///
/// The future bound is stated, not inferred, because the whole chain is
/// awaited inside the per-connection `tokio::spawn`.
pub(super) async fn selector_call<F, Fut>(
    id: u64,
    spec: SelectorSpec,
    call: F,
    ok: fn(Vec<ProcessInfo>) -> Response,
) -> Outcome
where
    F: FnOnce(ProcessSelector) -> Fut + Send,
    Fut: Future<Output = Result<Vec<ProcessInfo>, SupervisorError>> + Send,
{
    let result = match selector_of(spec) {
        Ok(selector) => call(selector).await.map(ok).map_err(|err| rpc_error(&err)),
        Err(err) => Err(err),
    };
    Outcome::Reply(Reply { id, result })
}

/// `Trigger`'s own resolve-then-map path. [`selector_call`] cannot serve it:
/// that helper maps `Vec<ProcessInfo>`, and `Response::Triggered` carries
/// `Vec<ActionReply>`, a row `ProcessInfo` cannot hold a reply body on.
///
/// How long each app gets to answer is `AppConfig::action_timeout`, one value
/// per matched sheep, read where the wait is armed (`Actor::begin_action`).
/// `shep_core::config::normalize` refuses only a value no caller could ever
/// outlast; one past the default budget is accepted, and the caller's own
/// deadline decides whether that pays off.
pub(super) async fn trigger(
    id: u64,
    spec: SelectorSpec,
    action: String,
    params: Option<String>,
    ctx: &RpcContext,
) -> Outcome {
    let result = match selector_of(spec) {
        Err(err) => Err(err),
        Ok(selector) => ctx
            .supervisor
            .trigger(selector, action, params)
            .await
            .map(Response::Triggered)
            .map_err(|err| rpc_error(&err)),
    };
    Outcome::Reply(Reply { id, result })
}

/// `Signal`'s own resolve-then-map path, mirroring [`trigger`]. The signal
/// name is re-validated here even though the CLI validated it too: peer input
/// is untrusted, the rule `Request::Start` follows a few arms up.
pub(super) async fn signal_request(
    id: u64,
    spec: SelectorSpec,
    signal: String,
    ctx: &RpcContext,
) -> Outcome {
    let result = match OperatorSignal::parse(&signal) {
        None => Err(RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: format!(
                "`{signal}` is not a signal shep will send; accepted: {}",
                OperatorSignal::ACCEPTED.join(", ")
            ),
            daemon_version: None,
        }),
        Some(sig) => match selector_of(spec) {
            Err(err) => Err(err),
            Ok(selector) => ctx
                .supervisor
                .signal(selector, sig)
                .await
                .map(Response::Signalled)
                .map_err(|err| rpc_error(&err)),
        },
    };
    Outcome::Reply(Reply { id, result })
}
