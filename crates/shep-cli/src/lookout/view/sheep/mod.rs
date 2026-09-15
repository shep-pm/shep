//! The sheep pane: one sheep given the whole screen.
//!
//! [`view::draw`](super::draw) hands this module the whole body between the
//! title band and the status bar, never a sub-rect. [`draw`] spends it on the
//! identity band, the two charts, and below them the read-only config column
//! beside the bleats feed.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::app::{App, RowKey};
use super::super::pane_bleats::{BleatsPane, Filters, MatchKind};
use super::super::pane_sheep::SheepPane;
use super::super::tail::{Stream, TailLine};
use super::super::theme::Palette;
use super::detail;
use super::detail::chip_text;
use super::flock::fit;

mod charts;
mod column;

use self::charts::{
    ChartTier, chart_tier, draw_charts, draw_cpu_only, draw_hairline, draw_sparkline_row,
};
use self::column::{column_top_row, draw_column, draw_divider};

pub(crate) use self::column::column_len;

/// Left gutter width, in cells, for both charts: room for a label like
/// `100%` or `52.0M` plus a trailing space. Shared rather than computed
/// twice, which is half of why the two charts' bodies line up
/// ([`chart_body_cells`] is the other half).
const GUTTER: usize = 8;

/// Right margin width, in cells, decision 8's own arithmetic reserves for
/// the ceiling label and the axis's own overrun. Not drawn into directly:
/// callers append margin text to a line at whatever length it needs, and
/// this only feeds [`chart_body_cells`].
const MARGIN: usize = 12;

/// The CPU section header's row, relative to `area`.
const CPU_HEADER_ROW: u16 = 1;
/// The CPU chart's first row, relative to `area`.
const CPU_CHART_ROW: u16 = 2;
/// The CPU chart's row count: 16 half-steps in 8 rows.
const CPU_ROWS: usize = 8;
/// The memory section header's row, relative to `area`.
const MEM_HEADER_ROW: u16 = 10;
/// The memory chart's first row, relative to `area`.
const MEM_CHART_ROW: u16 = 11;
/// The memory chart's row count.
const MEM_ROWS: usize = 5;
/// The shared x axis's row, relative to `area`, `now` ending on its last
/// column.
const AXIS_ROW: u16 = 16;
/// The full-width hairline rule's row, relative to `area`, between the axis
/// and the column headers.
const HAIRLINE_ROW: u16 = AXIS_ROW + 1;
/// Terminal rows [`view::draw`](super::draw) spends outside this pane's
/// body: the title band above it and the status bar below it
/// (`view/mod.rs`). An operator counts terminal rows, and every doc that
/// repeats decision 8's row ladder states its thresholds that way, but
/// `area.height` here is always this many short of that count. [`chart_tier`]
/// adds it back before comparing against [`MIN_HEIGHT_FOR_CHARTS`] and
/// [`FULL_TIER_MIN_HEIGHT`], both stated in terminal rows below, rather than
/// leaving those two constants quietly meaning body rows.
const TERMINAL_OVERHEAD: u16 = 2;

/// The shortest terminal height any chart tier draws into at all: 21 rows
/// (`18 + 1 + 2`), one past [`COLUMN_HEADER_ROW`] plus [`TERMINAL_OVERHEAD`],
/// rather than decision 8's own "under 20 rows" floor exactly. The extra row
/// is deliberate: it keeps this constant derived from [`COLUMN_HEADER_ROW`]
/// instead of restated as a bare 20, and that coupling is what prevents the
/// config column and the chart tier from claiming the same row. Below it the
/// pane still opens; the charts just stay blank and [`column_top_row`] moves
/// the config and feed columns up to reclaim the rows the charts would have
/// used, rather than the all-or-nothing gate this constant named before
/// this task.
const MIN_HEIGHT_FOR_CHARTS: u16 = COLUMN_HEADER_ROW + 1 + TERMINAL_OVERHEAD;

/// The terminal height past which the full two-chart body has room for the
/// memory chart's own five rows on top of the CPU chart's own eight.
/// Below it, [`chart_tier`] downgrades [`ChartTier::Full`] to
/// [`ChartTier::CpuOnly`] regardless of width, per decision 8's "under 26
/// rows the memory chart goes."
const FULL_TIER_MIN_HEIGHT: u16 = 26;

/// The config/env column's own header row, relative to `area`.
const COLUMN_HEADER_ROW: u16 = 18;
/// The column's first body row.
const COLUMN_FIRST_ROW: u16 = 19;
/// The column's last body row.
const COLUMN_LAST_ROW: u16 = 44;
/// How many body rows the column draws: [`COLUMN_FIRST_ROW`] through
/// [`COLUMN_LAST_ROW`], inclusive.
pub(crate) const COLUMN_BODY_ROWS: usize = (COLUMN_LAST_ROW - COLUMN_FIRST_ROW + 1) as usize;
/// The column's own width, left of the divider Task 10's feed sits after.
const COLUMN_WIDTH: u16 = 76;
/// The KEY cell within the column: wide enough for `exp_backoff_restart_delay`
/// (25 characters) plus its `!` flag (26), pinned by
/// `the_longest_pending_field_name_is_not_truncated` rather than trusted
/// from this comment alone. Matches `view::pane`'s own `KEY_W`, not rounded
/// down: the column is narrower than that pane's own body, but the longest
/// name is the same schema's, so shrinking this cell would truncate it
/// regardless of how much room the rest of the row has.
const COLUMN_NAME_W: u16 = 26;
/// The design-size `area.height` every full-design fixture in this
/// module's own tests builds its `area` at. No longer read by [`draw`]
/// itself: [`column_top_row`] decides whether and where the column draws
/// now, so this is test-only.
#[cfg(test)]
const MIN_HEIGHT_FOR_COLUMN: u16 = COLUMN_LAST_ROW + 1;

/// The divider column between the config/env column and the feed, relative
/// to `area`: one cell past [`COLUMN_WIDTH`], drawn its own full height
/// rather than folded into either side's own width.
const DIVIDER_COL: u16 = COLUMN_WIDTH;
/// The feed's own first column, relative to `area`: one past the divider.
const FEED_X: u16 = DIVIDER_COL + 1;
/// The feed's own width in cells: `160 - 76 - 1`, the same arithmetic
/// [`COLUMN_WIDTH`]'s own doc gives for the divider.
const FEED_WIDTH: u16 = 83;

/// `top` (the same row [`draw_column`] and [`draw_divider`] were handed)
/// through [`COLUMN_BODY_ROWS`] rows below it, right of the divider: the
/// header, then the window's newest surviving lines that fit, oldest at
/// the top.
///
/// Reads [`SheepPane::feed_sheep`]'s tail through [`App::feed`], never
/// [`App::selected`]: the same rule [`draw`]'s own identity band, charts and
/// config column already follow, now enforced one level up too, in
/// [`App::feed_row`] itself: `app.feed()` already answers with the pinned
/// sheep's own lines while this pane is open, not the dashboard's selection.
///
/// No scrolling: unlike [`super::bleats_full::draw`], this column has no
/// cursor of its own to page through, so a line past what fits is only
/// counted by the header's own `N earlier` clause, never drawn.
fn draw_feed(
    app: &App,
    pane: &SheepPane,
    top: u16,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let feed = pane.feed();
    let tail = app.feed();
    let levels = app.feed_classifier();
    // Filtered once for both halves. The header counts what the body does
    // not show, so computing it twice is how the two come to disagree, and
    // it also compiled the classifier's regexes a second time per frame.
    let survivors = feed.visible(&tail.lines, &levels);
    let hidden = survivors.len().saturating_sub(COLUMN_BODY_ROWS);
    write_feed_row(buffer, area, top, &feed_header_line(feed, hidden, palette));

    for (i, line) in survivors.iter().skip(hidden).enumerate() {
        write_feed_row(buffer, area, top + 1 + i as u16, &feed_line(line, palette));
    }
}

/// One feed line: the stream tag, muted the same way
/// [`super::bleats::feed_lines`] draws it (stderr is most runtimes' default,
/// not `--bark`), then the text, truncated rather than wrapped, since this
/// column has no row budget to spend on a second line for one that
/// overruns.
fn feed_line(line: &TailLine, palette: Palette) -> Line<'static> {
    let tag = match line.stream {
        Stream::Out => "out",
        Stream::Err => "err",
    };
    Line::from(vec![
        Span::styled(format!("{tag}  "), palette.muted()),
        Span::raw(fit(&line.text, FEED_WIDTH.saturating_sub(5))),
    ])
}

/// The feed's header row: the `BLEATS` chip, then `out then err`, then how
/// many of the window's surviving lines this column has no room to show,
/// then a bracketed chip per filter axis currently set, then `/ narrow`,
/// naming the one key this row does not otherwise spell out.
fn feed_header_line(feed: &BleatsPane, hidden: usize, palette: Palette) -> Line<'static> {
    let chip = chip_text("BLEATS");
    let chip_width = u16::try_from(chip.chars().count() + 1).unwrap_or(FEED_WIDTH);
    let budget = FEED_WIDTH.saturating_sub(chip_width);
    Line::from(vec![
        Span::styled(chip, palette.band(crate::vocabulary::Role::Butter)),
        Span::raw(" "),
        Span::styled(
            fit(&feed_header_text(feed, hidden), budget),
            palette.muted(),
        ),
    ])
}

/// [`feed_header_line`]'s own sentence, built separately so a test can pin
/// its wording without rendering a [`Line`] back into a string.
fn feed_header_text(feed: &BleatsPane, hidden: usize) -> String {
    let mut parts = vec!["out then err".to_string()];
    if hidden > 0 {
        parts.push(format!("{hidden} earlier"));
    }
    for chip in feed_chip_labels(feed.filters()) {
        parts.push(format!("[{chip}]"));
    }
    parts.push("/ narrow".to_string());
    parts.join(" \u{b7} ")
}

/// One chip's text per filter axis currently set on the embedded feed, in
/// field order. Deliberately its own, smaller vocabulary rather than
/// `view::bleats_full`'s private `chip_labels`: this row has 83 cells for
/// the whole sentence, not a dedicated filter row underneath it. The match
/// axis's suffix is the one exception, shared as [`MatchKind::chip_suffix`].
fn feed_chip_labels(filters: &Filters) -> Vec<String> {
    let mut chips = Vec::new();
    if let Some(stream) = filters.stream {
        chips.push(match stream {
            Stream::Out => "out only".to_string(),
            Stream::Err => "err only".to_string(),
        });
    }
    if let Some(min) = filters.min_level {
        chips.push(format!("level\u{2265}{min}"));
    }
    if let Some(text) = filters.match_text() {
        let suffix = filters.match_kind().map_or("", MatchKind::chip_suffix);
        chips.push(format!("match {text}{suffix}"));
    }
    chips
}

/// Writes one already-styled line into `buffer`, `row` cells below `area`'s
/// own top and [`FEED_X`] cells right of its own left edge, clipped to
/// [`FEED_WIDTH`].
fn write_feed_row(buffer: &mut Buffer, area: Rect, row: u16, line: &Line<'static>) {
    if row >= area.height {
        return;
    }
    buffer.set_line(area.x + FEED_X, area.y + row, line, FEED_WIDTH);
}

/// Draws the pane into `area`: the identity band on its first row, the two
/// histories over rows `CPU_HEADER_ROW` through `AXIS_ROW`, then the
/// read-only config column and the bleats feed either side of
/// `DIVIDER_COL`.
///
/// `area` is the whole pane body, under the title band
/// [`view::draw`](super::draw) already painted and over the status bar it
/// paints after this returns, not a sub-rect of either, the way every other
/// full-screen pane's own `draw` is handed one.
pub fn draw(app: &App, pane: &SheepPane, area: Rect, buffer: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let palette = app.palette();
    // `App::sheep_pane_row`, not `App::selected_row`: the pinned sheep, not
    // the selection. `Msg::Snapshot` reseats the selection whatever screen
    // is showing, so a pane pinned to a sheep that then leaves the flock
    // would otherwise draw its neighbour's facts under a band still naming
    // the first, and the charts below it would draw its neighbour's
    // history under the same wrong name.
    let sheep_row = app.sheep_pane_row();
    let mut spans = match sheep_row {
        Some(row) => detail::identity_line(app, row, area.width, palette).spans,
        None => {
            let RowKey::Sheep(id) = pane.sheep() else {
                unreachable!("a sheep pane is only ever opened on `RowKey::Sheep`")
            };
            vec![Span::styled(
                // Same phrasing `view::bleats_full`'s own title uses for the
                // same case: one sentence for "this sheep is gone", not two.
                format!("sheep {id}: it is no longer in the flock"),
                palette.muted(),
            )]
        }
    };
    // Only the read-only pane's own pending count, not
    // `ProcessInfo::pending`'s: `detail_lines` already folds that one into
    // its own `cfg` cell, and this is the confirmation that the config this
    // pane is showing agrees with it, once the read has landed.
    if let Some(pending) = pane
        .config()
        .map(|view| view.pending.len())
        .filter(|count| *count > 0)
    {
        spans.push(Span::styled(
            format!("   !{pending} pending"),
            app.palette().attention(),
        ));
    }
    let line = Line::from(spans);
    buffer.set_line(area.x, area.y, &line, area.width);

    // Rows `CPU_HEADER_ROW` to `AXIS_ROW`, however much of them the current
    // tier draws, on the pinned sheep only. A sheep that has left the flock
    // has no history left either (`App::cpu_history`'s own doc says
    // `record_samples` drops it), so there is nothing to chart once
    // `sheep_row` is `None`.
    if let Some(row) = sheep_row {
        let tier = chart_tier(area.width, area.height);
        match tier {
            ChartTier::Full => {
                draw_charts(app, row.info.id, row.info.max_memory, area, buffer, palette);
            }
            ChartTier::CpuOnly => {
                draw_cpu_only(app, row.info.id, row.info.max_memory, area, buffer, palette);
            }
            ChartTier::Sparkline => {
                draw_sparkline_row(app, &row.info, area, buffer, palette);
            }
            ChartTier::None => {}
        }
        // Only the two tiers that draw an axis at `AXIS_ROW` have a rule to
        // draw under it: `Sparkline` and `None` never reach that row.
        if matches!(tier, ChartTier::Full | ChartTier::CpuOnly) {
            draw_hairline(area, buffer, palette);
        }
    }
    // The config and env column, starting at `top`: `COLUMN_HEADER_ROW`
    // whenever the terminal still reaches it, or right below the identity
    // band once the charts above have stopped drawing entirely. It gives
    // ground last: nothing about its own gate depends on width, since
    // `write_column_row` already clips every row to `COLUMN_WIDTH`.
    let top = column_top_row(area.height);
    if area.height > top {
        draw_column(pane, top, area, buffer, palette);
    }
    // The same rows, right of the divider: the bleats feed, embedded rather
    // than a strip of its own. Gated on `area.width` reaching past
    // `FEED_WIDTH`'s own room, not on `sheep_row`: a sheep that has left the
    // flock still draws a feed, the same "no longer read" header
    // `view::bleats_full`'s own title states for the full-screen pane, once
    // `App::feed_row` next answers `None` and the tail this reads goes
    // empty.
    if area.height > top && usize::from(area.width) >= usize::from(FEED_X + FEED_WIDTH) {
        draw_divider(top, area, buffer, palette);
        draw_feed(app, pane, top, area, buffer, palette);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::super::app::{Body, Control, KeyPress, Msg};
    use super::super::super::frames::render_text;
    use super::super::super::level::{Classifier, Level};
    use super::super::super::tail::Tail;
    use super::super::fixtures;
    use super::*;

    /// The bug the reviewer reproduced: pane pinned to sheep 1, sheep 1
    /// deleted, `pane.sheep()` still reads `Sheep(1)` while `app.selected()`
    /// has moved to `Sheep(2)` (`alpha`'s alphabetical neighbour, `bravo`,
    /// the only row left once the reseat runs). Reading `App::selected_row`
    /// here would draw `bravo`'s facts under a band still naming `alpha`.
    #[test]
    fn the_band_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, 80, 3);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("bravo"),
            "must not draw the sheep that replaced the pinned one: {text:?}"
        );
        assert!(
            text.contains("sheep 1: it is no longer in the flock"),
            "got {text:?}"
        );
    }

    /// `draw_feed` draws whatever `App::feed` holds, filtered through the
    /// pane's own pinned `BleatsPane`, and never reaches for `App::selected`
    /// itself: this pins that half. It does *not* pin `App::feed_row`'s own
    /// scoping to the pane's pinned sheep rather than the reseated
    /// selection: a fixture with no polling loop cannot re-fetch the tail
    /// after the reseat below, so the tail this test asserts on is exactly
    /// the one `Msg::Bleats` injected before it, whatever `feed_row` would
    /// answer now. `the_feed_row_follows_the_sheep_panes_own_pinned_sheep_when_the_selection_moves`
    /// in `app.rs` is what pins that half; a mutation removing `feed_row`'s
    /// own `Body::Sheep` branch left this test green.
    ///
    /// The area is 160x46, at [`FEED_X`] plus [`FEED_WIDTH`] and
    /// [`MIN_HEIGHT_FOR_COLUMN`] exactly: `draw`'s own gate skips
    /// `draw_feed` below either floor, and a narrower or shorter area would
    /// pass this test whether or not `draw_feed` ever ran, the same way a
    /// too-small area let Task 8's own chart bug through once.
    #[test]
    fn the_feed_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        app.update(Msg::Bleats {
            tail: Tail {
                lines: vec![TailLine {
                    stream: Stream::Out,
                    text: "alpha wrote this".to_string(),
                }],
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            text.contains("alpha wrote this"),
            "the pinned sheep's own line must still draw: {text:?}"
        );
    }

    /// [`feed_header_text`]'s own chip: `[level≥warn]`, the same word
    /// [`Level`]'s own doc says the filter row
    /// renders it as, once a minimum is set on the embedded feed.
    #[test]
    fn the_headers_level_chip_names_the_minimum() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_min_level(Some(Level::Warn));
        let text = feed_header_text(&feed, 0);
        assert!(text.contains("[level≥warn]"), "got {text:?}");
        assert!(text.contains("/ narrow"), "got {text:?}");
    }

    /// A regex matcher's chip names itself a regex, the same suffix
    /// `bleats_full.rs`'s own `chip_labels` carries: an operator who
    /// narrows the embedded feed with `/…/` needs the same tell this
    /// column's full-screen twin already gives.
    #[test]
    fn the_headers_match_chip_names_a_regex() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("/po+l/".to_string());
        let text = feed_header_text(&feed, 0);
        assert!(text.contains("match /po+l/ (regex)"), "got {text:?}");
    }

    /// A literal matcher's chip is the typed text and nothing after it.
    /// The two assertions around this one read a suffix they expect, so
    /// neither can see a suffix that should not be there; this one anchors
    /// on the brackets `feed_header_text` draws around each chip, which
    /// anything appended would fall outside of.
    #[test]
    fn the_headers_match_chip_carries_nothing_after_a_literal() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("pool".to_string());
        let text = feed_header_text(&feed, 0);
        assert!(text.contains("[match pool]"), "got {text:?}");
    }

    /// A pattern that fails to compile says so on the chip, rather than the
    /// embedded feed just going quiet with no explanation until the
    /// operator presses `b` to reach the full-screen pane's own chip.
    #[test]
    fn the_headers_match_chip_names_an_invalid_regex() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("/pool(/".to_string());
        let text = feed_header_text(&feed, 0);
        assert!(
            text.contains("invalid regex, matches nothing"),
            "got {text:?}"
        );
    }

    /// The `N earlier` clause counts survivors the body has no room to show,
    /// never the raw line count: with `COLUMN_BODY_ROWS` rows to draw into
    /// and one more line than that, exactly one line is hidden.
    #[test]
    fn the_headers_earlier_clause_counts_hidden_survivors_not_raw_lines() {
        let feed = BleatsPane::new(RowKey::Sheep(1));
        let tail = Tail {
            lines: (0..COLUMN_BODY_ROWS + 1)
                .map(|n| TailLine {
                    stream: Stream::Out,
                    text: format!("line-{n}"),
                })
                .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 0,
            note: None,
        };
        let hidden = feed
            .visible(&tail.lines, &Classifier::new(&[]))
            .len()
            .saturating_sub(COLUMN_BODY_ROWS);
        let text = feed_header_text(&feed, hidden);
        assert!(text.contains("1 earlier"), "got {text:?}");
    }

    /// The feed shows the window's newest lines, oldest at the top, and
    /// truncates a line too long for [`FEED_WIDTH`] rather than wrapping it
    /// onto a second row: this column has no row budget to spend on one.
    #[test]
    fn the_feed_shows_the_newest_lines_and_truncates_a_long_one() {
        let mut app = fixtures::app_with(
            vec![ProcessInfo::builder(1, "alpha", ProcStatus::Online).build()],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Bleats {
            tail: Tail {
                lines: (0..COLUMN_BODY_ROWS + 5)
                    .map(|n| TailLine {
                        stream: Stream::Out,
                        text: format!("line-{n}"),
                    })
                    .chain(std::iter::once(TailLine {
                        stream: Stream::Err,
                        text: "x".repeat(200),
                    }))
                    .collect(),
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("line-0\n") && !text.contains("line-0 "),
            "the oldest lines scroll off the top: {text:?}"
        );
        assert!(
            text.contains(&format!("{}\u{2026}", "x".repeat(77))),
            "the long line truncates with an ellipsis at the column's own \
             width (78 cells: 77 characters plus the ellipsis), not before \
             it and not after: {text:?}"
        );
        assert!(
            !text.contains(&"x".repeat(78)),
            "and no further: a truncated line, not a wrapped one: {text:?}"
        );
    }

    /// A sheep pane over several real polls, cpu and rss both rising: task
    /// 4's counter differencing needs a poll to differ against, so a scene
    /// built from a single snapshot would demonstrate nothing while looking
    /// fine, the same trap `Scene::CfgDrift` was rewritten to avoid.
    fn render_at(width: u16, height: u16) -> String {
        let t0 = std::time::Instant::now();
        let mut app = App::new(
            fixtures::plain(),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(7, "web", ProcStatus::Online)
                    .cpu_ms(Some(0))
                    .memory_bytes(Some(10 << 20))
                    .max_memory(Some(64 << 20))
                    .build(),
            ],
            at: t0,
        });
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for i in 1..6u64 {
            let at = t0 + Duration::from_secs(2 * i);
            app.update(Msg::Snapshot {
                rows: vec![
                    ProcessInfo::builder(7, "web", ProcStatus::Online)
                        .cpu_ms(Some(i * 400))
                        .memory_bytes(Some((10 + i * 4) << 20))
                        .max_memory(Some(64 << 20))
                        .build(),
                ],
                at,
            });
        }
        let Body::Sheep(pane) = app.body() else {
            panic!("setup: the pane opened on web")
        };
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        render_text(&buffer)
    }

    /// Memory goes first and CPU stays: a CPU chart is the more diagnostic
    /// of the two, and memory still has a gauge to fall back on.
    #[test]
    fn below_a_hundred_and_forty_columns_only_the_cpu_chart_draws() {
        let rendered = render_at(139, 48);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(rendered.contains("rss "), "got {rendered:?}");
        assert!(
            !rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
    }

    /// Below 100 both go and the pane falls back to 1a's pair.
    #[test]
    fn below_a_hundred_columns_both_charts_become_the_sparkline_pair() {
        let rendered = render_at(99, 48);
        assert!(
            !rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(
            !rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
        assert!(rendered.contains("CPU 20s"), "got {rendered:?}");
    }

    /// The column block and the hairline rule above it, pinned against the
    /// spec's own absolute row numbering rather than against
    /// `COLUMN_HEADER_ROW` or `HAIRLINE_ROW` themselves: a transcription
    /// bug that moves a constant to match its own miscount would still
    /// pass a test that only re-reads the constant back.
    #[test]
    fn the_hairline_and_column_headers_sit_at_the_spec_s_own_rows() {
        let rendered = render_at(160, 48);
        let lines: Vec<&str> = rendered.lines().collect();
        // Spec row 17: the shared axis, `now` on the last column.
        assert!(
            lines[16].contains("now"),
            "row 16 (spec 17): {:?}",
            lines.get(16)
        );
        // Spec row 18: a full-width hairline rule, nothing else.
        let hairline = lines[17].trim_end();
        assert!(
            !hairline.is_empty() && hairline.chars().all(|c| c == '\u{2500}'),
            "row 17 (spec 18) is not a hairline rule: {:?}",
            lines.get(17)
        );
        // Spec row 19: the column headers.
        assert!(
            lines[18].contains("\u{2588}\u{2588} CONFIG & ENV"),
            "row 18 (spec 19): {:?}",
            lines.get(18)
        );
    }

    /// Rows too: the charts hold 2 to 17, and the config and feed columns
    /// are what the pane is for, so they give ground last.
    ///
    /// `render_at` builds `area` directly, so its `height` is `area.height`
    /// (body rows), not the terminal rows `MIN_HEIGHT_FOR_CHARTS` and
    /// `FULL_TIER_MIN_HEIGHT` are stated in: 20 body rows is 22 terminal
    /// rows, under `FULL_TIER_MIN_HEIGHT` (26), so the memory chart is
    /// already gone; 18 body rows is 20 terminal rows, under
    /// `MIN_HEIGHT_FOR_CHARTS` (21), so every chart is gone.
    #[test]
    fn a_short_terminal_drops_the_charts_before_the_columns() {
        assert!(!render_at(160, 20).contains("\u{2588}\u{2588} MEM"));
        assert!(render_at(160, 20).contains("\u{2588}\u{2588} CONFIG & ENV"));
        assert!(!render_at(160, 18).contains("\u{2588}\u{2588} CPU"));
        assert!(render_at(160, 18).contains("\u{2588}\u{2588} CONFIG & ENV"));
    }

    /// The Full tier's own floor: at exactly 140 columns both charts still
    /// draw. `below_a_hundred_and_forty_columns_only_the_cpu_chart_draws`
    /// pins the cell below this one; nothing pinned the boundary itself,
    /// so a one-cell-generous mutation of `width >= 140` could hold the
    /// Full tier open past its own name and every other test would stay
    /// green.
    #[test]
    fn at_a_hundred_and_forty_columns_both_charts_still_draw() {
        let rendered = render_at(140, 48);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(
            rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
    }

    /// The CpuOnly tier's own floor: at exactly 100 columns the CPU chart
    /// still draws rather than falling to the sparkline pair.
    #[test]
    fn at_a_hundred_columns_the_cpu_chart_still_draws() {
        let rendered = render_at(100, 48);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(!rendered.contains("CPU 20s"), "got {rendered:?}");
    }

    /// [`MIN_HEIGHT_FOR_CHARTS`]'s own floor: at exactly that height a
    /// chart still draws rather than the pane falling straight to
    /// [`ChartTier::None`].
    ///
    /// `render_at`'s `height` is `area.height` (body rows), while
    /// [`MIN_HEIGHT_FOR_CHARTS`] is stated in terminal rows, so
    /// [`TERMINAL_OVERHEAD`] comes back off here to land on the exact body
    /// row `chart_tier` compares against.
    #[test]
    fn at_the_chart_height_floor_a_chart_still_draws() {
        let rendered = render_at(160, MIN_HEIGHT_FOR_CHARTS - TERMINAL_OVERHEAD);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
    }

    /// [`FULL_TIER_MIN_HEIGHT`]'s own floor: at exactly that height the
    /// Full tier still holds rather than being downgraded to
    /// [`ChartTier::CpuOnly`].
    ///
    /// Same body-row/terminal-row split as
    /// `at_the_chart_height_floor_a_chart_still_draws`, above.
    #[test]
    fn at_the_full_tier_height_floor_the_memory_chart_still_draws() {
        let rendered = render_at(160, FULL_TIER_MIN_HEIGHT - TERMINAL_OVERHEAD);
        assert!(
            rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
    }
}
