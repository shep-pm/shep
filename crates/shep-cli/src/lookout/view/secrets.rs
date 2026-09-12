//! The secrets pane, drawn straight into the buffer: this screen owns the
//! whole body between the title band and the status bar.
//!
//! Every cell goes through [`fit`], so a long key ends in `…` rather than
//! spilling into the next column. Rows carry no gap between columns, unlike
//! [`super::flock`]'s two-space-separated table: [`cell`] reads a column
//! back by stepping [`Column::width`] alone, so a gap here would be a gap
//! `cell` never accounts for.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::app::{App, Control, REVEAL_HOLDS, Reveal, SecretsPane, TypingWhat};
use super::super::secrets::{SecretRow, Source};
use super::super::theme::Palette;
use super::flock::{GUTTER, fit, gutter};
use super::status;
use crate::output::human_duration;
use crate::secret_readers::Reader;
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

/// The narrowest terminal this pane draws a table into: [`SECRET_TIERS`]'
/// own floor tier, a terminal width rather than a table width (the
/// thresholds count the gutter, unlike [`super::flock::MIN_WIDTH`]).
///
/// There is nothing below it to fall back to. The floor tier's three
/// columns are what [`SECRET_TIERS`]' own doc calls the pane, and a
/// narrower terminal used to get them anyway, clipped mid-column by
/// `Buffer::set_line` with nothing on screen saying so.
const MIN_WIDTH: u16 = SECRET_TIERS[SECRET_TIERS.len() - 1].0;

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

/// `len` in bytes, singular when there is one of them.
fn byte_count(len: usize) -> String {
    if len == 1 {
        "1 byte".to_string()
    } else {
        format!("{len} bytes")
    }
}

/// What one row shows in `VALUE`.
///
/// A run proportional to the value's length rather than equal to it: the
/// column is 30 cells against `MAX_VALUE_BYTES`'s 4096, so an equal run
/// cannot be drawn. It stops one cell short of `width` so it never touches
/// `IN FORCE`'s own text.
///
/// `typing` wins over a reveal: the operator is looking at what they are
/// about to send, not at what the store already holds. The block run and a
/// reveal both name a length or a plaintext already on screen; typed text is
/// the same kind of thing, one keystroke ahead of the store.
fn value_cell(
    row: &SecretRow,
    revealed: Option<&Reveal>,
    typing: Option<&str>,
    width: u16,
) -> String {
    if let Some(buffer) = typing {
        return fit(&format!("{buffer}\u{2588}"), width);
    }
    if let Some(reveal) = revealed {
        return fit(&reveal.value, width);
    }
    let Some(len) = row.byte_len else {
        return "not set here".to_string();
    };
    let suffix = format!(" {}", byte_count(len));
    let run = usize::from(width)
        .saturating_sub(suffix.len())
        .saturating_sub(1)
        .min(len);
    format!("{}{suffix}", "█".repeat(run.max(1)))
}

/// `SET IN`'s text: a numerator against every tab in [`tab_line`], `all`
/// included, since `all` is a slot a key can hold and a tab an operator can
/// select. Every tab named once each is the common case and needs no list;
/// anything short of that names which ones.
fn set_in_cell(row: &SecretRow, environment_count: usize) -> String {
    if row.set_in.is_empty() {
        return "-".to_string();
    }
    let count = row.set_in.len();
    if environment_count > 0 && count >= environment_count {
        format!("{count} of {environment_count}")
    } else {
        format!(
            "{count} of {environment_count} \u{b7} {}",
            row.set_in.join(", ")
        )
    }
}

/// The reveal countdown's gauge, in cells.
const GAUGE_CELLS: usize = 10;

/// How long a revealed value has left, in words and in blocks.
///
/// The gauge is scaled from the number printed beside it rather than from
/// the duration underneath, so the two can never read a second apart. That
/// number rounds up: a part-second still on screen is a second the operator
/// can still read the value in.
fn countdown(until: Instant, now: Instant) -> String {
    let left = until
        .saturating_duration_since(now)
        .as_millis()
        .div_ceil(1000);
    let hold = u128::from(REVEAL_HOLDS.as_secs()).max(1);
    let cells = u128::try_from(GAUGE_CELLS).unwrap_or(0);
    let filled = usize::try_from(left * cells / hold)
        .unwrap_or(0)
        .min(GAUGE_CELLS);
    format!(
        "visible {left}s {}{}",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(GAUGE_CELLS - filled)
    )
}

/// One data row's text for `column`.
///
/// `Lands` carries the reveal's own countdown for the revealed row and `-`
/// everywhere else: no [`SecretRow`] field carries a propagation ETA yet,
/// and `-` is `view/detail.rs`'s convention for an absent value.
fn row_cell(
    row: &SecretRow,
    column: Column,
    revealed: Option<&Reveal>,
    typing: Option<&str>,
    environment_count: usize,
    now: Instant,
) -> String {
    match column {
        Column::Key => row.key.clone(),
        Column::Value => value_cell(row, revealed, typing, column.width()),
        Column::InForce => row.in_force.clone().unwrap_or_else(|| "-".to_string()),
        Column::SetIn => set_in_cell(row, environment_count),
        Column::ReadBy => {
            if row.readers.is_empty() {
                "-".to_string()
            } else {
                let online = row.readers.iter().filter(|reader| reader.online).count();
                format!("{} ({online} online)", row.readers.len())
            }
        }
        Column::Lands => {
            revealed.map_or_else(|| "-".to_string(), |reveal| countdown(reveal.until, now))
        }
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
    now: Instant,
) -> Line<'static> {
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let revealed = pane.reveal.as_ref().filter(|reveal| reveal.key == row.key);
    let typing = if selected {
        pane.typing.as_ref().and_then(|typing| match &typing.what {
            TypingWhat::ValueFor(key) if key == &row.key => Some(typing.buffer.as_str()),
            TypingWhat::ValueFor(_) | TypingWhat::NewKey => None,
        })
    } else {
        None
    };
    let environment_count = pane.model.environments.len();
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len());
    let mut used = 0u16;
    for column in columns {
        let text = fit(
            &row_cell(row, *column, revealed, typing, environment_count, now),
            column.width(),
        );
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

/// The `+ new key` row's own [`Line`]: `KEY` names the affordance, `VALUE`
/// echoes the name step's own buffer while it is open, and every other
/// column is blank: there is no key yet for any of them to describe.
fn new_key_row_line(
    pane: &SecretsPane,
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
    let typing_name = selected
        .then_some(pane.typing.as_ref())
        .flatten()
        .and_then(|typing| {
            matches!(typing.what, TypingWhat::NewKey).then_some(typing.buffer.as_str())
        });
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(columns.len());
    let mut used = 0u16;
    for column in columns {
        let text = match column {
            Column::Key => "+ new key".to_string(),
            Column::Value => {
                typing_name.map_or_else(String::new, |buffer| format!("{buffer}\u{2588}"))
            }
            Column::InForce | Column::SetIn | Column::ReadBy | Column::Lands => String::new(),
        };
        spans.push(Span::styled(fit(&text, column.width()), ground));
        used += column.width();
    }
    pad(&mut spans, used, width, ground);
    Line::from(spans)
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
    // Distinct from a key nothing reads: that reads "-" in `READ BY` with a
    // roll behind it. This is the roll itself missing.
    Line::from(Span::styled(
        "no muster roll yet: READ BY and WHO READS IT show nothing until the shepherd writes one",
        palette.muted(),
    ))
}

/// The tab row: every environment [`super::super::secrets::SecretsModel`]
/// found a slot for, plus `all`. The active one is bracketed
/// (`[production]`) in addition to whatever the palette paints, since a
/// signal carried by colour alone says nothing under `NO_COLOR`.
///
/// Right-aligned within `width`: design rule 2, every measurement states
/// its denominator, and this one is the count of tabs drawn above it,
/// `all` included, so the number is checkable against the row it sits under.
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
    let environment_count = pane.model.environments.len();
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
/// namespace. The operator group never collapses: `on_secrets_key`'s `z`
/// only ever inserts a namespace into `collapsed`.
fn group_header_line(
    pane: &SecretsPane,
    source: &Source,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let count = pane.model.rows_for(source).count();
    let text = match source {
        Source::Operator => format!("\u{25be} operator \u{d7}{count} \u{b7} you set these"),
        Source::Namespace(namespace) => {
            let marker = if pane.collapsed.contains(namespace) {
                '\u{25b8}'
            } else {
                '\u{25be}'
            };
            format!(
                "{marker} {namespace} (dog) \u{d7}{count} \u{b7} pushed by a provider \u{b7} \
                 read-only here"
            )
        }
    };
    Line::from(Span::styled(fit(&text, width), palette.muted()))
}

/// The FOCUSED panel's width, fixed: the frame's own layout,
/// `docs/brainstorming/specs/2026-09-08-lookout-1h-secrets-design.md`
/// ("The frame"). WHO READS IT takes whatever is left of the row.
const FOCUSED_WIDTH: u16 = 88;

/// How many rows the two panels' own content spends, below their shared
/// header line: fixed at the frame's own count, whatever either panel has
/// to say.
const PANEL_CONTENT_ROWS: u16 = 4;

/// Everything below the hairline: the panels' own header line plus their
/// content, on top of the hairline itself.
const PANEL_ROWS: u16 = 1 + 1 + PANEL_CONTENT_ROWS;

/// One [`Reader`]'s line in WHO READS IT: glyph and words together, since a
/// signal carried by colour alone says nothing under `NO_COLOR`.
///
/// Never "holds the value": nothing tells a sheep spawned before a `set`
/// from one spawned after, so the caption states only what the roll can
/// prove either way.
fn reader_line(reader: &Reader) -> String {
    if reader.online {
        format!(
            "\u{2588} {}   online, was given a value at spawn",
            reader.name
        )
    } else {
        format!(
            "\u{2591} {}   not running, reads it at next start",
            reader.name
        )
    }
}

/// What both panels say when no row is selected, so the `+ new key`
/// affordance cannot have one panel calling it nothing and the other
/// describing a key that does not exist.
const NO_KEY_SELECTED: &str = "no key selected";

/// FOCUSED's four content lines for `row`, or a placeholder when nothing is
/// selected (the `+ new key` row, or an empty pane).
///
/// The clipboard sentence lives here rather than only in `y`'s own status
/// notice, since that notice reaches an operator after the value is already
/// on their system clipboard: this is the standing warning, read before `y`
/// is ever pressed.
fn focused_lines(
    row: Option<&SecretRow>,
    width: u16,
) -> [Line<'static>; PANEL_CONTENT_ROWS as usize] {
    let Some(row) = row else {
        return [
            Line::from(Span::raw(fit(NO_KEY_SELECTED, width))),
            Line::default(),
            Line::default(),
            Line::default(),
        ];
    };
    let set_in = if row.set_in.is_empty() {
        "not set here".to_string()
    } else {
        format!("set in {}", row.set_in.join(", "))
    };
    let detail = match row.byte_len {
        Some(len) => format!(
            "{set_in} \u{b7} length {} \u{b7} named by {} of the flock",
            byte_count(len),
            row.readers.len()
        ),
        None => set_in,
    };
    [
        Line::from(Span::raw(fit(
            "the value leaves the screen after 10s. nothing records that you looked.",
            width,
        ))),
        Line::from(Span::raw(fit(
            "the store is 0600: anyone who can reveal can also delete a log.",
            width,
        ))),
        Line::from(Span::raw(fit(
            "the system clipboard is readable by every process on the desktop.",
            width,
        ))),
        Line::from(Span::raw(fit(&detail, width))),
    ]
}

/// How many readers [`who_reads_it_lines`] has room to list before it has
/// to spend a line on an overflow notice instead.
const READER_ROWS: usize = PANEL_CONTENT_ROWS as usize - 1;

/// The text for each of [`READER_ROWS`] reader lines: one per reader when
/// they all fit, otherwise the first `READER_ROWS - 1` plus a line naming
/// how many more there are, so this panel's own total always matches
/// [`focused_lines`]' `named by {} of the flock` rather than looking
/// complete at three when a key has five.
fn reader_row_texts(readers: &[Reader]) -> Vec<String> {
    if readers.is_empty() {
        return vec!["nothing names this key".to_string()];
    }
    if readers.len() <= READER_ROWS {
        return readers.iter().map(reader_line).collect();
    }
    let mut texts: Vec<String> = readers[..READER_ROWS - 1].iter().map(reader_line).collect();
    let overflow = readers.len() - (READER_ROWS - 1);
    texts.push(format!(
        "+ {overflow} more, named by {} of the flock",
        readers.len()
    ));
    texts
}

/// WHO READS IT's four content lines for `row`: up to [`READER_ROWS`]
/// readers, then a caption naming where a reference can live, matching
/// [`focused_lines`]' own row count so the two panels stay lined up.
fn who_reads_it_lines(
    row: Option<&SecretRow>,
    width: u16,
) -> [Line<'static>; PANEL_CONTENT_ROWS as usize] {
    // No row is the `+ new key` affordance, where there is no key for
    // anything to name. Says what FOCUSED says rather than answering a
    // question about a key that does not exist yet.
    let Some(row) = row else {
        return [
            Line::from(Span::raw(fit(NO_KEY_SELECTED, width))),
            Line::default(),
            Line::default(),
            Line::default(),
        ];
    };
    let texts = reader_row_texts(&row.readers);
    let mut lines: Vec<Line<'static>> = (0..READER_ROWS)
        .map(|index| match texts.get(index) {
            Some(text) => Line::from(Span::raw(fit(text, width))),
            None => Line::default(),
        })
        .collect();
    lines.push(Line::from(Span::styled(
        fit("named in env, args, out_file or err_file", width),
        Style::default(),
    )));
    lines.try_into().unwrap_or_else(|_| {
        [
            Line::default(),
            Line::default(),
            Line::default(),
            Line::default(),
        ]
    })
}

/// Draws the FOCUSED and WHO READS IT panels into the last [`PANEL_ROWS`]
/// rows of `area`, below a hairline of their own: FOCUSED at
/// [`FOCUSED_WIDTH`], WHO READS IT taking the rest, per the frame.
fn draw_panels(pane: &SecretsPane, palette: Palette, area: Rect, buffer: &mut Buffer, top: u16) {
    let width = area.width;
    buffer.set_line(
        area.x,
        top,
        &status::rule_line(palette.line(), width),
        width,
    );
    let row = pane.model.rows.get(pane.selected);
    let key = row.map_or("", |row| row.key.as_str());
    let right_x = area.x + FOCUSED_WIDTH.min(width);
    let right_width = width.saturating_sub(FOCUSED_WIDTH);

    buffer.set_line(
        area.x,
        top + 1,
        &Line::from(Span::styled(
            fit(&format!("FOCUSED  {key}"), FOCUSED_WIDTH.min(width)),
            palette.muted(),
        )),
        FOCUSED_WIDTH.min(width),
    );
    if right_width > 0 {
        buffer.set_line(
            right_x,
            top + 1,
            &Line::from(Span::styled(
                fit(&format!("WHO READS IT  {key}"), right_width),
                palette.muted(),
            )),
            right_width,
        );
    }

    for (offset, line) in focused_lines(row, FOCUSED_WIDTH.min(width))
        .iter()
        .enumerate()
    {
        let offset = u16::try_from(offset).unwrap_or(0);
        buffer.set_line(area.x, top + 2 + offset, line, FOCUSED_WIDTH.min(width));
    }
    if right_width > 0 {
        for (offset, line) in who_reads_it_lines(row, right_width).iter().enumerate() {
            let offset = u16::try_from(offset).unwrap_or(0);
            buffer.set_line(right_x, top + 2 + offset, line, right_width);
        }
    }
}

/// Rows [`draw`] spends before the first group header: this pane's own
/// band, the store's terms, the two gates, the roll's status, the tab row,
/// the heading row and the hairline. `the_chrome_is_the_rows_it_claims`
/// reads the count back off a render, so the two cannot drift.
const CHROME_ROWS: u16 = 7;

/// The shortest `area` with room for the chrome, one row of content and
/// both panels.
const PANELS_MIN_ROWS: u16 = CHROME_ROWS + 1 + PANEL_ROWS;

/// `area`'s own bottom, short by [`PANEL_ROWS`] whenever there is room for
/// the two panels below it, so no data row ever draws underneath them.
///
/// Room counts the chrome, not the panels alone. Measuring the panels by
/// themselves reserved six rows at every height from `PANEL_ROWS + 1` up,
/// and `draw`'s own chrome guards reached that boundary first: the panels
/// were skipped and their rows stayed blank.
fn content_bottom(area: Rect) -> u16 {
    let bottom = area.y + area.height;
    if area.height >= PANELS_MIN_ROWS {
        bottom - PANEL_ROWS
    } else {
        bottom
    }
}

/// Draws the `+ new key` affordance at the current `y` and advances it,
/// stopping short of [`content_bottom`] the same way every row above it
/// does. `table_width` and `bottom` are `draw`'s own locals, not this
/// function's arguments, since both are cheap to recompute from `area` and
/// doing so keeps this under clippy's argument-count lint.
///
/// The one place `draw` places it: right after the last visible operator
/// row and before the first namespace group's header, closing the operator
/// group even when that group has no members of its own to close.
fn draw_new_key_row(
    pane: &SecretsPane,
    columns: &[Column],
    palette: Palette,
    area: Rect,
    buffer: &mut Buffer,
    y: &mut u16,
) {
    let bottom = content_bottom(area);
    if *y >= bottom {
        return;
    }
    let table_width = area.width.saturating_sub(GUTTER);
    let selected = pane.selected_is_new_key_row();
    let (gutter_text, gutter_style) = gutter(selected, palette);
    buffer.set_line(
        area.x,
        *y,
        &Line::from(Span::styled(gutter_text, gutter_style)),
        1,
    );
    buffer.set_line(
        area.x + GUTTER,
        *y,
        &new_key_row_line(pane, columns, table_width, palette, selected),
        table_width,
    );
    *y += 1;
}

/// What [`draw`] puts on screen below [`MIN_WIDTH`].
///
/// Two short lines, for the reason `view::draw`'s own floor refusal gives:
/// `Buffer::set_line` truncates in silence, so a refusal written as one
/// sentence could lose the number it is about. The way out is already on
/// the status bar (`esc/S close`) and is not repeated here.
fn draw_too_narrow(width: u16, palette: Palette, area: Rect, buffer: &mut Buffer) {
    buffer.set_line(
        area.x,
        area.y,
        &Line::from(Span::styled("too narrow for secrets", palette.refusal())),
        width,
    );
    if area.height < 2 {
        return;
    }
    buffer.set_line(
        area.x,
        area.y + 1,
        &Line::from(Span::styled(
            format!("need {MIN_WIDTH} columns"),
            palette.muted(),
        )),
        width,
    );
}

/// Draws the secrets pane into `area`, straight into `buffer`.
///
/// Seven rows of chrome before the first group header: this pane's own
/// band (design rule 1), the store's terms, the two gates, the roll's own
/// status, the tab row, the heading row, a hairline, then one group header
/// per source change in [`SecretsPane::model`]'s rows, which are already
/// contiguous by source (`SecretsModel::rows`'s own doc comment: operator
/// rows first, then each namespace's), with the `+ new key` affordance
/// closing the operator group before the first namespace header prints.
///
/// The last [`PANEL_ROWS`] rows, when `area` is tall enough to spare them,
/// belong to [`draw_panels`] instead: FOCUSED and WHO READS IT, below their
/// own hairline.
pub fn draw(app: &App, pane: &SecretsPane, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let palette = app.palette();
    let width = area.width;
    if width < MIN_WIDTH {
        draw_too_narrow(width, palette, area, buffer);
        return;
    }
    let table_width = width.saturating_sub(GUTTER);
    // `width`, not `table_width`: [`SECRET_TIERS`]' thresholds are the
    // design's own row width, gutter included.
    let columns = columns_for(width);
    let bottom = area.y + area.height;
    let content_bottom = content_bottom(area);
    let mut y = area.y;

    buffer.set_line(area.x, y, &pane_band(width, palette), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(
        area.x,
        y,
        &Line::from(Span::styled(STORE_LINE, palette.muted())),
        width,
    );
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &gates_line(pane, app.control(), palette), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &roll_status_line(pane, palette), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &tab_line(pane, palette, width), width);
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(
        area.x + GUTTER,
        y,
        &heading_line(columns, palette),
        table_width,
    );
    y += 1;
    if y >= content_bottom {
        return;
    }

    buffer.set_line(area.x, y, &status::rule_line(palette.line(), width), width);
    y += 1;

    // `pane.new_key_anchor()` is the one place that decides where the
    // affordance sits: `SecretsPane::screen_slots` (the cursor) reads the
    // same call, so a row drawn here at the wrong spot would also move the
    // cursor there, never leave the two disagreeing about which line it is.
    let anchor = pane.new_key_anchor();
    let mut new_key_row_drawn = false;
    if anchor.is_none() {
        draw_new_key_row(pane, columns, palette, area, buffer, &mut y);
        new_key_row_drawn = true;
    }
    let mut last_source: Option<&Source> = None;
    for (index, row) in pane.model.rows.iter().enumerate() {
        if y >= content_bottom {
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
            if y >= content_bottom {
                break;
            }
        }
        if pane.is_collapsed(&row.source) {
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
            &row_line(
                pane,
                row,
                columns,
                table_width,
                palette,
                selected,
                app.now(),
            ),
            table_width,
        );
        y += 1;

        // This row is the anchor: the affordance draws right after it,
        // closing the operator group even when zero-membered, matching
        // `SecretsPane::screen_slots`'s own insertion point exactly.
        if !new_key_row_drawn && anchor == Some(index) {
            draw_new_key_row(pane, columns, palette, area, buffer, &mut y);
            new_key_row_drawn = true;
            if y >= content_bottom {
                break;
            }
        }
    }

    // The anchor row never got drawn (truncated by `content_bottom` before
    // reaching it): the affordance still belongs on screen if there is room
    // left.
    if !new_key_row_drawn {
        draw_new_key_row(pane, columns, palette, area, buffer, &mut y);
    }

    if content_bottom < bottom {
        draw_panels(pane, palette, area, buffer, content_bottom);
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
    use std::time::Duration;

    use super::*;
    use crate::lookout::app::{Body, KeyPress, Msg};
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

    /// The heading row `draw` places, fixed like [`first_row`] for the same
    /// reason: it is the row right above the hairline, one above
    /// [`first_row`]'s own chrome count.
    fn heading_row() -> u16 {
        7
    }

    /// The leading number off the tab row's own trailing caption, `N` in `N
    /// environments in this store`.
    fn header_environment_count(buffer: &Buffer) -> usize {
        let row = row_of(buffer, "environments in this store");
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect();
        line.split("environments")
            .next()
            .and_then(|prefix| prefix.split_whitespace().next_back())
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("no leading number in {line:?}"))
    }

    /// The whole rendered frame as text, for the assertions below that
    /// have to say a thing is *not* drawn: [`row_of`] panics instead.
    fn frame_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                let line: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                format!("{}\n", line.trim_end())
            })
            .collect()
    }

    /// [`CHROME_ROWS`] is read by [`content_bottom`], so a row added to
    /// `draw`'s own chrome without a matching bump here would go back to
    /// reserving the panels over the top of it. Counted off a render
    /// rather than off the source, band through hairline inclusive.
    #[test]
    fn the_chrome_is_the_rows_it_claims() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);

        let band = row_of(&buffer, "SECRETS   flock-wide");
        let hairline = row_of(&buffer, "\u{2500}\u{2500}\u{2500}");

        assert_eq!(hairline - band + 1, CHROME_ROWS, "{}", frame_text(&buffer));
    }

    /// The regression: `content_bottom` measured room by the panels alone,
    /// so every height from `PANEL_ROWS + 1` up reserved six rows that
    /// `draw` never reached, and the pane below the heading row was blank.
    #[test]
    fn a_terminal_too_short_for_both_spends_the_panel_rows_on_the_table() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 14);
        let text = frame_text(&buffer);

        assert!(
            !text.contains("FOCUSED"),
            "no room for the panels at this height: {text}"
        );
        assert!(
            text.contains("DB_PASSWORD"),
            "their rows go to the table instead: {text}"
        );
        assert!(
            text.contains("operator \u{d7}"),
            "the group header is drawn too: {text}"
        );
    }

    /// The boundary, both sides of it. One row short of
    /// [`PANELS_MIN_ROWS`] the panels would have nothing above them but
    /// chrome, which is the height they are not worth.
    #[test]
    fn the_panels_arrive_with_the_row_that_makes_room_for_them() {
        let app = fixtures::app_with_secrets();
        let short = usize::from(PANELS_MIN_ROWS - 1);
        let exact = usize::from(PANELS_MIN_ROWS);

        // `body_rows` spends the title band and the status bar, so a body
        // of N rows wants a terminal of N + 2.
        let below = frame_text(&fixtures::render(
            &app,
            160,
            u16::try_from(short + 2).unwrap(),
        ));
        let at = frame_text(&fixtures::render(
            &app,
            160,
            u16::try_from(exact + 2).unwrap(),
        ));

        assert!(!below.contains("FOCUSED"), "one row short: {below}");
        assert!(at.contains("FOCUSED"), "exactly enough: {at}");
        assert!(at.contains("WHO READS IT"), "both panels or neither: {at}");
        assert!(
            at.contains("operator \u{d7}"),
            "and a content row above them: {at}"
        );
    }

    /// Swept rather than sampled: the defect was invisible at every height
    /// the other tests render at, and showed only between
    /// [`PANEL_ROWS`] and [`PANELS_MIN_ROWS`].
    #[test]
    fn no_supported_height_reserves_panel_rows_it_never_draws() {
        let app = fixtures::app_with_secrets();
        for height in crate::lookout::view::flock::MIN_HEIGHT..=40 {
            let buffer = fixtures::render(&app, 160, height);
            let area = Rect::new(0, 0, 160, crate::lookout::view::body_rows(buffer.area));
            let text = frame_text(&buffer);
            if content_bottom(area) < area.y + area.height {
                assert!(
                    text.contains("FOCUSED"),
                    "{height} rows reserved the panels: {text}"
                );
            } else {
                assert!(
                    !text.contains("FOCUSED"),
                    "{height} rows reserved nothing for them: {text}"
                );
            }
        }
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

    /// The gap between the dashboard's own floor (33) and this pane's
    /// (74) is where the clipping was: `columns_for` handed back the floor
    /// tier at every width below 74, and `Buffer::set_line` cut its 72-cell
    /// row off at the screen edge with nothing saying a column had gone.
    ///
    /// Swept rather than sampled, and the refusal is asserted whole: it has
    /// to fit the narrowest terminal it is complaining about, and a
    /// `contains` on a truncated line would not notice.
    #[test]
    fn a_terminal_narrower_than_the_floor_tier_says_so_instead_of_clipping() {
        let app = fixtures::app_with_secrets();

        assert!(
            SECRET_TIERS
                .iter()
                .all(|(threshold, _)| *threshold >= MIN_WIDTH),
            "MIN_WIDTH is the floor tier's own threshold, so no tier below \
             it is refused away unreached"
        );
        let spent: u16 = columns_for(MIN_WIDTH).iter().map(|c| c.width()).sum();
        assert!(
            spent + GUTTER <= MIN_WIDTH,
            "the width the refusal names has to be one the floor tier \
             actually fits: {spent} plus {GUTTER} of gutter"
        );

        for width in crate::lookout::view::MIN_TERM_WIDTH..MIN_WIDTH {
            let text = frame_text(&fixtures::render(&app, width, 24));
            assert!(text.contains("too narrow for secrets"), "{width}: {text}");
            assert!(
                text.contains(&format!("need {MIN_WIDTH} columns")),
                "{width}: {text}"
            );
            assert!(
                !text.contains("DB_PASSWORD"),
                "no half-drawn table at {width}: {text}"
            );
        }

        let text = frame_text(&fixtures::render(&app, MIN_WIDTH, 24));
        assert!(text.contains("DB_PASSWORD"), "the floor tier draws: {text}");
        assert!(!text.contains("too narrow"), "{text}");
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

    /// `.min(len)` in `value_cell` is what a 4096-byte value never binds:
    /// the run is already capped by the column's own width there. A short
    /// value is the case that pins it: with no cap the run would fill the
    /// column regardless of the value behind it, implying a length nowhere
    /// close to the real one.
    #[test]
    fn a_short_value_s_block_run_is_proportional_to_its_length() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let value = cell(&buffer, first_row(), Column::Value);
        let blocks = value.chars().take_while(|&c| c == '\u{2588}').count();

        // `DB_PASSWORD` carries `byte_len: Some(9)`.
        assert_eq!(
            blocks, 9,
            "the run states the value's own length: {value:?}"
        );
    }

    /// The affordance closes the operator group: it draws right after the
    /// last operator row and right before the provider group's own header,
    /// never trailing after it the way the pane once drew it.
    #[test]
    fn the_new_key_row_sits_before_the_first_namespace_group() {
        let app = fixtures::app_with_secrets_and_a_provider_row();
        let buffer = fixtures::render(&app, 160, 48);

        let operator_row = row_of(&buffer, "DB_PASSWORD");
        let new_key_row = row_of(&buffer, "+ new key");
        let provider_header = row_of(&buffer, "vercel (dog)");

        assert_eq!(
            new_key_row,
            operator_row + 1,
            "right after the last operator row"
        );
        assert_eq!(
            provider_header,
            new_key_row + 1,
            "and right before the provider group's own header"
        );
    }

    /// Interleaved sources (operator, namespace, operator): `draw`'s own
    /// scan used to close the operator group at the first row that was not
    /// the operator's own, index 1, while `SecretsPane::screen_slots` (the
    /// cursor) anchored on the highest operator index, 2. `G` would then
    /// select the affordance while it rendered two lines above where the
    /// cursor logic put it. Both now read [`SecretsPane::new_key_anchor`],
    /// so `G`'s target and the drawn affordance are the same line.
    #[test]
    fn interleaved_sources_still_agree_between_the_cursor_and_the_drawn_affordance() {
        let mut app = fixtures::app_with_interleaved_secret_sources();
        app.update(Msg::Key(KeyPress::SelectLast));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "G lands on the affordance: it is the last screen slot here"
        );
        let buffer = fixtures::render(&app, 160, 48);

        let last_operator_row = row_of(&buffer, "SECOND_OPERATOR_KEY");
        let new_key_row = row_of(&buffer, "+ new key");

        assert_eq!(
            new_key_row,
            last_operator_row + 1,
            "the affordance draws right after the highest-index operator \
             row, matching the cursor's own anchor"
        );
    }

    /// One row of one byte: the length is printed in two places and both
    /// used to read `1 bytes`.
    #[test]
    fn a_one_byte_value_is_one_byte_in_both_places_that_print_a_length() {
        assert_eq!(byte_count(1), "1 byte");
        assert_eq!(byte_count(0), "0 bytes");
        assert_eq!(byte_count(9), "9 bytes");
    }

    /// The affordance names no key, so WHO READS IT cannot answer a
    /// question about one. Both panels say the same thing there.
    #[test]
    fn the_new_key_row_says_no_key_is_selected_in_both_panels() {
        let mut app = fixtures::app_with_secrets_and_control();
        app.update(Msg::Key(KeyPress::SelectLast));
        let buffer = fixtures::render(&app, 160, 48);
        let text = fixtures::rows_of(&buffer);

        assert!(
            !text.iter().any(|l| l.contains("nothing names this key")),
            "there is no key here for anything to name: {text:?}"
        );
        let panels = text
            .iter()
            .find(|l| l.contains("no key selected"))
            .unwrap_or_else(|| panic!("neither panel says it: {text:?}"));
        assert_eq!(
            panels.matches("no key selected").count(),
            2,
            "FOCUSED and WHO READS IT sit on one line and both say it: {panels:?}"
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

    /// `SET IN`'s denominator has to be the tab row's own count, `all`
    /// included, or the header states a number that names an environment
    /// outside it. Checked against a key set in `all` and a key set in a
    /// named environment, since either alone would pass by half.
    #[test]
    fn the_header_denominator_matches_set_in_for_all_and_a_named_environment() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let header_count = header_environment_count(&buffer);

        let named_row = row_of(&buffer, "DB_PASSWORD");
        let all_row = row_of(&buffer, "SET_EVERYWHERE");
        assert_eq!(
            cell(&buffer, named_row, Column::SetIn).trim(),
            format!("1 of {header_count} \u{b7} production")
        );
        assert_eq!(
            cell(&buffer, all_row, Column::SetIn).trim(),
            format!("1 of {header_count} \u{b7} all")
        );
    }

    /// `draw` passes `columns_for` the full row width, gutter included:
    /// `columns_for(table_width)` fits one column short of what
    /// [`SECRET_TIERS`] calibrated for, and drops `LANDS` at 160 columns
    /// with no other test catching it.
    #[test]
    fn lands_is_drawn_at_the_widest_tier() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);

        assert_eq!(cell(&buffer, heading_row(), Column::Lands).trim(), "LANDS");
        assert_eq!(cell(&buffer, first_row(), Column::Lands).trim(), "-");
    }

    /// `set_in_cell`'s `count >= environment_count` branch: a key set in
    /// every environment slot names none of them, since the count alone
    /// already says so.
    #[test]
    fn a_key_set_everywhere_names_no_environments() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let header_count = header_environment_count(&buffer);
        let row = row_of(&buffer, "SET_IN_ALL_THREE");

        assert_eq!(
            cell(&buffer, row, Column::SetIn).trim(),
            format!("{header_count} of {header_count}")
        );
    }

    /// The two cells a reveal changes, and the one row it changes them on.
    #[test]
    fn a_revealed_row_prints_its_value_and_its_countdown() {
        let dir = tempfile::tempdir().unwrap();
        let app = fixtures::app_revealing(dir.path());

        let buffer = fixtures::render(&app, 160, 48);

        let revealed = row_of(&buffer, "DB_PASSWORD");
        assert_eq!(
            cell(&buffer, revealed, Column::Value).trim(),
            fixtures::REVEALED_VALUE
        );
        assert_eq!(
            cell(&buffer, revealed, Column::Lands).trim(),
            "visible 10s \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}"
        );

        let untouched = row_of(&buffer, "OTHER_KEY");
        assert_eq!(
            cell(&buffer, untouched, Column::Value).trim(),
            "\u{2588}\u{2588}\u{2588} 3 bytes",
            "a reveal reaches one row, not the pane"
        );
        assert_eq!(cell(&buffer, untouched, Column::Lands).trim(), "-");
    }

    /// The words and the blocks come off one remaining duration, so a
    /// partly-spent reveal has to agree with itself: four seconds left is
    /// four of ten cells.
    #[test]
    fn the_countdown_and_its_gauge_spend_together() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();
        let _ = app.update(Msg::Tick {
            now: start + Duration::from_secs(6),
        });

        let buffer = fixtures::render(&app, 160, 48);

        assert_eq!(
            cell(&buffer, row_of(&buffer, "DB_PASSWORD"), Column::Lands).trim(),
            "visible 4s \u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}"
        );

        // Half a second later the number has not changed, so neither has
        // the gauge: rounding one and truncating the other would read `4s`
        // against three blocks here.
        let _ = app.update(Msg::Tick {
            now: start + Duration::from_millis(6_500),
        });
        let buffer = fixtures::render(&app, 160, 48);
        assert_eq!(
            cell(&buffer, row_of(&buffer, "DB_PASSWORD"), Column::Lands).trim(),
            "visible 4s \u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}"
        );
    }

    #[test]
    fn a_reader_is_never_said_to_hold_the_current_value() {
        let buffer = fixtures::render_secrets_with_readers();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter()
                .any(|l| l.contains("was given a value at spawn")),
            "an online reader: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("reads it at next start")),
            "an offline one: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains("holds the value")),
            "nothing can tell a sheep spawned before a set from one spawned \
             after, so the pane must not claim it: {text:?}"
        );
    }

    /// The gate is named in the chrome and nowhere else: `focused_lines`
    /// never spells `allow_read`, so a test asserting it over the whole
    /// buffer under a name about the panel passes for the wrong reason.
    #[test]
    fn the_chrome_states_the_gate_and_no_panel_promises_an_audit() {
        let buffer = fixtures::render_secrets_gate_shut();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter()
                .any(|l| l.contains("allow_read") && l.contains("reveal")),
            "the chrome's own gate line: {text:?}"
        );
        assert!(
            text.iter()
                .any(|l| l.contains("nothing records that you looked")),
            "FOCUSED says what is not kept: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains("audit")),
            "there is no audit log, so promising one is a promise nothing keeps"
        );
    }

    #[test]
    fn a_stale_roll_states_its_age() {
        let buffer = fixtures::render_secrets_with_roll_age(Duration::from_secs(3600));
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|l| l.contains("roll") && l.contains("1h")),
            "a failed roll write only warns, so the age is the only signal: {text:?}"
        );
    }

    /// Five readers on one key, three rows to list them in: WHO READS IT
    /// has to say two are missing rather than looking complete at three,
    /// and its own total has to match FOCUSED's `named by 5 of the flock`.
    #[test]
    fn who_reads_it_states_an_overflow_it_cannot_list() {
        let buffer = fixtures::render_secrets_with_more_readers_than_fit();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|l| l.contains("named by 5 of the flock")),
            "FOCUSED states the true count: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("+ 3 more")),
            "WHO READS IT states what it could not list: {text:?}"
        );
    }

    #[test]
    fn a_missing_roll_says_so_rather_than_showing_an_empty_reader_list() {
        let buffer = fixtures::render_secrets_with_no_roll();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|l| l.contains("no muster roll")),
            "an absent roll and a key nothing reads look identical otherwise: {text:?}"
        );
    }
}
