//! The list sub-screen: one row per element of an array field.
//!
//! Unlike the env screen, an element's value is shown, because an array of
//! arguments or exit codes is not a place secrets live. A secret array is
//! masked element by element all the same.

use ratatui::text::{Line, Span};

use super::super::super::pane::{ConfigPane, ListPane, ListRow};
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::super::scroll::{self, Attempt};
use super::layout::{POSITION_W, body_width, mask_secret};

/// The list sub-screen: one array field's elements, and a row to add one on.
///
/// Values are drawn, unlike an env row: an array arrives with the
/// config, so hiding it would leave the screen unable to say which element
/// the cursor is on.
///
/// Laid out through [`scroll::to_cursor`], the same walk the field
/// list uses, so the cursor is drawn at every height this pane claims to
/// support.
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
    let secret = pane.fields().by_key(list.key()).is_some_and(|f| f.secret);
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
/// The position is drawn because `K` and `J` move an element by one, so a
/// row that did not say where it was would leave an operator counting.
/// `secret` masks an unset element the same way [`field_line`] masks a
/// `x-shep-secret` field; nothing in today's schemas sets it on an array,
/// but a future one could.
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
                        .unwrap_or_else(|| "(unset)".to_owned());
                    mask_secret(secret, raw)
                },
                |buffer| format!("{buffer}\u{258f}"),
            ),
        ),
        Some(ListRow::New) => match typed {
            Some(buffer) => (String::new(), format!("{buffer}\u{258f}")),
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
/// same rule [`body_from`] follows.
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        marked, pane_to, screen_at, secret_list_dog_pane, text_of, web_pane, web_with_args,
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

    /// No Flockfile field is secret today, but a dog's schema can mark one,
    /// and the list sub-screen has to mask it the same way the field list
    /// and the confirm sentence already do.
    #[test]
    fn the_list_screen_masks_a_secret_arrays_elements() {
        let text = text_of(&pane_lines(
            &secret_list_dog_pane(),
            fixtures::plain(),
            120,
            0,
        ));
        assert!(
            !text
                .iter()
                .any(|line| line.contains("ab12cd34") || line.contains("ef56gh78")),
            "a secret element is never rendered: {text:?}"
        );
        assert!(
            text.iter().filter(|line| line.contains("<set>")).count() == 2,
            "both elements mask to <set>: {text:?}"
        );
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
        pane.file_list_removal();
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
