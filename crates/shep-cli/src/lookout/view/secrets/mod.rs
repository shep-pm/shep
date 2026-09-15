//! The secrets pane, drawn straight into the buffer: this screen owns the
//! whole body between the title band and the status bar.
//!
//! Every cell goes through [`fit`](super::flock::fit), so a long key ends in `…` rather than
//! spilling into the next column. Rows carry no gap between columns, unlike
//! [`super::flock`]'s two-space-separated table: `cell` reads a column
//! back by stepping [`Column::width`](column_tiers::Column::width) alone, so a gap here would be a gap
//! `cell` never accounts for.

mod column_tiers;
mod detail_panels;
mod draw_layout;
mod pane_chrome;
mod row_cells;
#[cfg(test)]
mod testing;
pub use draw_layout::draw;
