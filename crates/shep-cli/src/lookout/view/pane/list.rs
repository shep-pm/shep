//! The list sub-screen: one row per element of an array field.
//!
//! Unlike the env screen, an element's value is shown, because an array of
//! arguments or exit codes is not a place secrets live. A secret array is
//! masked element by element all the same.

use ratatui::text::{Line, Span};

use super::super::super::pane::{ConfigPane, ListPane, ListRow, UNSET};
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::super::scroll::{self, Attempt};
use super::layout::{POSITION_W, body_width, mask_secret};

/// The list sub-screen: one array field's elements, and a row to add one on.
///
/// Values are drawn, unlike an env row: an array arrives with the config.
/// Laid out through [`scroll::to_cursor`], the same walk the field list
/// uses.
pub(super) fn list_lines(
    pane: &ConfigPane,
    list: &ListPane,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                &format!("{}  {} (list)", pane.target().name(), list.key()),
                body_width(width)
            )
        ),
        palette.muted(),
    ))];
    let body_budget = budget - 1;
    if body_budget == 0 {
        return lines;
    }
    let secret = list.is_secret();
    let rows = list.rows();
    let cursor_row = list.view().cursor().min(rows.len().saturating_sub(1));
    lines.extend(scroll::to_cursor(
        cursor_row,
        list.view().offset(),
        |offset| list_body_from(list, palette, width, body_budget, offset, secret),
        || vec![list_line(list, cursor_row, true, width, palette, secret)],
    ));
    lines
}

/// One row of the list sub-screen: the selection mark, the element's
/// position, and the element.
///
/// The position is drawn because `K` and `J` move an element by one.
/// `secret` masks an unset element the way
/// [`super::field_row::field_line`] masks a `x-shep-secret` field.
pub(super) fn list_line(
    list: &ListPane,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
    secret: bool,
) -> Line<'static> {
    let body = body_width(width);
    let position_w = POSITION_W.min(body);
    let value_w = body.saturating_sub(position_w + 2);
    let typed = selected
        .then(|| list.typing())
        .flatten()
        .map(|(_, buffer)| buffer);
    let (position, value) = match list.rows().get(index).copied() {
        Some(ListRow::Item(item)) => (
            format!("{item}"),
            typed.map_or_else(
                || {
                    let raw = list
                        .elements()
                        .get(item)
                        .cloned()
                        .unwrap_or_else(|| UNSET.to_owned());
                    mask_secret(secret, raw)
                },
                |buffer| buffer.drawn_for(secret, value_w),
            ),
        ),
        Some(ListRow::New) => match typed {
            Some(buffer) => (String::new(), buffer.drawn_for(secret, value_w)),
            None => (String::new(), "+ new".to_owned()),
        },
        None => return Line::default(),
    };
    let mut text = format!("{} ", mark(selected));
    text.push_str(&fit(&position, position_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(&value, value_w));
    }
    if matches!(list.rows().get(index), Some(ListRow::New)) {
        return Line::from(Span::styled(text, palette.muted()));
    }
    Line::from(Span::raw(text))
}

/// Lays the sub-screen's body out from row `offset`, spending at most
/// `budget` lines. Both markers are reserved before a row is admitted, the
/// same rule [`super::body::body_from`] follows.
pub(super) fn list_body_from(
    list: &ListPane,
    palette: Palette,
    width: u16,
    budget: usize,
    offset: usize,
    secret: bool,
) -> Attempt {
    let rows = list.rows();
    let total = rows.len();
    let cursor_row = list.view().cursor().min(total.saturating_sub(1));
    let above = usize::from(offset > 0);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut drawn = 0usize;
    for index in offset..total {
        if lines.len() + 1 + above + usize::from(index + 1 < total) > budget {
            break;
        }
        lines.push(list_line(
            list,
            index,
            index == cursor_row,
            width,
            palette,
            secret,
        ));
        drawn += 1;
    }
    let hidden_below = total.saturating_sub(offset + drawn);
    scroll::window_markers(&mut lines, hidden_below, offset, palette);
    Attempt {
        cursor_drawn: drawn > 0 && (offset..offset + drawn).contains(&cursor_row),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        marked, pane_to, screen_at, secret_array_dog_pane, text_of, web_pane, web_with_args,
    };
    use super::super::pane_lines;
    use super::*;
    use crate::lookout::app::{KeyPress, Msg};
    use crate::lookout::view::MIN_TERM_WIDTH;
    use crate::lookout::view::fixtures;
    use crate::output::width::visible_width;

    /// The list sub-screen over `args`, as an operator reaches it: cursor
    /// onto the row, then open.
    fn rendered_list(pane: &ConfigPane, width: u16, height: u16) -> Vec<String> {
        let mut pane = pane.clone();
        pane.move_to_key("args");
        pane.open_list();
        text_of(&pane_lines(&pane, fixtures::plain(), width, height))
    }

    /// The dashboard with `web` selected and its list sub-screen open on
    /// `args`, reached the way an operator reaches it: `e`, the shepherd's
    /// reply, walk to the row, `Enter`.
    fn app_on_the_list_screen(args: &[&str]) -> crate::lookout::app::App {
        let mut app = fixtures::with_selection(
            shep_core::protocol::ProcessInfo::builder(
                9,
                "web",
                shep_core::status::ProcStatus::Online,
            )
            .pid(Some(48_000))
            .build(),
        );
        app.set_control_for_tests(crate::lookout::app::Control::Allowed);
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: crate::lookout::app::Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(shep_core::protocol::Response::SheepConfig(Box::new(
                web_with_args(args),
            ))),
        });
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app
    }

    /// Env is write-only because the shepherd sends no value; an array
    /// arrives with the config, so a screen that hid it could not say
    /// which element the cursor is on.
    #[test]
    fn the_list_screen_shows_its_values_unlike_env() {
        let pane = ConfigPane::sheep(web_with_args(&["--port"]));
        let lines = rendered_list(&pane, 120, 20);
        assert!(lines.iter().any(|l| l.contains("--port")), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains(r#"["--port"]"#)),
            "the element is a row of its own, not the field list's JSON cell: {lines:?}"
        );
    }

    /// No Flockfile field is secret today, and a dog's pane has no widget
    /// for an array at all, so a secret array is one masked row everywhere
    /// this screen could otherwise open.
    #[test]
    fn a_dogs_secret_array_opens_no_list_screen_and_never_draws_its_elements() {
        let mut pane = secret_array_dog_pane();
        pane.open_list();
        assert!(pane.list().is_none(), "a locked row opens no sub-screen");
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        assert!(
            !text
                .iter()
                .any(|line| line.contains("ab12cd34") || line.contains("ef56gh78")),
            "a secret element is never rendered: {text:?}"
        );
        assert!(
            text.iter().any(|line| line.contains("<set>")),
            "the row says there is one: {text:?}"
        );
    }

    /// A secret list holding `ab12cd34`, drawn at `selected` with `typed`
    /// typed into the editor `begin_typing` opens on the cursor's row.
    fn secret_list_row(cursor: usize, typed: &str, secret: bool) -> String {
        let mut list = ListPane::new(
            "tokens".into(),
            crate::lookout::field::ListItem::Text,
            crate::lookout::field::Bounds::default(),
            secret,
            vec!["ab12cd34".into()],
        );
        list.move_to(cursor);
        list.begin_typing();
        for c in typed.chars() {
            list.type_char(c);
        }
        let line = list_line(&list, cursor, true, 120, fixtures::plain(), secret);
        text_of(&[line]).remove(0)
    }

    #[test]
    fn a_secret_list_masks_the_value_being_typed() {
        let item = secret_list_row(0, "ef56gh78", true);
        assert!(!item.contains("ab12cd34") && !item.contains("ef56gh78"), "{item:?}");
        assert!(item.contains(&format!("{}\u{258f}", "\u{2588}".repeat(16))), "{item:?}");

        let new = secret_list_row(1, "ef56gh78", true);
        assert!(!new.contains("ef56gh78"), "{new:?}");
        assert!(new.contains(&format!("{}\u{258f}", "\u{2588}".repeat(8))), "{new:?}");
    }

    #[test]
    fn a_plain_list_still_draws_the_value_being_typed() {
        let new = secret_list_row(1, "ef56gh78", false);
        assert!(new.contains("ef56gh78\u{258f}"), "{new:?}");
        let item = secret_list_row(0, "x", false);
        assert!(item.contains("ab12cd34x\u{258f}"), "{item:?}");
    }

    #[test]
    fn the_list_sub_screen_at_a_comfortable_width() {
        let lines = rendered_list(&web_pane(), 120, 0);
        insta::assert_snapshot!("list_sub_screen", lines.join("\n"));
    }

    /// The chrome this screen draws is counted like every other line: a
    /// title, and the confirm echo under it when something is armed.
    #[test]
    fn the_list_sub_screen_never_outgrows_the_height_or_the_width_it_was_given() {
        let mut pane = ConfigPane::sheep(web_with_args(&[
            "--port",
            "8080",
            "--host",
            "0.0.0.0",
            "--verbose",
            "--log",
            "debug",
        ]));
        pane.move_to_key("args");
        pane.open_list();
        pane.file_list_removal().expect("filed");
        for height in 1..=20u16 {
            pane.list_mut().unwrap().move_to_first();
            let total = pane.list().unwrap().rows().len();
            pane.list_mut()
                .unwrap()
                .set_rows(usize::from(height.saturating_sub(1)));
            for step in 0..=total {
                let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, height));
                assert!(
                    text.len() <= usize::from(height),
                    "height {height}, step {step}: {text:?}"
                );
                pane.list_mut().unwrap().move_by(1);
            }
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

    /// The invariant `view::scroll` exists to hold, at the shortest height
    /// the pane claims to draw: one row is marked at every step of a walk.
    ///
    /// Reached through `screen_at`, the way the field list's own twin
    /// (`the_cursor_survives_every_step_of_a_walk_down_and_back_up`) reaches
    /// it, so the budget is the real `body_rows`: four rows at
    /// `MIN_HEIGHT`, not `MIN_HEIGHT` itself.
    #[test]
    fn the_list_cursor_survives_every_step_at_the_minimum_height() {
        let args = [
            "--port",
            "8080",
            "--host",
            "0.0.0.0",
            "--verbose",
            "--log",
            "debug",
            "--quiet",
        ];
        for height in [crate::lookout::view::flock::MIN_HEIGHT, 7, 8, 12] {
            let mut app = app_on_the_list_screen(&args);
            let total = app.config_pane().unwrap().list().unwrap().rows().len();
            for step in 0..=total {
                let text = screen_at(&mut app, height);
                assert_eq!(marked(&text), 1, "height {height}, step {step}:\n{text}");
                app.update(Msg::Key(KeyPress::SelectDown));
            }
        }
    }
}
