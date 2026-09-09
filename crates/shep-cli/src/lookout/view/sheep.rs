//! The sheep pane: one sheep given the whole screen.
//!
//! [`super::mod`]'s own `draw` spends this module's whole area on the
//! identity band and the two charts; Tasks 9 and 10 add the read-only
//! config column and the feed to what is still blank below them.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::app::{App, CPU_CEILING_FLOOR, HISTORY, RowKey};
use super::super::pane_sheep::{SheepPane, scale_top, window};
use super::super::theme::Palette;
use super::{cell, detail};

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
/// The shortest `area` the charts will draw into: the identity band's own
/// row plus rows `CPU_HEADER_ROW` through `AXIS_ROW`. Below it, the pane
/// still opens; it just stays the identity band and blank rows, the way it
/// already did before this task. Task 11 replaces this all-or-nothing gate
/// with the responsive ladder's own row tiers.
const MIN_HEIGHT_FOR_CHARTS: u16 = AXIS_ROW + 1;

/// Draws the pane into `area`: the identity band on its first row, the two
/// histories over rows `CPU_HEADER_ROW` through `AXIS_ROW`, and nothing
/// else yet.
///
/// `area` is the whole pane body, under the title band [`super::mod`]'s own
/// `draw` already painted and over the status bar it paints after this
/// returns — not a sub-rect of either, the way every other full-screen
/// pane's own `draw` is handed one.
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

    // Rows `CPU_HEADER_ROW` to `AXIS_ROW`: the two charts, on the pinned
    // sheep only. A sheep that has left the flock has no history left
    // either (`App::cpu_history`'s own doc says `record_samples` drops it),
    // so there is nothing to chart once `sheep_row` is `None`.
    if let Some(row) = sheep_row
        && usize::from(area.width) >= GUTTER + MARGIN
        && area.height >= MIN_HEIGHT_FOR_CHARTS
    {
        draw_charts(app, row.info.id, row.info.max_memory, area, buffer, palette);
    }
    // Rows 18 to 46 stay blank here; Tasks 9 and 10 fill them.
}

/// Rows `CPU_HEADER_ROW` to `AXIS_ROW`: the CPU history, the memory
/// history, and the axis they share.
///
/// `body_cells` is computed once, here, and handed to both charts: the
/// other half of why a memory step and a CPU spike land on the same
/// column is [`GUTTER`] being one constant rather than two calculations
/// that could drift apart.
fn draw_charts(
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

/// Writes one styled line into `buffer`, `row` cells below `area`'s own
/// top. Callers only reach here once [`draw`] has already checked `area`
/// is tall enough for `row`.
fn write_row(buffer: &mut Buffer, area: Rect, row: u16, text: &str, style: Style) {
    let line = Line::from(Span::styled(text.to_string(), style));
    buffer.set_line(area.x, area.y + row, &line, area.width);
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
fn chart_body_cells(width: u16) -> usize {
    usize::from(width)
        .saturating_sub(GUTTER + MARGIN)
        .min(HISTORY)
}

/// `d` as `MmSSs`: `4m40s`, not `4m 40s` or a dropped `0m`. Neither
/// `output::human_duration`'s spacing nor its dropped-zero minor unit
/// would pass the window label's own test at 140 columns, where `4m00s`
/// has to keep its zero.
fn window_label(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}m{:02}s", secs / 60, secs % 60)
}

/// Row `CPU_HEADER_ROW`'s second half: the window this chart actually
/// drew, computed from `body_cells` rather than a literal, and how much of
/// it a buffer shorter than `body_cells` has filled so far.
fn cpu_header_text(history_len: usize, body_cells: usize) -> String {
    let full = window_label(window(body_cells));
    if history_len < body_cells {
        format!(
            "collecting \u{b7} {} of {full}",
            window_label(window(history_len))
        )
    } else {
        format!("{full}, one 2s sample per column")
    }
}

/// The CPU chart's `CPU_ROWS` rows: [`cell::chart`] scaled to
/// [`scale_top`] of the drawn window's own peak, floored at
/// [`CPU_CEILING_FLOOR`] rather than the flock-wide
/// [`App::cpu_ceiling`](super::super::app::App::cpu_ceiling), which this
/// one-sheep pane has no row to be comparable with.
fn cpu_chart_rows(history: &[f32], body_cells: usize) -> Vec<String> {
    let window_slice = &history[history.len().saturating_sub(body_cells)..];
    let peak = window_slice.iter().copied().fold(0.0_f32, f32::max);
    let ceiling = scale_top(f64::from(peak), f64::from(CPU_CEILING_FLOOR));
    let bars = cell::chart(history, ceiling as f32, body_cells, CPU_ROWS);
    gutter_lines(bars, &format!("{ceiling:.0}%"))
}

/// Row `MEM_HEADER_ROW`'s second half: a real ceiling names itself; with
/// none, the header states the substitute denominator instead, per the
/// design rule that every measurement states what it is measured against.
fn mem_header_text(max_memory: Option<u64>, window_peak: u64) -> String {
    match max_memory {
        Some(limit) => format!(
            "rss   same window   \u{b7}   ceiling {}",
            crate::output::human_bytes(limit)
        ),
        None => format!(
            "rss   same window   \u{b7}   no limit set \u{b7} scaled to peak {}",
            crate::output::human_bytes(window_peak)
        ),
    }
}

/// The memory chart's `MEM_ROWS` rows, and which one (if any) is the row
/// nearest a real `max_memory` ceiling: that row is [`draw_charts`]'s cue
/// to paint it in `--butter` instead of the chart's own colour.
///
/// Scales to `max(max_memory, window peak)` rather than `max_memory`
/// alone, so a sheep already over its limit still draws the spike rather
/// than clipping it off the top.
fn mem_chart_rows(
    history: &[u64],
    max_memory: Option<u64>,
    body_cells: usize,
) -> (Vec<String>, Option<usize>) {
    let window_slice = &history[history.len().saturating_sub(body_cells)..];
    let window_peak = window_slice.iter().copied().max().unwrap_or(0);
    let peak_for_scale = max_memory.map_or(window_peak, |limit| limit.max(window_peak));
    let ceiling = scale_top(peak_for_scale as f64, 0.0);
    let samples: Vec<f32> = history.iter().map(|&bytes| bytes as f32).collect();
    let bars = cell::chart(&samples, ceiling as f32, body_cells, MEM_ROWS);
    let top_label = crate::output::human_bytes(ceiling as u64);
    let mut lines = gutter_lines(bars, &top_label);

    let marked = max_memory.map(|limit| {
        if ceiling <= 0.0 {
            return 0;
        }
        let band = ceiling / MEM_ROWS as f64;
        ((ceiling - limit as f64) / band)
            .floor()
            .clamp(0.0, (MEM_ROWS - 1) as f64) as usize
    });
    if let Some(row) = marked {
        // `GUTTER` is ASCII throughout (digits, `%`, `M`, spaces), so this
        // is a valid byte index even though the chart body past it is not.
        let gutter = lines[row][..GUTTER].to_string();
        lines[row] = format!("{gutter}{} ceiling", "\u{254c}".repeat(body_cells));
    }
    (lines, marked)
}

/// Prefixes each of `bars`' lines with [`GUTTER`] cells: `top_label`
/// right-aligned on the top row, `0` on the bottom, blank between. Shared
/// by the CPU and memory charts so one gutter width backs both.
fn gutter_lines(bars: Vec<String>, top_label: &str) -> Vec<String> {
    let last = bars.len().saturating_sub(1);
    bars.into_iter()
        .enumerate()
        .map(|(i, bar)| {
            let gutter = if i == 0 {
                format!("{top_label:>7} ")
            } else if i == last {
                format!("{:>7} ", "0")
            } else {
                " ".repeat(GUTTER)
            };
            format!("{gutter}{bar}")
        })
        .collect()
}

/// The shared x axis: `body_cells` cells of rule, `now` ending on the last
/// one.
fn axis_row(body_cells: usize) -> String {
    let label = "now";
    let dashes = body_cells.saturating_sub(label.len());
    format!("{}{}{label}", " ".repeat(GUTTER), "\u{2500}".repeat(dashes))
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::super::app::{Body, KeyPress, Msg};
    use super::super::super::frames::render_text;
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

    /// Thin wrapper over the real header function: a full buffer, so the
    /// "drawn window" branch runs rather than the "collecting" one.
    fn header_at(width: u16) -> String {
        let body = chart_body_cells(width);
        cpu_header_text(body, body)
    }

    /// Thin wrapper over the real header function: `body_cells` passed
    /// straight through, the way [`draw_charts`] hands it the pane's own
    /// computed value rather than a raw terminal width.
    fn header_with_samples(history_len: usize, body_cells: usize) -> String {
        cpu_header_text(history_len, body_cells)
    }

    /// Thin wrapper over the real row function, over a fixture with a real
    /// shape: a flat history would prove nothing about the ceiling row's
    /// placement.
    fn mem_rows_with_limit(max_memory: Option<u64>) -> Vec<String> {
        let history = [20 << 20, 25 << 20, 30 << 20, 40 << 20, 48 << 20];
        mem_chart_rows(&history, max_memory, 20).0
    }

    /// Thin wrapper over the real header function.
    fn mem_header_with_limit(max_memory: Option<u64>, window_peak: u64) -> String {
        mem_header_text(max_memory, window_peak)
    }

    /// The header states the window it actually drew, not a literal. At two
    /// widths, because a literal passes at one of them.
    #[test]
    fn the_cpu_header_states_the_drawn_window() {
        assert!(header_at(160).contains("4m40s, one 2s sample per column"));
        assert!(header_at(140).contains("4m00s, one 2s sample per column"));
    }

    /// The buffer starts empty on every launch and dies with the process, so
    /// a chart that is not yet full says how full it is.
    #[test]
    fn a_partial_buffer_says_how_much_it_has() {
        assert!(header_with_samples(35, 140).contains("collecting \u{b7} 1m10s of 4m40s"));
    }

    /// With a limit set the ceiling is the limit, drawn as its own row and
    /// labelled.
    #[test]
    fn the_memory_chart_labels_a_real_ceiling() {
        let rows = mem_rows_with_limit(Some(52 << 20));
        assert!(rows.iter().any(|row| row.contains("ceiling")));
    }

    /// With no limit there is no ceiling row and the header says what it
    /// scaled to instead, per the design rule that every measurement states
    /// its denominator.
    #[test]
    fn the_memory_chart_states_its_substitute_denominator() {
        let header = mem_header_with_limit(None, 48 << 20);
        assert!(header.contains("no limit set"));
        assert!(header.contains("scaled to peak"));
        assert!(
            !mem_rows_with_limit(None)
                .iter()
                .any(|row| row.contains("ceiling"))
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

    /// The whole reason this frame was picked over side-by-side charts: a
    /// memory step and a CPU spike land on the same column because both
    /// bodies are drawn to the same `body_cells`, not two calculations that
    /// could quietly drift apart.
    #[test]
    fn the_two_charts_share_one_body_width() {
        let cpu_history = [10.0, 20.0, 30.0, 15.0, 5.0];
        let rss_history = [10 << 20, 20 << 20, 15 << 20, 12 << 20, 9 << 20];
        let cpu_rows = cpu_chart_rows(&cpu_history, 20);
        let (mem_rows, _) = mem_chart_rows(&rss_history, Some(30 << 20), 20);
        let cpu_width = cpu_rows[0].chars().count();
        let mem_width = mem_rows[0].chars().count();
        assert_eq!(
            cpu_width, mem_width,
            "CPU row is {cpu_width} cells, memory row is {mem_width}: {cpu_rows:?} vs {mem_rows:?}"
        );
    }
}
