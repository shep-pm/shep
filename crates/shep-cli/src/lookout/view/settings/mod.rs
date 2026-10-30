//! The settings screen: the `shep.toml` tree, then `[dogs]`.
//! It draws straight into the buffer. This screen owns the whole body between
//! the title and the status bar.
//!
//! Every row goes through [`fit`]: a value cut short ends in `…` rather than
//! spilling into the next column. A boolean and a closed set of names cycle.
//! Every other key is free text via [`Settings::typing`], where an empty
//! buffer unsets it. `e` on a dog's header row opens that dog's own tree.
//!
//! `dogs` holds the dog header rows, `tree` every other row and their widths.

pub(super) mod dogs;
mod tree;

#[cfg(test)]
mod secret_editors;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::app::{App, Settings, SettingsRow};
use super::super::keyvalue::Field;
use super::super::theme::Palette;
use super::flock::{GUTTER, fit};
use super::scroll::Attempt;
use super::status::key_hint::{editor_hint, editor_label, focused_field};
use super::status::status_layout::TEXT_ENTRY_HINT;

use dogs::{DOGS_CAPTION, dog_header_line};
use tree::tree_line;

/// The width the rows themselves are laid out in: the terminal minus
/// [`GUTTER`].
const fn body_width(width: u16) -> u16 {
    width.saturating_sub(GUTTER)
}

/// The `[dogs]` header, indented to match every row's own mark-and-gap
/// prefix, same as [`dog_header_line`]'s own reasoning.
fn section_header(label: &str, palette: Palette) -> Line<'static> {
    Line::from(Span::styled(format!("  {label}"), palette.muted()))
}

/// Every line of the screen's body, top to bottom: the tree, one line a row,
/// then `[dogs]`'s caption and table.
///
/// Walks [`Settings::rows`], which alone defines the cursor, rather than
/// listing the tree again. `height` is the body's row budget, and zero means
/// unlimited. [`to_cursor`](super::scroll::to_cursor) walks from the viewport's
/// offset until the cursor's row fits.
fn content_lines(
    app: &App,
    settings: &Settings,
    palette: Palette,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let total_rows = settings.rows().len();
    let cursor_row = settings.view().cursor().min(total_rows.saturating_sub(1));
    let budget = if height == 0 {
        usize::MAX
    } else {
        usize::from(height)
    };
    // The schema's sections are unconditional, so an empty screen is
    // unreachable today. Handled rather than indexed on faith, because the
    // fallback below has a row to draw and this does not.
    if total_rows == 0 {
        return body_from(app, settings, palette, width, budget, 0).lines;
    }
    super::scroll::to_cursor(
        cursor_row,
        settings.view().offset(),
        |offset| body_from(app, settings, palette, width, budget, offset),
        || cursor_only(app, settings, palette, width, budget, cursor_row),
    )
}

/// The cursor's own row, alone, for a body too short to hold the chrome
/// that row's section costs.
///
/// The last resort, reached only when every offset down to the cursor's own
/// left it undrawn. A dog row spends four lines of chrome before it may
/// draw, and `MIN_HEIGHT` has four lines of body. Markers are added while
/// they fit.
fn cursor_only(
    app: &App,
    settings: &Settings,
    palette: Palette,
    width: u16,
    budget: usize,
    cursor_row: usize,
) -> Vec<Line<'static>> {
    let rows = settings.rows();
    let table_width = body_width(width);
    let mut lines = Vec::new();
    match rows.get(cursor_row) {
        Some(SettingsRow::Dog(index)) => {
            lines.push(dog_header_line(
                app,
                settings,
                *index,
                true,
                table_width,
                palette,
            ));
        }
        Some(row) => lines.push(tree_line(settings, *row, true, table_width, palette)),
        None => {}
    }

    let hidden_below = rows.len().saturating_sub(cursor_row + 1);
    super::scroll::cursor_markers(&mut lines, hidden_below, cursor_row, budget, palette);
    lines
}

/// Lays the body out from data row `offset`, spending at most `budget`
/// lines.
///
/// Every line pushed is counted, including the ones no data row owns. Both
/// hidden-count markers are reserved before a row is admitted, so a binding
/// height cuts a row rather than the sentence that says a row was cut.
fn body_from(
    app: &App,
    settings: &Settings,
    palette: Palette,
    width: u16,
    budget: usize,
    offset: usize,
) -> Attempt {
    let cursor = settings.cursor();
    let rows = settings.rows();
    let total_rows = rows.len();
    let cursor_row = settings.view().cursor().min(total_rows.saturating_sub(1));
    // The `... N above` marker is inserted at the top once everything under
    // it is laid out, so its line is held back from the very first check.
    let above = usize::from(offset > 0);
    // Whether a row at `index` still leaves room for `need` more lines. The
    // `... N below` marker is only owed when a row follows this one: a row
    // that fills the last line and has nothing under it needs no marker.
    let room = |taken: usize, need: usize, index: usize| {
        taken + need + above + usize::from(index + 1 < total_rows) <= budget
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut drawn = 0usize;
    let mut full = false;
    // The tree rows sort first in `Settings::rows`, ahead of every
    // `SettingsRow::Dog`, and cost one line apiece.
    let tree_len = rows
        .iter()
        .take_while(|row| !matches!(row, SettingsRow::Dog(_)))
        .count();

    for (index, row) in rows.iter().copied().enumerate().take(tree_len) {
        if index < offset {
            continue;
        }
        if !room(lines.len(), 1, index) {
            full = true;
            break;
        }
        lines.push(tree_line(
            settings,
            row,
            cursor == Some(row),
            body_width(width),
            palette,
        ));
        drawn += 1;
    }

    // `body_width`, not `width`: every line below draws `mark`'s own two
    // columns before its first cell.
    let table_width = body_width(width);
    if !full {
        // The section's own chrome, spent once, before the first dog row this
        // pass draws wherever that row is. `pending` because it is drained as
        // it is used: a header is not owed to a row the offset skipped.
        let mut pending = vec![
            Line::default(),
            section_header("[dogs]", palette),
            // fitted: the caption is 68 columns and the screen draws from
            // `view::MIN_TERM_WIDTH` (33) up.
            Line::from(Span::styled(
                format!("  {}", fit(DOGS_CAPTION, table_width)),
                palette.muted(),
            )),
        ];
        if total_rows == tree_len {
            // Nothing to gate a header this function would otherwise never
            // draw: an empty flock still gets to say "[dogs]", same as
            // `flock::render_table`'s own header for an empty payload.
            if lines.len() + pending.len() + above <= budget {
                lines.append(&mut pending);
            }
        }
        for (index, row) in rows.iter().copied().enumerate().skip(tree_len) {
            if index < offset {
                continue;
            }
            if !room(lines.len(), pending.len() + 1, index) {
                break;
            }
            lines.append(&mut pending);
            let selected = cursor == Some(row);
            lines.push(match row {
                SettingsRow::Dog(dog) => {
                    dog_header_line(app, settings, dog, selected, table_width, palette)
                }
                // A dog's own keys, its grants checklist and its config tree
                // are rows like every other row on this screen, drawn by the
                // same function at one indent deeper than the key above them.
                other => tree_line(settings, other, selected, table_width, palette),
            });
            drawn += 1;
        }
    }

    // Counted off what this pass actually drew, not off `Viewport`'s own
    // arithmetic: the viewport hides rows against a line budget it cannot
    // see spent, so its answer and this one disagree once the chrome costs
    // anything.
    let hidden_below = total_rows.saturating_sub(offset + drawn);
    super::scroll::window_markers(&mut lines, hidden_below, offset, palette);

    // One prompt line under the table, echoing the status bar's Slot 1
    // (`view::status::status_line`): `attention` styled while it waits on
    // `Enter`, an in-flight sentence once it has gone out. Both branches
    // cost two lines, so neither is pushed unless the budget has them.
    if lines.len() + 2 <= budget {
        if let Some(prompt) = settings.pending() {
            lines.push(Line::default());
            let text = if prompt.sent {
                format!("{}  sent, waiting for the shepherd", prompt.text)
            } else {
                format!("{}  enter confirms, any other key cancels", prompt.text)
            };
            lines.push(Line::from(Span::styled(
                format!("  {}", fit(&text, table_width)),
                palette.attention(),
            )));
        } else if let Some((dog, editor)) = settings.env_typing() {
            lines.push(Line::default());
            let label = editor_label(editor, &format!("{dog} env"));
            let keys = format!("   {}", editor_hint(editor.focus()));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    fit(
                        &focused_field(editor).line(
                            editor.focus() == Field::Value,
                            &format!("{label}: "),
                            &keys,
                            table_width,
                        ),
                        table_width,
                    )
                ),
                palette.attention(),
            )));
        } else if let Some((label, buffer)) = settings.typing() {
            // The cursor is a character, not a style: the ANSI gallery
            // renders foregrounds only.
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    fit(
                        &buffer.line(
                            settings.typing_is_secret(),
                            &format!("editing {label}: "),
                            &format!("   {TEXT_ENTRY_HINT}"),
                            table_width,
                        ),
                        table_width,
                    )
                ),
                palette.attention(),
            )));
        }
    }

    Attempt {
        cursor_drawn: drawn > 0 && (offset..offset + drawn).contains(&cursor_row),
        lines,
    }
}

/// Draws the settings screen into `area`, straight into `buffer`.
///
/// `app` supplies the palette and the live flock a dog header row joins
/// against; every other fact this screen shows, including its one armed or
/// in-flight prompt line, comes off `settings`.
pub fn draw_settings(app: &App, settings: &Settings, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let palette = app.palette();
    for (offset, line) in content_lines(app, settings, palette, area.width, area.height)
        .iter()
        .enumerate()
        .take(usize::from(area.height))
    {
        let offset = u16::try_from(offset).unwrap_or(0);
        buffer.set_line(area.x, area.y + offset, line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::fixtures;
    use super::*;
    use crate::lookout::app::{KeyPress, Msg};
    use crate::lookout::frames::render_text;
    use crate::output::width::visible_width;

    /// The snapshot pins the section order, every row's four columns, and
    /// the dog rows underneath.
    #[test]
    fn settings_at_a_comfortable_width() {
        let app = fixtures::app_in_settings();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| super::super::draw(&app, frame))
            .unwrap();
        insta::assert_snapshot!(render_text(terminal.backend().buffer()));
    }

    /// One dog opened on its own row: the four `[daemon] keys under its header,
    /// the grants checklist under `grants`, and the dog's own tree under those.
    /// The header row itself is unchanged, which is the point of folding rather
    /// than opening a pane over the screen.
    #[test]
    fn a_dog_opened_on_its_own_row() {
        let (_dir, _paths, mut app) = fixtures::settings_over(fixtures::BARK_RECORDED);
        fixtures::open_dog_row(&mut app, "bark");
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| super::super::draw(&app, frame))
            .unwrap();
        insta::assert_snapshot!(render_text(terminal.backend().buffer()));
    }

    /// An adopted dog's `env` is a table under its header, one row per key
    /// reading `<set>`, then `+ new`. The value the file holds is on no row.
    #[test]
    fn an_adopted_dogs_env_draws_each_key_as_set_and_never_its_value() {
        let (_dir, _paths, mut app) = fixtures::settings_over(fixtures::OTEL_WITH_ENV);
        fixtures::open_adopted_dog_row(
            &mut app,
            "otel",
            std::path::Path::new("/usr/local/bin/shep-otel"),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| super::super::draw(&app, frame))
            .unwrap();
        let screen = render_text(terminal.backend().buffer());
        let lines: Vec<&str> = screen.lines().collect();
        let table = lines
            .iter()
            .position(|line| line.contains("▾ env"))
            .unwrap_or_else(|| panic!("the dog has an env table:\n{screen}"));
        assert!(
            lines[table + 1].contains("PADDOCK_KEY") && lines[table + 1].contains("<set>"),
            "{:?}",
            lines[table + 1]
        );
        assert!(lines[table + 2].contains("+ new"), "{:?}", lines[table + 2]);
        assert!(
            !screen.contains("hunter2"),
            "a value reached the screen:\n{screen}"
        );
    }

    /// The editor draws under the table, labelled with the field the cursor is
    /// in, then the question draws in its place once the write is armed. The
    /// question names the key and never the value.
    #[test]
    fn an_env_edit_draws_its_editor_then_a_question_that_names_no_value() {
        let (_dir, _paths, mut app) = fixtures::settings_over(fixtures::OTEL_WITH_ENV);
        fixtures::open_adopted_dog_row(
            &mut app,
            "otel",
            std::path::Path::new("/usr/local/bin/shep-otel"),
        );
        let draw = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
            terminal
                .draw(|frame| super::super::draw(app, frame))
                .unwrap();
            render_text(terminal.backend().buffer())
        };
        fixtures::select_setting(&mut app, "dog.otel.env+");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(draw(&app).contains("new otel env KEY: "), "{}", draw(&app));

        for typed in "TOKEN".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        assert!(
            draw(&app).contains("otel env TOKEN VALUE: "),
            "{}",
            draw(&app)
        );

        for typed in "hunter2".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let armed = draw(&app);
        assert!(
            armed.contains("set TOKEN in otel's env in shep.toml?"),
            "{armed}"
        );
        assert!(!armed.contains("hunter2"), "{armed}");
    }

    #[test]
    fn every_settings_line_fits_the_terminal_it_was_drawn_for() {
        let app = fixtures::app_in_settings_with_dog_drift();
        let settings = app.settings().unwrap();
        let palette = app.palette();
        for width in super::super::MIN_TERM_WIDTH..=200 {
            for line in content_lines(&app, settings, palette, width, 0) {
                let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    visible_width(&rendered) <= usize::from(width),
                    "width {width} drew {}: {rendered:?}",
                    visible_width(&rendered)
                );
            }
        }
    }

    #[test]
    fn the_cursor_mark_sits_on_exactly_the_selected_row() {
        let app = fixtures::app_in_settings();
        let settings = app.settings().unwrap();
        let palette = app.palette();
        let lines = content_lines(&app, settings, palette, 120, 0);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let marked: Vec<&String> = rendered.iter().filter(|l| l.starts_with('>')).collect();
        assert_eq!(marked.len(), 1, "exactly one row is marked: {rendered:?}");
        assert!(
            marked[0].contains("daemon"),
            "the cursor opens on the first row: {rendered:?}"
        );
    }

    /// The body as strings, at the height the terminal really has.
    fn body_at(app: &App, height: u16) -> Vec<String> {
        let settings = app.settings().unwrap();
        content_lines(app, settings, app.palette(), 120, height)
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    }

    /// `Viewport` scrolls in data rows while the height it is handed counts
    /// lines, so the blank separator, `[dogs]` caption and dog column
    /// header all have to be counted here too.
    #[test]
    fn a_short_terminal_scrolls_and_says_what_it_hid() {
        let mut app = fixtures::app_in_settings();
        let height = 10;
        app.note_body_rows(height);
        // Walk to the last row, whatever the tree and the dogs add up to.
        app.update(Msg::Key(KeyPress::SelectLast));
        let text = body_at(&app, height);
        assert!(
            text.len() <= usize::from(height),
            "the body fits the height it was given: {text:?}"
        );
        assert!(text[0].contains("above"), "{text:?}");
        assert!(
            text.iter().any(|l| l.contains("[dogs]")),
            "the visible section is labelled"
        );
        assert_eq!(
            text.iter().filter(|l| l.starts_with('>')).count(),
            1,
            "the cursor's own row is drawn: {text:?}"
        );
        let settings = app.settings().unwrap();
        assert_eq!(settings.view().cursor(), settings.rows().len() - 1);
    }

    /// The whole screen at `height`, through the same `note_body_rows` and
    /// `draw` the event loop runs before every frame.
    fn screen_at(app: &mut App, height: u16) -> String {
        let area = Rect::new(0, 0, 120, height);
        app.note_body_rows(super::super::body_rows(area));
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal
            .draw(|frame| super::super::draw(app, frame))
            .unwrap();
        render_text(terminal.backend().buffer())
    }

    /// How many rows the frame marks as selected. One, always.
    fn marked(text: &str) -> usize {
        text.lines().filter(|line| line.starts_with('>')).count()
    }

    /// Six is `view::MIN_HEIGHT`: four lines of body, where a dog row
    /// costs six lines to draw in place and falls back to drawing alone.
    #[test]
    fn the_cursor_survives_every_step_of_a_walk_down_and_back_up() {
        for height in [6u16, 7, 8, 10, 14, 20] {
            let mut app = fixtures::app_in_settings_with_dog_drift();
            let total = app.settings().unwrap().rows().len();
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

    /// The marker is pushed last, so a clip drops it first before it can
    /// say anything was cut.
    #[test]
    fn the_below_marker_survives_the_height_that_made_it_necessary() {
        let mut app = fixtures::app_in_settings();
        let height = 6;
        app.note_body_rows(height);
        // Cursor left on the first row, so everything hidden is hidden
        // below it.
        let text = body_at(&app, height);
        assert!(
            text.len() <= usize::from(height),
            "the body fits the height it was given: {text:?}"
        );
        assert!(
            text.last().is_some_and(|l| l.contains("below")),
            "the last line says how many rows were cut: {text:?}"
        );
        assert!(
            text.iter()
                .any(|l| l.starts_with("> ") && l.contains("daemon")),
            "the cursor's own row is drawn: {text:?}"
        );
    }

    #[test]
    fn the_editor_line_names_its_field_and_shows_the_buffer() {
        let mut app = fixtures::app_in_settings_on("daemon.socket");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let settings = app.settings().unwrap();
        let palette = app.palette();
        let lines = content_lines(&app, settings, palette, 120, 0);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("editing daemon.socket:")
                    && line.contains("/home/ada/.shep/run/shep.sock")),
            "got: {rendered:?}"
        );
    }
}
