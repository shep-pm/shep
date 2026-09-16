//! Assembling the pane: which lines the chrome takes, which rows the body
//! gets, and what happens when neither fits.
//!
//! Height is the scarce thing. Every line of chrome is paid for out of the
//! same budget as the rows, so this is where a short terminal sheds the
//! legend, then the tab row, and finally everything but the cursor.

use ratatui::text::{Line, Span};

use super::super::super::pane::{ConfigPane, PaneRow};
use super::super::super::theme::Palette;
use super::super::scroll::{self, Attempt};
use super::env::pending_and_env_lines;
use super::field_row::{field_line, section_header};

/// The active group's fields, laid out through [`scroll::to_cursor`]
/// exactly as [`body_from`] already does for the groupless fallback, then
/// [`pending_and_env_lines`] appended after.
pub(super) fn grouped_body_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    show_lands: bool,
) -> Vec<Line<'static>> {
    // The cursor's own row has to draw somewhere in this budget, per the
    // rule every screen in this file holds. When it is on an env row or
    // `+ add a key`, the field body has nothing to align to and its own
    // windowing would spend the whole budget on fields none of which is
    // selected, leaving nothing for the row that actually is. So
    // `pending_and_env_lines` goes first then, and the field body takes
    // whatever it leaves, rather than the other way around.
    if matches!(pane.cursor(), Some(PaneRow::Env(_) | PaneRow::AddEnv)) {
        let tail = pending_and_env_lines(pane, palette, width, budget, show_lands);
        let remaining = budget.saturating_sub(tail.len());
        let mut lines = if !pane.field_rows().is_empty() && remaining > 0 {
            body_from(pane, palette, width, remaining, 0, false, show_lands).lines
        } else {
            Vec::new()
        };
        lines.extend(tail);
        return lines;
    }
    let mut lines = if !pane.fields().is_empty() && budget > 0 {
        let field_rows = pane.field_rows();
        let cursor_row = field_rows
            .iter()
            .position(|row| Some(*row) == pane.cursor())
            .unwrap_or(0);
        scroll::to_cursor(
            cursor_row,
            pane.view().offset(),
            |offset| body_from(pane, palette, width, budget, offset, false, show_lands),
            || cursor_only(pane, palette, width, budget, cursor_row, show_lands),
        )
    } else {
        Vec::new()
    };
    let remaining = budget.saturating_sub(lines.len());
    lines.extend(pending_and_env_lines(
        pane, palette, width, remaining, show_lands,
    ));
    lines
}

/// Lays the body out from field `offset`, spending at most `budget` lines.
///
/// Every line pushed is counted, including section headers, blank
/// separators between them and both markers. The two markers are reserved
/// before a row is admitted rather than appended afterwards, so a height
/// that binds cuts a row instead of the sentence saying a row was cut.
pub(super) fn body_from(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    offset: usize,
    show_group_headers: bool,
    show_lands: bool,
) -> Attempt {
    let rows = pane.field_rows();
    let total = rows.len();
    // The cursor may be on an env row rather than a field: nothing in
    // this body is selected then, and row 0 stands in so the first
    // attempt (offset 0) always finds it and never scrolls hunting for a
    // row that is not here. See `grouped_body_lines`'s own doc.
    let cursor_row = rows
        .iter()
        .position(|row| Some(*row) == pane.cursor())
        .unwrap_or(0)
        .min(total.saturating_sub(1));
    // The `... N above` marker is inserted at the top once everything under
    // it is laid out, so its line is held back from the very first check.
    let above = usize::from(offset > 0);
    // Whether a row at `index` still leaves room for `need` more lines. The
    // `... N below` marker is only owed when a row follows this one: a row
    // that fills the last line with nothing under it needs no marker.
    let room = |taken: usize, need: usize, index: usize| {
        taken + need + above + usize::from(index + 1 < total) <= budget
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_group: Option<&str> = None;
    // A group's header, and the blank line ahead of it for every group after
    // the first, held here rather than pushed straight away: it is pushed
    // alongside the first row of its group that survives the offset skip,
    // so a window opening in the middle of `control` still says `control`.
    // Never populated when `show_group_headers` is false: the grouped
    // layout's own tab row already names the one group `rows` ever holds,
    // so a second header line here would only cost the tight-height cursor
    // guarantee a line it has no header content to spend.
    let mut pending_header: Vec<Line<'static>> = Vec::new();
    let mut drawn = 0usize;

    for (index, row) in rows.iter().enumerate() {
        let PaneRow::Field(field_index) = *row else {
            continue;
        };
        let group = pane
            .fields()
            .fields()
            .get(field_index)
            .and_then(|field| field.group.as_deref());
        if show_group_headers && current_group != group {
            let mut header = Vec::new();
            if current_group.is_some() {
                header.push(Line::default());
            }
            if let Some(group) = group {
                header.push(section_header(group, palette));
            }
            pending_header = header;
            current_group = group;
        }
        if index < offset {
            continue;
        }
        if !room(lines.len(), pending_header.len() + 1, index) {
            break;
        }
        lines.append(&mut pending_header);
        lines.push(field_line(
            pane,
            field_index,
            pane.cursor() == Some(*row),
            width,
            palette,
            show_lands,
        ));
        drawn += 1;
    }

    // Counted off what this pass actually drew, not off the viewport's own
    // arithmetic: the viewport hides rows against a line budget it cannot
    // see spent, so its answer and this one disagree the moment the chrome
    // costs anything.
    let hidden_below = total.saturating_sub(offset + drawn);
    if hidden_below > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ... {hidden_below} below"),
            palette.muted(),
        )));
    }
    if offset > 0 {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("  ... {offset} above"),
                palette.muted(),
            )),
        );
    }

    Attempt {
        cursor_drawn: drawn > 0 && (offset..offset + drawn).contains(&cursor_row),
        lines,
    }
}

/// The cursor's own row, alone, for a body too short to hold the chrome its
/// group costs.
///
/// The last resort, reached only when every offset down to the cursor's own
/// left it undrawn. A group's first row costs a blank line and a header
/// above it before it may be drawn at all, and the two markers on top of
/// that: four lines for one row, where `view::MIN_HEIGHT` leaves this pane
/// three after its title. A pane that declares a minimum height should draw
/// something at it, and the selected row is the something.
///
/// Markers are added around it while they fit, the cursor's row first: it is
/// the one line this function exists to guarantee.
pub(super) fn cursor_only(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    cursor_row: usize,
    show_lands: bool,
) -> Vec<Line<'static>> {
    let rows = pane.field_rows();
    let mut lines = Vec::new();
    if let Some(PaneRow::Field(index)) = rows.get(cursor_row).copied() {
        lines.push(field_line(pane, index, true, width, palette, show_lands));
    }
    let hidden_below = rows.len().saturating_sub(cursor_row + 1);
    if cursor_row > 0 && lines.len() < budget {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("  ... {cursor_row} above"),
                palette.muted(),
            )),
        );
    }
    if hidden_below > 0 && lines.len() < budget {
        lines.push(Line::from(Span::styled(
            format!("  ... {hidden_below} below"),
            palette.muted(),
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::draw::pane_lines;
    use super::super::fixtures::{
        bark_pane, blurb_anchor, config_pane_lines_for_tests, marked, screen_at, text_of, web_pane,
    };
    use super::super::layout::panel_width;
    use crate::lookout::app::{KeyPress, Msg};
    use crate::lookout::view::MIN_TERM_WIDTH;
    use crate::lookout::view::fixtures;
    use crate::output::width::visible_width;

    #[test]
    fn a_sheep_pane_scrolled_to_the_last_field_of_a_group_shows_it() {
        // `cron` has only two fields, both of which always fit; `process`,
        // the group a fresh pane opens on, has ten, which is what forces
        // the scroll this test is about.
        let mut pane = web_pane();
        pane.set_rows(8);
        // Not `move_to_last`: that now lands on `+ add a key`, past every
        // field. `move_to_key` reaches `user`, `process`'s own last field,
        // directly.
        pane.move_to_key("user");
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 9));
        assert!(text.len() <= 9, "{text:?}");
        assert!(text.iter().any(|line| line.contains("above")), "{text:?}");
        assert!(
            text.iter().any(|line| line.contains("user")),
            "process's last field is visible: {text:?}"
        );
        assert!(!text.iter().any(|line| line.contains("below")), "{text:?}");
    }

    /// Guards against the class of bug where chrome eats the budget and
    /// the selected row is never drawn, while every static frame still
    /// looks right. Six is `view::MIN_HEIGHT`; three lines of body under
    /// the title is less than a group's first row costs, so those steps
    /// go through `cursor_only`.
    #[test]
    fn the_cursor_survives_every_step_of_a_walk_down_and_back_up() {
        for height in [6u16, 7, 8, 10, 14, 20, 45] {
            let mut app = fixtures::app_in_sheep_pane();
            let total = app.config_pane().unwrap().rows().len();
            for step in 0..=total {
                let text = screen_at(&mut app, height);
                assert_eq!(marked(&text), 1, "{height} rows, {step} down:\n{text}");
                app.update(Msg::Key(KeyPress::SelectDown));
            }
            for step in 0..=total {
                let text = screen_at(&mut app, height);
                assert_eq!(marked(&text), 1, "{height} rows, {step} up:\n{text}");
                app.update(Msg::Key(KeyPress::SelectUp));
            }
        }
    }

    /// A roomy terminal (`view::ROOMY_HEIGHT` and up) spends a blank row
    /// under the title. `body_rows` has to know about that row, or the
    /// `Rect` it hands the pane reaches one row past the status bar and the
    /// last row the pane draws, here the cursor's own row after jumping to
    /// the last field, gets overwritten rather than shown. Caught the
    /// scrolled offset itself agreeing on a stale, too-tall budget: `marked`
    /// went to 0 at exactly `ROOMY_HEIGHT` and the row above it, with a
    /// `body_rows` that did not subtract the blank row.
    #[test]
    fn the_cursor_survives_a_jump_to_the_last_field_at_a_roomy_height() {
        // 30 is `view::ROOMY_HEIGHT`, private to that module.
        for height in [24u16, 29, 30, 31, 45] {
            let mut app = fixtures::app_in_sheep_pane();
            app.update(Msg::Key(KeyPress::SelectLast));
            let text = screen_at(&mut app, height);
            assert_eq!(marked(&text), 1, "{height} rows:\n{text}");
        }
    }

    /// The marker that says rows were cut would itself become the row
    /// that gets cut.
    ///
    /// Both widths, because they take different code. 120 has a panel, so
    /// every height walks `grouped_pane_lines_with_panel` with `top_lines`
    /// returning empty and never reaches the blurb loop's own arithmetic.
    /// 89 is one column under `panel_width`'s floor, so the blurb draws and
    /// the sweep crosses `remaining` entering that loop at 0, 1 and 2
    /// without having to name which height produces which value.
    ///
    /// One test over a width list, not two copies of it: they were twelve
    /// identical lines apart from the literal, and the assertion message
    /// names the width so a failure still says which case broke.
    ///
    /// What this does NOT see is a row too FEW, since the bound is an upper
    /// one and losing the cursor's row only shortens the output.
    /// `the_blurb_never_spends_the_row_the_cursor_needs` is that half, and
    /// it exists because a real defect hid in this gap.
    #[test]
    fn the_body_never_outgrows_the_height_it_was_given() {
        let mut pane = web_pane();
        for width in [120u16, 89] {
            for height in 1..=60u16 {
                pane.set_rows(usize::from(height.saturating_sub(1)));
                for cursor in [0usize, 7, 20, 38] {
                    pane.move_to_first();
                    pane.move_by(isize::try_from(cursor).unwrap());
                    let text = text_of(&pane_lines(&pane, fixtures::plain(), width, height));
                    assert!(
                        text.len() <= usize::from(height),
                        "width {width}, height {height}, cursor {cursor}: {text:?}"
                    );
                }
            }
        }
    }

    /// `push_wrapped_blurb` breaks on a budget of `<= 1` rather than
    /// `== 0`, so a long wrapped help must not spend the last line the
    /// cursor's own row needs. Nothing pinned it before the helper existed:
    /// the height sweep above passes with the floor at either value,
    /// because losing the field row only makes the output shorter and that
    /// assertion is an upper bound.
    ///
    /// So this asserts the pair instead: wherever the blurb reached the
    /// screen, the cursor's own row did too. Both pane shapes, grouped and
    /// ungrouped, even though one function now serves both, because this is
    /// the end-to-end check through the real `pane_lines` entry point
    /// rather than a check of the helper alone.
    ///
    /// The final count is the vacuity guard: a blurb that never drew would
    /// skip every height and pass.
    #[test]
    fn the_blurb_never_spends_the_row_the_cursor_needs() {
        for (which, mut pane, key) in [
            ("grouped", web_pane(), "autorestart"),
            ("ungrouped", bark_pane(), "history_bytes"),
        ] {
            pane.move_to_key(key);
            let anchor = blurb_anchor(&pane);
            let mut checked = 0;
            for height in 1..=20u16 {
                let rows = text_of(&pane_lines(&pane, fixtures::plain(), 89, height));
                if !rows.iter().any(|row| row.contains(&anchor)) {
                    continue;
                }
                checked += 1;
                // Excludes the blurb row itself: a help string that ever
                // came to mention its own field's name would let a cut
                // cursor row hide behind the blurb row satisfying `key` by
                // coincidence.
                assert!(
                    rows.iter()
                        .any(|row| row.contains(key) && !row.contains(&anchor)),
                    "{which} at height {height}: the blurb drew and the cursor's row did not: {rows:?}"
                );
            }
            assert!(
                checked > 0,
                "{which}: the blurb never drew, so nothing was checked"
            );
        }
    }

    /// Nothing is armed and nothing is in flight, so the slot under the
    /// title carries no question. A filed edit shows in its own row's
    /// value cell, which is where the operator is already looking.
    #[test]
    fn a_filed_edit_draws_no_question_under_the_title() {
        let mut pane = web_pane();
        pane.move_to_key("autorestart");
        pane.cycle();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        assert!(
            !text.iter().any(|line| line.contains("enter confirms")),
            "{text:?}"
        );
        assert!(
            !text.iter().any(|line| line.contains("set autorestart")),
            "{text:?}"
        );
    }

    /// At a width with no explanation panel, the field under the cursor
    /// still has its help text on screen, with no key pressed.
    ///
    /// 89 columns is the widest terminal `panel_width` refuses: the panel
    /// clamps to `PANEL_MIN` 50 and 89 - 50 is 39, one short of `LEFT_MIN`.
    ///
    /// `contains` cannot see indentation drift, so this compares against
    /// the header row's own margin instead of a literal 2, since every
    /// row in the pane shares one.
    #[test]
    fn the_blurb_shares_the_panes_own_indent() {
        let pane = web_pane();
        let lines = pane_lines(&pane, fixtures::plain(), 89, 40);
        let rows = text_of(&lines);
        let indent = |row: &str| row.len() - row.trim_start().len();
        let header = rows
            .iter()
            .find(|row| row.contains("FIELD") && row.contains("VALUE"))
            .expect("no column header");
        let anchor = blurb_anchor(&pane);
        let blurb = rows
            .iter()
            .find(|row| row.contains(&anchor))
            .expect("no blurb row");
        assert_eq!(
            indent(blurb),
            indent(header),
            "blurb {:?} against header {:?}",
            blurb.get(..12),
            header.get(..12)
        );
    }

    #[test]
    fn the_blurb_draws_at_a_width_with_no_panel() {
        let pane = web_pane();
        assert!(panel_width(89).is_none(), "89 must have no panel");
        let lines = pane_lines(&pane, fixtures::plain(), 89, 40);
        let anchor = blurb_anchor(&pane);
        let rows = text_of(&lines);
        assert!(
            rows.iter().any(|row| row.contains(&anchor)),
            "no blurb at 89 columns: {rows:?}"
        );
    }

    /// And it describes the row the cursor is on, not the first field.
    #[test]
    fn the_blurb_follows_the_cursor_with_no_panel() {
        let mut pane = web_pane();
        let first = blurb_anchor(&pane);
        pane.move_by(1);
        let second = blurb_anchor(&pane);
        // On the anchors, not the help strings: two fields can have
        // different help and share a longest word, and then the absence
        // assertion below cannot fail. Guarding the help alone would look
        // like it covered this.
        //
        // Substring, not just inequality: "memory" != "max_memory" passes
        // `assert_ne!` while `second`'s own row still contains `first`,
        // which would fail the absence assertion below on a fixture
        // mismatch rather than a real regression.
        assert!(
            !first.contains(&second) && !second.contains(&first),
            "the fixture needs two fields whose longest help words are not substrings of each other: {first:?} / {second:?}"
        );
        let lines = pane_lines(&pane, fixtures::plain(), 89, 40);
        let rows = text_of(&lines);
        // The specific blurb row, not the whole screen: `first` scanned
        // against every row would false-fail on a coincidental substring
        // in an unrelated one, a field name or a value cell.
        let blurb_row = rows
            .iter()
            .find(|row| row.contains(&second))
            .unwrap_or_else(|| panic!("the cursor moved and the blurb did not: {rows:?}"));
        assert!(
            !blurb_row.contains(&first),
            "the previous field's blurb is still on screen: {rows:?}"
        );
    }

    /// The hard constraint this item's brief calls out: the blurb rows
    /// under the title are counted against the same budget every other
    /// line in the pane is, at every width and height the pane claims to
    /// draw at.
    ///
    /// "A line drawn into the fixed slot" until 072b6ff8: the slot held one
    /// line while `h` toggled it, and holds as many as the help text wraps
    /// to now that it is unconditional.
    #[test]
    fn help_text_still_respects_the_width_and_height_budgets() {
        let mut pane = web_pane();
        pane.move_to_key("max_memory");
        for width in MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width}: {line:?}"
                );
            }
        }
        for height in 1..=30u16 {
            let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, height));
            assert!(
                text.len() <= usize::from(height),
                "height {height}: {text:?}"
            );
        }
    }

    /// The only place on screen that says which field the buffer belongs
    /// to.
    #[test]
    fn an_open_editor_draws_its_buffer_in_the_fields_own_row() {
        let mut pane = web_pane();
        pane.move_to_key("cwd");
        pane.begin_typing();
        for typed in "/srv".chars() {
            pane.type_char(typed);
        }
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        let row = text
            .iter()
            .find(|line| line.contains(" cwd"))
            .expect("cwd is drawn at 120 columns");
        assert!(row.contains("/srv\u{258f}"), "{row:?}");
        assert!(
            !text.iter().any(|line| line.contains("set cwd")),
            "an editor is not a confirm: {text:?}"
        );
    }

    // --- the responsive ladder: what each width drops ---

    #[test]
    fn a_short_body_sheds_the_legend_before_the_tab_row() {
        let app = fixtures::app_in_sheep_pane();
        let rows = fixtures::render_all(&config_pane_lines_for_tests(&app, 160, 6));
        assert!(rows.contains("tab next group"), "the tab row must survive");
        assert!(!rows.contains("changed by you"), "the legend must go first");
    }
}
