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

/// The heading row: `Group::DRAWN`'s four headings, each left-aligned in
/// its own [`COLUMN`]-wide cell, in reverse video over the group's own
/// role. Padded through [`fit`] rather than concatenated raw, so the
/// `REVERSED` modifier paints the whole cell and not just the word.
fn heading_line(palette: Palette) -> Line<'static> {
    let mut spans = Vec::with_capacity(usize::from(COLUMN_COUNT) * 2 - 1);
    for (index, group) in Group::DRAWN.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(usize::from(GUTTER))));
        }
        spans.push(Span::styled(
            fit(group.heading(), COLUMN),
            palette.band(group.role()),
        ));
    }
    Line::from(spans)
}

/// One entry row: four cells, one per drawn group, joined by [`GUTTER`]
/// spaces.
fn entry_line(index: usize, grouped: &[Vec<Binding>], palette: Palette) -> Line<'static> {
    let mut spans = Vec::new();
    for (column, group) in Group::DRAWN.into_iter().enumerate() {
        if column > 0 {
            spans.push(Span::raw(" ".repeat(usize::from(GUTTER))));
        }
        spans.extend(entry_cell(
            group,
            grouped[column].get(index).copied(),
            index,
            palette,
        ));
    }
    Line::from(spans)
}

/// One [`COLUMN`]-wide cell: the group's own row at `index` if it has one,
/// styled `keys` in [`Palette::attention`] and `does` unstyled (the design's
/// own ink-2, "default fg"); the sheep at entry rows five through eight of
/// the DOING column once that group runs out; a blank cell otherwise.
fn entry_cell(
    group: Group,
    binding: Option<Binding>,
    index: usize,
    palette: Palette,
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
    if group == Group::Doing
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
fn gate_line(app: &App, palette: Palette, interior: u16) -> Line<'static> {
    let text = if matches!(app.link(), Link::Lost { .. }) {
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
    };
    Line::from(Span::styled(fit(&text, interior), palette.attention()))
}

/// The two lines that close the box: the `NO_COLOR` disclosure, and the
/// keys that leave the overlay or lookout itself.
///
/// The second line's quit caption comes from [`Group::Closing`]'s own row
/// rather than a literal, so the two cannot drift the way a hand-copied
/// string would.
fn closing_lines(all_rows: &[Binding], palette: Palette, interior: u16) -> [Line<'static>; 2] {
    let colour_sentence = " colour is decoration only: every coloured cell says the same \
                            thing in words. NO_COLOR loses nothing but the colour.";
    let quit = all_rows
        .iter()
        .find(|row| row.group == Group::Closing)
        .expect("binding() always gives Quit a Closing row");
    let closes_and_quits = format!(" h or ?  closes this  \u{b7}  {}  quits lookout", quit.keys);
    [
        Line::from(Span::styled(
            fit(colour_sentence, interior),
            palette.muted(),
        )),
        Line::from(Span::styled(
            fit(&closes_and_quits, interior),
            palette.muted(),
        )),
    ]
}

/// The overlay's rows, in order: the heading, [`ENTRY_ROWS`] entry rows, a
/// blank, the gate line, and the two closing lines.
pub(super) fn lines(app: &App, interior: u16) -> Vec<Line<'static>> {
    let palette = app.palette();
    let all_rows = rows();
    let grouped: Vec<Vec<Binding>> = Group::DRAWN
        .into_iter()
        .map(|group| {
            all_rows
                .iter()
                .copied()
                .filter(|row| row.group == group)
                .collect()
        })
        .collect();

    let mut out = Vec::with_capacity(ENTRY_ROWS + 5);
    out.push(heading_line(palette));
    out.extend((0..ENTRY_ROWS).map(|index| entry_line(index, &grouped, palette)));
    out.push(Line::default());
    out.push(gate_line(app, palette, interior));
    out.extend(closing_lines(&all_rows, palette, interior));
    out
}

/// The overlay, boxed at [`INTERIOR`] and above; nothing draws under that
/// floor or when the box is taller than `area`.
///
/// Task 7: the borderless fallback for a terminal under `overlay::floor_for(INTERIOR)`.
pub(super) fn draw(app: &App, area: Rect, buffer: &mut Buffer) {
    let palette = app.palette();
    if overlay::is_boxed(area.width, INTERIOR) {
        let lines = lines(app, INTERIOR);
        if overlay::boxed_height(&lines) <= area.height {
            overlay::draw_boxed(&lines, INTERIOR, palette, palette.ground(), area, buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

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
        assert_eq!(COLUMN, KEY_CELL + GAP + TEXT_CELL);
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
        for (index, group) in Group::DRAWN.iter().enumerate() {
            let offset = usize::from(COLUMN + GUTTER) * index;
            // +1 for the box's own left border cell, and the box starts at
            // (160 - 128) / 2 = 16.
            let start = 16 + 1 + offset;
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
        assert!(
            row.find(SHEEP[0]).is_some_and(|at| at >= 113),
            "the sheep is left of the DOING column: {row:?}"
        );
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
