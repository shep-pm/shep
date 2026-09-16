use super::super::super::app::App;
use super::super::super::keymap::{Binding, ENTRY_ROWS, Group, rows};
use super::super::super::theme::Palette;
use super::super::overlay;
use super::keymap_rows::{
    COLUMN, COLUMN_COUNT, GUTTER, INTERIOR, closing_lines, entry_line_for,
    folded_gate_and_quit_line, gate_line, heading_line_for, lines, quit_line,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

/// How many columns fit `width` cells with no border.
///
/// `n` columns take `n * COLUMN + (n - 1) * GUTTER`, which is `32n - 2`, so
/// the widest `n` that fits is `(width + 2) / 32`. Clamped to one at the
/// bottom, since [`MIN_TERM_WIDTH`](super::super::MIN_TERM_WIDTH) is 33 and one
/// column is 30, and to [`COLUMN_COUNT`] at the top, since only four groups
/// draw as columns.
pub(super) const fn columns_for(width: u16) -> u16 {
    // `saturating_add`: the one production caller passes `area.width`,
    // bounded by a real terminal, but the function's own doc promises a
    // clamp to `1..=COLUMN_COUNT` for any `u16`, and a bare `+` breaks
    // that promise above `u16::MAX - GUTTER` instead of keeping it.
    let fits = width.saturating_add(GUTTER) / (COLUMN + GUTTER);
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
pub(super) const HEIGHT_FLOOR: u16 = 13;

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
pub(super) enum Shed {
    Boxed,
    Nothing,
    Decoration,
    Blank,
    /// Two heights, and the variant alone does not say which output they
    /// get: [`borderless_lines`] draws the folded gate-and-quit line at 14
    /// and nothing at all at [`HEIGHT_FLOOR`], decided by a second condition
    /// one function deeper: `if effective_height > HEIGHT_FLOOR`. Named here
    /// because a reader holding a `Shed` value would otherwise have to find
    /// that `if` to know what it renders.
    ///
    /// "Condition", not "test": in a Rust file that word reads as
    /// `#[test]`, a reference to a test function that does not exist.
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
pub(in crate::lookout::view) fn draw(app: &App, area: Rect, buffer: &mut Buffer) {
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
pub(super) fn draw_borderless(app: &App, area: Rect, buffer: &mut Buffer, palette: Palette) {
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

    // One allocation for the whole form rather than one per row.
    let blank = overlay::blank_of(area.width);
    for (offset, line) in lines.iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        if offset >= area.height {
            break;
        }
        let y = area.y + offset;
        overlay::blank_row(buffer, area.x, y, &blank, palette.ground());
        buffer.set_line(area.x, y, line, area.width);
    }
}

/// The borderless form's lines, once [`draw_borderless`] has already ruled
/// out [`Shed::Refuse`].
pub(super) fn borderless_lines(
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
    use super::super::keymap_rows::{COLUMN_COUNT, GUTTER, SHEEP};

    use super::super::super::super::keymap::Group;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::super::MIN_TERM_WIDTH;
    use super::*;
    use crate::lookout::app::{KeyPress, Msg};

    use crate::lookout::view::fixtures;

    use super::super::testing::*;

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

    /// The overlay draws over a body other than the dashboard, and does it
    /// for four of the six.
    ///
    /// Four, not six, and the title says four rather than "every" on
    /// purpose. A universal over a set cannot be held by a test that walks
    /// a hand-written subset of it, and `Settings` and `Edit` cannot join:
    /// neither sets `App::body` synchronously, so a case built on them
    /// renders the dashboard and passes while proving nothing. The other
    /// half of the claim is structural instead, and stronger than a list:
    /// `view::draw` calls `draw_keymap_overlay` once, after `draw_body` has
    /// returned from whichever of the six exits it took.
    ///
    /// What this adds on top of that call is the rendering: that the
    /// overlay's own geometry survives over a body which is not the flock
    /// table. An earlier draft of this frame's brief said to hook it "at the
    /// end of `view::draw`", which would have left it invisible over five
    /// bodies, and this is the net for that mistake.
    ///
    /// Each case asserts the body it claims to be in before rendering, which
    /// is what keeps a case that silently fell back to the dashboard from
    /// passing.
    #[test]
    fn the_overlay_renders_over_the_four_bodies_a_test_can_reach() {
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
        let secrets = {
            let app = fixtures::app_with_secrets();
            assert!(app.secrets_pane_is_open(), "not in the secrets pane");
            app
        };

        for (name, mut app) in [
            ("sheep", sheep),
            ("bleats", bleats),
            ("config", config),
            ("secrets", secrets),
        ] {
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
            .draw(|frame| super::super::super::draw(&app, frame))
            .expect("draw");
        let behind = terminal.backend().buffer()[(2, 4)].style();
        assert_eq!(behind.fg, fixtures::plain_dimmed().fg);
    }

    /// The interior carries the design's own paper-2 ground, the same way
    /// [`overlay::draw_boxed`](crate::lookout::view::overlay::draw_boxed) gives 1g's own box a ground when its caller
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
            .draw(|frame| super::super::super::draw(&app, frame))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        // Row 20 is an entry row; column 20 sits inside MOVING's own cell,
        // clear of the border glyphs at columns 16 and 143 (margin
        // (160 - 128) / 2 = 16). Column 2, same row, is the dimmed flock
        // table behind the box.
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
        assert_every_visible_row_drawn(&rendered, "shed with the decoration");
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
            // Derived, and it matters most here: this is an ABSENCE
            // assertion, so a renamed heading would leave the old literal
            // absent for the wrong reason and the test would pass on a
            // screen that drew the heading under its new name.
            !rendered.contains(Group::Moving.heading()),
            "a partial list drew anyway: {rendered}"
        );

        // Styled `palette.refusal()`, the same bark colour the secrets
        // pane's own too-narrow refusal and the status bar use, not drawn
        // as ordinary body text. `.contains` on the text above would pass
        // whatever the style is, so this checks the buffer cell directly.
        let app = coloured_app_with_overlay();
        let mut terminal = Terminal::new(TestBackend::new(160, 12)).expect("terminal");
        terminal
            .draw(|frame| super::super::super::draw(&app, frame))
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

        // Derived from `boundary`, not the hand-picked `20..=35` this was.
        // The doc above says the boundary is found by the test, and a fixed
        // range does not deliver that: change `HEIGHT_FLOOR` so the boundary
        // leaves the range and every height lands on one side, the opposite
        // branch never runs, and the test passes having verified no
        // transition at all. The tallies below are the vacuity guard.
        let mut refusals = 0;
        let mut whole_draws = 0;
        for height in boundary.saturating_sub(4)..=boundary + 4 {
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
                refusals += 1;
                assert_eq!(
                    headings_present, 0,
                    "height {height} refused but still drew a heading: {rendered}"
                );
            } else {
                whole_draws += 1;
                assert_every_visible_row_drawn(
                    &rendered,
                    &format!("height {height} drew the headings but a row went missing"),
                );
            }
        }

        assert!(
            refusals > 0 && whole_draws > 0,
            "the sweep never crossed the boundary of {boundary}:              {refusals} refusals and {whole_draws} whole draws"
        );
    }
}
