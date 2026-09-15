//! Reading a finished frame back.
//!
//! [`super::super::fixtures`] builds what goes into a scene; this reads
//! what came out. Both halves are shared, so neither lives in a test
//! module of its own.

use ratatui::buffer::Buffer;

use crate::lookout::frames::coloured_palette;

/// The table row for `name`, or `None` if the table does not draw one.
///
/// Strips the leading `>` selection marker first, so a marked and
/// unmarked row share the same token index.
///
/// The numeric-id guard on token 0 is load bearing: the status bar's
/// own lines also open `{verb} {name} (id {id})`, so without it this
/// could match the bar line instead of a table row.
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn row_for<'a>(frame: &'a str, name: &str) -> Option<&'a str> {
    frame.lines().find(|line| {
        let mut tokens = line.trim_start_matches('>').split_whitespace();
        tokens.next().is_some_and(|id| id.parse::<u32>().is_ok()) && tokens.next() == Some(name)
    })
}

/// [`coloured_palette`] always paints a ground, so every pinned
/// snapshot's selected row is a painted gutter rather than a `>` glyph
/// ([`crate::lookout::view::flock::gutter`]).
pub(super) fn gallery_ground() -> ratatui::style::Color {
    coloured_palette()
        .ground()
        .bg
        .expect("the gallery's coloured palette always paints a ground")
}

/// Whether row `y` of `buffer` is the selected one: its gutter cell
/// (column 0) carries [`gallery_ground`].
pub(super) fn row_is_selected(buffer: &Buffer, y: u16) -> bool {
    buffer
        .cell((0, y))
        .is_some_and(|cell| cell.bg == gallery_ground())
}

/// The rendered text of whichever row of `buffer` is selected, or
/// `None` if none is (a section header, for instance, is never
/// selected).
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn selected_line<'a>(text: &'a str, buffer: &Buffer) -> Option<&'a str> {
    (0..buffer.area.height)
        .find(|&y| row_is_selected(buffer, y))
        .and_then(|y| text.lines().nth(usize::from(y)))
}

/// How many rows of `buffer`, EXCLUDING the status bar, are painted as
/// selected: a scene invariant is exactly one, the same invariant a `>`
/// glyph count used to check.
///
/// The status bar is always the buffer's last row and now carries the
/// same [`gallery_ground`] the selected row's gutter does
/// (`Palette::ground`, painted by `view::status` in this task); it is
/// chrome, not a candidate row, so it is excluded rather than counted.
pub(super) fn selected_row_count(buffer: &Buffer) -> usize {
    (0..buffer.area.height.saturating_sub(1))
        .filter(|&y| row_is_selected(buffer, y))
        .count()
}

/// The dogs table's own row lookup: unlike [`row_for`], a dog row opens
/// with `mark` and a name, never a numeric id, so the same "first two
/// tokens" shape does not apply.
pub(super) fn dog_row_for<'a>(frame: &'a str, name: &str) -> Option<&'a str> {
    frame
        .lines()
        .find(|line| line.trim_start_matches('>').split_whitespace().next() == Some(name))
}

/// Whether the selected row's name starts with `prefix`.
///
/// Handles truncation: the NAME column's truncated string depends on
/// terminal width, so `prefix` only needs to fit the eight-column
/// floor `name_width` never shrinks below. Selection is read off
/// `buffer`'s own painted gutter ([`row_is_selected`]), not a `>`
/// glyph the gallery's palette no longer draws.
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn marked_row_name_starts_with(text: &str, buffer: &Buffer, prefix: &str) -> bool {
    selected_line(text, buffer).is_some_and(|line| {
        line.trim_start_matches('>')
            .split_whitespace()
            .nth(1)
            .is_some_and(|name| name.starts_with(prefix))
    })
}
