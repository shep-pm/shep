//! The catalogue screen: the dog index as a list, and one entry opened from
//! it.
//!
//! Drawn by hand like the secrets pane, straight into the buffer. The list
//! drops columns as the terminal narrows, the way the flock table does; the
//! entry wraps its values instead, since a repo URL cut short is no repo.

use super::super::app::{App, CataloguePane, CatalogueRow};
use super::cell;
use super::flock::{GUTTER, fit, gutter};
use super::status;
use crate::dog_index::DogSourceKind;
use crate::output::width::text_columns;
use crate::vocabulary::Role;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

#[cfg(test)]
mod tests;

/// The band's label on the list.
const BAND_LABEL: &str = "CATALOGUE   dogs the index lists";

/// Columns the entry's labels take, gap included.
const LABEL_WIDTH: u16 = 13;

/// The list's columns, widest first: the shortest `width` that gets the
/// column set, and the widths of NAME, CATEGORY, VERSION and STATE in it.
/// A zero drops the column.
///
/// STATE takes the rest of the row, so each floor is the widths above it plus
/// [`STATE_MIN`].
const TIERS: [(u16, [u16; 3]); 3] = [(70, [20, 13, 18]), (52, [20, 0, 18]), (0, [17, 0, 0])];

/// Longest STATE word, `not installed`, and a gap.
const STATE_MIN: u16 = 14;

/// Draws the catalogue into `area`, straight into `buffer`.
pub(super) fn draw(app: &App, pane: &CataloguePane, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = app.catalogue_rows();
    let open = pane
        .entry
        .as_ref()
        .and_then(|name| rows.iter().find(|row| &row.dog.adopt_as == name));
    match open {
        Some(row) => draw_entry(app, row, area, buffer),
        None => draw_list(app, pane, rows, area, buffer),
    }
}

fn band(label: &str, app: &App, area: Rect, buffer: &mut Buffer) {
    buffer.set_line(
        area.x,
        area.y,
        &Line::from(Span::styled(
            cell::band(label, usize::from(area.width)),
            app.palette().band(Role::Meadow),
        )),
        area.width,
    );
}

fn draw_list(
    app: &App,
    pane: &CataloguePane,
    rows: &[CatalogueRow],
    area: Rect,
    buffer: &mut Buffer,
) {
    let palette = app.palette();
    let bottom = area.y + area.height;
    band(BAND_LABEL, app, area, buffer);
    let mut y = area.y + 1;
    if y >= bottom {
        return;
    }
    buffer.set_line(
        area.x,
        y,
        &Line::from(Span::styled(app.catalogue_caption(), palette.muted())),
        area.width,
    );
    y += 1;
    if rows.is_empty() || y + 2 >= bottom {
        return;
    }
    let table = area.width.saturating_sub(GUTTER);
    let [name, category, version] = TIERS
        .iter()
        .find(|(least, _)| table >= *least)
        .map_or([0; 3], |(_, widths)| *widths);
    let state = table
        .saturating_sub(name + category + version)
        .max(STATE_MIN);
    let line = |cells: [&str; 4], style| {
        let mut text = String::new();
        for (cell, width) in cells.into_iter().zip([name, category, version, state]) {
            if width > 0 {
                text.push_str(&fit(cell, width));
            }
        }
        Line::from(Span::styled(text, style))
    };
    buffer.set_line(
        area.x + GUTTER,
        y,
        &line(["NAME", "CATEGORY", "VERSION", "STATE"], palette.muted()),
        table,
    );
    y += 1;
    buffer.set_line(
        area.x,
        y,
        &status::rule_line(palette.line(), area.width),
        area.width,
    );
    y += 1;
    let visible = usize::from(bottom - y);
    let first = (pane.selected + 1).saturating_sub(visible);
    for (index, row) in rows.iter().enumerate().skip(first).take(visible) {
        let selected = index == pane.selected;
        let (mark, mark_style) = gutter(selected, palette);
        buffer.set_line(area.x, y, &Line::from(Span::styled(mark, mark_style)), 1);
        let ink = if selected {
            palette.ground()
        } else {
            ratatui::style::Style::default()
        };
        buffer.set_line(
            area.x + GUTTER,
            y,
            &line(
                [
                    &row.dog.name,
                    &row.dog.category,
                    &row.version,
                    row.state.as_str(),
                ],
                ink,
            ),
            table,
        );
        y += 1;
    }
}

/// How the entry words a dog's source: the kind, and the approved version
/// when it has one.
fn kind_of(source: &DogSourceKind) -> String {
    match source {
        DogSourceKind::Cargo {
            version: Some(version),
            ..
        } => format!("cargo {version}"),
        DogSourceKind::Cargo { version: None, .. } => "cargo, no approved version".to_owned(),
        DogSourceKind::CargoGit { .. } => "cargo, from git".to_owned(),
        DogSourceKind::GoInstall { .. } => "go install".to_owned(),
        DogSourceKind::Manual { .. } => "manual".to_owned(),
    }
}

/// The permissions a row lists, one string each, or the one word for none.
fn permission_values(row: &CatalogueRow) -> Vec<String> {
    match &row.dog.permissions {
        None => vec!["not listed".to_owned()],
        Some(listed) if listed.is_empty() => vec!["none".to_owned()],
        Some(listed) => listed
            .iter()
            .map(|permission| {
                let mark = if permission.required {
                    "required"
                } else {
                    "optional"
                };
                format!("{} ({mark})", permission.name)
            })
            .collect(),
    }
}

/// `text` broken into lines of at most `width` columns, at spaces where it
/// can and inside a word where it must.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_owned();
        while text_columns(&word) > width {
            if !current.is_empty() {
                lines.push(core::mem::take(&mut current));
            }
            let mut head = String::new();
            let mut rest = String::new();
            for ch in word.chars() {
                if rest.is_empty()
                    && text_columns(&head) + text_columns(ch.encode_utf8(&mut [0; 4])) <= width
                {
                    head.push(ch);
                } else {
                    rest.push(ch);
                }
            }
            if head.is_empty() {
                break;
            }
            lines.push(head);
            word = rest;
        }
        let joined =
            text_columns(&current) + usize::from(!current.is_empty()) + text_columns(&word);
        if !current.is_empty() && joined > width {
            lines.push(core::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn draw_entry(app: &App, row: &CatalogueRow, area: Rect, buffer: &mut Buffer) {
    let palette = app.palette();
    let bottom = area.y + area.height;
    band(&format!("CATALOGUE   {}", row.dog.name), app, area, buffer);
    let value_width = usize::from(area.width.saturating_sub(GUTTER + LABEL_WIDTH));
    let mut fields: Vec<(&str, Vec<String>)> = vec![
        ("description", vec![row.dog.description.clone()]),
        ("repo", vec![row.dog.repo.clone()]),
        ("license", vec![row.dog.license.clone()]),
        ("kind", vec![kind_of(&row.dog.source)]),
        (
            "state",
            vec![format!("{} \u{b7} {}", row.state, row.version)],
        ),
    ];
    if app.can_install(row) {
        fields.push(("install", vec!["enter installs it".to_owned()]));
    }
    if !matches!(
        row.dog.source,
        DogSourceKind::Cargo {
            version: Some(_),
            ..
        }
    ) {
        fields.push((
            "build",
            vec![crate::commands::query::install_instruction(&row.dog)],
        ));
        fields.push((
            "adopt",
            vec![crate::commands::query::adopt_instruction(&row.dog)],
        ));
    }
    fields.push(("permissions", permission_values(row)));
    let mut y = area.y + 2;
    for (label, values) in fields {
        let mut label = Some(label);
        for value in values {
            for line in wrap(&value, value_width) {
                if y >= bottom {
                    return;
                }
                let cells = vec![
                    Span::styled(
                        fit(label.take().unwrap_or(""), LABEL_WIDTH),
                        palette.muted(),
                    ),
                    Span::raw(line),
                ];
                buffer.set_line(area.x + GUTTER, y, &Line::from(cells), area.width - GUTTER);
                y += 1;
            }
        }
    }
}
