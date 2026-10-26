//! Signal a process group, own a Windows job and stop a child and everything
//! it started, for any process.
//!
//! It is for a process and what it started: a shepherd and its sheep, a dog
//! and its helpers, a build script and its compilers. It depends on no other
//! shep crate and on no async runtime.
//!
//! - [`Tree`] spawns a child in a group of its own (unix) or a job (Windows)
//!   and [`Tree::stop`] ends it in a ladder: a signal, a grace to exit, then a
//!   kill of the whole tree.
//! - `group::signal_group` (unix) sends one [`Signal`] to a whole process
//!   group. It and the sweep are what a future `shep signal` over lambs would
//!   use.
//! - `job::Job` (Windows) is the job object [`Tree`] uses, public for a
//!   caller that spawns by other means.
//!
//! Unix reaches a group by its leader's pid, and a descendant that calls
//! `setsid` leaves it. A Windows job cannot be left, so the kill there reaches
//! more of the tree.

#![deny(unsafe_code)]

pub mod group;
#[cfg(windows)]
pub mod job;
mod stop;

pub use group::Signal;
pub use stop::Tree;
