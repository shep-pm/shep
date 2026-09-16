use super::super::super::app::{App, Control, Link};
use super::super::super::keymap::{Binding, ENTRY_ROWS, Group, KEY_CELL, TEXT_CELL, rows};
use super::super::super::theme::Palette;
use super::super::flock::fit;
use super::super::status;
use ratatui::text::{Line, Span};

/// Cells inside the border.
///
/// ```text
/// interior 126 = 4 columns x 30 + 3 gutters x 2
/// column    30 = 12 key (KEY_CELL) + 1 gap (GAP) + 17 text (TEXT_CELL)
/// box      128 = 126 interior + 1 border cell each side
/// floor    130 = 128 + 1 margin cell each side  (overlay::floor_for)
/// ```
///
/// Written out because a frame pinned one column short of its own column
/// set drops the thing it exists to show, which happened once already in
/// this bundle. `the_columns_sum_to_the_interior` asserts every line of
/// the block above.
pub(in crate::lookout::view) const INTERIOR: u16 = 126;

pub(super) const COLUMN_COUNT: u16 = 4;

const GAP: u16 = 1;

pub(super) const GUTTER: u16 = 2;

pub(super) const COLUMN: u16 = KEY_CELL + GAP + TEXT_CELL;

/// The entry row the sheep's first line draws at (zero-indexed): "entry
/// rows 5 through 8" in the design's own numbering.
const SHEEP_FIRST_ROW: usize = 4;

/// The one decoration in the whole TUI that carries no information, drawn
/// in the DOING column's own thirty cells at entry rows five through eight.
///
/// The design centres it in a 38-cell cell. The rightmost 38 cells of a
/// 126-cell interior start at 88, inside CHANGING's column (64..93), which
/// runs to entry row ten and leaves three free rows there rather than four.
/// The art is ten columns wide, so DOING's column at 96..125 holds it with
/// room. Allowed exactly once, here.
pub(super) const SHEEP: [&str; 4] = [
    "  \u{259f}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2599}",
    " \u{259f}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2599}",
    " \u{259c}\u{2588}\u{2588}\u{2580} \u{2580}\u{2588}\u{2588}\u{259b}",
    "   \u{2580}\u{2598} \u{2580}\u{2598}",
];

/// [`GUTTER`] spaces, the separator between two adjacent cells whether the
/// cells are columns of the same bank or (nowhere yet) two banks side by
/// side.
///
/// `" ".repeat` rather than a `&'static str` of the right length, here and
/// for the [`COLUMN`]-wide blank and the [`GAP`] in [`entry_cell`]: the
/// three together allocate roughly eighty two-byte strings per drawn
/// frame, and a lookout frame is drawn on a keypress or a two-second tick,
/// not at 60 fps.
///
/// Two more of the same tier, worth naming here rather than left for a
/// reader to rediscover: [`rows`] runs twice when a form fits the width
/// and not the height, since [`lines`] builds and discards one before
/// [`draw_borderless`](crate::lookout::view::keymap::shed::draw_borderless) builds another, and [`entry_cell`]'s
/// `filter().nth()` scans `all_rows` once per cell, about forty-eight
/// scans a frame.
/// What a literal costs is the derivation: `"  "` is right only while
/// `GUTTER` is 2, and nothing would say so when it changed. A width that
/// disagrees with its own constant is the defect this frame's own gallery
/// scenes exist to catch, so the constant wins over the allocation.
fn gutter_span() -> Span<'static> {
    Span::raw(" ".repeat(usize::from(GUTTER)))
}

/// `groups`' headings, each left-aligned in its own [`COLUMN`]-wide cell, in
/// reverse video over the group's own role, joined by [`GUTTER`]. Padded
/// through [`fit`] rather than concatenated raw, so the `REVERSED` modifier
/// paints the whole cell and not just the word.
///
/// Shared by the boxed heading row ([`heading_line`], all four of
/// [`Group::DRAWN`]) and each borderless bank ([`draw_borderless`](crate::lookout::view::keymap::shed::draw_borderless)'s own
/// call, however many groups [`columns_for`](crate::lookout::view::keymap::shed::columns_for) gave that bank).
pub(super) fn heading_line_for(groups: &[Group], palette: Palette) -> Line<'static> {
    // `* 2` and not `* 2 - 1`: the exact count is one gutter short of this,
    // and subtracting it underflows to `usize::MAX` on an empty slice,
    // which panics in debug and aborts on the allocation in release. Every
    // caller passes a `chunks()` bank, which is never empty, so this is one
    // spare slot against a panic site in a capacity HINT.
    let mut spans = Vec::with_capacity(groups.len() * 2);
    for (index, &group) in groups.iter().enumerate() {
        if index > 0 {
            spans.push(gutter_span());
        }
        spans.push(Span::styled(
            fit(group.heading(), COLUMN),
            palette.band(group.role()),
        ));
    }
    Line::from(spans)
}

/// The boxed form's heading row: [`heading_line_for`] over all four of
/// [`Group::DRAWN`].
fn heading_line(palette: Palette) -> Line<'static> {
    heading_line_for(&Group::DRAWN, palette)
}

/// One entry row across `groups`: one cell per group, joined by [`GUTTER`].
///
/// Shared the same way [`heading_line_for`] is: the boxed form's [`lines`]
/// calls it with all four of [`Group::DRAWN`] and `show_sheep: true`; every
/// borderless bank calls it with only its own groups and `show_sheep:
/// false`, since the sheep is boxed-only (see [`borderless_lines`](crate::lookout::view::keymap::shed::borderless_lines)'s own
/// comment on why).
pub(super) fn entry_line_for(
    groups: &[Group],
    all_rows: &[Binding],
    index: usize,
    palette: Palette,
    show_sheep: bool,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (column, &group) in groups.iter().enumerate() {
        if column > 0 {
            spans.push(gutter_span());
        }
        let binding = all_rows
            .iter()
            .copied()
            .filter(|row| row.group == group)
            .nth(index);
        spans.extend(entry_cell(group, binding, index, palette, show_sheep));
    }
    Line::from(spans)
}

/// One [`COLUMN`]-wide cell: the group's own row at `index` if it has one,
/// styled `keys` in [`Palette::attention`] and `does` unstyled (the design's
/// own ink-2, "default fg"); the sheep at entry rows five through eight of
/// the DOING column once that group runs out and `show_sheep` says so; a
/// blank cell otherwise.
fn entry_cell(
    group: Group,
    binding: Option<Binding>,
    index: usize,
    palette: Palette,
    show_sheep: bool,
) -> Vec<Span<'static>> {
    if let Some(binding) = binding {
        return vec![
            Span::styled(fit(binding.keys, KEY_CELL), palette.attention()),
            // Unstyled: the design's own colour table gives ordinary cell
            // text as ink-2, "default fg", and there is no `Palette`
            // method for it because unstyled is how it renders.
            // `palette.ground()` paints a background
            // (`Palette::ground`'s own doc: "the one painted background"),
            // and painting only this span would band the description
            // column while the key column and the headings beside it sat
            // on whatever `blank_row` had filled the rest of the row
            // with instead. The interior's own paper-2 ground comes from
            // `shed::draw`'s call into `overlay::draw_boxed`, one background
            // for the whole row, not per span: `Buffer::set_line` patches
            // a span's own style onto that rather than replacing it, so
            // this foreground-only span leaves the ground under it alone.
            Span::raw(format!(
                "{}{}",
                " ".repeat(usize::from(GAP)),
                fit(binding.does, TEXT_CELL)
            )),
        ];
    }
    if show_sheep
        && group == Group::Doing
        && let Some(row) = index
            .checked_sub(SHEEP_FIRST_ROW)
            .and_then(|row| SHEEP.get(row))
    {
        return vec![Span::raw(fit(row, COLUMN))];
    }
    vec![Span::raw(" ".repeat(usize::from(COLUMN)))]
}

/// What the three destructive keys cost, and whether they can act at all.
///
/// One full-width line rather than printed beside the three keys as the
/// frame draws it: `each one arms, ↵ confirms, 10s to answer` plus the
/// control label is 57 characters against a 17-cell text field, and split
/// over three stacked cells inside DOING it reads worse than stated once.
///
/// `Link::Lost` outranks the control gate, because `x`/`R`/`L` then refuse
/// for a reason `Control` does not carry, and a line saying `control
/// enabled` beside a dead link is false. `view/status.rs`'s own gate for
/// the status bar's right-hand label takes the same precedence, for the
/// same reason (`status::status_line`'s `Link::Lost` arm, checked ahead of
/// `Control`).
pub(super) fn gate_line(app: &App, palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        fit(&gate_text(app), width),
        palette.attention(),
    ))
}

/// [`gate_line`]'s own sentence, unfit: split out so
/// [`folded_gate_and_quit_line`] can combine it with the quit caption
/// before either is padded to a width.
fn gate_text(app: &App) -> String {
    if matches!(app.link(), Link::Lost { .. }) {
        " the three above are refused  \u{b7}  \u{2588} the link is down  \u{b7}  nothing acts"
            .to_string()
    } else {
        let label = match app.control() {
            Control::ReadOnly => status::READ_ONLY_LABEL,
            Control::Allowed => status::CONTROL_ENABLED_LABEL,
        };
        format!(
            " the three above each arm a confirm  \u{b7}  \u{21b5} confirms  \u{b7}  10s to \
             answer  \u{b7}  \u{2588} {label}"
        )
    }
}

/// The `NO_COLOR` disclosure. Its own function so [`Shed::Decoration`](crate::lookout::view::keymap::shed::Shed::Decoration) can
/// drop it without touching the quit line beside it.
const fn colour_sentence() -> &'static str {
    " colour is decoration only: every coloured cell says the same \
     thing in words. NO_COLOR loses nothing but the colour."
}

/// The quit line's unfit text: `h or ? closes this · <keys> quits lookout`,
/// with `<keys>` coming from [`Group::Closing`]'s own row rather than a
/// literal, so the two cannot drift the way a hand-copied string would.
///
/// # Panics
///
/// Never, in practice: `keymap::binding` is an exhaustive match with no
/// wildcard
/// arm, and its `KeyPress::Quit` arm is the only one that returns a
/// [`Group::Closing`] row, so [`rows`] always carries exactly one. The
/// `.expect` stays rather than a silent fallback, because the alternative
/// to panicking here is drawing a quit line that names no key at all, which
/// is a worse failure than a panic in a private function guarded by a
/// match the compiler already checks is exhaustive.
#[track_caller]
fn quit_text(all_rows: &[Binding]) -> String {
    let quit = all_rows
        .iter()
        .find(|row| row.group == Group::Closing)
        .expect("binding() always gives Quit a Closing row");
    format!(" h or ?  closes this  \u{b7}  {}  quits lookout", quit.keys)
}

/// The keys that leave the overlay or lookout itself.
///
/// Its own function because [`Shed::Decoration`](crate::lookout::view::keymap::shed::Shed::Decoration) and [`Shed::Blank`](crate::lookout::view::keymap::shed::Shed::Blank) keep
/// it while dropping the `NO_COLOR` disclosure beside it. Not every
/// shorter tier keeps it: [`Shed::Gate`](crate::lookout::view::keymap::shed::Shed::Gate) folds
/// the same text into [`folded_gate_and_quit_line`] when it has a row of
/// slack and drops it entirely at [`HEIGHT_FLOOR`](crate::lookout::view::keymap::shed::HEIGHT_FLOOR), where the gate's own
/// warning is the last thing standing.
pub(super) fn quit_line(all_rows: &[Binding], palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        fit(&quit_text(all_rows), width),
        palette.muted(),
    ))
}

/// The two lines that close the box: the `NO_COLOR` disclosure, and
/// [`quit_line`].
pub(super) fn closing_lines(
    all_rows: &[Binding],
    palette: Palette,
    width: u16,
) -> [Line<'static>; 2] {
    [
        Line::from(Span::styled(fit(colour_sentence(), width), palette.muted())),
        quit_line(all_rows, palette, width),
    ]
}

/// [`Shed::Gate`](crate::lookout::view::keymap::shed::Shed::Gate)'s own line, one row under [`Shed::Blank`](crate::lookout::view::keymap::shed::Shed::Blank)'s two:
/// [`gate_text`] and [`quit_text`] combined into a single sentence, styled
/// [`Palette::attention`] since the gate's own warning is still the reason
/// the line exists — the quit caption rides along rather than taking over.
pub(super) fn folded_gate_and_quit_line(
    app: &App,
    all_rows: &[Binding],
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let text = format!(
        "{}  \u{b7}  {}",
        gate_text(app).trim(),
        quit_text(all_rows).trim()
    );
    Line::from(Span::styled(fit(&text, width), palette.attention()))
}

/// The overlay's rows, in order: the heading, [`ENTRY_ROWS`] entry rows, a
/// blank, the gate line, and the two closing lines.
#[must_use]
pub(in crate::lookout::view) fn lines(app: &App, interior: u16) -> Vec<Line<'static>> {
    let palette = app.palette();
    let all_rows = rows();
    let mut out = Vec::with_capacity(ENTRY_ROWS + 5);
    out.push(heading_line(palette));
    out.extend(
        (0..ENTRY_ROWS).map(|index| entry_line_for(&Group::DRAWN, &all_rows, index, palette, true)),
    );
    out.push(Line::default());
    out.push(gate_line(app, palette, interior));
    out.extend(closing_lines(&all_rows, palette, interior));
    out
}

#[cfg(test)]
mod tests {

    use super::super::super::super::keymap::{Group, KEY_CELL, TEXT_CELL};

    use super::super::super::overlay;

    use super::*;

    use super::super::testing::*;
    use crate::output::width::visible_width;

    /// The arithmetic, asserted rather than commented.
    ///
    ///   interior 126 = 4 columns x 30 + 3 gutters x 2
    ///   column    30 = 12 key + 1 gap + 17 text
    ///   box      128 = 126 interior + 1 border each side
    ///   floor    130 = 128 + 1 margin each side
    #[test]
    fn the_columns_sum_to_the_interior() {
        // Literals. The form this avoids is
        // `assert_eq!(COLUMN, KEY_CELL + GAP + TEXT_CELL)`, which cannot
        // fail: `COLUMN` is defined as that sum, so moving `KEY_CELL` to 20
        // makes both sides 38 and all three constants can be wrong
        // together. Pinning each against a literal fails instead.
        assert_eq!((KEY_CELL, GAP, TEXT_CELL), (12, 1, 17));
        assert_eq!(
            COLUMN * COLUMN_COUNT + GUTTER * (COLUMN_COUNT - 1),
            INTERIOR
        );
        assert_eq!(COLUMN, 30);
        assert_eq!(INTERIOR, 126);
    }

    /// Every row of the box is exactly the interior wide, so no column
    /// drifts and the right border lands in one place on every row.
    #[test]
    fn every_row_is_the_interior_wide() {
        let app = app_with_overlay();
        for line in lines(&app, INTERIOR) {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            // Exactly the interior, not merely under it: the lone
            // exception is the blank separator row, which is an empty
            // `Line` by the same convention `overlay::blank_row`'s own
            // doc names for `close_dialog_lines`, relying on
            // `draw_boxed`'s per-row `blank_row` call to pad it rather
            // than padding it here. A width merely `<=` the interior
            // would still pass with a gutter missing from every column.
            assert!(
                text.is_empty() || visible_width(&text) == usize::from(INTERIOR),
                "a row is {} cells against {INTERIOR}: {text:?}",
                visible_width(&text)
            );
        }
    }

    /// Nineteen rows: a border pair, a heading, twelve entries, a blank,
    /// the gate line and two closing lines.
    #[test]
    fn the_box_is_nineteen_rows() {
        let app = app_with_overlay();
        assert_eq!(overlay::boxed_height(&lines(&app, INTERIOR)), 19);
    }

    /// All four headings draw, each at its own column start. Counting the
    /// starts rather than looking for the words is what catches a column
    /// that drew at the wrong offset.
    #[test]
    fn the_four_headings_sit_at_their_column_starts() {
        // `Group::DRAWN`'s literal order, pinned: the layout loop below
        // derives its expected offsets from `Group::DRAWN` itself, so it
        // stays green under a swap that moves both the heading and the
        // offset together. The design specifies MOVING, LOOKING, CHANGING,
        // DOING left to right, and this is the one assertion that would
        // catch a swap the layout loop cannot.
        assert_eq!(
            Group::DRAWN,
            [Group::Moving, Group::Looking, Group::Changing, Group::Doing]
        );

        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 48);
        let heading_row = rendered
            .lines()
            .find(|row| row.contains(Group::Moving.heading()))
            .expect("no heading row");
        // Char-indexed, not byte-indexed: the border glyph at column 16
        // (`▐`, `overlay::BOX_LEFT`) is three bytes, so a byte slice at the
        // column's own numeric offset lands inside it.
        let chars: Vec<char> = heading_row.chars().collect();
        const MARGIN: usize = 16;
        assert_eq!(usize::from(160 - (INTERIOR + 2)) / 2, MARGIN);
        for (index, group) in Group::DRAWN.iter().enumerate() {
            let offset = usize::from(COLUMN + GUTTER) * index;
            // +1 for the box's own left border cell, and the box starts at
            // (160 - 128) / 2 = 16, which `MARGIN` above pins rather than
            // derives: a literal fails on a margin bug, and the assertion
            // beside it fails if `INTERIOR` moves, instead of quietly
            // measuring the wrong column.
            let start = MARGIN + 1 + offset;
            let from_start: String = chars[start..].iter().collect();
            assert!(
                from_start.starts_with(group.heading()),
                "{} is not at column {start}: {heading_row:?}",
                group.heading()
            );
        }
    }

    /// The sheep, and only here. The design allows it exactly once.
    #[test]
    fn the_sheep_draws_in_the_doing_column() {
        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 48);
        assert!(rendered.contains(SHEEP[0]), "no sheep: {rendered}");
        // The rightmost column starts at 16 + 1 + 3 * (30 + 2) = 113.
        let row = rendered
            .lines()
            .find(|row| row.contains(SHEEP[0]))
            .expect("no sheep row");
        // `str::find` answers in BYTES and 113 is a character column, so the
        // byte index is inflated by every multi-byte glyph before it. That
        // made the old assertion a lower bound rather than a measurement.
        let column = row
            .find(SHEEP[0])
            .map(|at| row[..at].chars().count())
            .expect("no sheep on the row that contains it");
        // A pair, not a lower bound. `>= 113` alone cannot see a sheep
        // drawn too far RIGHT, and DOING is the last column, so nothing
        // else would catch it. The upper half is reachable rather than
        // decoration: shifting `overlay::draw_boxed`'s own margin by 31
        // puts the sheep at column 144 and fails here, measured.
        let doing = 113..113 + usize::from(COLUMN);
        assert!(
            doing.contains(&column),
            "the sheep starts at column {column}, outside DOING's {doing:?}: {row:?}"
        );

        // Its ROW as well as its column. `SHEEP_FIRST_ROW` is a named
        // layout constant and nothing measured it, so a sheep shifted up
        // or down inside DOING passed everything: the column assertions
        // above do not care which row they found it on.
        //
        // Counted from the heading row rather than from the top of the
        // screen, so the box's own vertical centring is not part of the
        // claim. One heading row, then `SHEEP_FIRST_ROW` entry rows, then
        // the sheep.
        // A literal, and the derivation asserted beside it. Written as
        // `1 + SHEEP_FIRST_ROW` first, which is a tautology: moving the
        // constant to 6 moved both sides of the comparison and the test
        // stayed green, so it pinned the arithmetic and not the layout.
        const UNDER_THE_HEADING: usize = 5;
        assert_eq!(1 + SHEEP_FIRST_ROW, UNDER_THE_HEADING);

        let rows: Vec<&str> = rendered.lines().collect();
        let heading_at = rows
            .iter()
            .position(|row| row.contains(Group::Moving.heading()))
            .expect("no heading row");
        let sheep_at = rows
            .iter()
            .position(|row| row.contains(SHEEP[0]))
            .expect("no sheep row");
        assert_eq!(
            sheep_at - heading_at,
            UNDER_THE_HEADING,
            "the sheep is {} rows under the heading, not {UNDER_THE_HEADING}",
            sheep_at - heading_at
        );
    }

    /// Three columns at 100, with DOING on a bank of its own below.
    #[test]
    fn three_columns_put_doing_on_its_own_bank() {
        let rendered = render_overlay(&app_with_overlay(), 100, 48);
        let heading_rows: Vec<&str> = rendered
            .lines()
            .filter(|row| Group::DRAWN.iter().any(|g| row.contains(g.heading())))
            .collect();
        assert_eq!(heading_rows.len(), 2, "{heading_rows:?}");
        // Every group, asserted by bank rather than a sample of two: at 100
        // columns the first bank carries three, so naming only MOVING and
        // CHANGING let a regression drop LOOKING and still pass.
        for group in [Group::Moving, Group::Looking, Group::Changing] {
            assert!(
                heading_rows[0].contains(group.heading()),
                "{} missing from the first bank: {:?}",
                group.heading(),
                heading_rows[0]
            );
        }
        assert!(heading_rows[1].contains(Group::Doing.heading()));
        assert!(
            !heading_rows[1].contains(Group::Moving.heading()),
            "the second bank should hold DOING alone: {:?}",
            heading_rows[1]
        );
    }
}
