use crate::output::width;

/// Trims `frame` to what a terminal `columns` wide and `rows` tall shows,
/// saying how much it dropped.
///
/// Rows, not lines: a line wider than the terminal wraps onto more than one
/// of them, so counting lines would overrun a narrow window.
///
/// The notice's height is measured, not assumed. A size of nothing is not a window of nothing.
pub(super) fn fit_rows(frame: &str, columns: u16, rows: u16) -> String {
    let (columns, budget) = (usize::from(columns), usize::from(rows).saturating_sub(1));
    if columns == 0 || budget == 0 {
        return frame.to_owned();
    }
    let lines: Vec<&str> = frame.lines().collect();
    // Widest the notice can get, since more dropped lines means more digits.
    let reserve = line_rows(&notice(lines.len()), columns);
    let fill = budget.saturating_sub(reserve);
    let mut used = 0;
    let mut kept = 0;
    for line in &lines {
        let height = line_rows(line, columns);
        if used + height > fill {
            break;
        }
        used += height;
        kept += 1;
    }
    if kept == lines.len() {
        return frame.to_owned();
    }
    let mut fitted = lines[..kept].join("\n");
    if kept > 0 {
        fitted.push('\n');
    }
    // `budget - used` rather than the whole notice: a window too narrow to
    // hold it is the one case the reservation above cannot satisfy, and
    // printing it whole scrolls the screen, which is what this function
    // exists to prevent. Clipped, the operator still reads the leading digits.
    fitted.push_str(&clip_rows(
        &notice(lines.len() - kept),
        columns,
        budget.saturating_sub(used),
    ));
    fitted.push('\n');
    fitted
}

/// `text` cut to at most `rows` rows at `columns` wide.
///
/// Only [`fit_rows`]'s notice reaches this, and only on a window too narrow
/// to print it whole.
fn clip_rows(text: &str, columns: usize, rows: usize) -> String {
    let ceiling = rows.saturating_mul(columns);
    if width::visible_width(text) <= ceiling {
        return text.to_owned();
    }
    text.chars().take(ceiling).collect()
}

/// What a trimmed frame says in place of the lines it dropped.
///
/// A function rather than a literal because [`fit_rows`] measures this twice:
/// once at its widest to reserve the rows, and once with the count it settled
/// on.
pub(super) fn notice(dropped: usize) -> String {
    // Singular is reachable: `dropped` is at least one wherever the notice
    // prints at all, and a follow redraws this once a second.
    let lines = if dropped == 1 { "line" } else { "lines" };
    format!("{dropped} more {lines} than this terminal shows")
}

/// How many terminal rows `line` occupies once it wraps at `columns`.
///
/// An empty line still occupies one.
fn line_rows(line: &str, columns: usize) -> usize {
    width::visible_width(line).div_ceil(columns).max(1)
}

#[cfg(test)]
mod tests {

    use super::*;

    /// fails if a frame that fits gets trimmed anyway. Three lines in a
    /// window with rows to spare come back byte-identical, trailing newline
    /// and all.
    /// fails if the notice's own height stops being reserved. It wraps like
    /// any other line, so at 30 columns it is two rows.
    ///
    /// Including the window too narrow to hold the notice whole: it is clipped now.
    #[test]
    fn a_trimmed_frame_never_overruns_the_rows_it_was_given() {
        let frame: String = (0..40)
            .map(|n| format!("sheep-{n:02}  online  1234  0.5%  12.3 MB  0d 0h 1m\n"))
            .collect();
        for columns in [12u16, 16, 20, 24, 30, 38, 40, 60, 80, 120] {
            for rows in [3u16, 4, 5, 8, 12, 24, 40] {
                let out = fit_rows(&frame, columns, rows);
                let used: usize = out
                    .lines()
                    .map(|line| line_rows(line, usize::from(columns)))
                    .sum();
                let budget = usize::from(rows).saturating_sub(1);
                assert!(
                    used <= budget,
                    "{columns}x{rows}: used {used} rows against a budget of {budget}"
                );
            }
        }
    }

    /// fails if the notice goes back to one spelling. Dropping exactly one
    /// line is the common case on a window one row short, and it redraws
    /// every second.
    #[test]
    fn the_notice_counts_one_dropped_line_in_the_singular() {
        let frame = "one\ntwo\nthree\n";

        // Four rows: one held for the cursor, one for the notice, two for
        // content, so exactly one line drops and the word is singular.
        assert_eq!(
            fit_rows(frame, 80, 4),
            "one\ntwo\n1 more line than this terminal shows\n"
        );
        // Three rows leaves one for content, so two drop and it is plural.
        assert_eq!(
            fit_rows(frame, 80, 3),
            "one\n2 more lines than this terminal shows\n"
        );
    }

    #[test]
    fn a_frame_that_fits_is_left_alone() {
        let frame = "one\ntwo\nthree\n";

        assert_eq!(fit_rows(frame, 80, 24), frame);
    }

    /// fails if the fit counts lines instead of rows. Four lines is four
    /// lines, but at ten columns each of these wraps onto three, so twelve
    /// rows of content do not go into a window of eight.
    #[test]
    fn a_wrapped_line_costs_more_than_one_row() {
        let wide = "0123456789012345678901234";
        let frame = format!("{wide}\n{wide}\n{wide}\n{wide}\n");

        let fitted = fit_rows(&frame, 10, 8);

        assert_eq!(
            fitted, "0123456789012345678901234\n3 more lines than this terminal shows\n",
            "the notice reserves its own four rows at ten columns, leaving three of the seven \
                 for content, which is one wrapped line"
        );
    }

    /// fails if the notice steals the row of a line it is reporting, or if
    /// the count goes wrong. Ten lines into a window six rows tall keeps
    /// four and says so.
    #[test]
    fn a_frame_too_tall_says_how_much_it_dropped() {
        let frame = (0..10)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        let fitted = fit_rows(&frame, 80, 6);

        assert_eq!(
            fitted,
            "0\n1\n2\n3\n6 more lines than this terminal shows\n"
        );
    }

    /// fails if a pty that has never been told its size swallows the flock.
    /// `script(1)` reports zero rows and zero columns, and trimming to that
    /// left nothing on screen but the notice, once a second.
    #[test]
    fn a_terminal_reporting_no_size_gets_the_frame_whole() {
        let frame = "a\nb\nc\n";

        assert_eq!(fit_rows(frame, 0, 0), frame, "no size at all");
        assert_eq!(fit_rows(frame, 80, 0), frame, "no rows");
        assert_eq!(fit_rows(frame, 0, 24), frame, "no columns");
        assert_eq!(
            fit_rows(frame, 80, 1),
            frame,
            "one row leaves nothing to trim to once the cursor has its own"
        );
    }
}
