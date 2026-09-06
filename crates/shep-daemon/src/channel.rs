//! The shepherd channel's message shapes, re-exported from shep-core.
//!
//! Defined in `shep-channel`, reached here through shep-core's `protocol`
//! module. This module keeps the name shep-daemon code already uses.
//! `ChildMessage`/`ShepherdMessage` appear across the runner, the
//! supervisor, the scripted fake, and `tests/real_runner.rs`.
//!
//! Nothing is defined here. Add a variant, a field, or a fixture in
//! shep-channel.

pub use shep_core::protocol::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};
