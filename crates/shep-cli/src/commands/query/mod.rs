//! Query verbs: `flock_display::flock`, `secret_inspection::describe`, `flock_display::fold`, `ping`. None mutate the flock,
//! and none autostart: `main` hands each one an already-connected [`Client`](shep_client::Client).
//!
//! `secret_inspection::describe` and `flock_display::fold` share one shape (`Request::Describe` against a
//! [`SelectorSpec`](shep_core::protocol::SelectorSpec)); `flock_display::fold` supplies `SelectorSpec::Fold` directly.
//!
//! `ping` does not ask the daemon for its version and pid: the handshake
//! already answered that, in the
//! [`HelloAck`](shep_core::protocol::HelloAck) [`Client::daemon`](shep_client::Client::daemon) holds. It
//! still issues `Request::Ping` as the liveness check.

mod dog_browsing;
mod flock_display;
mod secret_inspection;
mod terminal_fitting;
#[cfg(test)]
mod testing;
pub use dog_browsing::{available_dogs, dogs};
pub use flock_display::{flock, flock_from_roll, fold};
pub(crate) use flock_display::{flock_follow, read_roll};
pub use secret_inspection::describe;
