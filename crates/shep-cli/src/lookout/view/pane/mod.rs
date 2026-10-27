//! Drawing a [`ConfigPane`](super::super::pane::ConfigPane): a title naming
//! the target, the field set's groups as section headers, one row per
//! field, and a cost column saying what changing that field would cost.
//!
//! The layout is [`super::settings`]'s: both screens own the whole body
//! between the title and the status bar, both have more rows than a
//! terminal has lines, and both pay for chrome the viewport cannot see.
//! The scroll walk is shared ([`super::scroll::to_cursor`]); the layout
//! below is this pane's own, since a field list under nine headers and a
//! settings screen with a dogs table share almost no lines.
//!
//! A sheep pane is 40 rows plus a title, nine headers and eight blank
//! separators: eighteen lines of chrome before a marker is paid for.
//!
//! `layout` holds the width budget, `field_row` one row, and `chrome`,
//! `env`, `list`, `dogs` and `panel` the regions around them. `body` decides
//! which rows fit, `draw` assembles the pane, `close` is the dialog over it.

mod body;
mod chrome;
pub(super) mod close;
mod dogs;
mod draw;
mod entry_row;
mod env;
mod field_row;
pub(super) use field_row::open_glyph;
#[cfg(test)]
mod fixtures;
mod layout;
mod list;
mod panel;

pub use draw::draw_pane;
// Named by path only from tests: `lookout::app` and `view::fixtures` both
// render a pane's rows without drawing them.
#[cfg(test)]
pub use draw::pane_lines;
// Named by path only from `view::fixtures`, which is `#[cfg(test)]`.
#[cfg(test)]
pub(super) use panel::panel_for_field;
#[cfg(test)]
pub(super) use panel::panel_lines;
