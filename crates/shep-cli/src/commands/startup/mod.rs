//! `shep startup`/`step_execution::unstartup`: installs and removes the init unit that
//! starts the shepherd at boot. [`mod@unit`] renders a unit from a
//! [`unit::UnitSpec`] with no filesystem or process access; this module
//! resolves a real `unit::UnitSpec`, decides whether this process may install
//! it, and writes, enables, disables or removes it.
//!
//! # Privilege
//!
//! shep never escalates: no `sudo`, no setuid, no re-exec through a
//! helper. [`startup`] reads `geteuid()` once as a [`Privilege`](privilege_check::Privilege) value; an
//! [`install`](step_execution::install) or [`remove`](step_execution::remove) given [`Privilege::Unprivileged`](privilege_check::Privilege::Unprivileged) prints the
//! command an operator can paste and exits non-zero.

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
