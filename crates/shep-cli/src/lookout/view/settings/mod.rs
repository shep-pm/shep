//! The settings screen: the `shep.toml` tree, then `[dogs]`, drawn straight
//! into the buffer since this screen owns the whole body between the title and
//! the status bar.
//!
//! Every row goes through [`fit`]: a value cut short ends in `…` rather
//! than spilling into the next column.
//!
//! A boolean and a closed set of names cycle; every other key is free text
//! via [`Settings::typing`], where an empty buffer unsets it. The dogs
//! table's RUNNING column ([`dog_rows`]) joins the file's own
//! `enabled`/`SOURCE` against the live flock.
//!
//! `dogs` holds the dogs table and its columns, `tree` the rows above it and
//! their widths; this module lays the screen out around them.

// `pub(super)`, not private to `settings`: the two re-exported functions
// below hand back this module's `DogColumn` and `DogRow`, and a private
// `dogs` would leave those types nameable nowhere a caller of
// `view::settings::columns_for` stands. `view::flock` keeps its own columns
// the same way (`pub(super) mod columns;` behind a re-exported
// `columns_for`).
pub(super) mod dogs;
mod tree;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::app::{App, Settings, SettingsRow};
use super::super::theme::Palette;
use super::flock::{GUTTER, fit};
use super::scroll::Attempt;

use dogs::{DOGS_CAPTION, dog_header_line, dog_line};
use tree::tree_line;

pub use dogs::{columns_for, dog_rows};

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
/// Walks [`Settings::rows`] rather than listing the tree again: the cursor,
/// which [`Settings::rows`] alone defines, would otherwise land on a row this
/// function drew somewhere else.
///
/// Takes `app` as well as `settings`: [`dog_rows`] needs the live flock,
/// which lives on `App`, not on `Settings`.
///
/// `height` is the body's row budget; zero means unlimited, for a test
/// with no real terminal. [`Viewport`] scrolls in data rows while height
/// counts lines, so its offset is only a starting point: this walks
/// forward from it until the cursor's row fits, through
/// [`super::scroll::to_cursor`], shared with the config pane.
///
/// [`cursor_only`] is that walk's last resort, for a body too short for
/// even one row's own chrome.
///
/// [`Viewport`]: crate::lookout::viewport::Viewport
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
/// left it undrawn. `MIN_HEIGHT` is six rows, four of body, and a dog row
/// needs the blank line, the `[dogs]` header, the caption and the column
/// header above it before it may be drawn at all: five lines for one row.
/// A screen that declares a minimum height should draw something at it,
/// and the selected row is the something.
///
/// Markers are added around it while they fit, the cursor's row first: it
/// is the one line this function exists to guarantee.
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
            let dogs = dog_rows(app, table_width);
            if let Some(dog) = dogs.get(*index) {
                lines.push(dog_line(dog, columns_for(table_width), table_width, true));
            }
        }
        Some(row) => lines.push(tree_line(settings, *row, true, table_width, palette)),
        None => {}
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

/// Lays the body out from data row `offset`, spending at most `budget`
/// lines.
///
/// Every line pushed is counted, including the ones no data row owns. The
/// `... N above` marker and the `... N below` marker are both reserved for
/// before a row is admitted rather than appended afterwards, so a height
/// that binds cuts a row instead of cutting the sentence that says a row
/// was cut.
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
        let mut dogs_header = vec![Line::default(), section_header("[dogs]", palette)];
        // fitted: the caption is 48 columns and the screen draws from
        // `view::MIN_TERM_WIDTH` (33) up.
        dogs_header.push(Line::from(Span::styled(
            format!("  {}", fit(DOGS_CAPTION, table_width)),
            palette.muted(),
        )));
        let rendered_columns = columns_for(table_width);
        dogs_header.push(dog_header_line(rendered_columns, table_width, palette));

        let dogs = dog_rows(app, table_width);
        if dogs.is_empty() {
            // Nothing to gate a header this function will never draw
            // otherwise: the empty flock still gets to say "[dogs]", same
            // as `flock::render_table`'s own header for an empty payload.
            if lines.len() + dogs_header.len() + above <= budget {
                lines.append(&mut dogs_header);
            }
        } else {
            let mut pending_dogs_header = dogs_header;
            for (index, dog) in dogs.iter().enumerate() {
                let global_index = tree_len + index;
                if global_index < offset {
                    continue;
                }
                if !room(lines.len(), pending_dogs_header.len() + 1, global_index) {
                    break;
                }
                lines.append(&mut pending_dogs_header);
                let selected = cursor == Some(SettingsRow::Dog(index));
                lines.push(dog_line(dog, rendered_columns, table_width, selected));
                drawn += 1;
            }
        }
    }

    // Counted off what this pass actually drew, not off `Viewport`'s own
    // arithmetic: the viewport hides rows against a line budget it cannot
    // see spent, so its answer and this one disagree once the chrome costs
    // anything.
    let hidden_below = total_rows.saturating_sub(offset + drawn);
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
        } else if let Some((label, buffer)) = settings.typing() {
            // The cursor is a character, not a style: the ANSI gallery
            // renders foregrounds only.
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}",
                    fit(
                        &buffer.line(
                            &format!("editing {label}: "),
                            "   enter applies   esc cancels",
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
/// `app` supplies the palette and the live flock [`dog_rows`] joins against;
/// every other fact this screen shows, including its one armed or in-flight
/// prompt line, comes off `settings`.
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
    /// the dogs table underneath.
    #[test]
    fn settings_at_a_comfortable_width() {
        let app = fixtures::app_in_settings();
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| super::super::draw(&app, frame))
            .unwrap();
        insta::assert_snapshot!(render_text(terminal.backend().buffer()));
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
