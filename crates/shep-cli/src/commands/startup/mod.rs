//! `shep startup`/`unstartup`: installs and removes the init unit that
//! starts the shepherd at boot. [`mod@unit`] renders a unit from a
//! [`unit::UnitSpec`]; this module resolves a real one and decides whether
//! this process may install it.
//!
//! # Privilege
//!
//! shep never escalates: no `sudo`, no setuid, no re-exec. [`startup`] reads
//! `geteuid()` once as a [`Privilege`](privilege_check::Privilege); an unprivileged
//! [`install`](step_execution::install) prints the command to paste and exits non-zero.

mod install_commands;
mod privilege_check;
mod step_execution;
mod target_resolution;
#[cfg(test)]
mod testing;
pub(crate) mod unit;
mod unit_layout;
pub(crate) use step_execution::shell_quote;
pub use step_execution::{startup, unstartup};
#[cfg(test)]
pub(crate) use unit_layout::unit_path_for;
