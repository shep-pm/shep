//! Portable RPC dispatch: verb routing, typed errors, per-call deadlines
//!
//! `dispatch` is the one function the connection layer calls per request
//! envelope. Everything here compiles and tests on every platform: no
//! `cfg(unix)`, no sockets, no bytes on a wire. [`RpcContext`] bundles the
//! daemon-wide handles a request handler may touch; `Outcome` tells the
//! caller what to do next (reply, forward bus events, or begin shutdown).
//!
//! Every envelope gets a `budget`: its own `deadline_ms`, clamped to
//! `MAX_DEADLINE_MS` so a peer cannot pin a daemon task open, or
//! `DEFAULT_DEADLINE_MS` when it sent none.

#[cfg(test)]
use core::time::Duration;

#[cfg(test)]
use shep_core::protocol::{Envelope, Lamb, ProcessInfo, Reply, RpcError};
#[cfg(test)]
use shep_core::selector::ProcessSelector;
#[cfg(test)]
use shep_core::status::ProcStatus;

#[cfg(test)]
use crate::dogs::DogSpec;
#[cfg(test)]
use crate::supervisor::{ConnId, SupervisorError};

mod context;

pub use context::{RpcContext, SavedRoll};
pub(crate) use context::{KnownDogs, Outcome};
#[cfg(test)]
use context::{DEFAULT_DEADLINE_MS, MAX_DEADLINE_MS, budget};

mod dispatch;

pub(crate) use dispatch::dispatch;
#[cfg(test)]
use dispatch::with_deadline;

mod batch;

mod enrichment;

mod error;

#[cfg(test)]
use error::rpc_error;

mod walk;

#[cfg(test)]
use walk::{ordered_walk, reload_stage_bound, restart_in_stages, walk_for};

mod selector_verbs;

#[cfg(test)]
use selector_verbs::selector_of;

#[cfg(test)]
mod tests;
