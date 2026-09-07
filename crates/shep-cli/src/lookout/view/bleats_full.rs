//! The full-screen bleats pane: the same feed [`super::bleats`] draws in its
//! five-line strip, given the whole body instead.
//!
//! No filters yet: this task draws the window plainly. Level parsing, a
//! `Filters` type and the filter row all land in later tasks of the same
//! plan.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::app::{App, RowKey};
use super::super::pane_bleats::BleatsPane;
use super::super::tail::Stream;
use super::cell;
use super::flock::fit;
use crate::vocabulary::Role;

/// Draws the pane over the whole body: a title band naming the sheep and
/// both log paths, then either the window's newest lines, each tagged `out`
/// or `err` with the newest at the bottom, or, when there are none yet,
/// [`Tail::note`](super::super::tail::Tail::note) naming why.
pub fn draw(app: &App, pane: &BleatsPane, area: Rect, buffer: &mut Buffer) {
    let width = area.width;
    buffer.set_line(area.x, area.y, &title_line(app, pane, width), width);

    let feed = app.feed();

    if feed.lines.is_empty() {
        // The same rule `view::bleats::feed_lines` follows: the note names
        // why there is nothing, and only shows when there is nothing to
        // show instead of it.
        if area.height > 1
            && let Some(note) = feed.note.as_deref()
        {
            let rendered = Line::from(Span::styled(fit(note, width), app.palette().muted()));
            buffer.set_line(area.x, area.y + 1, &rendered, width);
        }
        return;
    }

    let rows = usize::from(area.height.saturating_sub(1));
    // The last lines that fit, oldest first, so the newest one lands on the
    // bottom row: the same order `view::bleats::feed_lines` renders in.
    let skip = feed.lines.len().saturating_sub(rows);
    for (offset, line) in feed.lines.iter().skip(skip).enumerate() {
        let offset = u16::try_from(offset).unwrap_or(0);
        let y = area.y + 1 + offset;
        if y >= area.y + area.height {
            break;
        }
        let tag = match line.stream {
            Stream::Out => "out",
            Stream::Err => "err",
        };
        let rendered = Line::from(vec![
            // Muted, both of them: the word carries the meaning, and a red
            // `err` would say a stderr line is damage. See
            // `view::bleats::feed_lines` for the same choice.
            Span::styled(format!("{tag}  "), app.palette().muted()),
            Span::raw(fit(&line.text, width.saturating_sub(5))),
        ]);
        buffer.set_line(area.x, y, &rendered, width);
    }
}

/// The pane's title: the sheep's name and both log paths, banded in
/// [`Role::Meadow`].
fn title_line(app: &App, pane: &BleatsPane, width: u16) -> Line<'static> {
    let text = match sheep_id(pane).and_then(|id| app.row(id)) {
        Some(row) => format!(
            "{}  out {}  err {}",
            row.info.name,
            row.info.out_file.as_deref().unwrap_or("-"),
            row.info.err_file.as_deref().unwrap_or("-"),
        ),
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

/// The sheep id a [`BleatsPane`] describes, or `None` for the two `RowKey`
/// variants it is never opened on. `ask_for_bleats` only ever builds one on
/// a [`RowKey::Sheep`]; this stays exhaustive rather than assuming that
/// holds forever.
fn sheep_id(pane: &BleatsPane) -> Option<u32> {
    match pane.sheep() {
        RowKey::Sheep(id) => Some(*id),
        RowKey::Group(_) | RowKey::Section(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::super::frames::render_text;
    use super::super::super::tail::{Stream, Tail, TailLine};
    use super::super::fixtures::{full_app, with_feed, with_no_selection, with_selection};
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
            lines: vec![TailLine {
                stream: Stream::Out,
                text: "listening".to_string(),
            }],
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

    /// A group row can never own this pane ([`ask_for_bleats`] refuses one),
    /// but `sheep_id` stays exhaustive, and the title has to say something
    /// sane if it is ever handed one anyway.
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
}
