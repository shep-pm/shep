//! Whole-flock handover: whether this daemon's flock can be replaced in
//! place, the [`Handover`] blob that describes it, and the flock's side of
//! the exec that carries it.
//!
//! The mechanism is `shep-handover`'s: carrying descriptors, the envelope,
//! `execve` and reaping across it. What is here is the flock's own: which
//! sheep and descriptors to carry, the blob's contents, the fitness rules
//! and adopting the flock on the other side.
//!
//! [`fitness`](fn@fitness) is the gate, and it refuses whole.

pub(crate) mod adopt;
mod blob;
mod carried;
mod exec;
mod fitness;
pub(crate) mod reap;
pub(crate) mod uptime;

#[cfg(test)]
mod fixtures;

pub use blob::{Counters, DaemonFds, Handover};
pub(crate) use carried::SheepFd;
pub use carried::{CarriedFds, CarriedSheep, DogConnection};
pub use exec::{HANDOVER_ENV, arrive, claim, exec_target, hand_over, record_launch_path};
pub use fitness::{Candidate, Fitness, OwnedCandidate, fitness};

#[allow(
    unused_imports,
    reason = "`Handover::read`'s error type, and the format number its tests name"
)]
pub use blob::{LoadError, VERSION};
#[allow(
    unused_imports,
    reason = "the payload of `Fitness::Refused`, named by this crate's own tests"
)]
pub use fitness::RefusedReason;
