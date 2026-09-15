//! What each chart's rows actually say: the two headers, the bar rows
//! under them, the gutter labels down their left edge, and the axis they
//! share.
//!
//! Nothing here writes into a `Buffer`. Every function returns text, which
//! is why the header wording and the ceiling row's placement can be pinned
//! without rendering a frame.

use std::time::Duration;

use crate::lookout::app::CPU_CEILING_FLOOR;
use crate::lookout::pane_sheep::{scale_top, window};

use super::super::super::cell;
use super::super::layout::{CPU_ROWS, GUTTER, MEM_ROWS};

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
pub(super) fn cpu_header_text(history_len: usize, body_cells: usize) -> String {
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
/// [`App::cpu_ceiling`](crate::lookout::app::App::cpu_ceiling), which this
/// one-sheep pane has no row to be comparable with.
pub(super) fn cpu_chart_rows(history: &[f32], body_cells: usize) -> Vec<String> {
    let window_slice = &history[history.len().saturating_sub(body_cells)..];
    let peak = window_slice.iter().copied().fold(0.0_f32, f32::max);
    let ceiling = scale_top(f64::from(peak), f64::from(CPU_CEILING_FLOOR));
    let bars = cell::chart(history, ceiling as f32, body_cells, CPU_ROWS);
    gutter_lines(bars, ceiling, GutterCadence::Alternating, |value| {
        format!("{value:.0}%")
    })
}

/// Row `MEM_HEADER_ROW`'s second half: a real ceiling names itself; with
/// none, the header states the substitute denominator instead, per the
/// design rule that every measurement states what it is measured against.
pub(super) fn mem_header_text(max_memory: Option<u64>, window_peak: u64) -> String {
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
/// nearest a real `max_memory` ceiling: that row is
/// [`draw_charts`](super::draw_charts)'s cue
/// to paint it in `--butter` instead of the chart's own colour.
///
/// Scales to `max(max_memory, window peak)` rather than `max_memory`
/// alone, so a sheep already over its limit still draws the spike rather
/// than clipping it off the top.
pub(super) fn mem_chart_rows(
    history: &[u64],
    max_memory: Option<u64>,
    body_cells: usize,
) -> (Vec<String>, Option<usize>) {
    let window_slice = &history[history.len().saturating_sub(body_cells)..];
    let window_peak = window_slice.iter().copied().max().unwrap_or(0);
    let peak_for_scale = max_memory.map_or(window_peak, |limit| limit.max(window_peak));
    // `scale_top`'s own ladder is decimal, but `human_bytes` renders the
    // binary MiB `crate::output::human_bytes` always has: scaling in MiB
    // rather than raw bytes is what keeps a labelled row's own value round
    // in the unit it is shown in (`64.0M`, never `76.3M` for a 64M
    // ceiling).
    const MIB: f64 = (1u64 << 20) as f64;
    let ceiling_mib = scale_top(peak_for_scale as f64 / MIB, 0.0);
    let ceiling = ceiling_mib * MIB;
    let samples: Vec<f32> = history.iter().map(|&bytes| bytes as f32).collect();
    let bars = cell::chart(&samples, ceiling as f32, body_cells, MEM_ROWS);
    let mut lines = gutter_lines(bars, ceiling, GutterCadence::EveryRow, |value| {
        crate::output::human_bytes(value as u64)
    });

    let marked = max_memory.map(|limit| {
        if ceiling <= 0.0 {
            return 0;
        }
        let band = ceiling / MEM_ROWS as f64;
        // The row nearest the limit, not the row at or below it: a ceiling
        // that lands between two rows still has to pick one, and rounding
        // down always draws the line above the real limit, which is the
        // unsafe direction (a sheep already over its limit would still
        // draw under the line).
        ((ceiling - limit as f64) / band)
            .round()
            .clamp(0.0, (MEM_ROWS - 1) as f64) as usize
    });
    if let Some(row) = marked {
        // The marked row's own label states the ceiling's own configured
        // value, never the ladder row it happens to land nearest: decision
        // 8's frame draws `52M` for a 52M ceiling, not whatever round
        // number the ladder rounded up to.
        let limit = max_memory.unwrap_or_default();
        let label = format!("{:>7} ", crate::output::human_bytes(limit));
        lines[row] = format!("{label}{} ceiling", "\u{254c}".repeat(body_cells));
    }
    (lines, marked)
}

/// A gutter's labelling cadence: how many of its rows carry a value versus
/// a bare tick. The frame gives the two charts different cadences (decision
/// 8): 8 rows is enough to crowd if every one is labelled, 5 is few enough
/// to label completely.
#[derive(Clone, Copy)]
enum GutterCadence {
    /// A label on even rows, a bare `|` tick on odd ones. The CPU chart's
    /// 8 rows.
    Alternating,
    /// A label on every row. The memory chart's 5 rows.
    EveryRow,
}

/// Prefixes each of `bars`' lines with [`GUTTER`] cells: a value on the
/// rows `cadence` picks, right-aligned and formatted by `format_value`,
/// with a bare `|` tick on any row `cadence` skips; the bottom row is
/// always the literal `0` rather than `format_value(0.0)`, since a unit on
/// a value that is always zero states nothing a bare `0` doesn't.
/// [`scale_top`]'s own ladder is why a labelled row doesn't need to be the
/// top or bottom to land on a round number.
fn gutter_lines(
    bars: Vec<String>,
    ceiling: f64,
    cadence: GutterCadence,
    format_value: impl Fn(f64) -> String,
) -> Vec<String> {
    let rows = bars.len();
    let last = rows.saturating_sub(1);
    bars.into_iter()
        .enumerate()
        .map(|(i, bar)| {
            let labelled = match cadence {
                GutterCadence::Alternating => i % 2 == 0,
                GutterCadence::EveryRow => true,
            };
            let gutter = if i == last {
                format!("{:>7} ", "0")
            } else if labelled {
                #[allow(clippy::cast_precision_loss)] // display only, a gutter label
                let value = ceiling * (rows - i) as f64 / rows as f64;
                format!("{:>7} ", format_value(value))
            } else {
                format!("{:>7} ", "|")
            };
            format!("{gutter}{bar}")
        })
        .collect()
}

/// The shared x axis: `body_cells` cells of rule, `now` ending on the last
/// one.
pub(super) fn axis_row(body_cells: usize) -> String {
    let label = "now";
    let dashes = body_cells.saturating_sub(label.len());
    format!("{}{}{label}", " ".repeat(GUTTER), "\u{2500}".repeat(dashes))
}

#[cfg(test)]
mod tests {
    use super::super::chart_body_cells;
    use super::*;

    /// Thin wrapper over the real header function: a full buffer, so the
    /// "drawn window" branch runs rather than the "collecting" one.
    fn header_at(width: u16) -> String {
        let body = chart_body_cells(width);
        cpu_header_text(body, body)
    }

    /// Thin wrapper over the real header function: `body_cells` passed
    /// straight through, the way [`draw_charts`](super::super::draw_charts)
    /// hands it the pane's own computed value rather than a raw terminal
    /// width.
    fn header_with_samples(history_len: usize, body_cells: usize) -> String {
        cpu_header_text(history_len, body_cells)
    }

    /// Thin wrapper over the real row function, over a fixture with a real
    /// shape: a flat history would prove nothing about the ceiling row's
    /// placement.
    fn mem_rows_with_limit(max_memory: Option<u64>) -> (Vec<String>, Option<usize>) {
        let history = [20 << 20, 25 << 20, 30 << 20, 40 << 20, 48 << 20];
        mem_chart_rows(&history, max_memory, 20)
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
    /// labelled, and specifically the row this fixture's own scaling puts
    /// it at: row 0 (the top) would be wrong here, so pinning presence alone
    /// would not catch a placement bug that always drew row 0.
    #[test]
    fn the_memory_chart_labels_a_real_ceiling() {
        let (rows, marked) = mem_rows_with_limit(Some(52 << 20));
        assert_eq!(
            marked,
            Some(2),
            "a 52M limit against this fixture's 48M peak scales to a 100M \
             ceiling, 20M per row, so the marked row is nearest at 2, not \
             the top"
        );
        assert!(rows[2].contains("ceiling"), "got {rows:?}");
        assert!(
            rows[2].contains("52.0M"),
            "the marked row states the limit's own value, not the ladder \
             row (60.0M) it happens to land nearest: {:?}",
            rows[2]
        );
    }

    /// The marked row is the nearest to the limit, not the row at or below
    /// it: a 64M limit against a 10M peak scales to a 100M ceiling, 20M per
    /// row, and 64 is 4M from the row at 60 but 16M from the row at 80.
    /// Rounding down (the old behaviour) picked 80 and drew the ceiling
    /// line above every real reading under 80M, so a sheep at 70M (over
    /// its own 64M limit) drew below the line instead of above it.
    #[test]
    fn the_marked_row_is_the_nearest_one_not_the_floor() {
        let history = [10 << 20; 20];
        let (rows, marked) = mem_chart_rows(&history, Some(64 << 20), 20);
        assert_eq!(marked, Some(2), "60.0M is nearer 64M than 80.0M is");
        assert!(rows[2].contains("64.0M"), "got {:?}", rows[2]);
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
                .0
                .iter()
                .any(|row| row.contains("ceiling"))
        );
    }

    /// Two labels bought nothing once `scale_top`'s own ladder makes every
    /// division round too: the gutter carries a value every other row,
    /// alternating with a bare tick, not just at the top and the bottom.
    #[test]
    fn the_gutter_labels_more_than_the_ends() {
        let history = [10.0_f32, 20.0, 30.0, 40.0, 45.0];
        let rows = cpu_chart_rows(&history, 20);
        let labelled: Vec<&str> = rows
            .iter()
            .map(|row| row[..GUTTER].trim())
            .filter(|cell| !cell.is_empty())
            .collect();
        assert!(
            labelled.len() > 2,
            "expected more than just the top and bottom label: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row[..GUTTER].contains('|')),
            "expected a bare tick between labels: {rows:?}"
        );
    }

    /// The frame gives the two gutters different cadences (the memory
    /// chart has few enough rows to label every one; the CPU chart has
    /// enough that alternating avoids crowding), so a fix that pins one
    /// must not flatten the other's.
    #[test]
    fn the_gutter_cadence_differs_by_chart() {
        let (mem_rows, _) = mem_rows_with_limit(None);
        let labelled_mem = mem_rows
            .iter()
            .filter(|row| {
                let cell = row[..GUTTER].trim();
                !cell.is_empty() && cell != "|"
            })
            .count();
        assert_eq!(
            labelled_mem, MEM_ROWS,
            "every memory row should carry a label: {mem_rows:?}"
        );

        let cpu_history = [10.0_f32, 20.0, 30.0, 40.0, 45.0];
        let cpu_rows = cpu_chart_rows(&cpu_history, 20);
        let ticked_cpu = cpu_rows
            .iter()
            .filter(|row| row[..GUTTER].contains('|'))
            .count();
        assert!(
            ticked_cpu > 0,
            "the CPU gutter should still alternate with bare ticks: {cpu_rows:?}"
        );
        assert!(
            ticked_cpu < CPU_ROWS,
            "the CPU gutter should not label every row: {cpu_rows:?}"
        );
    }
}
