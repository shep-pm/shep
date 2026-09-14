//! The keymap overlay, frame 1k: every key lookout binds, grouped by what
//! it does, boxed over the dimmed body.
//!
//! The rows come from [`crate::lookout::keymap::rows`], which derives them
//! by running `map_key`. Nothing here decides which keys exist.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::app::{App, Control, Link};
use super::super::keymap::{Binding, ENTRY_ROWS, Group, KEY_CELL, TEXT_CELL, rows};
use super::super::theme::Palette;
use super::flock::fit;
use super::{overlay, status};

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
pub(super) const INTERIOR: u16 = 126;

const COLUMN_COUNT: u16 = 4;
const GAP: u16 = 1;
const GUTTER: u16 = 2;
const COLUMN: u16 = KEY_CELL + GAP + TEXT_CELL;

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
const SHEEP: [&str; 4] = [
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
/// for the [`COLUMN`]-wide blank and the [`GAP`] in [`entry_cell`]. Raised
/// twice by review and declined twice, so the reason belongs here: the three
/// together allocate roughly eighty two-byte strings per drawn frame, and a
/// lookout frame is drawn on a keypress or a two-second tick, not at 60 fps.
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
/// [`Group::DRAWN`]) and each borderless bank ([`draw_borderless`]'s own
/// call, however many groups [`columns_for`] gave that bank).
fn heading_line_for(groups: &[Group], palette: Palette) -> Line<'static> {
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
/// false`, since the sheep is boxed-only (see [`borderless_lines`]'s own
/// comment on why).
fn entry_line_for(
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
            // `draw`'s call into `overlay::draw_boxed`, one background
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
fn gate_line(app: &App, palette: Palette, width: u16) -> Line<'static> {
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

/// The `NO_COLOR` disclosure. Its own function so [`Shed::Decoration`] can
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
/// Never, in practice: [`binding`] is an exhaustive match with no wildcard
/// arm, and its `KeyPress::Quit` arm is the only one that returns a
/// [`Group::Closing`] row, so [`rows`] always carries exactly one. The
/// `.expect` stays rather than a silent fallback, because the alternative
/// to panicking here is drawing a quit line that names no key at all, which
/// is a worse failure than a panic in a private function guarded by a
/// match the compiler already checks is exhaustive.
fn quit_text(all_rows: &[Binding]) -> String {
    let quit = all_rows
        .iter()
        .find(|row| row.group == Group::Closing)
        .expect("binding() always gives Quit a Closing row");
    format!(" h or ?  closes this  \u{b7}  {}  quits lookout", quit.keys)
}

/// The keys that leave the overlay or lookout itself.
///
/// Its own function because [`Shed::Decoration`] and [`Shed::Blank`] keep
/// it while dropping the `NO_COLOR` disclosure beside it, and used to reach
/// it as `closing_lines(..)[1].clone()`, building the disclosure in order
/// to throw it away. Not every shorter tier keeps it: [`Shed::Gate`] folds
/// the same text into [`folded_gate_and_quit_line`] when it has a row of
/// slack and drops it entirely at [`HEIGHT_FLOOR`], where the gate's own
/// warning is the last thing standing.
fn quit_line(all_rows: &[Binding], palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        fit(&quit_text(all_rows), width),
        palette.muted(),
    ))
}

/// The two lines that close the box: the `NO_COLOR` disclosure, and
/// [`quit_line`].
fn closing_lines(all_rows: &[Binding], palette: Palette, width: u16) -> [Line<'static>; 2] {
    [
        Line::from(Span::styled(fit(colour_sentence(), width), palette.muted())),
        quit_line(all_rows, palette, width),
    ]
}

/// [`Shed::Gate`]'s own line, one row under [`Shed::Blank`]'s two:
/// [`gate_text`] and [`quit_text`] combined into a single sentence, styled
/// [`Palette::attention`] since the gate's own warning is still the reason
/// the line exists — the quit caption rides along rather than taking over.
fn folded_gate_and_quit_line(
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
pub(super) fn lines(app: &App, interior: u16) -> Vec<Line<'static>> {
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

/// How many columns fit `width` cells with no border.
///
/// `n` columns take `n * COLUMN + (n - 1) * GUTTER`, which is `32n - 2`, so
/// the widest `n` that fits is `(width + 2) / 32`. Clamped to one at the
/// bottom, since [`MIN_TERM_WIDTH`](super::MIN_TERM_WIDTH) is 33 and one
/// column is 30, and to [`COLUMN_COUNT`] at the top, since only four groups
/// draw as columns.
const fn columns_for(width: u16) -> u16 {
    let fits = (width + GUTTER) / (COLUMN + GUTTER);
    if fits < 1 {
        1
    } else if fits > COLUMN_COUNT {
        COLUMN_COUNT
    } else {
        fits
    }
}

/// The heading plus [`ENTRY_ROWS`] entries: the floor under which the
/// borderless form has nothing left to shed but itself.
const HEIGHT_FLOOR: u16 = 13;

/// What a form of `height` rows has to give up.
///
/// The sheep is not a step of this ladder at all: it is boxed-only at every
/// tier, [`borderless_lines`]'s own doc says why. It is not a shedding step
/// because it frees nothing to shed either way — it sits at entry rows five
/// through eight of the DOING column, and those rows exist because LOOKING
/// has twelve entries.
///
/// ```text
/// 19  Boxed       everything, border pair included
/// 18  Nothing     borderless: a box cannot shed its border pair
/// 17  Nothing     borderless, everything
/// 16  Decoration  the NO_COLOR line goes
/// 15  Blank       and the blank separator
/// 14  Gate        and the gate line, folded onto the closing line
/// 13  Gate        the floor: the heading and twelve entry rows
/// 12  Refuse
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shed {
    Boxed,
    Nothing,
    Decoration,
    Blank,
    Gate,
    Refuse,
}

/// [`Shed`] from `height` alone, the ladder above as arithmetic.
///
/// `height` here is a form's own budget, already reduced by whatever
/// [`draw_borderless`] spent on banks beyond the first: the ladder is one
/// bank's worth of shedding, and an extra bank costs rows before this
/// function ever sees them.
const fn rows_for_height(height: u16) -> Shed {
    if height >= 19 {
        Shed::Boxed
    } else if height >= 17 {
        Shed::Nothing
    } else if height == 16 {
        Shed::Decoration
    } else if height == 15 {
        Shed::Blank
    } else if height >= HEIGHT_FLOOR {
        Shed::Gate
    } else {
        Shed::Refuse
    }
}

/// The overlay, boxed at [`INTERIOR`] and above; the borderless fallback
/// below that floor, or whenever the box itself would run taller than
/// `area`.
pub(super) fn draw(app: &App, area: Rect, buffer: &mut Buffer) {
    let palette = app.palette();
    if overlay::is_boxed(area.width, INTERIOR) {
        let lines = lines(app, INTERIOR);
        if overlay::boxed_height(&lines) <= area.height {
            overlay::draw_boxed(&lines, INTERIOR, palette, palette.ground(), area, buffer);
            return;
        }
    }
    draw_borderless(app, area, buffer, palette);
}

/// The borderless fallback: no border, drawn at `area`'s own width. Groups
/// wrap into banks of [`columns_for`]`(area.width)` columns, banks
/// separated by a blank row; [`rows_for_height`] then says how much of the
/// trailing decoration this many rows, after paying for every bank beyond
/// the first, still has room for.
fn draw_borderless(app: &App, area: Rect, buffer: &mut Buffer, palette: Palette) {
    let all_rows = rows();
    let columns = usize::from(columns_for(area.width));
    let banks: Vec<&[Group]> = Group::DRAWN.chunks(columns).collect();
    // Each bank beyond the first costs its own floor (`HEIGHT_FLOOR`) plus
    // the blank row that separates it from the one before.
    // `expect`, not `unwrap_or(u16::MAX)`: `banks` is `Group::DRAWN`
    // chunked by at least one column, so it holds at most four, and the
    // fallback was unreachable. It was also the wrong fallback, since
    // `u16::MAX` here would have made `extra_banks_cost` below overflow
    // rather than refuse: a silent wrong answer where a named panic says
    // which invariant broke.
    let bank_count = u16::try_from(banks.len()).expect("at most COLUMN_COUNT banks of groups");
    let extra_banks_cost = bank_count.saturating_sub(1) * (HEIGHT_FLOOR + 1);
    let effective_height = area.height.saturating_sub(extra_banks_cost);
    let shed = rows_for_height(effective_height);

    let lines = if shed == Shed::Refuse {
        let needed = extra_banks_cost + HEIGHT_FLOOR;
        // `palette.refusal()`, the same bark-coloured style the secrets
        // pane's own too-narrow refusal and the status bar use: the design
        // reserves it for exactly this case, so an unstyled line here would
        // read as ordinary body text while every other refusal in the app
        // is marked.
        vec![Line::from(Span::styled(
            format!(
                "the keymap needs {needed} rows, this terminal has {}",
                area.height
            ),
            palette.refusal(),
        ))]
    } else {
        borderless_lines(
            &banks,
            &all_rows,
            app,
            palette,
            area.width,
            effective_height,
            shed,
        )
    };

    for (offset, line) in lines.iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        if offset >= area.height {
            break;
        }
        let y = area.y + offset;
        overlay::blank_row(buffer, area.x, y, area.width, palette.ground());
        buffer.set_line(area.x, y, line, area.width);
    }
}

/// The borderless form's lines, once [`draw_borderless`] has already ruled
/// out [`Shed::Refuse`].
fn borderless_lines(
    banks: &[&[Group]],
    all_rows: &[Binding],
    app: &App,
    palette: Palette,
    width: u16,
    effective_height: u16,
    shed: Shed,
) -> Vec<Line<'static>> {
    // The sheep is boxed-only, at every shed tier, not just from
    // `Shed::Decoration` down: it is the one element in the whole TUI that
    // carries no information, and the borderless form only exists because
    // the frame itself could not be drawn. A decoration is the first thing
    // to go once the frame is gone — the same reasoning that drops it
    // alongside the `NO_COLOR` line once the *text* starts being trimmed,
    // just one step earlier, at the point the box itself is trimmed.
    let show_sheep = false;
    let mut out = Vec::new();
    for (index, bank) in banks.iter().enumerate() {
        if index > 0 {
            out.push(Line::default());
        }
        out.push(heading_line_for(bank, palette));
        out.extend(
            (0..ENTRY_ROWS).map(|row| entry_line_for(bank, all_rows, row, palette, show_sheep)),
        );
    }

    match shed {
        Shed::Boxed | Shed::Nothing => {
            out.push(Line::default());
            out.push(gate_line(app, palette, width));
            out.extend(closing_lines(all_rows, palette, width));
        }
        Shed::Decoration => {
            out.push(Line::default());
            out.push(gate_line(app, palette, width));
            out.push(quit_line(all_rows, palette, width));
        }
        Shed::Blank => {
            out.push(gate_line(app, palette, width));
            out.push(quit_line(all_rows, palette, width));
        }
        Shed::Gate => {
            // One row of slack over `HEIGHT_FLOOR` buys the folded line;
            // none leaves the floor as everything the form shows.
            if effective_height > HEIGHT_FLOOR {
                out.push(folded_gate_and_quit_line(app, all_rows, palette, width));
            }
        }
        Shed::Refuse => unreachable!("draw_borderless already returned on Shed::Refuse"),
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::MIN_TERM_WIDTH;
    use super::*;
    use crate::lookout::app::{KeyPress, Msg};
    use crate::lookout::frames::render_text;
    use crate::lookout::view::fixtures;
    use crate::output::width::visible_width;

    /// The arithmetic, asserted rather than commented.
    ///
    ///   interior 126 = 4 columns x 30 + 3 gutters x 2
    ///   column    30 = 12 key + 1 gap + 17 text
    ///   box      128 = 126 interior + 1 border each side
    ///   floor    130 = 128 + 1 margin each side
    #[test]
    fn the_columns_sum_to_the_interior() {
        // Literals, not the defining expression: COLUMN *is*
        // KEY_CELL + GAP + TEXT_CELL, so comparing them constant-folds to
        // 30 == 30 and would survive all three being wrong together.
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
            .find(|row| row.contains("MOVING"))
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

    /// Every row `keymap::rows` produces reaches the screen. The point of
    /// deriving them is lost if the drawer silently drops the tail of a
    /// column.
    ///
    /// `Group::Closing` is checked by its caption rather than its `does`,
    /// which is the four letters `quit` and would be satisfied by the
    /// closing line's own `quits lookout` whatever the drawer did with the
    /// row.
    #[test]
    fn every_derived_row_is_drawn() {
        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 48);
        for row in crate::lookout::keymap::rows() {
            let wanted = if row.group == Group::Closing {
                row.keys
            } else {
                row.does
            };
            assert!(
                rendered.contains(wanted),
                "`{wanted}` ({}) never reached the screen",
                row.keys
            );
        }
    }

    /// The gate line's three states. Each asserts the other two strings are
    /// absent, so a line carrying both answers fails.
    #[test]
    fn the_gate_line_names_the_gate_and_then_the_link() {
        let allowed = render_overlay(&app_with_overlay(), 160, 48);
        assert!(allowed.contains("control enabled"), "{allowed}");
        assert!(!allowed.contains("read-only"));
        assert!(!allowed.contains("the link is down"));

        let read_only = render_overlay(&read_only_app_with_overlay(), 160, 48);
        assert!(read_only.contains("read-only"), "{read_only}");
        assert!(!read_only.contains("control enabled"));
        assert!(!read_only.contains("the link is down"));

        let frozen = render_overlay(&frozen_app_with_overlay(), 160, 48);
        assert!(frozen.contains("the link is down"), "{frozen}");
        assert!(
            !frozen.contains("control enabled") && !frozen.contains("read-only"),
            "a frozen overlay still names the control gate: {frozen}"
        );

        // `Link::Lost` outranks `Control::ReadOnly` too, not only
        // `Control::Allowed`: a mutation that checks `Control` ahead of
        // `Link` passes the case above (`Control::Allowed` still falls
        // through to the same link-lost text by coincidence) but fails
        // here, where the read-only branch would otherwise win first.
        let frozen_read_only = render_overlay(&frozen_read_only_app_with_overlay(), 160, 48);
        assert!(
            frozen_read_only.contains("the link is down"),
            "{frozen_read_only}"
        );
        assert!(
            !frozen_read_only.contains("control enabled")
                && !frozen_read_only.contains("read-only"),
            "a frozen, read-only overlay names the control gate instead of the link: {frozen_read_only}"
        );
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
            .position(|row| row.contains("MOVING"))
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

    /// The overlay draws over every body, not only the dashboard.
    ///
    /// `App::keymap_open`'s own doc says it is reached from every body's
    /// `Help` arm, and `view::draw` returns early for each full-screen body,
    /// so the hook has to sit at all six exits. An earlier draft of this
    /// frame's brief said "at the end of `view::draw`", which would have
    /// left the overlay invisible over five of them. This is the net for
    /// that mistake.
    ///
    /// Each case asserts the body it claims to be in before rendering.
    /// Without that, `Edit` and `Settings` make the loop vacuous: neither
    /// sets `App::body` synchronously, so a case built on them renders the
    /// dashboard and passes while proving nothing.
    #[test]
    fn the_overlay_draws_over_every_body() {
        let sheep = {
            let mut app = fixtures::full_app();
            let _ = app.update(Msg::Key(KeyPress::Confirm));
            assert!(app.sheep_pane().is_some(), "not in the sheep pane");
            app
        };
        let bleats = {
            let mut app = fixtures::full_app();
            let _ = app.update(Msg::Key(KeyPress::Confirm));
            let _ = app.update(Msg::Key(KeyPress::Bleats));
            assert!(app.bleats_pane().is_some(), "not in the bleats pane");
            app
        };
        let config = {
            let app = fixtures::app_in_sheep_pane();
            assert!(app.config_pane().is_some(), "not in the config pane");
            app
        };

        for (name, mut app) in [("sheep", sheep), ("bleats", bleats), ("config", config)] {
            let _ = app.update(Msg::Key(KeyPress::Help));
            assert!(app.keymap_open(), "{name}: the overlay did not open");
            let rendered = render_overlay(&app, 160, 48);
            for group in Group::DRAWN {
                assert!(
                    rendered.contains(group.heading()),
                    "{name}: {} missing from the overlay",
                    group.heading()
                );
            }
            assert!(
                rendered.contains(&top_border_row()),
                "{name}: no box border over this body"
            );
        }
    }

    /// The body behind is dimmed, not painted over: the frame draws the
    /// overlay as a question about what is underneath.
    ///
    /// Follows `view/pane.rs`'s `the_pane_behind_the_dialog_is_muted`: read
    /// a cell outside the box's own columns, so what is read is the muted
    /// body rather than the overlay's own ground.
    #[test]
    fn the_body_behind_is_dimmed() {
        let app = app_with_overlay();
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(&app, frame))
            .expect("draw");
        let behind = terminal.backend().buffer()[(2, 4)].style();
        assert_eq!(behind.fg, fixtures::plain_dimmed().fg);
    }

    /// The interior carries the design's own paper-2 ground, the same way
    /// [`overlay::draw_boxed`] gives 1g's own box a ground when its caller
    /// asks for one: a cell inside the box has a background, and a cell
    /// outside it — the dimmed body behind — does not.
    ///
    /// In colour: [`Palette::ground`] is a no-op under [`fixtures::plain`],
    /// so a plain-palette render would pass whether or not the ground
    /// call ever reached `blank_row`.
    #[test]
    fn the_interior_carries_the_paper_two_ground() {
        let app = coloured_app_with_overlay();
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(&app, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        // Row 20 is an entry row; column 20 sits inside MOVING's own cell,
        // clear of the border glyphs at columns 16 and 143 (margin
        // (160 - 128) / 2 = 16). Column 2 is the dimmed flock table behind
        // the box, on the same row.
        // `Style::reset()` sets `bg` to `Some(Color::Reset)` rather than
        // `None` (it is a sentinel meant to overwrite whatever a `patch`
        // would otherwise leave standing), so "has a ground" means "some
        // colour other than `Color::Reset`", not merely `bg.is_some()`.
        let ground = app.palette().ground().bg;
        assert!(
            !matches!(ground, None | Some(ratatui::style::Color::Reset)),
            "fixtures::coloured() should give `Palette::ground` a real colour"
        );
        let inside = buffer[(20, 20)].style();
        assert_eq!(
            inside.bg, ground,
            "the interior does not carry the paper-2 ground: {inside:?}"
        );
        let outside = buffer[(2, 20)].style();
        assert_ne!(
            outside.bg, ground,
            "the dimmed body behind the box carries the interior's own ground: {outside:?}"
        );
    }

    /// `n` columns take `32n - 2` cells, so `n = (width + 2) / 32`, clamped
    /// to one through four.
    ///
    /// Each boundary is asserted from both sides. The 126..129 band is the
    /// only place the full four-column layout draws unboxed, and it is four
    /// widths wide: the box needs two cells the columns themselves do not.
    ///
    /// The last two assert the clamp rather than a boundary, and no caller
    /// can reach it. `draw_borderless` runs below the box floor, and the
    /// widest width below that floor fits `COLUMN_COUNT` columns exactly,
    /// for any constants [`the_columns_sum_to_the_interior`] accepts:
    ///
    /// ```text
    /// INTERIOR = N*C + (N-1)*G          the identity that test holds
    /// widest   = INTERIOR + 4 - 1       one under `overlay::floor_for`
    /// fits     = (widest + G)/(C + G) = N + 3/(C + G) = N   for C + G > 3
    /// ```
    ///
    /// So a mutation raising the clamp's threshold survived the whole
    /// suite, and it would have survived a narrower `KEY_CELL` too, because
    /// narrowing a cell narrows `INTERIOR` and lowers the floor with it.
    /// The arm is the second net under that identity rather than a guard
    /// against any width a terminal can have: it fires only once the
    /// identity is broken, which is the one thing that test exists to stop.
    /// Kept because `columns_for`'s own doc promises the clamp, and pinned
    /// here since nothing else can reach it.
    ///
    /// `u16::MAX - GUTTER` rather than `u16::MAX`: `columns_for` adds
    /// `GUTTER` before dividing, so the maximum is one `GUTTER` under the
    /// type's. A real `area.width` is nowhere near either.
    #[test]
    fn the_column_ladder_has_a_boundary_on_each_side() {
        assert_eq!(columns_for(126), 4);
        assert_eq!(columns_for(125), 3);
        assert_eq!(columns_for(94), 3);
        assert_eq!(columns_for(93), 2);
        assert_eq!(columns_for(62), 2);
        assert_eq!(columns_for(61), 1);
        assert_eq!(columns_for(MIN_TERM_WIDTH), 1);
        assert_eq!(columns_for(158), COLUMN_COUNT, "five columns' room");
        assert_eq!(columns_for(u16::MAX - GUTTER), COLUMN_COUNT);
    }

    /// The boxed form's own top border row: the top-left corner, `INTERIOR`
    /// copies of the top glyph, and the top-right corner, run together with
    /// no gap. A full-width run, not a single glyph: five of the eight
    /// border glyphs (`▛ ▜ ▙ ▟ ▀`) also appear in the sheep's own art
    /// (`SHEEP[2]` alone carries `▜`, `▀` and `▛`), so a check for any one
    /// of them in isolation would pass or fail on the sheep's presence
    /// rather than the border's.
    fn top_border_row() -> String {
        format!(
            "{}{}{}",
            overlay::BOX_TOP_LEFT,
            overlay::BOX_TOP.to_string().repeat(usize::from(INTERIOR)),
            overlay::BOX_TOP_RIGHT
        )
    }

    /// One column under the floor: no border, and still four columns.
    #[test]
    fn one_column_under_the_floor_keeps_four_columns_and_loses_the_border() {
        let app = app_with_overlay();
        let boxed = render_overlay(&app, 130, 48);
        let bare = render_overlay(&app, 129, 48);
        let border = top_border_row();
        assert!(boxed.contains(&border), "130 must be boxed: {boxed}");
        assert!(!bare.contains(&border), "129 must not be: {bare}");
        for group in Group::DRAWN {
            assert!(
                bare.contains(group.heading()),
                "{} is missing at 129 columns",
                group.heading()
            );
        }
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

    /// The heights, from the top of the ladder to the refusal.
    ///
    ///   19  boxed, everything
    ///   18  borderless (a box cannot shed its border pair)
    ///   17  borderless, everything
    ///   16  the NO_COLOR line and the sheep go
    ///   15  the blank separator goes
    ///   14  the gate line folds onto the closing line
    ///   13  the floor: the heading and twelve entry rows
    ///   12  refuse
    #[test]
    fn the_height_ladder_shows_its_boundaries() {
        assert_eq!(rows_for_height(19), Shed::Boxed);
        assert_eq!(rows_for_height(18), Shed::Nothing);
        assert_eq!(rows_for_height(17), Shed::Nothing);
        assert_eq!(rows_for_height(16), Shed::Decoration);
        assert_eq!(rows_for_height(15), Shed::Blank);
        assert_eq!(rows_for_height(14), Shed::Gate);
        assert_eq!(rows_for_height(13), Shed::Gate);
        assert_eq!(rows_for_height(12), Shed::Refuse);
    }

    /// At 16 rows the sheep and the colour sentence are gone and every key
    /// row is still there.
    ///
    /// 16, not 22: the box needs 19 rows and 22 holds it whole, so nothing
    /// sheds at 22 and this test would have passed on an unshed form. The
    /// sheep is absent here for a reason this test does not exercise: it is
    /// boxed-only at every height, not something the `Decoration` tier
    /// sheds (see [`borderless_lines`]'s own comment).
    #[test]
    fn a_short_terminal_sheds_the_decoration_and_keeps_the_keys() {
        let app = app_with_overlay();
        let rendered = render_overlay(&app, 160, 16);
        assert!(
            !rendered.contains(SHEEP[0]),
            "the sheep survived: {rendered}"
        );
        assert!(
            !rendered.contains("decoration only"),
            "the NO_COLOR line survived: {rendered}"
        );
        for row in crate::lookout::keymap::rows() {
            if row.group == Group::Closing {
                continue;
            }
            assert!(
                rendered.contains(row.does),
                "`{}` was shed with the decoration",
                row.does
            );
        }
    }

    /// Below the key rows themselves, the overlay says so rather than
    /// drawing a partial list. A key list missing rows silently is worse
    /// than one that refuses.
    ///
    /// 12 rows: one under the floor of 13, which is the heading plus the
    /// twelve entries. `MIN_HEIGHT` is 6, so the dashboard behind still
    /// draws at this height and the refusal is the overlay's own.
    #[test]
    fn too_short_refuses_instead_of_clipping() {
        let rendered = render_overlay(&app_with_overlay(), 160, 12);
        assert!(
            rendered.contains("the keymap needs"),
            "no refusal at 12 rows: {rendered}"
        );
        assert!(
            !rendered.contains("MOVING"),
            "a partial list drew anyway: {rendered}"
        );

        // Styled `palette.refusal()`, the same bark colour the secrets
        // pane's own too-narrow refusal and the status bar use, not drawn
        // as ordinary body text. `.contains` on the text above would pass
        // whatever the style is, so this checks the buffer cell directly.
        let app = coloured_app_with_overlay();
        let mut terminal = Terminal::new(TestBackend::new(160, 12)).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(&app, frame))
            .expect("draw");
        let refusal = app.palette().refusal().fg;
        assert!(
            refusal.is_some(),
            "fixtures::coloured() should give Palette::refusal a real colour"
        );
        assert_eq!(
            terminal.backend().buffer()[(0, 0)].style().fg,
            refusal,
            "the refusal line does not carry palette.refusal()'s own colour"
        );
    }

    /// A multi-bank width crossed against the height ladder.
    ///
    /// Every other height-ladder test runs at width 160, which is always
    /// one bank; every bank-grouping test runs at height 48 or 100, always
    /// above the ladder's own top. Neither exercises `extra_banks_cost` and
    /// `effective_height` together, and those two are exactly the
    /// arithmetic this task adds.
    ///
    /// Swept rather than picking two heights by hand, so the boundary is
    /// found by the test: at every height in the sweep the overlay either
    /// refuses cleanly (no heading drew, and it named the rows it needs) or
    /// both banks drew whole (every heading, and every entry row `rows()`
    /// produces). Nothing in between — a heading with no entries under it,
    /// or entries with no heading — passes either check.
    #[test]
    fn a_multi_bank_width_crosses_the_height_ladder_cleanly() {
        let app = app_with_overlay();
        let width = 100;
        // The same formula `draw_borderless` runs, from the same constants
        // `the_column_ladder_has_a_boundary_on_each_side` and
        // `the_height_ladder_shows_its_boundaries` already pin: two banks
        // at this width (columns_for(100) == 3, four groups in chunks of
        // three), so one bank beyond the first, costing its own floor plus
        // the separator row before it. This is what makes the boundary
        // below found rather than picked: it comes from the constants the
        // ladder's own tests already check, not a height chosen by hand.
        let bank_count =
            u16::try_from(Group::DRAWN.chunks(usize::from(columns_for(width))).count())
                .expect("four groups never chunk into more banks than fit a u16");
        let boundary = bank_count.saturating_sub(1) * (HEIGHT_FLOOR + 1) + HEIGHT_FLOOR;

        for height in 20..=35 {
            let rendered = render_overlay(&app, width, height);
            let headings_present = Group::DRAWN
                .iter()
                .filter(|group| rendered.contains(group.heading()))
                .count();
            let refused = rendered.contains("the keymap needs");
            let whole = headings_present == Group::DRAWN.len();

            assert!(
                refused != whole,
                "height {height} is neither a clean refusal nor a whole draw \
                 ({headings_present} of {} headings): {rendered}",
                Group::DRAWN.len()
            );
            assert_eq!(
                refused,
                height < boundary,
                "height {height} against a boundary of {boundary}: {rendered}"
            );
            if refused {
                assert_eq!(
                    headings_present, 0,
                    "height {height} refused but still drew a heading: {rendered}"
                );
            } else {
                for row in crate::lookout::keymap::rows() {
                    if row.group == Group::Closing {
                        continue;
                    }
                    assert!(
                        rendered.contains(row.does),
                        "height {height} drew the headings but not `{}`'s row: {rendered}",
                        row.does
                    );
                }
            }
        }
    }

    /// Renders one overlay and returns the screen as text.
    fn render_overlay(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(app, frame))
            .expect("draw");
        render_text(terminal.backend().buffer())
    }

    /// A healthy dashboard, control open, nothing frozen.
    fn healthy_app() -> App {
        healthy_app_with_palette(fixtures::plain())
    }

    /// The same, at `palette`: what the ground test reads, since
    /// [`Palette::ground`] is a no-op under [`fixtures::plain`].
    fn healthy_app_with_palette(palette: Palette) -> App {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .pid(Some(48_001))
                    .build(),
            ],
            palette,
        );
        app.set_control_for_tests(Control::Allowed);
        app
    }

    /// A healthy dashboard with the overlay up.
    fn app_with_overlay() -> App {
        let mut app = healthy_app();
        let _ = app.update(Msg::Key(KeyPress::Help));
        // Asserted in the helper, not left to the caller. A dozen tests
        // render from this and assert on headings, so an overlay that
        // failed to open reaches every one of them as "no heading row" or
        // "MOVING missing", which names the symptom and not the cause.
        assert!(app.keymap_open(), "the overlay did not open");
        app
    }

    /// The same, in colour, for the one test that needs an actual
    /// background to check against.
    fn coloured_app_with_overlay() -> App {
        let mut app = healthy_app_with_palette(fixtures::coloured());
        let _ = app.update(Msg::Key(KeyPress::Help));
        app
    }

    /// The same, `--read-only`, with the overlay up.
    fn read_only_app_with_overlay() -> App {
        let mut app = healthy_app();
        app.set_control_for_tests(Control::ReadOnly);
        let _ = app.update(Msg::Key(KeyPress::Help));
        app
    }

    /// The same, with the link gone, with the overlay up. `Msg::Frozen` is
    /// how `frames.rs`'s own `Scene::Frozen` raises the same state.
    fn frozen_app_with_overlay() -> App {
        let mut app = healthy_app();
        let _ = app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let _ = app.update(Msg::Key(KeyPress::Help));
        app
    }

    /// The same, `--read-only` as well: the design's own table gives the
    /// link-lost text for `Link::Lost` under either `Control`, so this is
    /// what tells `Link::Lost` outranking `Control::Allowed` apart from
    /// `Link::Lost` outranking `Control` altogether.
    fn frozen_read_only_app_with_overlay() -> App {
        let mut app = healthy_app();
        app.set_control_for_tests(Control::ReadOnly);
        let _ = app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let _ = app.update(Msg::Key(KeyPress::Help));
        app
    }
}
