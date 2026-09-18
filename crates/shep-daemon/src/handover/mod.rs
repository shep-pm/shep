//! Whole-flock handover: whether this daemon's flock can be replaced in
//! place, the [`Handover`] blob that describes it, and the exec that carries
//! it.
//!
//! [`fitness`](fn@fitness) is the gate, and it refuses whole. One refusal: a
//! live sheep whose log pump did not report its descriptors in time. A
//! sheep's stdout, stderr, log files, stdin pipe and shepherd channel all
//! cross the exec, per sheep rather than per app.

pub(crate) mod adopt;
mod blob;
mod carried;
mod exec;
mod fds;
mod fitness;
pub(crate) mod reap;
pub(crate) mod uptime;

#[cfg(test)]
mod fixtures;

pub use blob::{Counters, DaemonFds, Handover};
pub(crate) use carried::SheepFd;
pub use carried::{CarriedFds, CarriedSheep};
pub use exec::{HANDOVER_ENV, exec_target, hand_over, record_launch_path};
pub use fitness::{Candidate, Fitness, OwnedCandidate, fitness};

#[allow(
    unused_imports,
    reason = "`Handover::read`'s error type, and the format number its tests name"
)]
pub use blob::{LoadError, VERSION};
#[allow(
    unused_imports,
    reason = "driven directly by `dogs`'s own tests, which resolve a dog's program through it"
)]
pub(crate) use exec::resolve_target;
#[allow(
    unused_imports,
    reason = "the payload of `Fitness::Refused`, named by this crate's own tests"
)]
pub use fitness::RefusedReason;
