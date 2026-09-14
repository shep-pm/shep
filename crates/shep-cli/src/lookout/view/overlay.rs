//! The box, the borderless fallback's frame, and the mute pass behind
//! them. Shared by the close dialog (1g) and the keymap overlay (1k).
//!
//! Both frames draw a bordered box over a dimmed body, differing in one
//! number: 1g's interior is 86 cells and 1k's is 126. Written once here
//! rather than twice, because two box drawers differing in a constant is
//! the shape four `create_*_file` helpers took in shep-core before one
//! helper replaced them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use super::super::theme::Palette;

/// The border's four corners, checked against `unicodedata.east_asian_width`
/// and found Neutral, same as [`BOX_LEFT`].
///
/// `pub(super)`: `keymap`'s own tests build the boxed top border's full
/// shape (`BOX_TOP_LEFT` + `INTERIOR` copies of `BOX_TOP` + `BOX_TOP_RIGHT`)
/// from these directly, rather than checking any one glyph in isolation —
/// several of them (`▛ ▜ ▙ ▟ ▀`) also appear in the keymap's own sheep art,
/// so a single-glyph check proves nothing about the border specifically.
pub(super) const BOX_TOP_LEFT: char = '▛';
pub(super) const BOX_TOP_RIGHT: char = '▜';
const BOX_BOTTOM_LEFT: char = '▙';
const BOX_BOTTOM_RIGHT: char = '▟';

/// The left edge. Neutral, unlike the other three edge glyphs below.
const BOX_LEFT: char = '▐';

/// The top, bottom and right edges. All three are East-Asian Ambiguous,
/// checked the same way the rulings ask `▌` to be. Kept anyway: `─` already
/// draws every hairline rule in this pane at full width and `█░` fill every
/// gauge, both Ambiguous too, so "no Ambiguous glyph" was never this
/// codebase's bar. The right edge is the one with real exposure, since no
/// Neutral right-half block exists to swap `▌` for and a terminal that
/// doubles it shifts every interior row;
/// `the_border_vocabulary_is_the_one_that_was_checked` pins the set so a
/// later glyph change gets the same check rather than inheriting this
/// answer.
pub(super) const BOX_TOP: char = '▀';
const BOX_BOTTOM: char = '▄';
const BOX_RIGHT: char = '▌';

/// The narrowest terminal that draws a box with an `interior`-cell inside.
///
/// The interior, a border cell each side, and a margin cell each side. 1g's
/// 86 gives 90 and 1k's 126 gives 130, and
/// `draw_boxed`'s own `margin` arithmetic comes out at 1 at either floor.
///
/// `docs/lookout/design-files/README.md` states 132 for 1k, which is a
/// two-cell margin 1g does not ask for. Corrected there rather than
/// special-cased here, so this stays one expression for both frames.
///
/// That file, not `rulings.md`: the rulings never give 1k a width, so the
/// 132 is a frame's own arithmetic slip rather than a ruling to overturn.
pub(super) const fn floor_for(interior: u16) -> u16 {
    // `saturating_add`, for the reason `draw_boxed` gives two functions down:
    // this module protects every addition, and one bare `+` reads as an
    // oversight whether or not it is one. Round 5 raised `interior + 4` here
    // and I declined it as unreachable, which it is at a const 126. Round 11
    // raised it again against the policy I had written in between, which is
    // the better argument: consistency a reader can check beats a
    // reachability argument a reader has to reconstruct.
    interior.saturating_add(4)
}

/// Whether a `width`-column terminal draws the box, or gives way to the
/// borderless form.
pub(super) const fn is_boxed(width: u16, interior: u16) -> bool {
    width >= floor_for(interior)
}

/// The rows a boxed overlay occupies: its own lines plus a border above
/// and below.
///
/// Both the fit check and the draw read this rather than each doing the
/// addition, because they did it differently once. The check saturated
/// from a `u16::MAX` fallback and the draw added plainly from a `0` one,
/// so a `lines.len()` past `u16::MAX` would have refused to draw in one
/// place and drawn a two-row box in the other. Neither is reachable with
/// a dialog of a dozen rows, which is why nothing caught it; one function
/// is what stops it coming back.
pub(super) fn boxed_height(lines: &[Line<'static>]) -> u16 {
    u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
}

/// The boxed form: `interior` cells wide, centred in `area`, its rows
/// vertically centred too.
///
/// `lines` comes from the caller, which has already measured them against
/// `area.height` to decide this form fits at all.
///
/// `ground` is the interior's own background, passed straight to
/// [`blank_row`] for every row: 1g passes [`Style::reset`], approximating
/// nothing, and 1k passes `palette.ground()`, the paper-2 panel the design
/// calls for. One parameter rather than two box drawers differing in it,
/// the same reasoning this module's own doc gives for not duplicating the
/// box at all.
pub(super) fn draw_boxed(
    lines: &[Line<'static>],
    interior: u16,
    palette: Palette,
    ground: Style,
    area: Rect,
    buffer: &mut Buffer,
) {
    let box_height = boxed_height(lines);
    let rows = box_height.saturating_sub(2);
    // `saturating_add`, not `+`, on `interior` specifically: it is a const
    // 126 at both call sites and 65534 is unreachable, but `boxed_height`
    // right above this line saturates the same class of addition, and one
    // bare `+` between two saturating ones reads as an oversight whether or
    // not it is. The rest of this function's arithmetic is screen
    // coordinates, `area`'s own fields and small row offsets, bounded by a
    // real terminal's dimensions rather than by a value this module hands
    // out; those stay bare.
    let margin = area.width.saturating_sub(interior.saturating_add(2)) / 2;
    let box_x = area.x + margin;
    let box_y = area.y + area.height.saturating_sub(box_height) / 2;
    let line_style = palette.line();
    // Built once rather than per row. `Buffer::set_string` wants a `&str` and
    // the glyphs are `char` consts, so each row was allocating two one-glyph
    // strings and one `interior`-wide blank inside the loop below.
    let left = BOX_LEFT.to_string();
    let right = BOX_RIGHT.to_string();
    let blank = blank_of(interior);

    buffer.set_string(
        box_x,
        box_y,
        format!(
            "{BOX_TOP_LEFT}{}{BOX_TOP_RIGHT}",
            BOX_TOP.to_string().repeat(usize::from(interior))
        ),
        line_style,
    );
    for (offset, line) in lines.iter().enumerate() {
        // `expect`, not `unwrap_or(0)`: the fallback wrote line 65536 over
        // line 0, which is the silent-wrong-position failure this module's
        // own `boxed_height` doc spends a paragraph arguing against. The
        // caller has already measured `lines` against `area.height` to pick
        // this form, so a count past `u16` cannot reach here.
        let offset = u16::try_from(offset).expect("a boxed form is at most area.height lines");
        let y = box_y + 1 + offset;
        buffer.set_string(box_x, y, &left, line_style);
        blank_row(buffer, box_x + 1, y, &blank, ground);
        buffer.set_line(box_x + 1, y, line, interior);
        buffer.set_string(box_x + 1 + interior, y, &right, line_style);
    }
    buffer.set_string(
        box_x,
        box_y + 1 + rows,
        format!(
            "{BOX_BOTTOM_LEFT}{}{BOX_BOTTOM_RIGHT}",
            BOX_BOTTOM.to_string().repeat(usize::from(interior))
        ),
        line_style,
    );
}

/// `blank`'s worth of plain space at `(x, y)`, in `style`: the dialog itself
/// is never muted, only the pane behind it. [`blank_of`] builds the argument,
/// and every caller builds it once above its own row loop.
///
/// [`Buffer::set_line`] only ever writes as many cells as its `Line` carries
/// content for, so a blank separator row (`Line::from(Span::raw(""))`,
/// [`close_dialog_lines`]'s own two of them) writes nothing and would leave
/// whatever the field list drew there showing through, muted, in the middle
/// of what is meant to read as a solid dialog. Called ahead of every row
/// this module draws the dialog's own lines into, boxed or not.
///
/// `style` is the caller's own choice of interior background: 1g passes
/// [`Style::reset`] both here and through [`draw_boxed`], so its four
/// pinned snapshots see no change; 1k passes `palette.ground()`.
/// [`Buffer::set_line`] then patches each span's own style onto whatever
/// `style` already set, so a foreground-only span (every span this module
/// draws) leaves this row's background alone underneath it.
pub(super) fn blank_row(buffer: &mut Buffer, x: u16, y: u16, blank: &str, style: Style) {
    buffer.set_string(x, y, blank, style);
}

/// A row's worth of spaces, for [`blank_row`] to write.
///
/// Split from it because all three callers draw inside a loop over rows and
/// `blank_row` used to take a `width` and allocate the string itself, once per
/// row. Hoisting is free here in a way that replacing a derived constant with
/// a literal is not: the width still comes from one expression, and only the
/// allocation moves.
pub(super) fn blank_of(width: u16) -> String {
    " ".repeat(usize::from(width))
}

/// Dims everything already drawn in `area`, so an overlay reads as a
/// question about what is behind it rather than as a new screen.
///
/// Two calls, not one: `Buffer::set_style` (ratatui-core 0.1.2,
/// `buffer/buffer.rs:405`) patches a cell rather than replacing it, so a
/// single `palette.muted()` would leave a title band's reverse video and a
/// selected row's own ground sitting under the new ink. `Style::reset()`
/// clears both back to the terminal's default first; `palette.muted()` then
/// repaints the one ink the overlay leaves the body in. Under `NO_COLOR` the
/// second call is a no-op (`Palette::muted` has no colour to give), so only
/// the reset runs and the body goes completely flat, which is the right
/// outcome there: the border and the reverse-video heading carry the
/// separation on their own.
pub(super) fn mute(buffer: &mut Buffer, area: Rect, palette: Palette) {
    buffer.set_style(area, Style::reset());
    buffer.set_style(area, palette.muted());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::width::char_columns;

    /// The two rows a border costs, and the saturation this function's own
    /// doc spends a paragraph on.
    ///
    /// That paragraph is the reason the helper exists: the fit check and the
    /// draw did the addition separately and differently, one saturating from
    /// a `u16::MAX` fallback and one adding plainly from `0`. Nothing
    /// exercised the arm it describes, so round 7 asked for this.
    ///
    /// 65_534 lines is where `saturating_add` starts to bite, and it is
    /// unreachable from any caller: the overlay's own form is nineteen rows.
    /// Pinned anyway, because an unreachable arm the doc argues about is
    /// exactly the kind that gets "simplified" into a plain `+ 2` by someone
    /// who reads the code and not the comment.
    #[test]
    fn the_box_costs_two_rows_and_saturates_rather_than_wrapping() {
        assert_eq!(boxed_height(&[]), 2);
        assert_eq!(boxed_height(&vec![Line::default(); 17]), 19);
        assert_eq!(
            boxed_height(&vec![Line::default(); usize::from(u16::MAX) - 1]),
            u16::MAX
        );
        assert_eq!(
            boxed_height(&vec![Line::default(); usize::from(u16::MAX) + 5]),
            u16::MAX,
            "past u16 the try_from fallback takes over, and it saturates too"
        );
    }

    /// The floor is the interior plus a border cell and a margin cell each
    /// side. 1g's own floor is 90 over an 86-cell interior and 1k's is 130
    /// over a 126-cell one, and both come out of this one expression: the
    /// design's own README states 132 for 1k, which would be a two-cell
    /// margin neither frame asks for.
    #[test]
    fn both_frames_floors_come_out_of_one_expression() {
        assert_eq!(floor_for(86), 90, "1g");
        assert_eq!(floor_for(126), 130, "1k");
    }

    /// The box draws at its floor and gives way one column under it.
    #[test]
    fn the_floor_is_the_narrowest_boxed_width() {
        assert!(is_boxed(90, 86));
        assert!(!is_boxed(89, 86));
        assert!(is_boxed(130, 126));
        assert!(!is_boxed(129, 126));
    }

    /// The three the check found. `▐` is Neutral and the four corners are
    /// too; `▀`, `▄` and `▌` are East-Asian Ambiguous, and a terminal that
    /// doubles the right edge shifts every interior row. Recorded rather
    /// than fixed, since no Neutral right-half block exists to swap in.
    /// `▘` (U+2598) joined for the keymap overlay's sheep and `⌫`
    /// (U+232B) for its text-mode row: both are below the design README's
    /// U+2600 ceiling but absent from the design's own vocabulary table, so
    /// neither inherits this test's answer without being in it.
    #[test]
    fn the_border_vocabulary_is_the_one_that_was_checked() {
        for glyph in ['▛', '▜', '▙', '▟', '▐', '▀', '▄', '▌', '▘', '⌫'] {
            assert_eq!(char_columns(glyph), 1, "{glyph}");
        }
    }
}
