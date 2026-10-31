//! `Signal` aimed at one lamb of one sheep.
//!
//! The sheep's own pid is the supervisor's to signal. A lamb is not
//! supervised, so this path walks the process table itself and sends only to a
//! pid the walk found under the sheep: it is not a general `kill`.

use core::fmt;

use std::sync::Arc;

use shep_core::protocol::{
    ProcessInfo, Response, RpcError, RpcErrorCode, SelectorSpec, SignalOutcome, SignalReply,
};
use shep_core::signals::OperatorSignal;

use crate::limits::stats::StatsState;
use crate::runner::RunnerError;

use super::context::RpcContext;
use super::error::{invalid_config, rpc_error};
use super::selector_verbs::{matching_rows, not_found, selector_of};

/// Delivers a signal to one process by pid.
///
/// A seam so a test can see what was sent, and what was not, without a
/// process to signal.
pub(crate) trait PidSignals: Send + Sync + fmt::Debug {
    /// Sends `sig` to the process `pid` alone, never its group.
    ///
    /// # Errors
    ///
    /// [`RunnerError::SignalFailed`] when the OS refuses the delivery or the
    /// platform has no way to make it.
    fn send(&self, pid: u32, sig: OperatorSignal) -> Result<(), RunnerError>;
}

/// [`PidSignals`] over the operating system.
#[derive(Debug)]
pub(crate) struct OsPidSignals;

impl PidSignals for OsPidSignals {
    fn send(&self, pid: u32, sig: OperatorSignal) -> Result<(), RunnerError> {
        crate::tokio_runner::signal_pid(pid, sig)
    }
}

/// Answers a `Signal` that names a lamb.
///
/// `selector` must match exactly one registered sheep. A sheep with no live
/// process answers [`SignalOutcome::NotRunning`].
pub(super) async fn signal_lamb(
    spec: SelectorSpec,
    sig: OperatorSignal,
    lamb: u32,
    ctx: &RpcContext,
) -> Result<Response, RpcError> {
    let selector = selector_of(spec)?;
    let infos = ctx
        .supervisor
        .list_checked()
        .await
        .map_err(|err| rpc_error(&err))?;
    let exact = selector.is_exact();
    // A sweep passes a parked app by, as it does for every lifecycle verb, so
    // one running sheep beside a disabled one is still exactly one.
    let rows = matching_rows(infos, &selector)
        .into_iter()
        .filter(|row| exact || row.disabled != Some(true))
        .collect();
    let row = one_sheep(rows)?;
    if row.dog.is_some() {
        return Err(invalid_config(format!(
            "{} is a dog, and a lamb signal reaches a sheep's lambs only",
            row.name
        )));
    }

    let outcome = match row.pid {
        None => SignalOutcome::NotRunning,
        Some(root) => {
            let stats = Arc::clone(&ctx.stats);
            let signals = Arc::clone(&ctx.pid_signals);
            tokio::task::spawn_blocking(move || deliver(&stats, signals.as_ref(), root, lamb, sig))
                .await
                .map_err(|err| RpcError {
                    code: RpcErrorCode::Internal,
                    message: format!("the lamb signal task did not finish: {err}"),
                    daemon_version: None,
                })?
        }
    };
    if let (SignalOutcome::Delivered, Some(root)) = (&outcome, row.pid) {
        // The signal is already sent, so a sheep that cannot be told, because
        // it has no channel, a respawn has replaced the process the lamb
        // belonged to, or the engine is stopping, does not change the answer.
        let _ = ctx
            .supervisor
            .tell_lamb_signalled(row.id, root, lamb, sig)
            .await;
    }
    Ok(Response::Signalled(vec![SignalReply {
        id: row.id,
        name: row.name,
        outcome,
    }]))
}

/// The one row a lamb signal may aim through.
fn one_sheep(mut rows: Vec<ProcessInfo>) -> Result<ProcessInfo, RpcError> {
    match rows.len() {
        0 => Err(not_found()),
        1 => Ok(rows.remove(0)),
        count => {
            let names: Vec<_> = rows
                .iter()
                .map(|row| format!("{}#{}", row.name, row.id))
                .collect();
            Err(invalid_config(format!(
                "a lamb signal needs a selector that matches exactly one sheep, \
                 and this matched {count}: {}",
                names.join(", ")
            )))
        }
    }
}

/// Walks the lambs of the sheep at `root`, and sends only if `lamb` is one.
///
/// Walk and send are one synchronous call, so a pid that exits and is reused
/// has only this window to land on.
fn deliver(
    stats: &StatsState,
    signals: &dyn PidSignals,
    root: u32,
    lamb: u32,
    sig: OperatorSignal,
) -> SignalOutcome {
    let index = stats.lamb_index();
    if !stats.lambs_of(&index, root).iter().any(|l| l.pid == lamb) {
        return SignalOutcome::NotALamb;
    }
    match signals.send(lamb, sig) {
        Ok(()) => SignalOutcome::Delivered,
        Err(err) => SignalOutcome::Failed {
            reason: err.to_string(),
        },
    }
}
