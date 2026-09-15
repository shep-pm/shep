//! The dogs subsystem: what a dog is, the handshake-refusal ladder, silent-dog
//! detection, shep's own narration into a dog's log, and the local restart
//! bookkeeping the daemon keeps for its plugin processes.
//!
//! Split by concern across this directory's files; this module just wires
//! them together and re-exports what the rest of the daemon calls by the old
//! `dogs::` path.

mod config;
mod contacts;
mod narrate;
mod refusals;
mod silent;
mod spec;
#[cfg(test)]
mod test_support;
mod verdict;
mod watch;

pub(crate) use narrate::{narrate, narrate_by_name};
pub(crate) use silent::silent_dogs;
// Only server.rs's handshake test names these, and it is `#[cfg(unix)]`
// because `peer_pid` answers `None` on Windows. So they go unused in a
// non-test build and in every Windows build.
#[cfg_attr(any(not(test), not(unix)), allow(unused_imports))]
pub(crate) use silent::{SilentDogs, check_silent_dogs};
pub use config::{dog_section, set_dog_section};
pub use contacts::{Contact, PeerContacts};
pub use refusals::{DogRefusals, Refusal, record_refused_dog};
pub use silent::{DOG_SILENCE_BUDGET, spawn_silent_dog_watch};
pub use spec::{DogError, DogSpec, dog_app, spawn_enabled_dogs};
pub use watch::spawn_dog_watch;
