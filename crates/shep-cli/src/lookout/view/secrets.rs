//! The secrets pane, drawn straight into the buffer: this screen owns the
//! whole body between the title band and the status bar.
//!
//! Every cell goes through [`fit`], so a long key ends in `…` rather than
//! spilling into the next column. Rows carry no gap between columns, unlike
//! [`super::flock`]'s two-space-separated table: [`cell`] reads a column
//! back by stepping [`Column::width`] alone, so a gap here would be a gap
//! `cell` never accounts for.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::app::{App, Control, SecretsPane};
use super::super::secrets::{SecretRow, Source};
use super::super::theme::Palette;
use super::flock::{GUTTER, fit, gutter};
use super::status;
use crate::output::human_duration;
use crate::vocabulary::Role;

/// One column of the secrets table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Column {
    /// The key, `namespace/KEY` for a provider row.
    Key,
    /// A block run and the in-force value's exact byte count, or the
    /// revealed plaintext.
    Value,
    /// Which environment slot supplies this tab's value.
    InForce,
    /// Every environment with a slot, and how many that is.
    SetIn,
    /// How many sheep name this key, and how many are running.
    ReadBy,
    /// When a change reaches a process, or a reveal's countdown.
    Lands,
}

impl Column {
    /// This column's width in cells at the design tier.
    pub(super) const fn width(self) -> u16 {
        match self {
            Self::Key => 28,
            Self::Value => 30,
            Self::InForce => 14,
            Self::SetIn => 22,
            Self::ReadBy => 22,
            Self::Lands => 42,
        }
    }

    /// The heading printed over it.
    const fn heading(self) -> &'static str {
        match self {
            Self::Key => "KEY",
            Self::Value => "VALUE",
            Self::InForce => "IN FORCE",
            Self::SetIn => "SET IN",
            Self::ReadBy => "READ BY",
            Self::Lands => "LANDS",
        }
    }
}

/// The columns each width still fits, widest first.
///
/// Each threshold is the narrowest terminal that still fits its row, which
/// `every_tier_fits_the_width_it_claims` holds to. `LANDS` drops first,
/// then `READ BY`, then `SET IN`. The remaining three are the pane: a key
/// with no value and no scope answers nothing.
pub(super) const SECRET_TIERS: &[(u16, &[Column])] = &[
    (
        160,
        &[
            Column::Key,
            Column::Value,
            Column::InForce,
            Column::SetIn,
            Column::ReadBy,
            Column::Lands,
        ],
    ),
    (
        118,
        &[
            Column::Key,
            Column::Value,
            Column::InForce,
            Column::SetIn,
            Column::ReadBy,
        ],
    ),
    (
        96,
        &[Column::Key, Column::Value, Column::InForce, Column::SetIn],
    ),
    (74, &[Column::Key, Column::Value, Column::InForce]),
];

/// [`pane_band`]'s label.
const PANE_BAND_LABEL: &str = "SECRETS   flock-wide values a Flockfile refers to and never carries";

/// What the store is, never printed to a log or read before spawn.
const STORE_LINE: &str = "store $SHEP_HOME/secrets.json \u{b7} not encrypted \u{b7} never \
     printed to a log, never carried in a bleat \u{b7} read at spawn, not now";

/// This pane's own band: design rule 1, `docs/lookout/design-files/README.md:45`.
/// Butter ground, since the pane is one you can change something in.
fn pane_band(width: u16, palette: Palette) -> Line<'static> {
    Line::from(Span::styled(
        super::cell::band(PANE_BAND_LABEL, usize::from(width)),
        palette.band(Role::Butter),
    ))
}

/// Both secrets gates, since neither implies the other: `[secrets]
/// allow_read` decides whether a value may be shown, `lookout.allow_control`
/// whether the pane may change anything.
fn gates_line(pane: &SecretsPane, control: Control, palette: Palette) -> Line<'static> {
    let allowed = matches!(control, Control::Allowed);
    Line::from(Span::styled(
        format!(
            "reveal  [secrets] allow_read = {} in shep.toml \u{b7} change  \
             lookout.allow_control = {allowed}",
            pane.model.allow_read
        ),
        palette.muted(),
    ))
}

/// The widest tier `width` still fits, or the narrowest tier below every
/// threshold.
pub(super) fn columns_for(width: u16) -> &'static [Column] {
    SECRET_TIERS
        .iter()
        .find(|(threshold, _)| width >= *threshold)
        .map_or(SECRET_TIERS[SECRET_TIERS.len() - 1].1, |(_, columns)| {
            *columns
        })
}

/// What one row shows in `VALUE`.
///
/// A run proportional to the value's length rather than equal to it: the
/// column is 30 cells and `MAX_VALUE_BYTES` is 4096, so an equal run
/// cannot be drawn. The byte count carries the exact figure, which is what
/// the design's second rule asks for.
fn value_cell(row: &SecretRow, revealed: Option<&str>, width: u16) -> String {
    if let Some(plain) = revealed {
        return fit(plain, width);
    }
    let Some(len) = row.byte_len else {
        return "not set here".to_string();
    };
    let suffix = format!(" {len} bytes");
    let run = usize::from(width).saturating_sub(suffix.len()).min(len);
    format!("{}{suffix}", "█".repeat(run.max(1)))
}

/// One data row's text for `column`.
///
/// `Lands` has no source yet: nothing in [`SecretRow`] carries a propagation
/// ETA. Task 6 gives it one; until then every cell reads `-`, matching
/// `view/detail.rs`'s convention for an absent value.
fn row_cell(row: &SecretRow, column: Column, revealed: Option<&str>) -> String {
    match column {
        Column::Key => row.key.clone(),
        Column::Value => value_cell(row, revealed, column.width()),
        Column::InForce => row.in_force.clone().unwrap_or_else(|| "-".to_string()),
        Column::SetIn => {
            if row.set_in.is_empty() {
                "-".to_string()
            } else {
                format!("{} ({})", row.set_in.join(", "), row.set_in.len())
            }
        }
        Column::ReadBy => {
            if row.readers.is_empty() {
                "-".to_string()
            } else {
                let online = row.readers.iter().filter(|reader| reader.online).count();
                format!("{} ({online} online)", row.readers.len())
            }
        }
        Column::Lands => "-".to_string(),
    }
}

/// One row's [`Line`], every column packed against the next with no gap
/// (see the module doc): [`GUTTER`] plus `columns` is exactly what [`cell`]
/// reads back.
fn row_line(
    pane: &SecretsPane,
    row: &SecretRow,
    columns: &[Column],
    width: u16,
    palette: Palette,
    selected: bool,
) -> Line<'static> {
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let revealed = pane
        .reveal
        .as_ref()
        .filter(|reveal| reveal.key == row.key)
        .map(|reveal| reveal.value.as_str());
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len());
    let mut used = 0u16;
    for column in columns {
        let text = fit(&row_cell(row, *column, revealed), column.width());
        spans.push(Span::styled(text, ground));
        used += column.width();
    }
    pad(&mut spans, used, width, ground);
    Line::from(spans)
}

/// Pads `spans` out to `width` with blank cells, the way a data row's
/// trailing slack past its last column still has to be blanked. Mirrors
/// `flock::pad_ground`, private there.
fn pad(spans: &mut Vec<Span<'static>>, used: u16, width: u16, style: Style) {
    let short = width.saturating_sub(used);
    if short > 0 {
        spans.push(Span::styled(" ".repeat(usize::from(short)), style));
    }
}

/// The column headings, muted, packed the same way [`row_line`] packs a
/// data row.
fn heading_line(columns: &[Column], palette: Palette) -> Line<'static> {
    let mut text = String::new();
    for column in columns {
        text.push_str(&fit(column.heading(), column.width()));
    }
    Line::from(Span::styled(text, palette.muted()))
}

/// The operator store's own read refusal when there is one, otherwise how
/// long ago the muster roll (and so `READ BY`) was written. Blank when
/// neither applies.
fn roll_status_line(pane: &SecretsPane, palette: Palette) -> Line<'static> {
    if let Some(message) = &pane.model.unreadable {
        return Line::from(Span::styled(
            format!("operator store unreadable: {message}"),
            palette.refusal(),
        ));
    }
    if let Some(age) = pane.model.roll_age {
        let ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
        return Line::from(Span::styled(
            format!("READ BY as of the roll, read {} ago", human_duration(ms)),
            palette.muted(),
        ));
    }
    Line::default()
}

/// The tab row: every environment [`super::super::secrets::SecretsModel`]
/// found a slot for, plus `all`. The active one is bracketed
/// (`[production]`) in addition to whatever the palette paints, since a
/// signal carried by colour alone says nothing under `NO_COLOR`.
///
/// Right-aligned within `width`: design rule 2, every measurement states
/// its denominator, and this one is the store's own count of named
/// environments (`environments` minus [`super::super::secrets::ALL_ENVIRONMENTS`]).
fn tab_line(pane: &SecretsPane, palette: Palette, width: u16) -> Line<'static> {
    let mut spans = Vec::with_capacity(pane.model.environments.len() * 2);
    let mut drawn = 0usize;
    for (index, name) in pane.model.environments.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
            drawn += 2;
        }
        if index == pane.tab {
            let text = format!("[{name}]");
            drawn += text.chars().count();
            spans.push(Span::styled(text, palette.attention()));
        } else {
            drawn += name.chars().count();
            spans.push(Span::styled(name.clone(), palette.muted()));
        }
    }
    let environment_count = pane.model.environments.len().saturating_sub(1);
    let suffix =
        format!("{environment_count} environments in this store \u{b7} \u{2190}/\u{2192} or tab");
    let pad = usize::from(width)
        .saturating_sub(drawn)
        .saturating_sub(suffix.chars().count());
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.push(Span::styled(suffix, palette.muted()));
    Line::from(spans)
}

/// One group's header row: its label, its member count, and `read-only
/// here` for a provider namespace, which owns nothing an operator can edit.
///
/// The disclosure triangle mirrors `flock::fold_header_cell`'s own: filled
/// when open, outlined when [`SecretsPane::collapsed`] holds this
/// namespace. The operator group never collapses — `on_secrets_key`'s `z`
/// only ever inserts a namespace into `collapsed`.
fn group_header_line(
    pane: &SecretsPane,
    source: &Source,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let count = pane.model.rows_for(source).count();
    let text = match source {
        Source::Operator => format!("\u{25be} operator \u{d7}{count}"),
        Source::Namespace(namespace) => {
            let marker = if pane.collapsed.contains(namespace) {
                '\u{25b8}'
            } else {
                '\u{25be}'
            };
            format!("{marker} {namespace} \u{d7}{count}  read-only here")
        }
    };
    Line::from(Span::styled(fit(&text, width), palette.muted()))
}

/// Whether `source`'s rows are folded away: only a provider namespace can
/// be, mirroring `on_secrets_key`'s `Collapse` arm.
fn is_collapsed(pane: &SecretsPane, source: &Source) -> bool {
    match source {
        Source::Operator => false,
        Source::Namespace(namespace) => pane.collapsed.contains(namespace),
    }
}

/// Draws the secrets pane into `area`, straight into `buffer`.
///
/// Seven rows of chrome before the first group header: this pane's own
/// band (design rule 1), the store's terms, the two gates, the roll's own
/// status, the tab row, the heading row, a hairline, then one group header
/// per source change in [`SecretsPane::model`]'s rows, which are already
/// contiguous by source (`SecretsModel::rows`'s own doc comment: operator
/// rows first, then each namespace's).
pub fn draw(app: &App, pane: &SecretsPane, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let palette = app.palette();
    let width = area.width;
    let table_width = width.saturating_sub(GUTTER);
    // `width`, not `table_width`: [`SECRET_TIERS`]' thresholds are the
    // design's own row width, gutter included.
    let columns = columns_for(width);
    let bottom = area.y + area.height;
    let mut y = area.y;

    buffer.set_line(area.x, y, &pane_band(width, palette), width);
    y += 1;
    if y >= bottom {
        return;
    }

    buffer.set_line(
        area.x,
        y,
        &Line::from(Span::styled(STORE_LINE, palette.muted())),
        width,
    );
    y += 1;
    if y >= bottom {
        return;
    }

    buffer.set_line(area.x, y, &gates_line(pane, app.control(), palette), width);
    y += 1;
    if y >= bottom {
        return;
    }

    buffer.set_line(area.x, y, &roll_status_line(pane, palette), width);
    y += 1;
    if y >= bottom {
        return;
    }

    buffer.set_line(area.x, y, &tab_line(pane, palette, width), width);
    y += 1;
    if y >= bottom {
        return;
    }

    buffer.set_line(
        area.x + GUTTER,
        y,
        &heading_line(columns, palette),
        table_width,
    );
    y += 1;
    if y >= bottom {
        return;
    }

    buffer.set_line(area.x, y, &status::rule_line(palette.line(), width), width);
    y += 1;

    let mut last_source: Option<&Source> = None;
    for (index, row) in pane.model.rows.iter().enumerate() {
        if y >= bottom {
            break;
        }
        if last_source != Some(&row.source) {
            buffer.set_line(
                area.x + GUTTER,
                y,
                &group_header_line(pane, &row.source, palette, table_width),
                table_width,
            );
            y += 1;
            last_source = Some(&row.source);
            if y >= bottom {
                break;
            }
        }
        if is_collapsed(pane, &row.source) {
            continue;
        }
        let selected = index == pane.selected;
        let (gutter_text, gutter_style) = gutter(selected, palette);
        buffer.set_line(
            area.x,
            y,
            &Line::from(Span::styled(gutter_text, gutter_style)),
            1,
        );
        buffer.set_line(
            area.x + GUTTER,
            y,
            &row_line(pane, row, columns, table_width, palette, selected),
            table_width,
        );
        y += 1;
    }
}

/// One cell's text, read at its own column offset.
///
/// Assertions go through this rather than searching the whole row.
/// `production` is the tab label, a `SET IN` entry and an `IN FORCE`
/// value at the same time, so a row-wide `contains` would pass on any of
/// the three.
#[cfg(test)]
pub(super) fn cell(buffer: &Buffer, row: u16, column: Column) -> String {
    let columns = columns_for(buffer.area.width);
    let mut x = GUTTER;
    for candidate in columns {
        if *candidate == column {
            return (x..x + column.width())
                .map(|x| buffer[(x, row)].symbol())
                .collect();
        }
        x += candidate.width();
    }
    panic!("{column:?} is not drawn at width {}", buffer.area.width);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::view::fixtures;

    /// The first data row `draw` places, fixed regardless of which source
    /// the first group is.
    /// `160x48`: the width and height every test in this module renders
    /// at, and so the exact chrome above the first data row: the title
    /// band and its blank line the dashboard draws before handing off to
    /// [`super::draw`], then this pane's own band, the store's terms, the
    /// two gates, the roll's own status, the tab row, the heading row, the
    /// hairline and the operator group's header.
    fn first_row() -> u16 {
        10
    }

    /// The row whose rendered text contains `needle`, or panics: every
    /// fixture below names a key unique enough that only one row can match.
    fn row_of(buffer: &Buffer, needle: &str) -> u16 {
        let area = buffer.area;
        (0..area.height)
            .find(|&y| {
                let line: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
                line.contains(needle)
            })
            .unwrap_or_else(|| panic!("{needle:?} is not drawn anywhere"))
    }

    #[test]
    fn every_tier_fits_the_width_it_claims() {
        for (threshold, columns) in SECRET_TIERS {
            let spent: u16 = columns.iter().map(|column| column.width()).sum();
            assert!(
                spent + GUTTER <= *threshold,
                "tier {threshold} spends {spent} plus {GUTTER} of gutter"
            );
        }
    }

    #[test]
    fn the_columns_drop_in_the_specified_order() {
        let widest = columns_for(160);
        assert!(widest.contains(&Column::Lands));
        assert!(
            !columns_for(118).contains(&Column::Lands),
            "LANDS goes first"
        );
        assert!(columns_for(118).contains(&Column::ReadBy));
        assert!(
            !columns_for(96).contains(&Column::ReadBy),
            "READ BY goes second"
        );
        assert!(
            !columns_for(74).contains(&Column::SetIn),
            "SET IN goes third"
        );
        for width in [160, 118, 96, 74, 40] {
            let columns = columns_for(width);
            assert!(columns.contains(&Column::Key), "KEY is the pane at {width}");
            assert!(
                columns.contains(&Column::Value),
                "VALUE is the pane at {width}"
            );
            assert!(
                columns.contains(&Column::InForce),
                "IN FORCE is the pane at {width}"
            );
        }
    }

    #[test]
    fn in_force_is_read_out_of_its_own_column_not_found_anywhere_in_the_row() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);

        // `production` is also the tab label and appears in SET IN. Reading
        // the cell rather than the row is what makes this assertion mean
        // anything.
        assert_eq!(
            cell(&buffer, first_row(), Column::InForce).trim(),
            "production"
        );
    }

    #[test]
    fn a_key_with_no_slot_here_says_so_rather_than_showing_a_block_run() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let row = row_of(&buffer, "ELSEWHERE_ONLY");

        assert_eq!(cell(&buffer, row, Column::Value).trim(), "not set here");
        assert_eq!(cell(&buffer, row, Column::InForce).trim(), "-");
    }

    #[test]
    fn a_four_kilobyte_value_never_overflows_its_column() {
        let app = fixtures::app_with_a_maximum_length_secret();
        let buffer = fixtures::render(&app, 160, 48);
        let value = cell(&buffer, first_row(), Column::Value);

        assert!(
            value.chars().count() <= Column::Value.width() as usize,
            "spilled: {value:?}"
        );
        assert!(
            value.contains("4096 bytes"),
            "the exact length is stated: {value:?}"
        );
    }

    #[test]
    fn a_provider_group_says_it_is_read_only() {
        let app = fixtures::app_with_a_pushed_secret();
        let buffer = fixtures::render(&app, 160, 48);
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|line| line.contains("read-only here")),
            "the group header states it: {text:?}"
        );
    }
}
