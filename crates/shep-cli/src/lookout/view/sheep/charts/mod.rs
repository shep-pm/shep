//! The two histories over rows `CPU_HEADER_ROW` to `AXIS_ROW`: which of
//! them a terminal this size has room for, and the rows they draw.
//!
//! [`chart_tier`] decides; [`draw_charts`], [`draw_cpu_only`] and
//! [`draw_sparkline_row`] are the three answers. [`rows`] holds the row
//! content each of them writes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::lookout::app::{App, HISTORY};
use crate::lookout::theme::Palette;

use super::super::{cell, flock};
use super::layout::{
    AXIS_ROW, CPU_CHART_ROW, CPU_HEADER_ROW, FULL_TIER_MIN_HEIGHT, GUTTER, HAIRLINE_ROW, MARGIN,
    MEM_CHART_ROW, MEM_HEADER_ROW, MIN_HEIGHT_FOR_CHARTS, TERMINAL_OVERHEAD,
};

mod rows;

use self::rows::{axis_row, cpu_chart_rows, cpu_header_text, mem_chart_rows, mem_header_text};

/// Which of the sheep pane's own charts fit `width` and `height`, decision
/// 8's own ladder: both charts at 140 columns and [`FULL_TIER_MIN_HEIGHT`]
/// rows, the CPU chart alone with a one-line memory summary from 100
/// columns, a single sparkline-and-gauge row below that (down to
/// [`flock::MIN_WIDTH`], the whole app's own floor, refused before this
/// pane ever opens), and nothing at all under [`MIN_HEIGHT_FOR_CHARTS`]
/// rows regardless of width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChartTier {
    /// Both charts, full height: [`draw_charts`].
    Full,
    /// The CPU chart and a one-line memory summary: [`draw_cpu_only`].
    CpuOnly,
    /// 1a's own CPU sparkline and memory gauge, one row: [`draw_sparkline_row`].
    Sparkline,
    /// No chart content at all; the row belongs to the config column instead.
    None,
}

pub(super) fn chart_tier(width: u16, height: u16) -> ChartTier {
    // `height` is `area.height`, the pane body's own row count.
    // [`MIN_HEIGHT_FOR_CHARTS`] and [`FULL_TIER_MIN_HEIGHT`] are both
    // stated in terminal rows, the count an operator actually reads off
    // their own terminal, so [`TERMINAL_OVERHEAD`] is added back before
    // either comparison.
    let terminal_height = height + TERMINAL_OVERHEAD;
    if terminal_height < MIN_HEIGHT_FOR_CHARTS {
        return ChartTier::None;
    }
    let by_width = if width >= 140 {
        ChartTier::Full
    } else if width >= 100 {
        ChartTier::CpuOnly
    } else if width >= flock::MIN_WIDTH {
        ChartTier::Sparkline
    } else {
        ChartTier::None
    };
    if by_width == ChartTier::Full && terminal_height < FULL_TIER_MIN_HEIGHT {
        ChartTier::CpuOnly
    } else {
        by_width
    }
}

/// Rows `CPU_HEADER_ROW` to `AXIS_ROW`: the CPU history, the memory
/// history, and the axis they share.
///
/// `body_cells` is computed once, here, and handed to both charts: the
/// other half of why a memory step and a CPU spike land on the same
/// column is [`GUTTER`] being one constant rather than two calculations
/// that could drift apart.
pub(super) fn draw_charts(
    app: &App,
    sheep_id: u32,
    max_memory: Option<u64>,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let body_cells = chart_body_cells(area.width);
    let cpu_history = app.cpu_history(sheep_id);
    let rss_history = app.rss_history(sheep_id);

    let cpu_header = cpu_header_text(cpu_history.len(), body_cells);
    write_row(
        buffer,
        area,
        CPU_HEADER_ROW,
        &format!("\u{2588}\u{2588} CPU   {cpu_header}"),
        palette.muted(),
    );
    for (i, row_text) in cpu_chart_rows(cpu_history, body_cells)
        .into_iter()
        .enumerate()
    {
        write_row(
            buffer,
            area,
            CPU_CHART_ROW + i as u16,
            &row_text,
            Style::default(),
        );
    }

    let mem_window = &rss_history[rss_history.len().saturating_sub(body_cells)..];
    let mem_window_peak = mem_window.iter().copied().max().unwrap_or(0);
    let mem_header = mem_header_text(max_memory, mem_window_peak);
    write_row(
        buffer,
        area,
        MEM_HEADER_ROW,
        &format!("\u{2588}\u{2588} MEM   {mem_header}"),
        palette.muted(),
    );
    let (mem_rows, marked) = mem_chart_rows(rss_history, max_memory, body_cells);
    for (i, row_text) in mem_rows.into_iter().enumerate() {
        let style = if marked == Some(i) {
            palette.attention()
        } else {
            Style::default()
        };
        write_row(buffer, area, MEM_CHART_ROW + i as u16, &row_text, style);
    }

    write_row(
        buffer,
        area,
        AXIS_ROW,
        &axis_row(body_cells),
        palette.muted(),
    );
}

/// [`ChartTier::CpuOnly`]'s own rows: the CPU chart, unchanged, and in
/// [`MEM_HEADER_ROW`]'s own slot, [`mem_line_text`]'s single line in place
/// of the memory chart's header and five rows: memory still has a gauge
/// to fall back on; the CPU chart is the more diagnostic of the two, so it
/// is the one that stays.
pub(super) fn draw_cpu_only(
    app: &App,
    sheep_id: u32,
    max_memory: Option<u64>,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let body_cells = chart_body_cells(area.width);
    let cpu_history = app.cpu_history(sheep_id);
    let rss_history = app.rss_history(sheep_id);

    let cpu_header = cpu_header_text(cpu_history.len(), body_cells);
    write_row(
        buffer,
        area,
        CPU_HEADER_ROW,
        &format!("\u{2588}\u{2588} CPU   {cpu_header}"),
        palette.muted(),
    );
    for (i, row_text) in cpu_chart_rows(cpu_history, body_cells)
        .into_iter()
        .enumerate()
    {
        write_row(
            buffer,
            area,
            CPU_CHART_ROW + i as u16,
            &row_text,
            Style::default(),
        );
    }

    let current_rss = rss_history.last().copied().unwrap_or(0);
    write_row(
        buffer,
        area,
        MEM_HEADER_ROW,
        &mem_line_text(current_rss, max_memory),
        Style::default(),
    );
    write_row(
        buffer,
        area,
        AXIS_ROW,
        &axis_row(body_cells),
        palette.muted(),
    );
}

/// [`ChartTier::CpuOnly`]'s own memory line: `rss` against its ceiling
/// when a ceiling exists, per the arithmetic in decision 8:
/// `rss 48.3M of 52M` plus a 10-cell gauge. With no ceiling set, the
/// gauge has nothing to fill against, the same no-limit case
/// [`cell::gauge`]'s own doc already covers.
fn mem_line_text(current_rss: u64, max_memory: Option<u64>) -> String {
    let gauge = cell::gauge(current_rss, max_memory, 10);
    match max_memory {
        Some(limit) => format!(
            "rss {} of {} {gauge}",
            crate::output::human_bytes(current_rss),
            crate::output::human_bytes(limit),
        ),
        None => format!("rss {} {gauge}", crate::output::human_bytes(current_rss)),
    }
}

/// The full-width hairline rule at [`HAIRLINE_ROW`], between the axis and
/// the column headers: the same `─` run
/// [`super::super::status::rule_line`] draws
/// under the pane's own header elsewhere in `lookout`.
pub(super) fn draw_hairline(area: Rect, buffer: &mut Buffer, palette: Palette) {
    write_row(
        buffer,
        area,
        HAIRLINE_ROW,
        &cell::rule(usize::from(area.width)),
        palette.muted(),
    );
}

/// [`ChartTier::Sparkline`]'s own row: 1a's own `CPU 20s` sparkline and
/// `MEM/CEIL` gauge, the pair [`super::super::flock`]'s own flat-view columns
/// already draw, on the one row this tier has left once both charts have
/// given up their own sixteen.
pub(super) fn draw_sparkline_row(
    app: &App,
    info: &shep_core::protocol::ProcessInfo,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let spark = cell::sparkline(app.cpu_history(info.id), 10, app.cpu_ceiling());
    let gauge = cell::gauge(info.memory_bytes.unwrap_or(0), info.max_memory, 10);
    write_row(
        buffer,
        area,
        CPU_HEADER_ROW,
        &format!("CPU 20s   {spark}      MEM/CEIL   {gauge}"),
        palette.muted(),
    );
}

/// Writes one styled line into `buffer`, `row` cells below `area`'s own
/// top. Callers only reach here once [`draw`](super::draw) has already
/// checked `area`
/// is tall enough for `row`.
///
/// `set_stringn`, not a `Line` of one `Span`: the `Span` needs an owned
/// `String` and this runs ten to fifteen times a frame, where the borrowed
/// `&str` is all the buffer ever needed.
fn write_row(buffer: &mut Buffer, area: Rect, row: u16, text: &str, style: Style) {
    buffer.set_stringn(area.x, area.y + row, text, usize::from(area.width), style);
}

/// The chart body's width in cells: `area`'s own `width` less [`GUTTER`]
/// and [`MARGIN`], per decision 8's arithmetic:
///
/// ```text
/// 160 = 8 gutter + 140 body + 12 margin
/// 140 = 8 gutter + 120 body + 12 margin
/// ```
///
/// Capped at [`HISTORY`]: past 160 columns the arithmetic above would ask
/// for more samples than the buffer ever holds, and an uncapped body keeps
/// [`cpu_header_text`] reading `collecting` forever even once the buffer is
/// full. Which charts draw at which width past that point is task 11's own
/// tier; this is only the ceiling the header's own claim has to respect.
pub(super) fn chart_body_cells(width: u16) -> usize {
    usize::from(width)
        .saturating_sub(GUTTER + MARGIN)
        .min(HISTORY)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use crate::lookout::app::{Body, Control, KeyPress, Msg, RowKey};
    use crate::lookout::frames::render_text;

    use super::super::super::fixtures;
    use super::super::draw;
    use super::super::layout::{CPU_ROWS, MEM_ROWS};
    use super::*;

    /// The chart-drawing twin of the test above: pane pinned to alpha,
    /// alpha leaves the flock, bravo (with its own, distinct `max_memory`)
    /// takes the reseated selection. `draw`'s own gate reads
    /// `App::sheep_pane_row`, which is `None` once alpha is gone, so
    /// nothing charts at all; reading `App::selected_row` instead would
    /// draw bravo's ceiling under a pane still naming alpha. The area here
    /// is tall and wide enough to reach `draw_charts`, unlike the test
    /// above's 80x3.
    #[test]
    fn the_charts_do_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online)
                    .max_memory(Some(64 << 20))
                    .build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        let _ = app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(2, "bravo", ProcStatus::Online)
                    .max_memory(Some(64 << 20))
                    .build(),
            ],
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
        let area = Rect::new(0, 0, 40, MIN_HEIGHT_FOR_CHARTS);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("ceiling"),
            "bravo's own max_memory must not draw once the pinned sheep is \
             gone: {text:?}"
        );
    }

    /// Past 160 columns [`chart_body_cells`] would ask for more samples than
    /// [`HISTORY`] ever holds; capped, or [`cpu_header_text`] would read
    /// `collecting` forever even once the buffer is full.
    #[test]
    fn chart_body_stays_within_the_history_buffer() {
        let body = chart_body_cells(300);
        assert_eq!(body, HISTORY);
        assert!(
            !cpu_header_text(HISTORY, body).contains("collecting"),
            "a full buffer past the cap must not still read collecting"
        );
    }

    /// One sheep, run through two real polls so both `cpu_history` and
    /// `rss_history` hold a differenced, nonzero last sample: the alignment
    /// test below needs the rightmost column of both charts' bottom row to
    /// be real content, not left-padding a too-short history would leave
    /// blank there too.
    fn app_with_two_polls(id: u32, name: &str, max_memory: Option<u64>) -> App {
        let t0 = std::time::Instant::now();
        let mut app = App::new(
            fixtures::plain(),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(id, name, ProcStatus::Online)
                    .cpu_ms(Some(0))
                    .memory_bytes(Some(10 << 20))
                    .max_memory(max_memory)
                    .build(),
            ],
            at: t0,
        });
        let t1 = t0 + Duration::from_secs(2);
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(id, name, ProcStatus::Online)
                    .cpu_ms(Some(2000))
                    .memory_bytes(Some(10 << 20))
                    .max_memory(max_memory)
                    .build(),
            ],
            at: t1,
        });
        app
    }

    /// The whole reason this frame was picked over side-by-side charts: a
    /// memory step and a CPU spike land on the same column because both
    /// bodies are drawn to the same `body_cells`, not two calculations that
    /// could quietly drift apart. Rendered through [`draw_charts`] itself,
    /// not two calls typed with the same literal by hand: that would still
    /// pass if the two calls' own `body_cells` argument drifted apart at the
    /// call site, which is exactly the mutation this test exists to catch.
    #[test]
    fn the_two_charts_share_one_body_width() {
        let app = app_with_two_polls(1, "alpha", None);
        let area = Rect::new(0, 0, 40, MIN_HEIGHT_FOR_CHARTS);
        let mut buffer = Buffer::empty(area);
        draw_charts(&app, 1, None, area, &mut buffer, fixtures::plain());
        let text = render_text(&buffer);
        let lines: Vec<&str> = text.lines().collect();
        let cpu_last_row = lines[usize::from(CPU_CHART_ROW) + CPU_ROWS - 1];
        let mem_last_row = lines[usize::from(MEM_CHART_ROW) + MEM_ROWS - 1];
        let cpu_end = cpu_last_row.trim_end().chars().count();
        let mem_end = mem_last_row.trim_end().chars().count();
        assert_eq!(
            cpu_end, mem_end,
            "CPU chart's bottom row ends at column {cpu_end}, memory's at \
             {mem_end}: {cpu_last_row:?} vs {mem_last_row:?}"
        );
    }

    /// The arithmetic, asserted rather than trusted:
    ///     160 = 8 gutter + 140 body + 12 margin
    ///     body = min(width - 20, HISTORY)
    /// A scene one cell short of its own column set silently drops the
    /// thing it exists to show, which is why this is a test and not a
    /// comment.
    #[test]
    fn the_chart_body_is_the_width_less_its_gutter_and_margin() {
        assert_eq!(chart_body_cells(160), 140);
        assert_eq!(chart_body_cells(140), 120);
    }

    /// Past the design target the buffer runs out before the columns do, so
    /// the margin grows rather than leaving cells that can never fill.
    #[test]
    fn a_wider_terminal_grows_the_margin_rather_than_the_body() {
        assert_eq!(chart_body_cells(200), 140);
    }
}
