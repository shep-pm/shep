//! The full-screen bleats pane: the same feed [`super::bleats`] draws in its
//! five-line strip, given the whole body instead, plus the filter row
//! stacked on it once an operator has set an axis.
//!
//! `geometry` holds the row arithmetic (how tall a line draws, which
//! survivors a window of body rows shows, how far a page key moves) and
//! `filter_row` the chips and the survivor sentence; this module lays the
//! screen out around them.

mod filter_row;
mod geometry;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::app::{App, RowKey};
use super::super::pane_bleats::BleatsPane;
use super::super::tail::FEED_WINDOW_BYTES;
use super::bleats::unavailable_line;
use super::cell;
use super::flock::fit;
use crate::output::human_bytes;
use crate::vocabulary::Role;

use filter_row::filter_row_line;
use geometry::{TAG_PREFIX_WIDTH, feed_line_rows, row_height, window_range};

pub(crate) use geometry::{max_scroll_offset, page_amount_down, page_amount_up};

/// Draws the pane over the whole body: a title band naming the sheep and
/// both log paths, then, once any filter axis is set, the filter row, then
/// either the window's newest surviving lines, each tagged `out` or `err`
/// with the newest at the bottom, or, when there are none, [`Tail::note`]
/// naming why.
///
/// [`Tail::note`]: super::super::tail::Tail::note
pub fn draw(app: &App, pane: &BleatsPane, area: Rect, buffer: &mut Buffer) {
    let width = area.width;
    let rows = usize::from(area.height);
    for (offset, line) in lines(app, pane, width, rows).into_iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        let y = area.y + offset;
        if y >= area.y + area.height {
            break;
        }
        buffer.set_line(area.x, y, &line, width);
    }
}

/// [`draw`]'s own lines, without a [`Buffer`] round trip: what
/// [`super::frames::render_text`] would read back off the buffer `draw`
/// writes, built directly instead.
///
/// # Panics
///
/// Panics if `app`'s body is not the bleats pane.
#[cfg(test)]
#[must_use]
pub(crate) fn draw_lines(app: &App, width: u16, rows: usize) -> Vec<Line<'static>> {
    let pane = app
        .bleats_pane()
        .expect("draw_lines is only called while the bleats pane is open");
    lines(app, pane, width, rows)
}

/// Shared by [`draw`] and the `#[cfg(test)]` `draw_lines`, so the two never
/// drift.
fn lines(app: &App, pane: &BleatsPane, width: u16, rows: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(rows);
    out.push(title_line(app, pane, width));
    if app.remote_host().is_some() {
        out.push(unavailable_line(app.palette(), width));
        out.truncate(rows);
        return out;
    }

    let filters = pane.filters();
    let feed = app.feed();
    let survivors = pane.visible(&feed.lines, &app.feed_classifier());

    if let Some(row) = filter_row_line(app, filters, survivors.len(), feed.lines.len(), width) {
        out.push(row);
    }

    if feed.lines.is_empty() {
        // The same rule `view::bleats::feed_lines` follows: the note names
        // why there is nothing, and only shows when there is nothing to
        // show instead of it, and only when a row remains for it.
        if rows > out.len()
            && let Some(note) = feed.note.as_deref()
        {
            out.push(Line::from(Span::styled(
                fit(note, width),
                app.palette().muted(),
            )));
        }
        return out;
    }

    let body_rows = rows.saturating_sub(out.len());
    let wrap = pane.wrapped();
    let text_width = width.saturating_sub(TAG_PREFIX_WIDTH);
    let window = window_range(survivors.len(), pane.scroll_offset(), body_rows, |i| {
        row_height(&survivors[i].text, text_width, wrap)
    });
    for line in &survivors[window] {
        out.extend(feed_line_rows(app, filters, line, width, wrap));
    }
    // Two things can leave `out` longer than `rows`. A single wrapped line
    // taller than the whole body: `window_range` always includes at least one
    // line while there are body rows to draw into. And a body of zero rows,
    // where the title and filter row alone are longer than the area.
    out.truncate(rows);
    out
}

/// The pane's title: the sheep's name and both log paths, banded in
/// [`Role::Meadow`].
fn title_line(app: &App, pane: &BleatsPane, width: u16) -> Line<'static> {
    let text = match sheep_id(pane).and_then(|id| app.row(id)) {
        Some(row) => {
            let feed = app.feed();
            // The window that was read, and what fell below it. Bytes below
            // the window are exact because `tail.rs` already seeked past
            // them; a whole-file line count and absolute line numbers would
            // need reading the file to say anything.
            let window = format!(
                "{} window, {} read",
                human_bytes(FEED_WINDOW_BYTES),
                human_bytes(feed.read_bytes),
            );
            let below = match feed.missed_bytes {
                0 => String::new(),
                bytes => format!("  {} below the window, unread", human_bytes(bytes)),
            };
            format!(
                "{}  out {}  err {}  {window}{below}",
                row.info.name,
                row.info.out_file.as_deref().unwrap_or("-"),
                row.info.err_file.as_deref().unwrap_or("-"),
            )
        }
        None => match sheep_id(pane) {
            Some(id) => format!("sheep {id}: it is no longer in the flock"),
            None => "no sheep is selected".to_string(),
        },
    };
    Line::from(Span::styled(
        cell::band(&text, usize::from(width)),
        app.palette().band(Role::Meadow),
    ))
}

/// The sheep id a [`BleatsPane`] describes, or `None` for the three `RowKey`
/// variants it is never opened on. `ask_for_bleats` only ever builds one on
/// a [`RowKey::Sheep`]; this stays exhaustive rather than assuming that
/// holds forever.
fn sheep_id(pane: &BleatsPane) -> Option<u32> {
    match pane.sheep() {
        RowKey::Sheep(id) => Some(*id),
        RowKey::Group(_) | RowKey::Section(_) | RowKey::Fold(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::super::app::{KeyPress, Msg};
    use super::super::super::frames::render_text;
    use super::super::super::tail::{Stream, Tail, TailLine};
    use super::super::fixtures::{
        full_app, render_all, with_feed, with_no_selection, with_selection,
    };
    use super::*;

    /// Draws `pane` over `app` at `width`x`height` and renders it back to
    /// plain text, styles dropped: the same round trip `frames.rs`'s own
    /// tests use.
    fn drawn(app: &App, pane: &BleatsPane, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        draw(app, pane, area, &mut buffer);
        render_text(&buffer)
    }

    /// Mirrors `view::bleats::tests::an_empty_feed_prints_the_reason_rather_than_nothing`:
    /// the full-screen pane owes the operator the same explanation the
    /// five-line strip already gives.
    #[test]
    fn an_empty_feed_shows_the_note_in_the_body() {
        let app = with_feed(Tail {
            lines: Vec::new(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 0,
            note: Some("no log path is configured for this sheep".to_string()),
        });
        let pane = BleatsPane::new(app.selected().expect("with_feed selects a sheep"));
        let text = drawn(&app, &pane, 80, 6);
        assert!(
            text.contains("no log path is configured for this sheep"),
            "got {text:?}"
        );
    }

    /// The note names why there is nothing to show; once there is something,
    /// it has to go, not sit above the lines.
    #[test]
    fn a_feed_with_lines_never_shows_the_note() {
        let app = with_feed(Tail {
            lines: vec![TailLine::new(Stream::Out, "listening")],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 9,
            note: Some("should never reach the screen".to_string()),
        });
        let pane = BleatsPane::new(app.selected().expect("with_feed selects a sheep"));
        let text = drawn(&app, &pane, 80, 6);
        assert!(
            !text.contains("should never reach the screen"),
            "got {text:?}"
        );
        assert!(text.contains("listening"), "got {text:?}");
    }

    /// The ordinary case: a sheep still in the flock names itself and both
    /// its log paths.
    #[test]
    fn the_title_names_the_sheep_and_both_log_paths() {
        let info = ProcessInfo::builder(3, "catcher", ProcStatus::Online)
            .out_file(Some("/home/ada/.shep/logs/catcher-out.log".to_string()))
            .err_file(Some("/home/ada/.shep/logs/catcher-err.log".to_string()))
            .build();
        let app = with_selection(info);
        let pane = BleatsPane::new(RowKey::Sheep(3));
        let text = title_line(&app, &pane, 120);
        let rendered: String = text
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains("catcher"), "got {rendered:?}");
        assert!(
            rendered.contains("/home/ada/.shep/logs/catcher-out.log"),
            "got {rendered:?}"
        );
        assert!(
            rendered.contains("/home/ada/.shep/logs/catcher-err.log"),
            "got {rendered:?}"
        );
    }

    /// A pane pinned to a sheep that has since left the flock says so,
    /// rather than drawing a title with nothing behind it.
    #[test]
    fn the_title_says_when_the_pinned_sheep_left_the_flock() {
        let info = ProcessInfo::builder(3, "catcher", ProcStatus::Online).build();
        let app = with_selection(info);
        let pane = BleatsPane::new(RowKey::Sheep(404));
        let text = title_line(&app, &pane, 120);
        let rendered: String = text
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            rendered.contains("sheep 404: it is no longer in the flock"),
            "got {rendered:?}"
        );
    }

    /// A [`RowKey::Group`] row can never own this pane:
    /// `lookout::app::bleats::ask_for_bleats` opens it only on a
    /// [`RowKey::Sheep`]. `sheep_id` stays exhaustive anyway, and the title
    /// has to say something sane if the pane is ever handed a group row.
    #[test]
    fn the_title_says_no_sheep_is_selected_for_a_non_sheep_row() {
        let app = with_no_selection();
        let pane = BleatsPane::new(RowKey::Group("web".to_string()));
        let text = title_line(&app, &pane, 120);
        let rendered: String = text
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            rendered.contains("no sheep is selected"),
            "got {rendered:?}"
        );
    }

    /// Ten lines over a three-row window: the newest three land on screen,
    /// oldest first, so the newest sits on the bottom row.
    #[test]
    fn draw_keeps_the_newest_lines_when_more_exist_than_fit() {
        let app = full_app();
        let pane = BleatsPane::new(app.selected().expect("full_app selects a sheep"));
        // height 4: one title row plus three body rows.
        let text = drawn(&app, &pane, 80, 4);
        for n in 7..10 {
            assert!(text.contains(&format!("line-{n}")), "got {text:?}");
        }
        for n in 0..7 {
            assert!(!text.contains(&format!("line-{n} ")), "got {text:?}");
        }
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.last().is_some_and(|line| line.contains("line-9")),
            "the newest line is on the bottom row: {text:?}"
        );
    }

    /// No axis set, no filter row at all: `draw_keeps_the_newest_lines_...`
    /// above depends on this, since it counts on three whole body rows.
    #[test]
    fn no_axis_set_draws_no_filter_row() {
        let mut app = full_app();
        app.update(Msg::Key(KeyPress::Bleats));
        let text = render_all(&draw_lines(&app, 80, 6));
        assert!(!text.contains("must hold"), "got {text:?}");
        assert!(!text.contains("lines in the window"), "got {text:?}");
    }

    /// The title names the window it read and what fell below it.
    ///
    /// `rulings.md` dropped the whole-file line count, the absolute line
    /// numbers and the density gutter, all three of which need reading the
    /// file. Bytes below the window are not in that set: `tail.rs` knows
    /// them exactly because it seeked past them, and the design asks row 1
    /// for them.
    #[test]
    fn the_title_names_the_window_and_what_fell_below_it() {
        let mut app = with_selection(
            ProcessInfo::builder(9, "web", ProcStatus::Online)
                .out_file(Some("/logs/web-out.log".to_string()))
                .build(),
        );
        app.update(Msg::Bleats {
            tail: Tail {
                lines: vec![TailLine::new(Stream::Out, "listening")],
                missed_lines: 0,
                missed_bytes: 831 * 1024,
                read_bytes: 4096,
                note: None,
            },
        });
        app.update(Msg::Key(KeyPress::Bleats));
        let text = render_all(&draw_lines(&app, 200, 40));
        assert!(text.contains("64.0K window"), "the window it reads: {text}");
        assert!(text.contains("4.0K read"), "and what it read: {text}");
        assert!(
            text.contains("below the window, unread"),
            "and what it could not: {text}"
        );
    }
}
