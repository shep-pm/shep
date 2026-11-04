//! The modal layer, drawn over the muted body.
//!
//! Boxed at [`INTERIOR`] and wider, full width with no border below that. The
//! text wraps to the box and scrolls; the keys sit on a last line that
//! scrolling never moves.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use super::overlay;
use super::status::key_hint::modal_keys;
use crate::lookout::app::{App, Modal};
use crate::output::width::{char_columns, text_columns};
use crate::terminal_safe::sanitise_multiline;

/// The box's interior width. With its border and margin it is drawn from an
/// 80-column terminal, and the borderless form takes over below.
const INTERIOR: u16 = 72;

/// A cell of padding each side of the text, inside the box or the row.
const PAD: u16 = 1;

/// What a modal draws, measured for one `area`.
struct Parts {
    boxed: bool,
    title: Option<String>,
    body: Vec<String>,
    keys: Vec<String>,
    rows: usize,
}

impl Parts {
    /// The lines of text that scroll off the box, so the last one can be
    /// reached.
    fn reach(&self) -> usize {
        self.body.len().saturating_sub(self.rows)
    }
}

/// The text a modal asks or shows, before wrapping.
fn text_of(modal: &Modal) -> String {
    match modal {
        Modal::Grant { prompt, .. } => {
            let listing = sanitise_multiline(prompt.listing()).0;
            let question = sanitise_multiline(prompt.question()).0;
            format!("{listing}\n{question}")
        }
        Modal::Output { text, .. } => text.as_str().to_owned(),
        Modal::Confirm { question, .. } => sanitise_multiline(question).0,
    }
}

/// The rows around the text: the border, the title, the blanks that set the
/// title and the keys apart, and the keys.
///
/// The borderless form gives the blanks up to keep the body.
fn chrome(boxed: bool, titled: bool, keys: usize) -> usize {
    let border = if boxed { 2 } else { 0 };
    let blanks = if boxed { 1 + usize::from(titled) } else { 0 };
    border + usize::from(titled) + blanks + keys
}

/// Measures the open modal against `area`, or `None` with none open.
fn measure(app: &App, area: Rect) -> Option<Parts> {
    let modal = app.modal()?;
    let title = match modal {
        Modal::Output { title, .. } => Some(title.as_str().to_owned()),
        Modal::Grant { .. } | Modal::Confirm { .. } => None,
    };
    let titled = title.is_some();
    let height = usize::from(area.height);
    let boxed = overlay::is_boxed(area.width, INTERIOR) && chrome(true, titled, 1) < height;
    let width = if boxed { INTERIOR } else { area.width };
    let inner = usize::from(width.saturating_sub(2 * PAD)).max(1);
    let body = wrap_text(&text_of(modal), inner);
    let rows_for = |keys: usize| {
        body.len()
            .min(height.saturating_sub(chrome(boxed, titled, keys)).max(1))
    };
    let resting = wrap_keys(modal_keys(modal, false), inner);
    let scrolls = body.len() > rows_for(resting.len());
    let keys = wrap_keys(modal_keys(modal, scrolls), inner);
    let rows = rows_for(keys.len());
    Some(Parts {
        boxed,
        title,
        body,
        keys,
        rows,
    })
}

/// How many lines of the open modal's text its box cannot show in `area`.
///
/// Zero with none open. The event loop hands it to
/// [`App::note_modal_reach`] before each draw.
pub(in crate::lookout) fn scroll_reach(app: &App, area: Rect) -> usize {
    measure(app, area).map_or(0, |parts| parts.reach())
}

/// The open modal over `buffer`, muting what is already drawn. Nothing
/// happens with none open.
pub(super) fn draw(app: &App, area: Rect, buffer: &mut Buffer) {
    let Some(parts) = measure(app, area) else {
        return;
    };
    let palette = app.palette();
    overlay::mute(buffer, area, palette);
    let pad = " ".repeat(usize::from(PAD));
    let blank = || Line::default();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(title) = &parts.title {
        lines.push(Line::styled(format!("{pad}{title}"), palette.attention()));
        if parts.boxed {
            lines.push(blank());
        }
    }
    let first = app.modal_scroll().min(parts.reach());
    lines.extend(
        parts
            .body
            .iter()
            .skip(first)
            .take(parts.rows)
            .map(|text| Line::from(format!("{pad}{text}"))),
    );
    if parts.boxed {
        lines.push(blank());
    }
    lines.extend(
        parts
            .keys
            .iter()
            .map(|text| Line::styled(format!("{pad}{text}"), palette.attention())),
    );
    if parts.boxed {
        overlay::draw_boxed(&lines, INTERIOR, palette, palette.ground(), area, buffer);
        return;
    }
    let shown = lines.len().min(usize::from(area.height));
    let top = area.y + area.height - u16::try_from(shown).unwrap_or(area.height);
    let row = overlay::blank_of(area.width);
    for (offset, line) in lines.iter().take(shown).enumerate() {
        let y = top + u16::try_from(offset).unwrap_or(0);
        overlay::blank_row(
            buffer,
            area.x,
            y,
            &row,
            Style::reset().patch(palette.ground()),
        );
        buffer.set_line(area.x, y, line, area.width);
    }
}

/// `text` wrapped to `width` columns, keeping its line breaks and the
/// indent each line opens with.
///
/// A word wider than `width` is split across rows: the text is read to be
/// agreed to, so nothing is cut off.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let indent = raw
            .chars()
            .take_while(|ch| *ch == ' ')
            .count()
            .min(width / 2);
        let lead = " ".repeat(indent);
        let mut row = lead.clone();
        let mut has_word = false;
        for word in raw.split_whitespace() {
            let columns = text_columns(word);
            if has_word && text_columns(&row) + 1 + columns <= width {
                row.push(' ');
                row.push_str(word);
                continue;
            }
            if has_word {
                out.push(core::mem::replace(&mut row, lead.clone()));
            }
            if text_columns(&row) + columns <= width {
                row.push_str(word);
            } else {
                push_split(word, width, &lead, &mut row, &mut out);
            }
            has_word = true;
        }
        out.push(if has_word { row } else { String::new() });
    }
    out
}

/// A key hint wrapped to `width` columns between its clauses, which are
/// three spaces apart, so a hint keeps the spacing the status bar gives it.
fn wrap_keys(keys: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for clause in keys.split("   ") {
        match out.last_mut() {
            Some(row) if text_columns(row) + 3 + text_columns(clause) <= width => {
                row.push_str("   ");
                row.push_str(clause);
            }
            _ => out.extend(wrap_text(clause, width)),
        }
    }
    out
}

/// Spreads `word` over rows of `width` columns, leaving the last in `row`.
fn push_split(word: &str, width: usize, lead: &str, row: &mut String, out: &mut Vec<String>) {
    for ch in word.chars() {
        let columns = char_columns(ch);
        if text_columns(row) + columns > width && !row.trim().is_empty() {
            out.push(core::mem::replace(row, lead.to_owned()));
        }
        row.push(ch);
    }
}

#[cfg(test)]
mod tests;
