//! Signal a process group, own a Windows job and stop a child and everything
//! it started, for any process.
//!
//! It is for a process and what it started: a shepherd and its sheep, a dog
//! and its helpers, a build script and its compilers. It depends on no other
//! shep crate and on no async runtime.
//!
//! - [`Tree`] spawns a child in a group of its own (unix) or a job (Windows)
//!   and [`Tree::stop`] ends it in a ladder: a signal, a grace, then a kill.
//! - `group::signal_group` (unix) sends one [`Signal`] to a whole group, and
//!   [`sweep`] ends what a group signal missed: the lambs a process left
//!   running once it is gone. A future `shep signal` over lambs uses both.
//! - [`sweep::Sweeper`] steps a [`sweep::LambSweep`] through a sweep, and
//!   `sweep::ProcessTable` is the real table behind it, under `sysinfo`.
//! - `job::Job` (Windows) is the job object [`Tree`] uses, public for a
//!   caller that spawns by other means.
//!
//! Unix reaches a group by its leader's pid, and a descendant that calls
//! `setsid` leaves it. A Windows job cannot be left.

#![deny(unsafe_code)]

pub mod group;
pub mod instant;
#[cfg(windows)]
pub mod job;
mod stop;
pub mod sweep;

pub use group::Signal;
pub use stop::Tree;
