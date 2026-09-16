//! `paint_frame::draw`: one `App`, one `Frame`, six regions of arithmetic.
//!
//! No `Layout`, no `Constraint`, no widget. The upstream surface this whole
//! module touches is six items wide: `Frame::area`, `Frame::buffer_mut`,
//! `Buffer::set_line`, `Line`, `Span`, `Style`, which keeps the render path
//! both testable and cheap to keep working across a ratatui release.

pub mod bleats;
pub mod bleats_full;
pub mod cell;
pub mod detail;
pub mod flock;
pub mod host;
mod keymap;
pub mod link_panel;
mod overlay;
pub mod pane;
pub mod scroll;
pub mod secrets;
pub mod settings;
pub mod sheep;
pub mod status;
// `pub`, not private: a test in `super::super`'s own `mod tests` (it drives
// `run_ui`) needs `fixtures::sample()` from here. `#[cfg(test)]` still keeps
// every item out of the ordinary build.
#[cfg(test)]
pub mod fixtures;
mod layout_budget;
mod paint_frame;
#[cfg(test)]
mod testing;
/// The narrowest terminal the dashboard draws into.
///
/// The table's own floor ([`flock::MIN_WIDTH`], 31) plus the selection
/// marker's gutter ([`flock::GUTTER`], 2). Below this the whole draw becomes
/// two short lines saying so.
pub const MIN_TERM_WIDTH: u16 = flock::MIN_WIDTH + flock::GUTTER;

pub use layout_budget::{body_rows, panes_for};
pub use paint_frame::draw;
