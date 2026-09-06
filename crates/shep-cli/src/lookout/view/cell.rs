//! The shared cells the redesigned panes draw magnitude with.
//!
//! Every one returns a `String` rather than a `Line` or a `Span`: the
//! caller owns the styling, and a pure string is testable against a
//! literal the way `flock::mark` already is.
//!
//! Deliberately not `ratatui::widgets::Sparkline` or `Gauge`. This module
//! builds its rows by hand so column widths stay fixed and a row stays a
//! string a test can assert on, which those widgets take away.

/// The eight sparkline steps, low to high.
///
/// No non-test caller yet: `sparkline` reaches it, and `sparkline` itself
/// has none until tasks 5 and 7 to 9 wire a caller in.
#[allow(dead_code)]
const STEPS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

/// A horizontal bar of `cells`, filled in proportion to `value` over
/// `ceiling` and padded with the light shade.
///
/// `None`, zero, and any ceiling a value cannot be measured against give
/// an all-tail bar: the bar says "no ceiling set" rather than guessing a
/// denominator. A value above its ceiling fills the bar rather than
/// overflowing the column.
#[must_use]
// No non-test caller yet: this cell's callers arrive in tasks 5 and 7 to 9.
#[allow(dead_code)]
pub fn gauge(value: u64, ceiling: Option<u64>, cells: usize) -> String {
    let filled = match ceiling {
        Some(ceiling) if ceiling > 0 => {
            let scaled = (value as f64 / ceiling as f64 * cells as f64).round();
            (scaled as usize).min(cells)
        }
        _ => 0,
    };
    let mut out = String::with_capacity(cells * 3);
    out.extend(std::iter::repeat_n('\u{2588}', filled));
    out.extend(std::iter::repeat_n('\u{2591}', cells - filled));
    out
}

/// The newest `cells` samples, one cell each, scaled to the window's own
/// peak.
///
/// Padded on the left with spaces when there are fewer samples than cells,
/// so the line grows into the column from the right as history arrives.
/// No samples at all is blank rather than a flat line at the floor: a flat
/// line reads as measured and idle, and blank reads as not measured yet.
#[must_use]
// No non-test caller yet: this cell's callers arrive in tasks 5 and 7 to 9.
#[allow(dead_code)]
pub fn sparkline(samples: &[f32], cells: usize) -> String {
    if samples.is_empty() || cells == 0 {
        return " ".repeat(cells);
    }
    let window = &samples[samples.len().saturating_sub(cells)..];
    let peak = window.iter().copied().fold(0.0_f32, f32::max);
    let mut out = String::with_capacity(cells * 3);
    for _ in window.len()..cells {
        out.push(' ');
    }
    for sample in window {
        let step = if peak <= 0.0 {
            0
        } else {
            let scaled = (sample / peak * (STEPS.len() - 1) as f32).round();
            (scaled as usize).min(STEPS.len() - 1)
        };
        out.push(STEPS[step]);
    }
    out
}

/// A rule of exactly `cells` box-drawing horizontals.
#[must_use]
// No non-test caller yet: this cell's callers arrive in tasks 5 and 7 to 9.
#[allow(dead_code)]
pub fn rule(cells: usize) -> String {
    "\u{2500}".repeat(cells)
}

/// A section band: the two-block marker, the label, and padding to `cells`.
///
/// The caller styles it; this only lays it out. Truncates rather than
/// overflowing, since a band that runs past its `Rect` shifts the row.
#[must_use]
// No non-test caller yet: this cell's callers arrive in tasks 5 and 7 to 9.
#[allow(dead_code)]
pub fn band(label: &str, cells: usize) -> String {
    let head = format!(" \u{2588}\u{2588} {label}");
    let drawn: usize = head.chars().map(crate::output::width::char_columns).sum();
    if drawn >= cells {
        return super::flock::fit(&head, cells as u16);
    }
    let mut out = head;
    out.extend(std::iter::repeat_n(' ', cells - drawn));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gauge_fills_from_the_left_and_pads_with_the_light_shade() {
        assert_eq!(gauge(0, Some(100), 10), "░░░░░░░░░░");
        assert_eq!(gauge(50, Some(100), 10), "█████░░░░░");
        assert_eq!(gauge(100, Some(100), 10), "██████████");
    }

    #[test]
    fn a_gauge_over_its_ceiling_is_full_rather_than_wider() {
        assert_eq!(gauge(250, Some(100), 10), "██████████");
    }

    #[test]
    fn a_gauge_with_no_ceiling_is_all_tail() {
        assert_eq!(gauge(48, None, 10), "░░░░░░░░░░");
    }

    #[test]
    fn a_zero_ceiling_is_all_tail_rather_than_a_division_by_zero() {
        assert_eq!(gauge(48, Some(0), 10), "░░░░░░░░░░");
    }

    #[test]
    fn a_gauge_rounds_to_the_nearest_cell() {
        // 4 of 10 cells: 44% rounds down, 45% rounds up.
        assert_eq!(gauge(44, Some(100), 10), "████░░░░░░");
        assert_eq!(gauge(45, Some(100), 10), "█████░░░░░");
    }

    #[test]
    fn a_sparkline_is_one_cell_per_sample_scaled_to_its_own_peak() {
        assert_eq!(sparkline(&[0.0, 50.0, 100.0], 3), "▁▅█");
    }

    #[test]
    fn a_sparkline_shorter_than_its_cells_pads_on_the_left() {
        assert_eq!(sparkline(&[100.0], 4), "   █");
    }

    #[test]
    fn a_sparkline_longer_than_its_cells_keeps_the_newest() {
        assert_eq!(sparkline(&[100.0, 0.0, 0.0], 2), "▁▁");
    }

    #[test]
    fn an_empty_sparkline_is_blank_rather_than_a_flat_line() {
        // A flat line at the floor reads as measured-and-idle. Blank reads
        // as no data yet, which is what twenty seconds after start is.
        assert_eq!(sparkline(&[], 4), "    ");
    }

    #[test]
    fn a_flat_sparkline_sits_at_the_floor() {
        assert_eq!(sparkline(&[0.0, 0.0, 0.0], 3), "▁▁▁");
    }

    #[test]
    fn a_rule_is_exactly_its_cells() {
        assert_eq!(rule(4), "────");
        assert_eq!(rule(0), "");
    }

    #[test]
    fn a_band_marks_its_label_and_pads_to_width() {
        assert_eq!(band("FLOCK", 20), " ██ FLOCK           ");
    }

    #[test]
    fn a_band_narrower_than_its_label_truncates_rather_than_overflowing() {
        assert_eq!(band("FLOCK", 6).chars().count(), 6);
    }
}
