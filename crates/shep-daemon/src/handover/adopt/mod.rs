//! Rebuilding a successor's Rust-side objects around descriptors it did not
//! open.
//!
//! Every descriptor named here crossed an `execve`: the predecessor cleared
//! `FD_CLOEXEC` on it (see [`super::fds`]) and wrote its number into the
//! blob. Nothing here opens a file, binds a socket or creates a pipe.
//! `O_APPEND` and the pidfile's `flock` are properties of the open file
//! description, so wrapping keeps both: reopening a log would leave a
//! `copytruncate` rotator a sparse hole, and re-acquiring the lock would open
//! a window for a second daemon to win this home. A descriptor the blob names
//! and the process does not have refuses the whole rehydrate. The pidfile is
//! adopted last, so an earlier refusal leaves it open, unowned, and locked.

mod adopt_descriptors;
mod rehearse_blob;
#[cfg(test)]
mod testing;
pub use adopt_descriptors::{AdoptedSheep, adopt};
pub use rehearse_blob::{discard_blob, dry_run};
