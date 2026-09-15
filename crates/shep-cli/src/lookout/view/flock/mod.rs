//! The flock table: which columns fit, and what each row's cells say.
//!
//! Builds `Line`s directly rather than through `ratatui::widgets::Table`,
//! sourcing cell values from the same `crate::output::{human_bytes,
//! human_duration}` `shep flock` uses, so a number reads identically in
//! both surfaces.
//!
//! Column widths are fixed rather than measured from content: a live table
//! whose columns resize as a pid gains a digit is a table that shivers.
//!
//! `columns` holds the two schemas and their tier ladders, `layout` the
//! measuring every cell goes through on its way to a span, and `row` and
//! `fold` the two row renderers that sit on top of both.

pub(super) mod columns;
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
pub use fold::fold_key_line;
pub use layout::{GUTTER, MIN_HEIGHT, MIN_WIDTH, fit, gutter, mark, scroll_offset};
pub use row::key_line;
#[cfg(test)]
pub use row::row_line;
