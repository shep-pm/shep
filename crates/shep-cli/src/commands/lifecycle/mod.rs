//! Lifecycle verbs: `start`, `stop`, `restart`, `delete`.
//!
//! Every verb here receives an already-connected
//! [`Client`](shep_client::Client), except [`remove::delete`], which owns its
//! connects because a dog's removal must work against a `$SHEP_HOME` with no
//! shepherd running. `start` alone resolves a target into
//! [`AppConfig`](shep_core::config::AppConfig)s before anything reaches the
//! wire; [`resolve_target`] is that resolution, kept out of the RPC so it
//! stays pure.

pub(crate) mod configure;
pub(crate) mod deadline;
pub(crate) mod operational;
pub(crate) mod remove;
pub mod resolve;
pub(crate) mod respawn;
pub(crate) mod selectors;
pub(crate) mod start;
pub(crate) mod toggle;

pub(crate) use configure::{any_restart_failed, default_cwd_to_flockfile_dir};
pub(crate) use deadline::staged_start_deadline;
pub(crate) use operational::{reload, reset, restart, stock, stop};
pub(crate) use remove::delete;
pub(crate) use resolve::{resolve_target, target_exit_code};
pub(crate) use start::{Load, add, reads_this_machine, refuse_local_read, start, start_on_host};

#[cfg(test)]
mod tests;
