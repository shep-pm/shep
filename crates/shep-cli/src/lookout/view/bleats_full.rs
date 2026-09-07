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
/// both log paths, then the window's newest lines, each tagged `out` or
/// `err`, newest at the bottom.
pub fn draw(app: &App, pane: &BleatsPane, area: Rect, buffer: &mut Buffer) {
    let width = area.width;
    buffer.set_line(area.x, area.y, &title_line(app, pane, width), width);

    let feed = app.feed();
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
