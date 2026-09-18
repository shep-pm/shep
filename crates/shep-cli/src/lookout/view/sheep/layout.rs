//! Decision 8's row ladder, and the widths either side of the divider.
//!
//! One file because it is one ladder. The charts hold rows 1 to 17, the
//! hairline 18, the config column 19 to 44, and
//! [`MIN_HEIGHT_FOR_CHARTS`] is derived from [`COLUMN_HEADER_ROW`] rather
//! than restated as a bare number precisely so the two halves cannot
//! quietly come to claim the same row. Splitting the ladder across the
//! modules that read it would lose that.
//!
//! Everything here is `pub(super)`, which reaches every module under
//! `sheep`. The one exception is [`COLUMN_BODY_ROWS`], which
//! `lookout::pane_sheep` reads by path to size its own viewport.

/// Left gutter width, in cells, for both charts: room for a label like
/// `100%` or `52.0M` plus a trailing space. Shared rather than computed
/// twice, which is half of why the two charts' bodies line up
/// ([`chart_body_cells`](super::charts::chart_body_cells) is the other half).
pub(super) const GUTTER: usize = 8;

/// Right margin width, in cells, decision 8's own arithmetic reserves for
/// the ceiling label and the axis's own overrun. Not drawn into directly:
/// callers append margin text to a line at whatever length it needs, and
/// this only feeds [`chart_body_cells`](super::charts::chart_body_cells).
pub(super) const MARGIN: usize = 12;

/// The CPU section header's row, relative to `area`.
pub(super) const CPU_HEADER_ROW: u16 = 1;
/// The CPU chart's first row, relative to `area`.
pub(super) const CPU_CHART_ROW: u16 = 2;
/// The CPU chart's row count: 16 half-steps in 8 rows.
pub(super) const CPU_ROWS: usize = 8;
/// The memory section header's row, relative to `area`.
pub(super) const MEM_HEADER_ROW: u16 = 10;
/// The memory chart's first row, relative to `area`.
pub(super) const MEM_CHART_ROW: u16 = 11;
/// The memory chart's row count.
pub(super) const MEM_ROWS: usize = 5;
/// The shared x axis's row, relative to `area`, `now` ending on its last
/// column.
pub(super) const AXIS_ROW: u16 = 16;
/// The full-width hairline rule's row, relative to `area`, between the axis
/// and the column headers.
pub(super) const HAIRLINE_ROW: u16 = AXIS_ROW + 1;
/// Terminal rows [`view::draw`](super::super::draw) spends outside this pane's
/// body: the title band above it and the status bar below it
/// (`view/mod.rs`). An operator counts terminal rows, and every doc that
/// repeats decision 8's row ladder states its thresholds that way, but
/// `area.height` here is always this many short of that count.
/// [`chart_tier`](super::charts::chart_tier)
/// adds it back before comparing against [`MIN_HEIGHT_FOR_CHARTS`] and
/// [`FULL_TIER_MIN_HEIGHT`], both stated in terminal rows below, rather than
/// leaving those two constants quietly meaning body rows.
pub(super) const TERMINAL_OVERHEAD: u16 = 2;

/// The shortest terminal height any chart tier draws into at all: 21 rows
/// (`18 + 1 + 2`), one past [`COLUMN_HEADER_ROW`] plus [`TERMINAL_OVERHEAD`],
/// rather than decision 8's own "under 20 rows" floor exactly. The extra row
/// is deliberate: it keeps this constant derived from [`COLUMN_HEADER_ROW`]
/// instead of restated as a bare 20, and that coupling is what prevents the
/// config column and the chart tier from claiming the same row. Below it the
/// pane still opens; the charts just stay blank and
/// [`column_top_row`](super::column::column_top_row) moves
/// the config and feed columns up to reclaim the rows the charts would have
/// used, rather than the all-or-nothing gate this constant named before
/// this task.
pub(super) const MIN_HEIGHT_FOR_CHARTS: u16 = COLUMN_HEADER_ROW + 1 + TERMINAL_OVERHEAD;

/// The terminal height past which the full two-chart body has room for the
/// memory chart's own five rows on top of the CPU chart's own eight.
/// Below it, [`chart_tier`](super::charts::chart_tier) downgrades
/// [`ChartTier::Full`](super::charts::ChartTier::Full) to
/// [`ChartTier::CpuOnly`](super::charts::ChartTier::CpuOnly) regardless of
/// width, per decision 8's "under 26
/// rows the memory chart goes."
pub(super) const FULL_TIER_MIN_HEIGHT: u16 = 26;

/// The config/env column's own header row, relative to `area`.
pub(super) const COLUMN_HEADER_ROW: u16 = 18;
/// The column's first body row.
pub(super) const COLUMN_FIRST_ROW: u16 = 19;
/// The column's last body row.
pub(super) const COLUMN_LAST_ROW: u16 = 44;
/// How many body rows the column draws: [`COLUMN_FIRST_ROW`] through
/// [`COLUMN_LAST_ROW`], inclusive.
pub(crate) const COLUMN_BODY_ROWS: usize = (COLUMN_LAST_ROW - COLUMN_FIRST_ROW + 1) as usize;
/// The column's own width, left of the divider Task 10's feed sits after.
pub(super) const COLUMN_WIDTH: u16 = 76;
/// The KEY cell within the column: wide enough for `exp_backoff_restart_delay`
/// (25 characters) plus its `!` flag (26), pinned by
/// `the_longest_pending_field_name_is_not_truncated` rather than trusted
/// from this comment alone. Matches `view::pane`'s own `KEY_W`, not rounded
/// down: the column is narrower than that pane's own body, but the longest
/// name is the same schema's, so shrinking this cell would truncate it
/// regardless of how much room the rest of the row has.
pub(super) const COLUMN_NAME_W: u16 = 26;
/// The design-size `area.height` every full-design fixture in this
/// module's own tests builds its `area` at. No longer read by
/// [`draw`](super::draw)
/// itself: [`column_top_row`](super::column::column_top_row) decides whether
/// and where the column draws
/// now, so this is test-only.
#[cfg(test)]
pub(super) const MIN_HEIGHT_FOR_COLUMN: u16 = COLUMN_LAST_ROW + 1;

/// The divider column between the config/env column and the feed, relative
/// to `area`: one cell past [`COLUMN_WIDTH`], drawn its own full height
/// rather than folded into either side's own width.
pub(super) const DIVIDER_COL: u16 = COLUMN_WIDTH;
/// The feed's own first column, relative to `area`: one past the divider.
pub(super) const FEED_X: u16 = DIVIDER_COL + 1;
/// The feed's own width in cells: `160 - 76 - 1`, the same arithmetic
/// [`COLUMN_WIDTH`]'s own doc gives for the divider.
pub(super) const FEED_WIDTH: u16 = 83;
