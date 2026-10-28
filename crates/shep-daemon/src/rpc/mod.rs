//! Portable RPC dispatch: verb routing, typed errors, per-call deadlines
//!
//! `dispatch` is the one function the connection layer calls per request
//! envelope. Everything here compiles and tests on every platform: no
//! `cfg(unix)`, no sockets, no bytes on a wire. [`RpcContext`] bundles the
//! daemon-wide handles a request handler may touch; `Outcome` tells the
//! caller what to do next (reply, forward bus events, or begin shutdown).
//!
//! Every envelope gets a `budget`: its own `deadline_ms`, clamped to
//! `MAX_DEADLINE_MS` so a peer cannot pin a daemon task open, or `DEFAULT_DEADLINE_MS`.

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

#[cfg(test)]
use context::{DEFAULT_DEADLINE_MS, MAX_DEADLINE_MS, budget};
pub(crate) use context::{KnownDogs, Outcome};
pub use context::{RpcContext, SavedRoll};

mod dispatch;

mod gate;

pub(crate) use gate::{check_carried, kind_of, spawn_checked_forwarder};

pub(crate) use dispatch::dispatch;
#[cfg(test)]
use dispatch::with_deadline;

mod batch;

mod dog_secrets;

mod enrichment;

mod error;

mod log_tail;

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
