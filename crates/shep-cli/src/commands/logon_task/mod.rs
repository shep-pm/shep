//! `shep startup`/`unstartup` on Windows: a per-user Scheduled Task with a
//! logon trigger, in place of the init unit unix writes.
//!
//! [`render`] builds the task and its step rows and compiles on every target;
//! `install` runs `schtasks.exe` and exists only on Windows.

#[cfg(windows)]
pub(crate) mod install;
mod render;
