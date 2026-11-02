//! Rebuilding a successor's Rust-side objects around descriptors it did not
//! open.
//!
//! Every descriptor named here crossed an `execve`: the predecessor carried
//! it through `shep-handover` and wrote its number into the blob. Nothing
//! here opens a file or creates a pipe, and the one bind is a `listen` socket
//! whose descriptor is unusable, made after every number is adopted. A
//! descriptor the blob names and the process does not have refuses the whole
//! rehydrate, except a `listen` socket. The pidfile is adopted after the last
//! refusal, so an earlier one leaves it open, unowned, and locked.

mod adopt_descriptors;
mod adopt_listeners;
#[cfg(test)]
mod dog_link_tests;
#[cfg(test)]
mod listener_tests;
mod rehearse_blob;
#[cfg(test)]
mod testing;
pub use adopt_descriptors::{AdoptedSheep, adopt};
pub use adopt_listeners::AdoptedListeners;
pub use rehearse_blob::{discard_blob, dry_run};
