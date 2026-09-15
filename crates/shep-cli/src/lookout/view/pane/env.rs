//! The env rows, and the pending section above them.
//!
//! A value never reaches the screen. A key is listed and its value renders
//! as `<set>`, whether it is stored, filed or being typed, so a shoulder
//! looking at the dashboard learns which keys exist and nothing else.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::pane::{ConfigPane, EnvTyping, PaneRow};
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::chrome::pending_edit_line;
use super::field_row::section_header;
use super::layout::{body_width, widths};

/// The pending-edits and env sections that follow the active group's own
/// fields: every filed edit regardless of which group it belongs to, then
/// the sheep's own env key names and a hint to add one.
///
/// Appended rather than scrolled: both are short by construction, an
/// operator files a handful of edits and a Flockfile carries a handful of
/// env keys, so neither earns the `... N below` machinery the field list's
/// 41 rows need.
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
    // The cursor's own row has to draw somewhere in this budget, the same
    // rule `body_from`/`cursor_only` hold for a field. Below the floor a
    // full section (headers, blanks, every key) would cut it along with
    // everything else, so at the floor this draws that one row alone and
    // nothing else, rather than truncating from the bottom and losing
    // whichever row the truncation happens to reach last.
    let pending_len = if pane.edits().is_empty() {
        0
    } else {
        2 + pane.edits().len()
    };
    let env_len = 2 + pane.env_key_names().len() + 1;
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
/// [`cursor_only`]'s twin for the env rows below the field list. Panics if
/// the cursor is not on [`PaneRow::Env`] or [`PaneRow::AddEnv`]; every
/// caller has already checked.
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
        Some(PaneRow::Field(_)) | None => {
            unreachable!("callers check the cursor is on an env row first")
        }
    }
}

/// One env key's own row: the selection mark, the key, and `(set)`.
///
/// Every key reads `(set)`, never its own value: `SheepConfigView::new`
/// clears `config.env` before this pane ever sees it, so no value for any
/// key reaches here, and this is the one word every row can honestly draw.
pub(super) fn env_row_line(
    pane: &ConfigPane,
    name: &str,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    let (key_w, value_w, _) = widths(body_width(width), show_lands);
    let typing = selected
        .then(|| pane.env_typing())
        .flatten()
        .filter(|typing| typing.key() == Some(name))
        .map(EnvTyping::buffer);
    let value = typing.map_or_else(|| "(set)".to_owned(), |buffer| format!("{buffer}\u{258f}"));
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
    // A single blank standing in for `field_line`'s lock glyph: an env row
    // has none, but `GUTTER` already spent that column out of
    // `body_width`, so this row has to spend it too rather than draw one
    // column short.
    //
    // Inside the key cell itself, `field_line` reserves a first character
    // for its flag (`!`/`*`/` `) before the key text; an env row has no
    // flag, but the reservation is the same cell `fit` pads to `key_w`, so
    // this row has to spend that character too, or its key text starts one
    // column left of every field's.
    let mut text = String::from(" ");
    text.push_str(&fit(&format!(" {name}"), key_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(&value, value_w));
    }
    Line::from(vec![mark_span, Span::styled(text, ground)])
}

/// The `+ add a key` row: the selection mark, and either the caption or,
/// while it is being typed, the whole `KEY=value` buffer.
pub(super) fn add_env_row_line(
    pane: &ConfigPane,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    let (key_w, _, _) = widths(body_width(width), show_lands);
    let typing = selected
        .then(|| pane.env_typing())
        .flatten()
        .filter(|typing| typing.key().is_none())
        .map(EnvTyping::buffer);
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
    let (caption, muted) = match typing {
        Some(buffer) => (format!("{buffer}\u{258f}"), false),
        None => ("+ add a key".to_owned(), true),
    };
    // Same key-cell reservation as [`env_row_line`]: a leading space stands
    // in for `field_line`'s flag character so `+ add a key` and every field
    // above it start their text in the same column.
    let mut text = String::from(" ");
    text.push_str(&fit(&format!(" {caption}"), key_w));
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
    use crate::lookout::view::MIN_TERM_WIDTH;
    use crate::lookout::view::fixtures;
    use crate::output::width::visible_width;

    /// The folded env rows at a comfortable width: both of the fixture's
    /// keys are listed with `(set)` rather than a value, and `+ add a key`
    /// is last.
    ///
    /// Asserts on the env rows alone via
    /// [`fixtures::config_pane_env_rows_for_tests`], not on the whole
    /// rendered frame: a search over the joined frame can match any row,
    /// field or env, once folding put both in the same screen.
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

    /// A filed env write draws nothing at all on this screen, and above
    /// all not the value that was typed into it.
    #[test]
    fn a_filed_env_write_never_reaches_the_screen() {
        let mut pane = web_pane();
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        for typed in "NEW_KEY=hunter2".chars() {
            pane.type_env_char(typed);
        }
        pane.apply_env_typing();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        assert!(!text.join("\n").contains("hunter2"), "{text:?}");
    }

    /// The shepherd sends keys with no values at all, so a rendered value
    /// could only have been invented. Replaces
    /// `the_env_sub_screen_never_renders_a_value`, which pinned the same
    /// fact on the sub-screen the env rows folded into.
    ///
    /// Asserts on the env rows alone via
    /// [`fixtures::config_pane_env_rows_for_tests`], not on the whole
    /// rendered frame: see [`the_env_rows_draw_at_a_comfortable_width`]'s
    /// own doc comment for why a frame-wide search is the wrong tool here.
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
        for typed in "A_VERY_LONG_ENV_KEY_NAME=and a value longer still".chars() {
            pane.type_env_char(typed);
        }
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
}
