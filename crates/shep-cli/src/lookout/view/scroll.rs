//! The one scroll driver every screen taller than its terminal shares.
//!
//! [`Viewport`](crate::lookout::viewport::Viewport) scrolls in data rows.
//! The height a screen is drawn into counts lines. A screen with chrome asks
//! it for room and gets an answer about the wrong thing. The fix is the same
//! shape everywhere. Treat the offset as a starting point and lay the body
//! out from there, then walk down one row at a time until the cursor's row
//! comes back. The walk, its last resort, and the two hidden-count markers
//! live here. They hold what every screen claims: the cursor draws at any
//! height.

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
/// The walk can run out. At `view::MIN_HEIGHT` there is no room for a
/// section's header and markers above one row. `fallback` then draws the
/// cursor's row without its chrome. At most one pass per row.
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
/// `hidden_below` is counted off what the pass drew, not the viewport's
/// arithmetic. `offset` is the data row the first line came from. Both
/// markers are already paid for: a window pass holds one line back for each.
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
