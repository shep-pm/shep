//! [`ReconnectingClient`]: a connection that re-establishes itself when the daemon on the other end is replaced.
//!
//! A handover carries the listening socket across `execve` but not an accepted
//! one, so a dog's own process survives holding a dead socket. [`Client`](crate::client::Client) has
//! no such mode: the CLI's one-shot verbs must never see a request silently
//! retried, so in-flight requests here fail too, and only the connection
//! re-establishes. A background task reconnects as soon as the connection
//! dies rather than on the next request, so an idle dog still re-handshakes
//! before `shep daemon reload` polls dog staleness, and a refusing successor
//! stops the supervisor ([`LinkState::Refused`]) rather than retrying.
//! [`ReconnectingClient::connect_as_dog`] names the dog on every handshake so
//! a refusal is actionable.
//!
//! The two differ in more than retry policy. This type's swap happens in its
//! supervisor task, concurrently with `&self` requests, so a request really
//! can be in flight when the daemon is replaced and really does fail.
//! [`Client::reconnect`](crate::client::Client::reconnect) takes `&mut self`, which excludes that case instead
//! of handling it.
//!
//! That signature also decides who can call it. A dog holds a
//! [`ReconnectingClient`], which is not [`Clone`] and keeps its [`Client`](crate::client::Client)
//! behind an [`Arc`](std::sync::Arc), so `&mut Client` is out of reach: a dog waits on this
//! type's own link state instead. The callers [`Client::reconnect`](crate::client::Client::reconnect) is for
//! are the ones that own their [`Client`](crate::client::Client) outright.
//!
//! # Which daemon answered
//!
//! [`Client::reconnect`](crate::client::Client::reconnect) reports [`Reconnected::SameDaemon`] when the daemon
//! now answering carries the [`HelloAck`](shep_core::protocol::HelloAck) pid the predecessor did. A handover
//! is an `execve`, which keeps the pid, and its blob carries the flock's id
//! counter across with it, so the two facts move together: a matching pid
//! means an id minted before the drop still names the same sheep. A daemon
//! stopped and started again gets a fresh pid and a fresh id space. The gap
//! is pid reuse inside one reconnect, which nothing on the wire today could
//! tell apart.

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
