//! Fold-view rows: a fold's own header line with its share bar, and the
//! group and member rows nested under it.
//!
//! A fold header rolls up every sheep beneath it, so its cells are sums and
//! minimums rather than one process's numbers. That is why the fold view
//! has a narrower schema of its own rather than reusing the flat table's.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::app::{App, GroupTotals, Row, RowKey};
use super::super::super::theme::Palette;
use super::super::cell;
use super::columns::{FoldColumn, fold_name_width};
use super::facts::FrameFacts;
use super::layout::{fit_owned, pad_ground};
use super::row::section_line;
use crate::output::{human_bytes, human_duration};

/// One row of the fold view, dispatching on `key` the way
/// [`super::row::key_line`] does for the flat table: a [`RowKey::Fold`]
/// renders the header, a [`RowKey::Group`] or [`RowKey::Sheep`] renders a
/// member, and a [`RowKey::Section`] renders the same band
/// [`super::row::key_line`] draws.
///
/// A fold's own members are never nested a second level deep (two levels of
/// grouping, never three): a [`RowKey::Sheep`] only ever reaches this table
/// standalone, since a grouped app collapses to its [`RowKey::Group`] header
/// with its instances held back, the same rule [`App::grouping`]'s
/// `ByFold` value enforces when it gathers the rows in the first place.
///
/// `view::mod`'s draw loop calls this in place of
/// [`super::row::key_line`] whenever [`App::grouping`] reads
/// `Grouping::ByFold`.
#[must_use]
pub fn fold_key_line(
    app: &App,
    facts: &FrameFacts<'_>,
    key: &RowKey,
    columns: &[FoldColumn],
    width: u16,
    selected: bool,
) -> Line<'static> {
    match key {
        RowKey::Fold(name) => fold_header_line(app, facts, name, columns, width, selected),
        RowKey::Group(name) => fold_group_line(app, name, columns, width, selected),
        RowKey::Sheep(id) => app.row(*id).map_or_else(
            || Line::from(Span::raw(" ".repeat(usize::from(width)))),
            |row| fold_member_line(app, row, columns, width, selected),
        ),
        RowKey::Section(label) => section_line(label, width, app.data_palette().muted()),
    }
}

/// One fold's header row: [`App::fold_totals`]'s rollup, plus the share bar
/// [`fold_key_line`]'s doc names.
///
/// Header rows render in ink (`Style::default()`) rather than
/// [`Palette::muted()`]: [`fold_member_line`] takes the muted role instead,
/// so a fold's own row reads as the more prominent of the two, the way the
/// design's `edge ×4` header draws brighter than the members under it.
fn fold_header_line(
    app: &App,
    facts: &FrameFacts<'_>,
    name: &str,
    columns: &[FoldColumn],
    width: u16,
    selected: bool,
) -> Line<'static> {
    let palette = app.data_palette();
    let totals = app.fold_totals(name);
    // `FrameFacts`, not a sum of its own: every header in a frame divides by
    // the same number, and computing it here walked the whole flock once per
    // header row.
    let total_memory = facts.flock_memory;
    let share_percent = fold_share_percent(totals.memory, total_memory);
    let share_fill = cell::gauge_fill(totals.memory.unwrap_or(0), total_memory, 20);
    let status = app.fold_uniform_status(name);
    let status_style = status.map_or(Style::default(), |status| palette.status(status));
    let name_width = fold_name_width(width, columns);
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len() * 2 + 1);
    let mut used: u16 = 0;
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", ground));
            used += 2;
        }
        let cell_width = if *column == FoldColumn::Name {
            name_width
        } else {
            column.width()
        };
        let text = fit_owned(
            fold_header_cell(app, name, *column, &totals, total_memory, share_percent),
            cell_width,
        );
        match column {
            FoldColumn::Share => {
                push_fold_share_cell(&mut spans, palette, text, share_fill, ground)
            }
            FoldColumn::Status => spans.push(Span::styled(text, status_style.patch(ground))),
            _ => spans.push(Span::styled(text, Style::default().patch(ground))),
        }
        used += cell_width;
    }
    pad_ground(&mut spans, used, width, ground);
    Line::from(spans)
}

/// One cell of a fold header row.
fn fold_header_cell(
    app: &App,
    name: &str,
    column: FoldColumn,
    totals: &GroupTotals,
    total_memory: Option<u64>,
    share_percent: Option<u32>,
) -> String {
    match column {
        // The disclosure triangle carries two facts nothing else in the row
        // does: that this is a fold header rather than a member or an app
        // group, and whether `z` has collapsed it. Both were colour-only
        // before, and below the `FOLD_ALL` tier there is no Share or Notes
        // cell to rescue them, so a 16-colour terminal or `NO_COLOR` lost the
        // hierarchy entirely. Design rule 3, `README.md:47`.
        FoldColumn::Name => {
            let marker = if app.is_fold_collapsed(name) {
                '\u{25b8}'
            } else {
                '\u{25be}'
            };
            format!("{marker} {name} \u{d7}{}", totals.count)
        }
        FoldColumn::Status => app.fold_status_text(name),
        // The text here is only the source [`push_fold_share_cell`] splits
        // into filled and tail spans; the fill point it uses is computed
        // once by the caller rather than re-derived from this string.
        FoldColumn::Share => cell::gauge(totals.memory.unwrap_or(0), total_memory, 20),
        FoldColumn::Mem => totals.memory.map_or_else(|| "-".to_string(), human_bytes),
        FoldColumn::Cpu => totals
            .cpu
            .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
        FoldColumn::Uptime => totals
            .uptime_ms
            .map_or_else(|| "-".to_string(), human_duration),
        FoldColumn::Restarts => totals.restarts.to_string(),
        FoldColumn::Notes => {
            share_percent.map_or_else(String::new, |percent| format!("{percent}% of flock memory"))
        }
    }
}

/// Pushes the `Share` column's text as two spans, filled and tail, the same
/// split `row::push_row_cell` gives `MemCeil`: the filled run in
/// [`Palette::sky`], the unfilled run in [`Palette::gauge_rest`], so the two
/// do not compete.
fn push_fold_share_cell(
    spans: &mut Vec<Span<'static>>,
    palette: Palette,
    text: String,
    fill: usize,
    ground: Style,
) {
    let fill = fill.min(text.chars().count());
    let mut chars = text.chars();
    let filled: String = chars.by_ref().take(fill).collect();
    let rest: String = chars.collect();
    spans.push(Span::styled(filled, palette.sky().patch(ground)));
    spans.push(Span::styled(rest, palette.gauge_rest().patch(ground)));
}

/// A fold's share of `total_memory`, as a whole percentage. `None` when
/// either side is unmeasured or `total_memory` is zero: the header's own
/// `Notes` cell then draws nothing, matching [`fold_header_line`]'s empty
/// bar for the same case.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // display only, a table cell
fn fold_share_percent(memory: Option<u64>, total_memory: Option<u64>) -> Option<u32> {
    let memory = memory?;
    let total_memory = total_memory?;
    if total_memory == 0 {
        return None;
    }
    Some((memory as f64 / total_memory as f64 * 100.0).round() as u32)
}

/// An app's group header row, laid out in [`FoldColumn`]'s widths rather
/// than [`super::columns::Column`]'s: the same rollup `row::group_line`
/// draws for the flat table, reused rather than recomputed, since
/// [`App::group_totals`] does
/// not care which table is about to draw it.
///
/// `Share` and `Notes` are blank: the share bar is a fact about a fold, and
/// a group nested inside one has no share of its own to state.
fn fold_group_line(
    app: &App,
    name: &str,
    columns: &[FoldColumn],
    width: u16,
    selected: bool,
) -> Line<'static> {
    let palette = app.data_palette();
    // One `group_members` call for the row, the same as `row::group_line`.
    let members = app.group_members(name);
    let totals = app.totals_for(&members);
    let status = App::uniform_status_for(&members);
    let status_style = status.map_or(Style::default(), |status| palette.status(status));
    let status_text = App::status_text_for(&members);
    let name_width = fold_name_width(width, columns);
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let base = palette.muted();
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len() * 2 + 1);
    let mut used: u16 = 0;
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", ground));
            used += 2;
        }
        let cell_width = if *column == FoldColumn::Name {
            name_width
        } else {
            column.width()
        };
        let text = fit_owned(
            fold_group_cell(name, *column, &totals, &status_text),
            cell_width,
        );
        let style = if *column == FoldColumn::Status {
            status_style
        } else {
            base
        };
        spans.push(Span::styled(text, style.patch(ground)));
        used += cell_width;
    }
    pad_ground(&mut spans, used, width, ground);
    Line::from(spans)
}

/// One cell of a group row nested inside a fold.
fn fold_group_cell(
    name: &str,
    column: FoldColumn,
    totals: &GroupTotals,
    status_text: &str,
) -> String {
    match column {
        FoldColumn::Name => format!("{name} \u{d7}{}", totals.count),
        FoldColumn::Status => status_text.to_string(),
        FoldColumn::Share | FoldColumn::Notes => String::new(),
        FoldColumn::Mem => totals.memory.map_or_else(|| "-".to_string(), human_bytes),
        FoldColumn::Cpu => totals
            .cpu
            .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
        FoldColumn::Uptime => totals
            .uptime_ms
            .map_or_else(|| "-".to_string(), human_duration),
        FoldColumn::Restarts => totals.restarts.to_string(),
    }
}

/// One sheep's row, laid out in [`FoldColumn`]'s widths. A fold's own
/// [`RowKey::Sheep`] rows are always standalone (two levels of grouping,
/// never three), so unlike [`super::row::row_line`] this takes no
/// `grouped` flag.
fn fold_member_line(
    app: &App,
    row: &Row,
    columns: &[FoldColumn],
    width: u16,
    selected: bool,
) -> Line<'static> {
    let palette = app.data_palette();
    let status_style = palette.reported(row.reported());
    let name_width = fold_name_width(width, columns);
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let base = palette.muted();
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len() * 2 + 1);
    let mut used: u16 = 0;
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", ground));
            used += 2;
        }
        let cell_width = if *column == FoldColumn::Name {
            name_width
        } else {
            column.width()
        };
        let text = fit_owned(fold_member_cell(app, row, *column), cell_width);
        let style = if *column == FoldColumn::Status {
            status_style
        } else {
            base
        };
        spans.push(Span::styled(text, style.patch(ground)));
        used += cell_width;
    }
    pad_ground(&mut spans, used, width, ground);
    Line::from(spans)
}

/// One cell of a standalone sheep's row in the fold view.
fn fold_member_cell(app: &App, row: &Row, column: FoldColumn) -> String {
    let info = &row.info;
    match column {
        FoldColumn::Name => info.name.clone(),
        // `Row::reported`, not `info.status.to_string()`, for the same
        // reason `cell`'s own `Column::Status` arm gives: a dog that has
        // never handshook must not read `online` here either.
        FoldColumn::Status => row.reported().word(),
        FoldColumn::Share | FoldColumn::Notes => String::new(),
        FoldColumn::Mem => info
            .memory_bytes
            .map_or_else(|| "-".to_string(), human_bytes),
        // `App::cpu_now`, not `info.cpu_percent`: see `cell`'s own
        // `Column::Cpu` arm.
        FoldColumn::Cpu => app
            .cpu_now(info.id)
            .map_or_else(|| "-".to_string(), |cpu| format!("{cpu:.1}%")),
        FoldColumn::Uptime => app
            .uptime_ms(info.id)
            .map_or_else(|| "-".to_string(), human_duration),
        FoldColumn::Restarts => info.restarts.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::fixtures;
    use super::super::columns::fold_columns_for;
    use super::*;

    /// A header sums its members and shows the share of total flock memory.
    #[test]
    fn a_fold_header_shows_its_rollup_and_its_share() {
        let app = fixtures::app_with(
            vec![
                fixtures::sheep_with(1, "api", Some("edge"), 120_000, Some(100 << 20), 2),
                fixtures::sheep_with(2, "cdn", Some("edge"), 30_000, Some(150 << 20), 5),
                fixtures::sheep_with(3, "batch", None, 60_000, Some(50 << 20), 0),
            ],
            fixtures::plain(),
        );
        let line = fold_key_line(
            &app,
            &FrameFacts::new(&app),
            &RowKey::Fold("edge".into()),
            fold_columns_for(160),
            160,
            false,
        );
        let text = fixtures::rendered(&line);
        assert!(text.contains("edge ×2"), "got {text}");
        assert!(text.contains("250.0M"), "memory sums: {text}");
        assert!(text.contains("30s"), "uptime is the minimum: {text}");
        assert!(
            text.contains("83%"),
            "250 of 300 MiB of flock memory: {text}"
        );
    }
}
