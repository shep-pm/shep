//! The env rows, and the pending section above them.
//!
//! A value never reaches the screen. A key is listed and its value renders
//! as `<set>`, whether it is stored, filed or being typed, so a shoulder
//! looking at the dashboard learns which keys exist and nothing else. While
//! the shared key/value editor is open on a row, its two fields draw side by
//! side in those same two columns, and only the field holding the cursor
//! draws a cursor.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::keyvalue::{Field, KeyValues};
use super::super::super::pane::{ConfigPane, PaneRow};
use super::super::super::textbox::TextBox;
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::chrome::pending_edit_line;
use super::field_row::section_header;
use super::layout::{body_width, widths};

/// The pending-edits and env sections that follow the active group's own
/// fields: every filed edit regardless of which group it belongs to, then the
/// sheep's own env key names and a hint to add one.
///
/// Appended rather than scrolled: both are short by construction, an operator
/// files a handful of edits and a Flockfile carries a handful of env keys, so
/// neither earns the `... N below` machinery the field list's 41 rows need.
pub(super) fn pending_and_env_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    show_lands: bool,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if budget == 0 {
        return lines;
    }
    // The cursor's own row has to draw somewhere in this budget, the same rule
    // `body_from`/`cursor_only` hold for a field. Below a full section
    // (headers, blanks, every key) would cut it along with everything else, so
    // at the floor this draws that one row alone and nothing else, rather than
    // truncating from the bottom and losing whichever row the truncation
    // happens to reach last.
    let pending_len = if pane.edits().is_empty() {
        0
    } else {
        2 + pane.edits().len()
    };
    let shows_env = pane.shows_env();
    let env_len = if shows_env {
        2 + pane.env_key_names().len() + 1
    } else {
        0
    };
    if pending_len + env_len > budget
        && let Some(PaneRow::Env(_) | PaneRow::AddEnv) = pane.cursor()
    {
        return vec![cursor_env_row_line(pane, width, palette, show_lands)];
    }
    let mut remaining = budget;

    if !pane.edits().is_empty() && remaining > 0 {
        lines.push(Line::default());
        remaining -= 1;
        if remaining > 0 {
            lines.push(section_header("pending edits", palette));
            remaining -= 1;
        }
        for (_, entry) in pane.edits().iter() {
            if remaining == 0 {
                break;
            }
            lines.push(pending_edit_line(pane, entry.edit(), width, palette));
            remaining -= 1;
        }
    }

    if !shows_env {
        return lines;
    }
    if remaining > 0 {
        lines.push(Line::default());
        remaining -= 1;
    }
    if remaining > 0 {
        lines.push(section_header("env", palette));
        remaining -= 1;
    }
    for (index, name) in pane.env_key_names().iter().enumerate() {
        if remaining == 0 {
            break;
        }
        let selected = pane.cursor() == Some(PaneRow::Env(index));
        lines.push(env_row_line(
            pane, name, selected, width, palette, show_lands,
        ));
        remaining -= 1;
    }
    if remaining > 0 {
        let selected = pane.cursor() == Some(PaneRow::AddEnv);
        lines.push(add_env_row_line(pane, selected, width, palette, show_lands));
    }
    lines
}

/// The cursor's own row, alone: [`pending_and_env_lines`]'s floor fallback,
/// [`super::body::cursor_only`]'s twin for the env rows below the field list.
/// Panics if the cursor is not on [`PaneRow::Env`] or [`PaneRow::AddEnv`];
/// every caller has already checked.
pub(super) fn cursor_env_row_line(
    pane: &ConfigPane,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    match pane.cursor() {
        Some(PaneRow::Env(index)) => {
            let name = pane
                .env_key_names()
                .get(index)
                .expect("the cursor names a real env row");
            env_row_line(pane, name, true, width, palette, show_lands)
        }
        Some(PaneRow::AddEnv) => add_env_row_line(pane, true, width, palette, show_lands),
        Some(
            PaneRow::Field(_)
            | PaneRow::Table(_)
            | PaneRow::Entry(_)
            | PaneRow::Choice { .. }
            | PaneRow::Arm { .. }
            | PaneRow::Dog(_)
            | PaneRow::Under { .. },
        )
        | None => {
            unreachable!("callers check the cursor is on an env row first")
        }
    }
}

/// The editor open on this row, if it is open on this row.
///
/// `under` is the key the row was opened under, so an editor opened on
/// `DB_HOST` keeps drawing on `DB_HOST` while its KEY field is being renamed
/// to something else: the row the operator chose does not move out from under
/// them halfway through naming it.
fn editor_on<'a>(pane: &'a ConfigPane, under: &str) -> Option<&'a KeyValues> {
    // The borrow is `pane`'s, not `under`'s: `under` is only compared.
    pane.env_typing()
        .filter(|editor| editor.opened_under() == Some(under))
}

/// One field of the open editor, drawn for a cell `width` wide.
///
/// The cursor is drawn only in the field that holds it, which is how the row
/// says which of the two the operator is typing into: the pane has no box, no
/// highlight and no colour to spend on the difference, and the ANSI gallery
/// renders foregrounds only.
fn field_cell(buffer: &TextBox, focused: bool, width: u16) -> String {
    if focused {
        buffer.drawn(width.saturating_sub(1))
    } else {
        buffer.as_str().to_owned()
    }
}

/// One env key's own row: the selection mark, the key, and `(set)`.
///
/// Every key reads `(set)`, never its own value: `SheepConfigView::new` clears
/// `config.env` before this pane ever sees it, so no value for any key reaches
/// here, and this is the one word every row can honestly draw. With the editor
/// open the two columns hold its KEY and VALUE fields instead.
pub(super) fn env_row_line(
    pane: &ConfigPane,
    name: &str,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    let (key_w, value_w, _) = widths(body_width(width), show_lands);
    let editor = selected.then(|| editor_on(pane, name)).flatten();
    let (key_cell, value_cell) = match editor {
        None => (name.to_owned(), "(set)".to_owned()),
        Some(editor) => (
            field_cell(editor.key(), editor.focus() == Field::Key, key_w),
            field_cell(editor.value(), editor.focus() == Field::Value, value_w),
        ),
    };
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mark_style = if selected {
        palette.attention().patch(ground)
    } else {
        Style::default()
    };
    let mark_span = Span::styled(mark(selected), mark_style);
    // A single blank standing in for `field_line`'s lock glyph: an env row has
    // none, but `GUTTER` already spent that column out of `body_width`, so this
    // row has to spend it too rather than draw one column short.
    //
    // Inside the key cell itself, `field_line` reserves a first character for
    // its flag (`!`/`*`/` `) before the key text; an env row has no flag, but
    // the reservation is the same cell `fit` pads to `key_w`, so this row has
    // to spend that character too, or its key text starts one column left of
    // every field's.
    let mut text = String::from(" ");
    text.push_str(&fit(&format!(" {key_cell}"), key_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(&value_cell, value_w));
    }
    Line::from(vec![mark_span, Span::styled(text, ground)])
}

/// The `+ add a key` row: the selection mark, and either the caption or, while
/// the editor is open, its KEY field in the key column and its VALUE field in
/// the value column.
pub(super) fn add_env_row_line(
    pane: &ConfigPane,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    let (key_w, value_w, _) = widths(body_width(width), show_lands);
    let editor = selected.then(|| pane.env_typing()).flatten();
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mark_style = if selected {
        palette.attention().patch(ground)
    } else {
        Style::default()
    };
    let mark_span = Span::styled(mark(selected), mark_style);
    let (caption, muted, value) = match editor {
        None => ("+ add a key".to_owned(), true, String::new()),
        Some(editor) => (
            field_cell(editor.key(), editor.focus() == Field::Key, key_w),
            false,
            field_cell(editor.value(), editor.focus() == Field::Value, value_w),
        ),
    };
    // Same key-cell reservation as [`env_row_line`]: a leading space stands in
    // for `field_line`'s flag character so `+ add a key` and every field above
    // it start their text in the same column.
    let mut text = String::from(" ");
    text.push_str(&fit(&format!(" {caption}"), key_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(&value, value_w));
    }
    let style = if muted {
        palette.muted()
    } else {
        Style::default()
    };
    Line::from(vec![mark_span, Span::styled(text, style.patch(ground))])
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{text_of, web_pane};
    use super::super::pane_lines;
    use super::*;
    use crate::lookout::app::KeyPress;
    use crate::lookout::view::MIN_TERM_WIDTH;
    use crate::lookout::view::fixtures;
    use crate::output::width::visible_width;

    /// Types `text` into whichever field of the pane's open editor holds the
    /// cursor.
    fn type_into(pane: &mut ConfigPane, text: &str) {
        for typed in text.chars() {
            pane.type_env_text(KeyPress::TextChar(typed));
        }
    }

    /// The folded env rows at a comfortable width: both of the fixture's keys
    /// are listed with `(set)` rather than a value, and `+ add a key` is last.
    ///
    /// Asserts on the env rows alone via
    /// [`fixtures::config_pane_env_rows_for_tests`], not on the whole rendered
    /// frame: a search over the joined frame can match any row, field or env,
    /// once folding put both in the same screen.
    #[test]
    fn the_env_rows_draw_at_a_comfortable_width() {
        let pane = web_pane();
        let rows = fixtures::config_pane_env_rows_for_tests(&pane);
        assert!(rows.iter().any(|row| row.contains("DB_HOST")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("LOG_LEVEL")), "{rows:?}");
        assert!(
            rows.iter()
                .all(|row| row.contains("(set)") || row.contains("+ add a key")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("+ add a key")),
            "{rows:?}"
        );
    }

    /// A filed env write draws nothing at all on this screen, and above all not
    /// the value that was typed into it.
    #[test]
    fn a_filed_env_write_never_reaches_the_screen() {
        let mut pane = web_pane();
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        type_into(&mut pane, "NEW_KEY");
        pane.step_env_field(false);
        type_into(&mut pane, "hunter2");
        pane.submit_env_typing();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        assert!(!text.join("\n").contains("hunter2"), "{text:?}");
    }

    /// The shepherd sends keys with no values at all, so a rendered value could
    /// only have been invented. Replaces
    /// `the_env_sub_screen_never_renders_a_value`, which pinned the same fact on
    /// the sub-screen the env rows folded into.
    ///
    /// Asserts on the env rows alone via
    /// [`fixtures::config_pane_env_rows_for_tests`], not on the whole rendered
    /// frame: see [`the_env_rows_draw_at_a_comfortable_width`]'s own doc comment
    /// for why a frame-wide search is the wrong tool here.
    #[test]
    fn every_env_value_renders_as_set_and_never_as_itself() {
        let pane = ConfigPane::sheep({
            let mut config = shep_core::config::AppConfig {
                name: "web".to_string(),
                ..Default::default()
            };
            config
                .env
                .insert("DB_PASSWORD".to_string(), "hunter2".to_string());
            shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new())
        });
        let rows = fixtures::config_pane_env_rows_for_tests(&pane);
        assert!(
            rows.iter().any(|row| row.contains("DB_PASSWORD")),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("hunter2")),
            "an env value reached the pane: {rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("(set)")), "{rows:?}");
    }

    #[test]
    fn every_env_line_fits_the_width_it_was_drawn_for() {
        let mut pane = web_pane();
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        type_into(&mut pane, "A_VERY_LONG_ENV_KEY_NAME");
        pane.step_env_field(false);
        type_into(&mut pane, "and a value longer still");
        for width in MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// The pane's own rows as text, at a width where the whole list fits.
    fn rows_of(pane: &ConfigPane) -> Vec<String> {
        text_of(&pane_lines(pane, fixtures::plain(), 160, 0))
    }

    /// The row `+ add a key` is drawn on, found while it still says so.
    ///
    /// Read off the frame rather than through
    /// [`fixtures::config_pane_env_rows_for_tests`], which finds the add row by
    /// its caption and so cannot name a row that is busy being typed.
    ///
    /// # Panics
    ///
    /// If the pane draws no `+ add a key` row.
    #[track_caller]
    fn add_row_at(pane: &ConfigPane) -> usize {
        rows_of(pane)
            .iter()
            .position(|row| row.contains("add a key"))
            .expect("the pane draws a + add a key row")
    }

    /// The two fields draw side by side, in the same two columns a stored row
    /// uses for its key and its `(set)`, and the cursor is on one of them.
    #[test]
    fn the_open_editor_draws_key_and_value_side_by_side_on_the_add_row() {
        let mut pane = web_pane();
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        let at = add_row_at(&pane);

        pane.begin_env_typing();
        type_into(&mut pane, "NEW_KEY");
        let rows = rows_of(&pane);
        let row = &rows[at];
        assert!(row.contains("NEW_KEY"), "KEY is drawn: {row:?}");
        assert!(row.contains('\u{258f}'), "the cursor is on KEY: {row:?}");
        assert!(
            !row.contains("add a key"),
            "the caption gives way to what is being typed: {row:?}"
        );

        pane.step_env_field(false);
        type_into(&mut pane, "hunter2");
        let rows = rows_of(&pane);
        let row = &rows[at];
        assert!(row.contains("NEW_KEY"), "KEY is still drawn: {row:?}");
        assert!(row.contains("hunter2"), "VALUE draws beside it: {row:?}");
        assert!(
            row.find("hunter2").expect("value") > row.find("NEW_KEY").expect("key"),
            "VALUE draws in its own column, after the key column: {row:?}"
        );
        assert!(
            row.find('\u{258f}').expect("cursor") > row.find("hunter2").expect("value"),
            "and the cursor moved with the field: {row:?}"
        );
    }

    /// A row the editor is open on keeps drawing where it is while the KEY
    /// field is being renamed, so the row the operator chose does not move out
    /// from under them halfway through naming it.
    #[test]
    fn a_rename_keeps_drawing_the_row_it_was_opened_on() {
        let mut pane = web_pane();
        pane.move_to_last();
        pane.move_by(-1);
        assert_eq!(pane.cursor(), Some(PaneRow::Env(1)));
        let before = rows_of(&pane);
        let at = before
            .iter()
            .position(|row| row.contains("LOG_LEVEL"))
            .expect("LOG_LEVEL is drawn on its own row");

        pane.begin_env_typing();
        pane.step_env_field(true);
        pane.type_env_text(KeyPress::TextEdit(
            crate::lookout::textbox::TextEdit::DeleteToStart,
        ));
        type_into(&mut pane, "LOG_VERBOSITY");

        let after = rows_of(&pane);
        assert_eq!(after.len(), before.len(), "no row appeared or went away");
        assert!(
            after[at].contains("LOG_VERBOSITY"),
            "the rename draws on the row it started on: before {:?}, after {:?}",
            before[at],
            after[at]
        );
        assert_eq!(
            pane.env_typing().expect("open").opened_under(),
            Some("LOG_LEVEL")
        );
    }
}
