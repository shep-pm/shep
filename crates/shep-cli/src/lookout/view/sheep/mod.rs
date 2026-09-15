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
use super::super::pane_sheep::SheepPane;
use super::detail;

mod charts;
mod column;
mod feed;
mod layout;

use self::charts::{
    ChartTier, chart_tier, draw_charts, draw_cpu_only, draw_hairline, draw_sparkline_row,
};
use self::column::{column_top_row, draw_column, draw_divider};
use self::feed::draw_feed;

use self::layout::{FEED_WIDTH, FEED_X};

pub(crate) use self::column::column_len;
pub(crate) use self::layout::COLUMN_BODY_ROWS;
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
    use super::super::fixtures;
    use super::layout::{FULL_TIER_MIN_HEIGHT, MIN_HEIGHT_FOR_CHARTS, TERMINAL_OVERHEAD};
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
