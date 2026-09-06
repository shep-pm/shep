//! Speak the shep shepherd channel: signal readiness, emit a metric,
//! answer an action. `docs/shepherd-channel.md` has the contract.
//!
//! With no channel, nothing is sent and no handler runs. Registering one
//! still succeeds, so an app needs no branch at its call sites.
//!
//! ```
//! let shepherd = shep_channel::serve();
//!
//! shepherd.on_action("gc", |params, _name| {
//!     format!("collected, params={params:?}")
//! });
//! shepherd.on_shutdown(|| { /* stop gracefully */ });
//!
//! // No channel here: this test runs outside shep. See `is_active`.
//! assert!(!shepherd.is_active());
//! shepherd.metric("rps", 4200.0);
//! shepherd.ready().unwrap();
//! ```
//!
//! - A slow handler delays the next message: the reader thread runs
//!   handlers itself. `action_timeout` (default 3s) sets the budget.
//! - `metric` drops a sample under backpressure. `ready` and a reply block
//!   for room instead, since losing either is worse.
//! - An unhandled shutdown warns on stderr. This crate never stops a
//!   process on its own.

#![doc(test(attr(deny(warnings))))]
// Not `forbid`: `endpoint` needs one `unsafe` block per platform.
// One takes the inherited descriptor on unix; the other peeks a
// Windows pipe's buffer. Each carries its own `// SAFETY:` comment.
#![deny(unsafe_code)]

#[cfg(feature = "client")]
mod channel;
#[cfg(feature = "client")]
mod dispatch;
#[cfg(feature = "client")]
mod endpoint;
#[cfg(feature = "client")]
mod error;
#[cfg(feature = "client")]
mod outbox;
#[cfg(feature = "client")]
mod serve;
#[cfg(feature = "client")]
mod session;
mod wire;

#[cfg(feature = "client")]
pub use channel::Channel;
#[cfg(feature = "client")]
pub use dispatch::{ActionHandler, ShutdownHandler};
#[cfg(feature = "client")]
pub use endpoint::{Endpoint, FD_VAR, PIPE_VAR, VERSION_VAR, discover};
#[cfg(feature = "client")]
pub use error::ChannelError;
#[cfg(feature = "client")]
pub use serve::{Shepherd, serve};
pub use wire::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};
