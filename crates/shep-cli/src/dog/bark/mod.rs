//! `shep dog bark`: the webhook-alert dog.
//!
//! [`sinks`] holds the webhook destinations and the delivery function;
//! [`rules`] decides which bus events and poll snapshots become a
//! [`rules::Firing`]. This module has [`BarkConfig`] and [`run_loop`],
//! which subscribes to the shepherd's bus and polls the flock.
//!
//! The bus is a `tokio::sync::broadcast`, so a lagging subscriber has
//! events dropped rather than queued, and load is when an alert matters
//! most. A dropped frame triggers an immediate poll, and [`rules::Rules`]'s
//! per-subject debounce is what lets an `Errored` seen by both routes fire
//! once.

mod config;
mod config_hot_reload;
mod dog_lifecycle;
mod firing_delivery;
pub mod rules;
pub mod sinks;
#[cfg(test)]
mod testing;
pub use config::{BarkConfig, rules_for};
pub use config_hot_reload::{ConfigSource, FlockSource};
pub use dog_lifecycle::{EventSource, Resubscribe, run_loop};
