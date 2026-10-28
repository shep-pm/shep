//! The one scroll driver every screen taller than its terminal shares.
//!
//! [`Viewport`](crate::lookout::viewport::Viewport) scrolls in data rows,
//! while the height a screen is drawn into counts lines. A screen with
//! chrome (a section header, a blank separator, a caption, a column
//! header, two hidden-count markers) cannot ask the viewport how much
//! fits and get an answer about its own body.
//!
//! The fix is the same shape on every screen: treat the viewport's offset
//! as a starting point, lay the body out from there, and walk down one
//! row at a time until the cursor's row is among the lines that came
//! back. The walk, its last resort, and the two markers naming the rows
//! off each end live here.
//!
//! What the screens share is the invariant this module holds: the cursor
//! is drawn at every height a screen claims it can be drawn at.

use ratatui::text::{Line, Span};

use super::super::theme::Palette;

/// One attempt at a body, laid out from a given data row.
///
/// No `Debug`: it is built and consumed inside [`to_cursor`], and a
/// `Vec<Line>` prints as noise rather than as anything a test would assert
/// on.
pub(super) struct Attempt {
    /// The body, never longer than the budget it was built against.
    pub lines: Vec<Line<'static>>,
    /// Whether the cursor's own row was one of them. `false` means the
    /// chrome ate the budget and the walk must scroll further.
    pub cursor_drawn: bool,
}

/// Lays the body out from the first offset at or below `start` whose
/// attempt draws the cursor's row, and falls back when none does.
///
/// The walk can run out. A four-line body has no room for a section's own
/// header, blank separator and both markers above a single row, and at
/// `view::MIN_HEIGHT` that is the whole budget. When it runs out it gives up
/// on the layout rather than on the cursor: `fallback` draws the cursor's
/// row without its section chrome. The cursor is therefore drawn at every
/// height the screen claims to support, and the frame at the floor looks
/// unlike every other frame on purpose.
///
/// At most one pass per row, and each pass is bounded by the budget.
pub(super) fn to_cursor<B, F>(
    cursor_row: usize,
    start: usize,
    mut body: B,
    fallback: F,
) -> Vec<Line<'static>>
where
    B: FnMut(usize) -> Attempt,
    F: FnOnce() -> Vec<Line<'static>>,
{
    let mut offset = start.min(cursor_row);
    loop {
        let attempt = body(offset);
        if attempt.cursor_drawn {
            return attempt.lines;
        }
        if offset >= cursor_row {
            return fallback();
        }
        offset += 1;
    }
}

/// Which end of a body a marker counts off.
enum Side {
    /// Rows above the first line the body drew.
    Above,
    /// Rows below the last one it drew.
    Below,
}

/// One hidden-count marker line.
///
/// The text and the muted style live here and nowhere else: `count` rows
/// sit off `side`, and a screen that wants a different sentence, or a
/// different style, changes this one match.
fn marker(count: usize, side: Side, palette: Palette) -> Line<'static> {
    let text = match side {
        Side::Above => format!("  ... {count} above"),
        Side::Below => format!("  ... {count} below"),
    };
    Line::from(Span::styled(text, palette.muted()))
}

/// Draws the two hidden-count markers around a body laid out as a window of
/// rows.
///
/// `hidden_below` is counted off what the pass drew rather than off the
/// viewport's arithmetic, and `offset` is the data row the first line came
/// from. Both lines are already paid for: a window-building pass holds a
/// line back for each marker before it admits a row.
///
/// The below marker goes on first so the above marker can be inserted at
/// index 0 once the whole window is known.
pub(super) fn window_markers(
    lines: &mut Vec<Line<'static>>,
    hidden_below: usize,
    offset: usize,
    palette: Palette,
) {
    if hidden_below > 0 {
        lines.push(marker(hidden_below, Side::Below, palette));
    }
    if offset > 0 {
        lines.insert(0, marker(offset, Side::Above, palette));
    }
}

/// Draws the two hidden-count markers around a body that is only the
/// cursor's own row.
///
/// `cursor_row` is that row's index: every row above it is hidden above,
/// every row after it hidden below. Unlike a window, this body reserves
/// nothing up front, because the cursor's row is the one line it exists to
/// guarantee, so each marker is drawn only while a line is left to spend.
pub(super) fn cursor_markers(
    lines: &mut Vec<Line<'static>>,
    hidden_below: usize,
    cursor_row: usize,
    budget: usize,
    palette: Palette,
) {
    if cursor_row > 0 && lines.len() < budget {
        lines.insert(0, marker(cursor_row, Side::Above, palette));
    }
    if hidden_below > 0 && lines.len() < budget {
        lines.push(marker(hidden_below, Side::Below, palette));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::view::fixtures::plain;

    /// `rows` body lines, numbered so a marker's position is readable.
    fn body(rows: usize) -> Vec<Line<'static>> {
        (0..rows)
            .map(|row| Line::from(format!("row {row}")))
            .collect()
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_window_marks_the_rows_off_both_ends_around_the_rows_it_drew() {
        let mut lines = body(2);
        window_markers(&mut lines, 3, 4, plain());
        assert_eq!(
            text(&lines),
            vec!["  ... 4 above", "row 0", "row 1", "  ... 3 below"]
        );
    }

    #[test]
    fn a_window_that_hid_nothing_marks_neither_end() {
        let mut lines = body(2);
        window_markers(&mut lines, 0, 0, plain());
        assert_eq!(text(&lines), vec!["row 0", "row 1"]);
    }

    #[test]
    fn the_cursor_body_spends_its_last_line_on_the_above_marker() {
        // Two lines of budget and one drawn row: the above marker takes the
        // line that is left, so the below marker is the one that goes.
        let mut lines = body(1);
        cursor_markers(&mut lines, 3, 2, 2, plain());
        assert_eq!(text(&lines), vec!["  ... 2 above", "row 0"]);

        let mut lines = body(1);
        cursor_markers(&mut lines, 3, 2, 3, plain());
        assert_eq!(
            text(&lines),
            vec!["  ... 2 above", "row 0", "  ... 3 below"]
        );
    }
}
