//! What the table measures itself against: the floors it will not draw
//! below, the selection marker's own two columns, and the padding and
//! truncation every cell goes through.
//!
//! Nothing here knows which columns exist. [`fit`] and [`pad_ground`] are
//! the two both row renderers call on every cell they draw.

use ratatui::style::Style;
use ratatui::text::Span;

use super::super::super::theme::Palette;
use crate::output::width::char_columns;

/// The narrowest terminal the table will draw into.
///
/// `ID` + `NAME` (floor 8) + `STATUS` (15, the width of `waiting-restart`)
/// plus two separators. This is the table's own floor, not the terminal's:
/// the terminal also needs room for [`GUTTER`], the selection marker's
/// column.
pub const MIN_WIDTH: u16 = 31;

/// The shortest terminal the pane will draw into: title, banner, header,
/// rule, one data row, status bar.
pub const MIN_HEIGHT: u16 = 6;

/// The columns the selection marker takes, to the left of the table.
///
/// One for the marker, one for the gap. The table itself is rendered into
/// `width - GUTTER` starting at `x + GUTTER`, so every threshold in
/// `columns::TIERS` and every arithmetic in
/// [`super::columns::name_width`] is untouched by the marker.
pub const GUTTER: u16 = 2;

/// The marker for the selected row, or a blank for every other row.
///
/// A plain ASCII `>`, not a colour and not a `REVERSED` modifier: every
/// signal on this screen survives `NO_COLOR` and a 16-colour terminal, and a
/// decoration-only cursor does not. `▸` is East-Asian *Ambiguous* width, and a
/// terminal that renders it double-wide would shift every column of that one
/// row by a cell.
#[must_use]
pub const fn mark(selected: bool) -> &'static str {
    if selected { ">" } else { " " }
}

/// The selected row's edge: a painted space, or the ASCII marker when
/// there is no colour to paint with.
///
/// A space rather than `▌`, for the reason [`mark`] gives about `▸`:
/// every block glyph in this pane's vocabulary is East-Asian
/// *Ambiguous*, and a doubled cell in the gutter shifts the whole row.
/// A space is one column on every terminal, and the background carries
/// the whole signal.
#[must_use]
pub fn gutter(selected: bool, palette: Palette) -> (&'static str, Style) {
    match (selected, palette.ground()) {
        (false, _) => (" ", Style::default()),
        (true, ground) if ground.bg.is_some() => (" ", ground),
        (true, _) => (mark(true), Style::default()),
    }
}

/// `text` in exactly `width` display columns: padded on the right, or
/// truncated with a trailing `…`.
///
/// Counted in terminal columns, never bytes or `char`s: bytes over-pad a
/// multi-byte name, and `char`s under-pad a double-width one, shoving every
/// column after it out of line. A truncated name looking whole would be one
/// an operator types into `shep stop`.
///
/// An ANSI escape counts as the literal text a `Span` draws it as, unlike
/// [`crate::output::width::visible_width`], since nothing here writes to a
/// real terminal that would interpret it.
/// [`fit`] for a caller that already owns its text.
///
/// The common case is a cell whose text already fits its column, and there
/// `fit` copies the whole string into a second one just to pad it. Every table
/// cell arrives here as a fresh `String` from its own `*_cell` function, so a
/// wide flock table was allocating twice per cell, rows times columns times
/// thirty a second.
///
/// Truncation falls through to [`fit`], which builds a shorter string either
/// way, so the rule stays in one place.
#[must_use]
pub fn fit_owned(mut text: String, width: u16) -> String {
    let columns: usize = text.chars().map(char_columns).sum();
    let width = usize::from(width);
    if columns <= width {
        text.extend(core::iter::repeat_n(' ', width - columns));
        return text;
    }
    fit(&text, u16::try_from(width).unwrap_or(u16::MAX))
}

#[must_use]
pub fn fit(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let columns: usize = text.chars().map(char_columns).sum();
    if columns <= width {
        let mut out = String::from(text);
        out.extend(core::iter::repeat_n(' ', width - columns));
        return out;
    }
    if width == 0 {
        return String::new();
    }
    // One column pays for the `…`. A double-width character straddling the
    // boundary is dropped rather than split, and its column is padded below
    // so the cell still measures `width`.
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let c_width = char_columns(c);
        if used + c_width > budget {
            break;
        }
        out.push(c);
        used += c_width;
    }
    out.push('…');
    out.extend(core::iter::repeat_n(' ', budget - used));
    out
}

/// Fills the gap between `used` and `width` with a trailing span in
/// `ground`, so a painted row's background reaches every column of the
/// table rather than stopping where its last cell's text does.
///
/// Ratatui only paints a `Span`'s background under the cells its own text
/// occupies. `NAME` is the only column [`super::columns::name_width`] can
/// leave short of the table's full width (it floors at
/// [`super::columns::NAME_MIN`] on a narrow terminal), so without this the
/// selected row's ground would end mid-row
/// on exactly the terminals narrow enough to need the signal most. A no-op
/// when `used >= width` or `ground` carries no colour.
pub(super) fn pad_ground(spans: &mut Vec<Span<'static>>, used: u16, width: u16, ground: Style) {
    let short = width.saturating_sub(used);
    if short > 0 {
        spans.push(Span::styled(" ".repeat(usize::from(short)), ground));
    }
}

/// Which slice of the flock is on screen, given where the cursor is.
///
/// Derived every frame from
/// [`super::super::super::app::App::selected_index`] rather than stored
/// beside it: a stored offset and a stored cursor can disagree,
/// and this way they cannot. The selection is centred where the flock is long
/// enough to allow it and pinned at both ends where it is not, so the last row
/// of the flock is always the last row of the pane.
#[must_use]
pub fn scroll_offset(selected: usize, viewport: usize, total: usize) -> usize {
    if viewport == 0 || total <= viewport {
        return 0;
    }
    let last = total - viewport;
    selected.saturating_sub(viewport / 2).min(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::width::char_columns;
    use std::ffi::OsStr;

    #[test]
    fn the_painted_gutter_is_one_column_and_holds_no_glyph() {
        let deep = Palette::detect(None, None, Some(OsStr::new("truecolor")));
        let (text, style) = gutter(true, deep);
        assert_eq!(
            text, " ",
            "a space, not a block: a block is Ambiguous width"
        );
        assert_eq!(
            crate::output::width::char_columns(text.chars().next().unwrap()),
            1
        );
        assert!(style.bg.is_some());
    }

    #[test]
    fn without_colour_the_gutter_falls_back_to_the_ascii_marker() {
        let off = Palette::detect(Some(OsStr::new("1")), None, None);
        let (text, style) = gutter(true, off);
        assert_eq!(text, ">");
        assert_eq!(style, Style::default());
        assert_eq!(gutter(false, off).0, " ");
    }

    #[test]
    fn a_name_too_long_for_its_column_ends_in_an_ellipsis() {
        let cut = fit("payments-reconciliation-worker", 12);
        assert_eq!(cut.chars().count(), 12);
        assert!(cut.ends_with('…'));
        assert!(cut.starts_with("payments"));
        assert_eq!(fit("web", 12), "web         ");
    }

    /// Asserts on content, not length: `fit(..).chars().count() == width`
    /// alone is `width` either way, blind to a byte-vs-char mutation. The
    /// pad branch under-pads a multi-byte name; the truncate branch cuts a
    /// string that already fits.
    #[test]
    fn fit_counts_columns_not_bytes_when_it_pads_and_when_it_truncates() {
        // Pad branch. `日本語` is 9 bytes and 6 columns: measured correctly
        // it fills a 6-wide cell, byte-counted it falls into the truncate
        // branch.
        assert_eq!(fit("日本語", 6), "日本語");
        // Exactly fits: 7 columns, 11 bytes. A byte count would cut it to
        // `ünïcöd…`.
        assert_eq!(fit("ünïcödé", 7), "ünïcödé");
    }

    /// A `char` count gives `日本語` three and lets it into a 3-wide cell it
    /// draws six columns in, and gives `日本語アプリ` a five-`char` prefix
    /// of `日本語ア…` that draws nine.
    ///
    /// Asserts on content and on measured columns: `chars().count()` alone
    /// is blind to the mutation, and here so is `len()`.
    #[test]
    fn a_double_width_name_is_cut_to_the_columns_it_draws_in() {
        // Truncate branch. Budget is 4 columns plus the `…`: two characters
        // fit, the third would draw past the cell.
        assert_eq!(fit("日本語アプリ", 5), "日本…");
        assert_eq!(columns_of(&fit("日本語アプリ", 5)), 5);
        // A `char` count would call this a pad (3 chars into 3 columns) and
        // emit `日本語`, six columns wide, with no `…` to say it was cut.
        assert_eq!(fit("日本語", 3), "日…");
        // The odd-width case the padding exists for: `日` fits the 2-column
        // budget, `本` does not, and half a character is not drawable, so
        // the leftover column is a space rather than a short cell.
        assert_eq!(fit("日本語", 4), "日… ");
        assert_eq!(columns_of(&fit("日本語", 4)), 4);
        // Nothing but the marker fits, at either width.
        assert_eq!(fit("日本語", 1), "…");
        assert_eq!(fit("日本語", 2), "… ");
    }

    /// Every caller concatenates cells with two-space separators and no
    /// caller re-measures, so one short cell shifts every column after it
    /// on that row, invisibly in a single cell's own test.
    #[test]
    fn every_cell_measures_exactly_the_width_it_was_given() {
        let names = [
            "web",
            "payments-reconciliation-worker",
            "日本語アプリ",
            "café",
            "cafe\u{301}",
            "羊",
            "",
        ];
        for name in names {
            for width in 0..=12u16 {
                let cell = fit(name, width);
                assert_eq!(
                    columns_of(&cell),
                    usize::from(width),
                    "fit({name:?}, {width}) == {cell:?}"
                );
            }
        }
    }

    /// An ANSI escape is text here, not styling. Measuring it as zero, which
    /// [`crate::output::width::visible_width`] does on its own path, would
    /// let a hostile log line claim more columns than the cell it was cut
    /// to fit.
    #[test]
    fn an_escape_sequence_is_measured_as_the_text_it_will_be_drawn_as() {
        let styled = "\u{1b}[32mup";
        // ESC is zero-width; `[32mup` is six columns.
        assert_eq!(columns_of(styled), 6);
        assert_eq!(columns_of(&fit(styled, 4)), 4);
        assert!(fit(styled, 4).ends_with('…'));
    }

    /// The columns a rendered cell actually draws in, by the same rule
    /// [`fit`] pads by. Not `chars().count()`: that is the measurement
    /// under test.
    fn columns_of(s: &str) -> usize {
        s.chars().map(char_columns).sum()
    }

    /// Colour and a `REVERSED` modifier are both rejected: every signal on
    /// this screen has to survive `NO_COLOR` and a 16-colour terminal. `>`
    /// rather than `▸`, since `▸` is East-Asian Ambiguous width and a
    /// terminal rendering it double-wide shifts every column of that row.
    #[test]
    fn the_marker_is_one_ascii_column_wide_in_both_states() {
        // The two literals are the test: `">"` is one ASCII char by
        // inspection, so a separate length/`is_ascii()` check is redundant.
        assert_eq!(
            mark(true),
            ">",
            "not `▸`: East-Asian Ambiguous width would shift the row"
        );
        assert_eq!(mark(false), " ");
    }

    #[test]
    fn the_offset_keeps_the_selection_visible_and_centred_where_it_can() {
        // Everything fits: no scrolling, wherever the cursor is.
        assert_eq!(scroll_offset(0, 10, 6), 0);
        assert_eq!(scroll_offset(5, 10, 6), 0);
        // Taller than the viewport: centred in the middle, pinned at the ends.
        assert_eq!(scroll_offset(0, 5, 20), 0);
        assert_eq!(scroll_offset(2, 5, 20), 0);
        assert_eq!(scroll_offset(3, 5, 20), 1);
        assert_eq!(scroll_offset(10, 5, 20), 8);
        assert_eq!(scroll_offset(19, 5, 20), 15, "the last page, not past it");
        assert_eq!(scroll_offset(usize::MAX, 5, 20), 15);
        // Degenerate: a viewport of zero rows scrolls nowhere.
        assert_eq!(scroll_offset(3, 0, 20), 0);
        // And the selection is always inside the window it returns.
        for total in [1usize, 2, 7, 40, 200] {
            for viewport in [1usize, 3, 8, 25] {
                for selected in 0..total {
                    let offset = scroll_offset(selected, viewport, total);
                    assert!(
                        selected >= offset && selected < offset + viewport,
                        "selected {selected} fell outside [{offset}, {}) for total {total}",
                        offset + viewport
                    );
                }
            }
        }
    }
}
