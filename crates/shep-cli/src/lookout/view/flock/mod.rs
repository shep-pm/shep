//! The flock table: which columns fit, and what each row's cells say.
//!
//! Builds `Line`s directly rather than through `ratatui::widgets::Table`,
//! sourcing cell values from the same `crate::output::{human_bytes,
//! human_duration}` `shep flock` uses, so a number reads identically in
//! both surfaces.
//!
//! Column widths are fixed rather than measured from content: a live table
//! whose columns resize as a pid gains a digit is a table that shivers.


pub(super) mod columns;
mod facts;
mod fold;
mod layout;
mod row;

// Named by path from tests in sibling modules and nowhere else:
// `view::status` measures against `cfg_tier_width`, `output::rows` builds a
// row from `Column`, and `view::detail` draws one with `row_line`.
#[cfg(test)]
pub use columns::Column;
#[cfg(test)]
pub(super) use columns::cfg_tier_width;
pub use columns::{columns_for, fold_columns_for, fold_columns_header_line, header_line};
pub use facts::FrameFacts;
pub use fold::fold_key_line;
/// Shared with the pane's rows and the host strip, which truncate their own
/// way: `pub(in crate::lookout::view)` rather than `pub`, since nothing
/// outside the view lays out columns.
pub(in crate::lookout::view) use layout::take_columns;
pub use layout::{
    GUTTER, MIN_HEIGHT, MIN_WIDTH, fit, fit_owned, gutter, mark, pad_ground, scroll_offset,
};
pub use row::key_line;
#[cfg(test)]
pub use row::row_line;
