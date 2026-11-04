//! Drawing a pane dog's status tab: the sections the dog answered `shep.pane`
//! with, in the order it gave them, and a line saying so when there is no
//! answer to draw.
//!
//! Every string is the dog's, so each is stripped of what could drive the
//! terminal and cut to its column before it is drawn. Layout, keys and
//! styling are lookout's own.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use shep_core::dogs::pane::{Fields, Pane, Section, Table};

use super::super::super::pane::{ConfigPane, DogStatus, PaneKey, PaneState, follow_start};
use super::super::super::theme::Palette;
use super::super::flock::fit;
use super::chrome::{tab_strip_line, title_line};
use super::layout::{clipped, wrap};
use crate::output::width::text_columns;
use crate::terminal_safe::sanitise;

#[cfg(test)]
mod tests;

/// The columns before a row's text: the cursor mark's cell, then an indent.
const ROW_INDENT: usize = 4;

/// The columns before a section's title.
const TITLE_INDENT: usize = 2;

/// The gap between two cells of a row.
const CELL_GAP: usize = 2;

/// The widest a table column or a fields label grows, however long its
/// content, so one long cell cannot push every other column off the screen.
const COLUMN_MAX: usize = 40;

/// What a header ends in when columns were dropped, gap included.
const MORE: &str = "  \u{2026}";

/// [`MORE`]'s width in columns.
const MORE_COLUMNS: usize = 3;

/// The gap between two entries of the actions line.
const ACTION_GAP: usize = 2;

/// The cursor mark, in the first column of its row.
const MARK: &str = "\u{25b8} ";

/// A line the window may scroll, and whether the cursor is on it.
struct Body {
    lines: Vec<Line<'static>>,
    cursor_line: Option<usize>,
}

/// The status tab's lines for `pane`, at most `budget` of them.
///
/// The title and the tab strip come first and give way, strip before title,
/// only when the budget would leave the body less than one line. The body
/// scrolls so the cursor's row is always drawn, and the footer holds the
/// lines about the pane as a whole.
pub(super) fn status_lines(
    pane: &ConfigPane,
    status: &DogStatus,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut remaining = budget;
    if remaining >= 3 {
        lines.push(title_line(pane, palette, width));
        remaining -= 1;
    }
    if remaining >= 2
        && let Some(strip) = tab_strip_line(pane, palette, width)
    {
        lines.push(strip);
        remaining -= 1;
    }
    let footer = footer_lines(status, palette, width, remaining.saturating_sub(1));
    remaining -= footer.len();
    let body = body(pane, status, palette, width);
    lines.extend(window(body, remaining, status.scroll(), palette));
    lines.extend(footer);
    lines
}

/// How much a footer line matters when the box has room for fewer than all of
/// them: the lower, the likelier it stays.
type Rank = u8;

/// What the footer holds, most to least important: the actions line, the last
/// result, a failed pull, an action the manifest lacks, skipped sections.
const ACTIONS: Rank = 0;
const SAID: Rank = 1;
const FAILED: Rank = 2;
const UNDECLARED: Rank = 3;
const SKIPPED: Rank = 4;

/// The lines under the body, in the order the operator reads them: why the
/// newest pull failed while an older pane is still held, how many sections
/// this lookout could not draw, the actions the manifest lacks, the last
/// result of an action, and the keys that run the pane's actions.
///
/// With room for fewer than all of them, the least important go first.
fn footer_lines(
    status: &DogStatus,
    palette: Palette,
    width: u16,
    room: usize,
) -> Vec<Line<'static>> {
    let mut texts: Vec<(Rank, String, Style)> = Vec::new();
    if let Some(reason) = status.failed() {
        texts.push((
            FAILED,
            format!("could not refresh: {}", sanitise(reason).0),
            palette.attention(),
        ));
    }
    if let PaneState::Ready(pane) = status.state()
        && pane.skipped > 0
    {
        let (count, noun) = match pane.skipped {
            1 => ("1".to_owned(), "section"),
            n => (n.to_string(), "sections"),
        };
        texts.push((
            SKIPPED,
            format!("{count} {noun} could not be drawn: this lookout does not know its kind"),
            palette.muted(),
        ));
    }
    for action in status.undeclared() {
        texts.push((
            UNDECLARED,
            format!("{} is not in the manifest", sanitise(&action).0),
            palette.attention(),
        ));
    }
    if let Some(said) = status.said() {
        texts.push((SAID, said.to_owned(), Style::default()));
    }
    let columns = usize::from(width).saturating_sub(TITLE_INDENT);
    if let Some(line) = actions_text(&status.keys(), columns) {
        texts.push((ACTIONS, line, palette.muted()));
    }
    let mut keep: Vec<usize> = (0..texts.len()).collect();
    keep.sort_by_key(|at| texts[*at].0);
    keep.truncate(room);
    keep.sort_unstable();
    keep.into_iter()
        .map(|at| {
            let (_, text, style) = &texts[at];
            let text = clipped(text, width.saturating_sub(TITLE_INDENT as u16));
            Line::from(Span::styled(format!("  {text}"), *style))
        })
        .collect()
}

/// The keys that run the pane's actions as `r revoke lease (row)` and
/// `p pause the board`, in declared order, in `columns` columns or fewer. An
/// entry that does not fit, and every one after it, gives way to a `…`.
fn actions_text(keys: &[PaneKey], columns: usize) -> Option<String> {
    let entries: Vec<String> = keys
        .iter()
        .filter_map(|key| {
            let letter = key.letter?;
            let label = sanitise(&key.action.label).0;
            let row = if key.rows.is_some() { " (row)" } else { "" };
            Some(format!("{letter} {label}{row}"))
        })
        .collect();
    let (first, rest) = entries.split_first()?;
    let mut line = cut(first, columns);
    for (at, entry) in rest.iter().enumerate() {
        let more = rest.len() - at - 1;
        let reserve = if more > 0 { MORE_COLUMNS } else { 0 };
        if text_columns(&line) + ACTION_GAP + text_columns(entry) + reserve > columns {
            line.push_str(MORE);
            return Some(line);
        }
        line.push_str(&" ".repeat(ACTION_GAP));
        line.push_str(entry);
    }
    Some(line)
}

/// Every line under the tab strip, led by a blank one.
fn body(pane: &ConfigPane, status: &DogStatus, palette: Palette, width: u16) -> Body {
    let mut body = Body {
        lines: vec![Line::default()],
        cursor_line: None,
    };
    match status.state() {
        PaneState::Waiting => {
            let text = format!("asking {}", sanitise(pane.target().name()).0);
            body.lines.push(plain_line(&text, width));
        }
        PaneState::Unavailable(reason) => {
            let room = usize::from(width).saturating_sub(TITLE_INDENT);
            for line in wrap(&sanitise(reason).0, room) {
                body.lines.push(plain_line(&line, width));
            }
        }
        PaneState::Ready(ready) => ready_lines(&mut body, ready, status.cursor(), palette, width),
    }
    body
}

/// `text` as a line at the title indent, cut to `width`.
fn plain_line(text: &str, width: u16) -> Line<'static> {
    let room = usize::from(width).saturating_sub(TITLE_INDENT);
    let text = clipped(text, u16::try_from(room).unwrap_or(u16::MAX));
    Line::from(format!("  {text}"))
}

/// The sections of `pane`, each under its title with a blank line between.
fn ready_lines(body: &mut Body, pane: &Pane, cursor: Option<usize>, palette: Palette, width: u16) {
    let mut first_row = 0;
    for section in &pane.sections {
        if !body.lines.last().is_some_and(|line| line.spans.is_empty()) {
            body.lines.push(Line::default());
        }
        match section {
            Section::Fields(fields) => fields_lines(body, fields, width),
            Section::Table(table) => {
                let at = cursor.and_then(|at| at.checked_sub(first_row));
                table_lines(body, table, at, palette, width);
                first_row += table.rows.len();
            }
            _unknown => {}
        }
    }
}

/// A section's title line.
fn title(body: &mut Body, text: &str, width: u16) {
    let room = usize::from(width).saturating_sub(TITLE_INDENT);
    let text = clipped(&sanitise(text).0, u16::try_from(room).unwrap_or(u16::MAX));
    body.lines.push(Line::from(Span::styled(
        format!("  {text}"),
        Style::default().add_modifier(Modifier::BOLD),
    )));
}

/// What a row's cells have room for.
fn room(width: u16) -> usize {
    usize::from(width).saturating_sub(ROW_INDENT)
}

/// The text of `width` columns or fewer, cut with a `…`, never padded.
fn cut(text: &str, width: usize) -> String {
    clipped(text, u16::try_from(width).unwrap_or(u16::MAX))
}

fn fields_lines(body: &mut Body, section: &Fields, width: u16) {
    title(body, &section.title, width);
    if section.fields.is_empty() {
        body.lines.push(none_line());
        return;
    }
    let pairs: Vec<(String, String)> = section
        .fields
        .iter()
        .map(|field| (sanitise(&field.label).0, sanitise(&field.value).0))
        .collect();
    let room = room(width);
    let longest = pairs
        .iter()
        .map(|(label, _)| text_columns(label))
        .max()
        .unwrap_or(0);
    // A value keeps at least a column beside its label.
    let label_w = longest
        .min(COLUMN_MAX)
        .min(room.saturating_sub(CELL_GAP + 1))
        .max(1);
    for (label, value) in pairs {
        let label = fit(&label, u16::try_from(label_w).unwrap_or(u16::MAX));
        let value = cut(&value, room.saturating_sub(label_w + CELL_GAP));
        let text = format!(
            "{}{label}{}{value}",
            " ".repeat(ROW_INDENT),
            " ".repeat(CELL_GAP)
        );
        body.lines.push(Line::from(text.trim_end().to_owned()));
    }
}

fn none_line() -> Line<'static> {
    Line::from(format!("{}none", " ".repeat(ROW_INDENT)))
}

/// A table: its title, the header, and a line per row. `cursor` is the row of
/// this table the cursor is on, when it is on one.
fn table_lines(
    body: &mut Body,
    section: &Table,
    cursor: Option<usize>,
    palette: Palette,
    width: u16,
) {
    title(body, &section.title, width);
    if section.rows.is_empty() {
        body.lines.push(none_line());
        return;
    }
    let columns: Vec<String> = section.columns.iter().map(|c| sanitise(c).0).collect();
    let rows: Vec<Vec<String>> = section
        .rows
        .iter()
        .map(|row| row.cells.iter().map(|cell| sanitise(cell).0).collect())
        .collect();
    let widths = column_widths(&columns, &rows, room(width));
    let dropped = widths.len() < columns.len();

    let mut header = cells_text(&columns, &widths);
    if dropped {
        header.push_str(MORE);
    } else {
        header.truncate(header.trim_end().len());
    }
    body.lines.push(Line::from(Span::styled(
        format!("{}{header}", " ".repeat(ROW_INDENT)),
        palette.muted(),
    )));
    for (at, cells) in rows.iter().enumerate() {
        let text = cells_text(cells, &widths).trim_end().to_owned();
        if cursor == Some(at) {
            body.cursor_line = Some(body.lines.len());
            let line = format!("{MARK}  {text}");
            let line = fit(&line, width);
            body.lines.push(Line::from(Span::styled(
                line,
                Style::default().add_modifier(Modifier::REVERSED),
            )));
        } else {
            body.lines
                .push(Line::from(format!("{}{text}", " ".repeat(ROW_INDENT))));
        }
    }
}

/// `cells` laid out at `widths`, one per kept column, the last one padded.
fn cells_text(cells: &[String], widths: &[usize]) -> String {
    let drawn: Vec<String> = cells
        .iter()
        .zip(widths)
        .map(|(cell, width)| fit(cell, u16::try_from(*width).unwrap_or(u16::MAX)))
        .collect();
    drawn.join(&" ".repeat(CELL_GAP))
}

/// The widths of the columns that fit in `room`, right-most dropped first.
///
/// A column is as wide as its widest cell or heading, up to [`COLUMN_MAX`]. A
/// table that overflows keeps the most columns that leave room for the `…` the
/// header ends in, and at least one, cut to what is left.
fn column_widths(columns: &[String], rows: &[Vec<String>], room: usize) -> Vec<usize> {
    let natural: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(at, name)| {
            rows.iter()
                .filter_map(|cells| cells.get(at))
                .map(|cell| text_columns(cell))
                .chain([text_columns(name)])
                .max()
                .unwrap_or(0)
                .min(COLUMN_MAX)
        })
        .collect();
    let span = |kept: usize| -> usize {
        natural[..kept].iter().sum::<usize>() + CELL_GAP * kept.saturating_sub(1)
    };
    let kept = if span(natural.len()) <= room {
        natural.len()
    } else {
        (1..natural.len())
            .rev()
            .find(|kept| span(*kept) + MORE_COLUMNS <= room)
            .unwrap_or(1)
    };
    let reserve = if kept < natural.len() {
        MORE_COLUMNS
    } else {
        0
    };
    let mut widths = natural[..kept].to_vec();
    if let Some(last) = widths.last_mut() {
        let before = span(kept) - *last;
        *last = (*last).min(room.saturating_sub(before + reserve).max(1));
    }
    widths
}

/// The `budget` lines of `body`, from line `scroll` when a key scrolled the
/// tab, else around the cursor, with a count of what is scrolled off where the
/// first or last line would have been.
fn window(
    body: Body,
    budget: usize,
    scroll: Option<usize>,
    palette: Palette,
) -> Vec<Line<'static>> {
    let Body {
        mut lines,
        cursor_line,
    } = body;
    let total = lines.len();
    if total <= budget {
        return lines;
    }
    if budget < 3 {
        let at = scroll.unwrap_or_else(|| cursor_line.unwrap_or(0));
        return lines.into_iter().skip(at).take(budget).collect();
    }
    let start = scroll
        .map(|at| at.min(total - budget))
        .unwrap_or_else(|| follow_start(cursor_line, total, budget));
    let end = start + budget;
    let mut shown: Vec<Line<'static>> = lines.drain(start..end).collect();
    if start > 0 {
        shown[0] = marker(start, "above", palette);
    }
    if end < total {
        let last = shown.len() - 1;
        shown[last] = marker(total - end, "below", palette);
    }
    shown
}

fn marker(count: usize, side: &str, palette: Palette) -> Line<'static> {
    Line::from(Span::styled(
        format!("  ... {count} {side}"),
        palette.muted(),
    ))
}
