//! `shep lookout` (alias `dash`): the terminal dashboard.
//!
//! Four panes, one screen: the flock table, a sheep detail pane
//! ([`view::detail`]) and a bleats feed ([`view::bleats`]) under the selected
//! row, and a host-usage strip ([`view::host`]) above. A narrow terminal drops
//! panes before columns; [`view::panes_for`] is the tier table.
//!
//! Two tasks talk over an `mpsc` each way: [`link::run_link`] owns the
//! connection, [`run_ui`](ui_event_loop::run_ui) the screen. A dead shepherd
//! freezes the dashboard; see [`link::RECONNECT_ATTEMPTS`].

pub mod app;
pub mod edits;
pub mod field;
// `#[cfg(test)]`: every item in `frames` is read by tests and by the gallery
// writer, and by nothing else. `pub` exempts nothing from `dead_code` here,
// since `mod lookout` in `lib.rs` is private rather than `pub mod`.
mod batch_dispatch;
mod entry;
#[cfg(test)]
pub mod frames;
pub mod input;
mod keymap;
pub mod keyvalue;
pub mod level;
pub mod link;
pub mod pane;
pub mod pane_bleats;
pub mod pane_sheep;
pub(crate) mod probe_sheep_dogs;
pub(crate) mod secrets;
pub mod source;
pub mod tail;
pub mod term;
#[cfg(test)]
mod testing;
pub mod textbox;
pub mod theme;
mod ui_event_loop;
pub mod view;
pub mod viewport;
mod wiring;
pub use entry::lookout;

use std::time::Duration;

/// The floor on the gap between two draws.
///
/// ~30 frames a second. A `shep muster` of a large flock emits a `process.*`
/// event per sheep, and this makes a burst of N events cost one draw per 33ms
/// rather than N draws. Armed only while something is dirty, so an idle
/// dashboard draws nothing at all.
pub const MIN_REDRAW: Duration = Duration::from_millis(33);
