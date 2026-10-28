//! [`ReconnectingClient`]: a connection that re-establishes itself when the
//! daemon on the other end is replaced.
//!
//! A handover carries the listening socket across `execve` but not an
//! accepted one. A dog's process therefore survives holding a dead socket.
//! A background task reconnects as soon as the connection dies. A refusing
//! successor stops the supervisor ([`LinkState::Refused`]) rather than
//! retrying. In-flight requests fail; only the connection re-establishes.
//! [`ReconnectingClient::connect_as_dog`] names the dog on every handshake
//! so a refusal is actionable.

mod client;
mod link_state;
mod reconnecting;
mod schedule;
#[cfg(test)]
mod testing;
pub use link_state::{LinkLost, LinkState, Reconnected};
pub use reconnecting::ReconnectingClient;
use schedule::next_delay;
pub use schedule::{RECONNECT_MAX_DELAY, RECONNECT_MIN_DELAY};
