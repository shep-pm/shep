//! Lifecycle verbs: `start`, `stop`, `restart`, `delete`.
//!
//! Every verb here receives an already-connected [`Client`]. `start` alone
//! resolves a target into [`AppConfig`]s before anything reaches the wire;
//! [`resolve_target`] is that resolution, kept out of the RPC so it stays
//! pure.

pub(crate) mod configure;
pub(crate) mod deadline;
pub(crate) mod operational;
pub mod resolve;
pub(crate) mod respawn;
pub(crate) mod selectors;
pub(crate) mod start;

pub(crate) use configure::{any_restart_failed, default_cwd_to_flockfile_dir};
pub(crate) use deadline::staged_start_deadline;
pub(crate) use operational::{delete, reload, restart, stock, stop};
pub(crate) use resolve::{resolve_target, target_exit_code};
pub(crate) use start::{Load, add, start};

#[cfg(test)]
mod tests;
