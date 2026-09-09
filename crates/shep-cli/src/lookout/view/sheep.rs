//! The sheep pane: one sheep given the whole screen.
//!
//! [`super::mod`]'s own `draw` spends this module's whole area on the
//! identity band and the two charts; Tasks 9 and 10 add the read-only
//! config column and the feed to what is still blank below them.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};
use shep_core::protocol::SheepConfigView;

use super::super::app::{App, CPU_CEILING_FLOOR, HISTORY, RowKey};
use super::super::field::{Field, FieldSet};
use super::super::pane;
use super::super::pane_bleats::{BleatsPane, Filters, MatchKind};
use super::super::pane_sheep::{SheepPane, scale_top, window};
use super::super::tail::{Stream, Tail, TailLine};
use super::super::theme::Palette;
use super::detail::chip_text;
use super::flock::{self, fit};
use super::{cell, detail};

/// Left gutter width, in cells, for both charts: room for a label like
/// `100%` or `52.0M` plus a trailing space. Shared rather than computed
/// twice, which is half of why the two charts' bodies line up
/// ([`chart_body_cells`] is the other half).
const GUTTER: usize = 8;

/// Right margin width, in cells, decision 8's own arithmetic reserves for
/// the ceiling label and the axis's own overrun. Not drawn into directly:
/// callers append margin text to a line at whatever length it needs, and
/// this only feeds [`chart_body_cells`].
const MARGIN: usize = 12;

/// The CPU section header's row, relative to `area`.
const CPU_HEADER_ROW: u16 = 1;
/// The CPU chart's first row, relative to `area`.
const CPU_CHART_ROW: u16 = 2;
/// The CPU chart's row count: 16 half-steps in 8 rows.
const CPU_ROWS: usize = 8;
/// The memory section header's row, relative to `area`.
const MEM_HEADER_ROW: u16 = 10;
/// The memory chart's first row, relative to `area`.
const MEM_CHART_ROW: u16 = 11;
/// The memory chart's row count.
const MEM_ROWS: usize = 5;
/// The shared x axis's row, relative to `area`, `now` ending on its last
/// column.
const AXIS_ROW: u16 = 16;
/// The shortest `area.height` any chart tier draws into at all: decision
/// 8's row ladder drops both charts under 20 rows, and one past
/// [`COLUMN_HEADER_ROW`] is that same floor stated in terms of the row the
/// config column would otherwise sit on. Below it the pane still opens; the
/// charts just stay blank and [`column_top_row`] moves the config and feed
/// columns up to reclaim the rows the charts would have used, rather than
/// the all-or-nothing gate this constant named before this task.
const MIN_HEIGHT_FOR_CHARTS: u16 = COLUMN_HEADER_ROW + 1;

/// `area.height` past which the full two-chart body has room for the
/// memory chart's own five rows on top of the CPU chart's own eight.
/// Below it, [`chart_tier`] downgrades [`ChartTier::Full`] to
/// [`ChartTier::CpuOnly`] regardless of width, per decision 8's "under 26
/// rows the memory chart goes."
const FULL_TIER_MIN_HEIGHT: u16 = 26;

/// The config/env column's own header row, relative to `area`.
const COLUMN_HEADER_ROW: u16 = 19;
/// The column's first body row.
const COLUMN_FIRST_ROW: u16 = 20;
/// The column's last body row.
const COLUMN_LAST_ROW: u16 = 45;
/// How many body rows the column draws: [`COLUMN_FIRST_ROW`] through
/// [`COLUMN_LAST_ROW`], inclusive.
pub(crate) const COLUMN_BODY_ROWS: usize = (COLUMN_LAST_ROW - COLUMN_FIRST_ROW + 1) as usize;
/// The column's own width, left of the divider Task 10's feed sits after.
const COLUMN_WIDTH: u16 = 76;
/// The KEY cell within the column: wide enough for `exp_backoff_restart_delay`
/// (25 characters) plus its `!` flag (26), pinned by
/// `the_longest_pending_field_name_is_not_truncated` rather than trusted
/// from this comment alone. Matches `view::pane`'s own `KEY_W`, not rounded
/// down: the column is narrower than that pane's own body, but the longest
/// name is the same schema's, so shrinking this cell would truncate it
/// regardless of how much room the rest of the row has.
const COLUMN_NAME_W: u16 = 26;
/// The design-size `area.height` every full-design fixture in this
/// module's own tests builds its `area` at. No longer read by [`draw`]
/// itself: [`column_top_row`] decides whether and where the column draws
/// now, so this is test-only.
#[cfg(test)]
const MIN_HEIGHT_FOR_COLUMN: u16 = COLUMN_LAST_ROW + 1;

/// Where the config/env column and the feed start, in rows relative to
/// `area`: [`COLUMN_HEADER_ROW`] whenever `height` still reaches it, or
/// right below the identity band once it drops under
/// [`MIN_HEIGHT_FOR_CHARTS`] and the charts stop drawing at all. The config
/// and feed columns are what the pane is for, so they give ground last,
/// reclaiming the rows the charts would have used rather than staying
/// pinned to a row a short terminal can never reach.
fn column_top_row(height: u16) -> u16 {
    if height > COLUMN_HEADER_ROW {
        COLUMN_HEADER_ROW
    } else {
        1
    }
}

/// The divider column between the config/env column and the feed, relative
/// to `area`: one cell past [`COLUMN_WIDTH`], drawn its own full height
/// rather than folded into either side's own width.
const DIVIDER_COL: u16 = COLUMN_WIDTH;
/// The feed's own first column, relative to `area`: one past the divider.
const FEED_X: u16 = DIVIDER_COL + 1;
/// The feed's own width in cells: `160 - 76 - 1`, the same arithmetic
/// [`COLUMN_WIDTH`]'s own doc gives for the divider.
const FEED_WIDTH: u16 = 83;

/// The eight Flockfile groups' fields, plus the env keys, as the column
/// scrolls through them: one entry per rendered row, and every group label
/// this pass actually emitted, in the order it emitted them.
///
/// Both halves are read off the same walk, so a group header the field
/// loop below skips or misorders shows up wrong in both places at once
/// rather than only in the one a test happens to check.
fn column_body(view: &SheepConfigView, palette: Palette) -> (Vec<Line<'static>>, Vec<String>) {
    let (fields, values) = pane::sheep_fields(&view.config);
    let mut lines = Vec::new();
    let mut groups = Vec::new();
    let mut current: Option<&str> = None;
    for field in fields.fields() {
        // `env` is its own section below, with its own keys; a second row
        // here would repeat it under a `Map` field's own placeholder text.
        if field.key == "env" {
            continue;
        }
        if field.group.as_deref() != current {
            current = field.group.as_deref();
            let label = current.unwrap_or_default().to_owned();
            push_group_header(&mut lines, &label, palette);
            groups.push(label);
        }
        let pending = view.pending.iter().any(|key| key == &field.key);
        lines.push(field_row_line(&fields, field, &values, pending, palette));
    }
    push_group_header(&mut lines, "env", palette);
    for key in &view.env_keys {
        let sealed = view.env_secrets.iter().any(|secret| secret == key);
        lines.push(env_row_line(key, sealed, palette));
    }
    (lines, groups)
}

/// A blank separator, a rule the column's own width, then `label`: the
/// chrome [`column_body`] spends above every group and above the env
/// section it closes with.
fn push_group_header(lines: &mut Vec<Line<'static>>, label: &str, palette: Palette) {
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "\u{2500}".repeat(usize::from(COLUMN_WIDTH)),
        palette.muted(),
    )));
    lines.push(Line::from(Span::styled(label.to_owned(), palette.muted())));
}

/// [`column_body`]'s lines alone, for [`draw_column`] and [`column_len`],
/// neither of which needs the group labels back.
fn column_body_lines(view: &SheepConfigView, palette: Palette) -> Vec<Line<'static>> {
    column_body(view, palette).0
}

/// The one line the column draws while [`SheepPane::config`] is still
/// `None`: read once by [`draw_column`] and by
/// [`SheepPane::body_len`](super::super::pane_sheep::SheepPane), so
/// scrolling can never claim a body one line longer than what is actually
/// drawn.
fn waiting_line(palette: Palette) -> Line<'static> {
    Line::from(Span::styled("reading config\u{2026}", palette.muted()))
}

/// How many lines [`draw_column`] would need to show every one of
/// `config`'s groups and env keys, or the single waiting line while there
/// is none yet. What [`SheepPane`]'s own `Viewport` scrolls through.
///
/// Colour never changes a line count, so [`Palette::detect`] is called
/// with nothing set rather than threading the real palette through from a
/// key handler that has no `Frame` to read one off.
#[must_use]
pub(crate) fn column_len(config: Option<&SheepConfigView>) -> usize {
    match config {
        Some(view) => column_body_lines(view, Palette::detect(None, None, None)).len(),
        None => 1,
    }
}

/// One field's row: the name, `!`-flagged and butter when
/// [`SheepConfigView::pending`] names it, the value, `(unset)`/`(default)`
/// muted like the name, anything else in the column's own body colour, and
/// `awaits respawn` right-aligned when pending.
///
/// Read-only: no lock glyph, no cost cell. Those belong to the editing
/// pane this column never opens; a value here is what `shep edit` already
/// shows, laid out for a screen with no cursor to carry.
fn field_row_line(
    fields: &FieldSet,
    field: &Field,
    values: &Map<String, Value>,
    pending: bool,
    palette: Palette,
) -> Line<'static> {
    let raw = field_value_text(fields, field, values);
    let flag = if pending { "!" } else { "" };
    let note = if pending { "awaits respawn" } else { "" };
    let value_w = usize::from(COLUMN_WIDTH).saturating_sub(usize::from(COLUMN_NAME_W) + 2);
    let left_w = value_w.saturating_sub(note.chars().count());
    let key_style = if pending {
        palette.attention()
    } else {
        palette.muted()
    };
    let value_style = if pending {
        palette.attention()
    } else if matches!(raw.as_str(), "(unset)" | "(default)") {
        palette.muted()
    } else {
        Style::default()
    };
    let mut spans = vec![
        Span::styled(
            fit(&format!("{flag}{}", field.key), COLUMN_NAME_W),
            key_style,
        ),
        Span::raw("  "),
        Span::styled(fit(&raw, left_w as u16), value_style),
    ];
    if !note.is_empty() {
        spans.push(Span::styled(note.to_owned(), palette.attention()));
    }
    Line::from(spans)
}

/// `field`'s current value, the way this column shows it: `<set>` for a
/// [`Field::secret`] one (never the value, the same guard
/// [`ConfigPane::field_line`](super::super::pane::ConfigPane) draws its own
/// row through), `(unset)` for a JSON `null` or a key the config never
/// carried, `(default)` when what is there is exactly the schema's own
/// default, else [`pane::resolved_display`]'s answer: a bare
/// `MemSize`/`UpDuration` number annotated with its unit, the same as
/// [`ConfigPane::display_value`](super::super::pane::ConfigPane::display_value)
/// draws for the same field.
fn field_value_text(fields: &FieldSet, field: &Field, values: &Map<String, Value>) -> String {
    let raw = match values.get(&field.key) {
        None | Some(Value::Null) => return "(unset)".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => other.to_string(),
    };
    if field.secret {
        "<set>".to_owned()
    } else if field.default.as_deref() == Some(raw.as_str()) {
        "(default)".to_owned()
    } else {
        pane::resolved_display(fields, &field.key, &raw)
    }
}

/// One env row: the key alone, or, when [`SheepConfigView::env_secrets`]
/// names it, a butter block run standing in for the value it never
/// carries, the word `sealed`, and `edit in S` right-aligned. No value is
/// ever read: the block run is a fixed run of glyphs, not sized to
/// anything the store holds.
fn env_row_line(key: &str, sealed: bool, palette: Palette) -> Line<'static> {
    if !sealed {
        return Line::from(Span::raw(key.to_owned()));
    }
    let note = "edit in S";
    let value_w = usize::from(COLUMN_WIDTH).saturating_sub(usize::from(COLUMN_NAME_W) + 2);
    let left_w = value_w.saturating_sub(note.chars().count());
    let mid = format!("{}  sealed", "\u{2588}".repeat(8));
    Line::from(vec![
        Span::styled(fit(key, COLUMN_NAME_W), palette.muted()),
        Span::raw("  "),
        Span::styled(fit(&mid, left_w as u16), palette.attention()),
        Span::styled(note.to_owned(), palette.muted()),
    ])
}

/// The header row: `e edit`, and how many fields
/// [`SheepConfigView::pending`] is still carrying, once there is a config
/// to read one off. No `tab next group`: no key routes one, the same defect
/// the status bar carried until Task 7 fixed it for that frame.
fn column_header_line(pane: &SheepPane, palette: Palette) -> Line<'static> {
    let mut text = "\u{2588}\u{2588} CONFIG & ENV   e edit".to_owned();
    if let Some(pending) = pane
        .config()
        .map(|view| view.pending.len())
        .filter(|count| *count > 0)
    {
        text.push_str(&format!("   {pending} pending"));
    }
    Line::from(Span::styled(text, palette.muted()))
}

/// `top` to [`COLUMN_BODY_ROWS`] rows below it: the header, then as much of
/// [`column_body`] as [`SheepPane::view`]'s own offset scrolls into.
///
/// `top` is [`column_top_row`]'s answer, not always [`COLUMN_HEADER_ROW`]:
/// a short terminal moves the whole column up rather than truncating it
/// from a fixed row that terminal can never reach.
///
/// Reads [`SheepPane::config`], never [`App::selected`]: the same rule
/// [`draw`]'s own identity band and [`draw_charts`] already follow, for
/// the reason both of their own doc comments give.
fn draw_column(pane: &SheepPane, top: u16, area: Rect, buffer: &mut Buffer, palette: Palette) {
    write_column_row(buffer, area, top, &column_header_line(pane, palette));
    let body = match pane.config() {
        Some(view) => column_body_lines(view, palette),
        None => vec![waiting_line(palette)],
    };
    let offset = pane.view().offset();
    for (i, line) in body.iter().skip(offset).take(COLUMN_BODY_ROWS).enumerate() {
        write_column_row(buffer, area, top + 1 + i as u16, line);
    }
}

/// Writes one already-styled line into `buffer`, `row` cells below `area`'s
/// own top, clipped to [`COLUMN_WIDTH`] rather than the pane's own width:
/// this column never spends the cells the feed sits after.
fn write_column_row(buffer: &mut Buffer, area: Rect, row: u16, line: &Line<'static>) {
    if row >= area.height {
        return;
    }
    buffer.set_line(area.x, area.y + row, line, COLUMN_WIDTH);
}

/// The `\u{2502}` rule between the config/env column and the feed, one cell
/// wide, for every row the two sides draw into, from `top` (the same row
/// [`draw_column`] and [`draw_feed`] were handed) through
/// [`COLUMN_BODY_ROWS`] below it.
fn draw_divider(top: u16, area: Rect, buffer: &mut Buffer, palette: Palette) {
    let rule = Line::from(Span::styled("\u{2502}", palette.muted()));
    for row in top..=(top + COLUMN_BODY_ROWS as u16) {
        if row >= area.height {
            break;
        }
        buffer.set_line(area.x + DIVIDER_COL, area.y + row, &rule, 1);
    }
}

/// `top` (the same row [`draw_column`] and [`draw_divider`] were handed)
/// through [`COLUMN_BODY_ROWS`] rows below it, right of the divider: the
/// header, then the window's newest surviving lines that fit, oldest at
/// the top.
///
/// Reads [`SheepPane::feed_sheep`]'s tail through [`App::feed`], never
/// [`App::selected`]: the same rule [`draw`]'s own identity band, charts and
/// config column already follow, now enforced one level up too, in
/// [`App::feed_row`] itself — `app.feed()` already answers with the pinned
/// sheep's own lines while this pane is open, not the dashboard's selection.
///
/// No scrolling: unlike [`super::bleats_full::draw`], this column has no
/// cursor of its own to page through, so a line past what fits is only
/// counted by the header's own `N earlier` clause, never drawn.
fn draw_feed(
    app: &App,
    pane: &SheepPane,
    top: u16,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let feed = pane.feed();
    let tail = app.feed();
    write_feed_row(buffer, area, top, &feed_header_line(feed, tail, palette));

    let survivors = feed.visible(&tail.lines);
    let shown = survivors.len().saturating_sub(COLUMN_BODY_ROWS);
    for (i, line) in survivors.iter().skip(shown).enumerate() {
        write_feed_row(buffer, area, top + 1 + i as u16, &feed_line(line, palette));
    }
}

/// One feed line: the stream tag, muted the same way
/// [`super::bleats::feed_lines`] draws it (stderr is most runtimes' default,
/// not `--bark`), then the text, truncated rather than wrapped — this
/// column has no row budget to spend on a second line for one that
/// overruns.
fn feed_line(line: &TailLine, palette: Palette) -> Line<'static> {
    let tag = match line.stream {
        Stream::Out => "out",
        Stream::Err => "err",
    };
    Line::from(vec![
        Span::styled(format!("{tag}  "), palette.muted()),
        Span::raw(fit(&line.text, FEED_WIDTH.saturating_sub(5))),
    ])
}

/// The feed's header row: the `BLEATS` chip, then `out then err`, then how
/// many of the window's surviving lines this column has no room to show,
/// then a bracketed chip per filter axis currently set, then `/ narrow`,
/// naming the one key this row does not otherwise spell out.
fn feed_header_line(feed: &BleatsPane, tail: &Tail, palette: Palette) -> Line<'static> {
    let chip = chip_text("BLEATS");
    let chip_width = u16::try_from(chip.chars().count() + 1).unwrap_or(FEED_WIDTH);
    let budget = FEED_WIDTH.saturating_sub(chip_width);
    Line::from(vec![
        Span::styled(chip, palette.band(crate::vocabulary::Role::Butter)),
        Span::raw(" "),
        Span::styled(fit(&feed_header_text(feed, tail), budget), palette.muted()),
    ])
}

/// [`feed_header_line`]'s own sentence, built separately so a test can pin
/// its wording without rendering a [`Line`] back into a string.
fn feed_header_text(feed: &BleatsPane, tail: &Tail) -> String {
    let survivors = feed.visible(&tail.lines);
    let hidden = survivors.len().saturating_sub(COLUMN_BODY_ROWS);
    let mut parts = vec!["out then err".to_string()];
    if hidden > 0 {
        parts.push(format!("{hidden} earlier"));
    }
    for chip in feed_chip_labels(feed.filters()) {
        parts.push(format!("[{chip}]"));
    }
    parts.push("/ narrow".to_string());
    parts.join(" \u{b7} ")
}

/// One chip's text per filter axis currently set on the embedded feed, in
/// field order. Deliberately its own, smaller vocabulary rather than
/// `view::bleats_full`'s private `chip_labels`: this row has 83 cells for
/// the whole sentence, not a dedicated filter row underneath it.
fn feed_chip_labels(filters: &Filters) -> Vec<String> {
    let mut chips = Vec::new();
    if let Some(stream) = filters.stream {
        chips.push(match stream {
            Stream::Out => "out only".to_string(),
            Stream::Err => "err only".to_string(),
        });
    }
    if let Some(min) = filters.min_level {
        chips.push(format!(
            "level\u{2265}{}",
            format!("{min:?}").to_lowercase()
        ));
    }
    if let Some(text) = &filters.matcher {
        let suffix = match filters.match_kind() {
            Some(MatchKind::Literal) | None => "",
            Some(MatchKind::Regex) => " (regex)",
            Some(MatchKind::Invalid) => " (invalid regex, matches nothing)",
        };
        chips.push(format!("match {text}{suffix}"));
    }
    chips
}

/// Writes one already-styled line into `buffer`, `row` cells below `area`'s
/// own top and [`FEED_X`] cells right of its own left edge, clipped to
/// [`FEED_WIDTH`].
fn write_feed_row(buffer: &mut Buffer, area: Rect, row: u16, line: &Line<'static>) {
    if row >= area.height {
        return;
    }
    buffer.set_line(area.x + FEED_X, area.y + row, line, FEED_WIDTH);
}

/// Draws the pane into `area`: the identity band on its first row, the two
/// histories over rows `CPU_HEADER_ROW` through `AXIS_ROW`, and nothing
/// else yet.
///
/// `area` is the whole pane body, under the title band [`super::mod`]'s own
/// `draw` already painted and over the status bar it paints after this
/// returns — not a sub-rect of either, the way every other full-screen
/// pane's own `draw` is handed one.
pub fn draw(app: &App, pane: &SheepPane, area: Rect, buffer: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let palette = app.palette();
    // `App::sheep_pane_row`, not `App::selected_row`: the pinned sheep, not
    // the selection. `Msg::Snapshot` reseats the selection whatever screen
    // is showing, so a pane pinned to a sheep that then leaves the flock
    // would otherwise draw its neighbour's facts under a band still naming
    // the first, and the charts below it would draw its neighbour's
    // history under the same wrong name.
    let sheep_row = app.sheep_pane_row();
    let mut spans = match sheep_row {
        Some(row) => detail::identity_line(app, row, area.width, palette).spans,
        None => {
            let RowKey::Sheep(id) = pane.sheep() else {
                unreachable!("a sheep pane is only ever opened on `RowKey::Sheep`")
            };
            vec![Span::styled(
                // Same phrasing `view::bleats_full`'s own title uses for the
                // same case: one sentence for "this sheep is gone", not two.
                format!("sheep {id}: it is no longer in the flock"),
                palette.muted(),
            )]
        }
    };
    // Only the read-only pane's own pending count, not
    // `ProcessInfo::pending`'s: `detail_lines` already folds that one into
    // its own `cfg` cell, and this is the confirmation that the config this
    // pane is showing agrees with it, once the read has landed.
    if let Some(pending) = pane
        .config()
        .map(|view| view.pending.len())
        .filter(|count| *count > 0)
    {
        spans.push(Span::styled(
            format!("   !{pending} pending"),
            app.palette().attention(),
        ));
    }
    let line = Line::from(spans);
    buffer.set_line(area.x, area.y, &line, area.width);

    // Rows `CPU_HEADER_ROW` to `AXIS_ROW`, however much of them the current
    // tier draws, on the pinned sheep only. A sheep that has left the flock
    // has no history left either (`App::cpu_history`'s own doc says
    // `record_samples` drops it), so there is nothing to chart once
    // `sheep_row` is `None`.
    if let Some(row) = sheep_row {
        match chart_tier(area.width, area.height) {
            ChartTier::Full => {
                draw_charts(app, row.info.id, row.info.max_memory, area, buffer, palette);
            }
            ChartTier::CpuOnly => {
                draw_cpu_only(app, row.info.id, row.info.max_memory, area, buffer, palette);
            }
            ChartTier::Sparkline => {
                draw_sparkline_row(app, &row.info, area, buffer, palette);
            }
            ChartTier::None => {}
        }
    }
    // The config and env column, starting at `top`: `COLUMN_HEADER_ROW`
    // whenever the terminal still reaches it, or right below the identity
    // band once the charts above have stopped drawing entirely. It gives
    // ground last: nothing about its own gate depends on width, since
    // `write_column_row` already clips every row to `COLUMN_WIDTH`.
    let top = column_top_row(area.height);
    if area.height > top {
        draw_column(pane, top, area, buffer, palette);
    }
    // The same rows, right of the divider: the bleats feed, embedded rather
    // than a strip of its own. Gated on `area.width` reaching past
    // `FEED_WIDTH`'s own room, not on `sheep_row`: a sheep that has left the
    // flock still draws a feed, the same "no longer read" header
    // `view::bleats_full`'s own title states for the full-screen pane, once
    // `App::feed_row` next answers `None` and the tail this reads goes
    // empty.
    if area.height > top && usize::from(area.width) >= usize::from(FEED_X + FEED_WIDTH) {
        draw_divider(top, area, buffer, palette);
        draw_feed(app, pane, top, area, buffer, palette);
    }
}

/// Which of the sheep pane's own charts fit `width` and `height`, decision
/// 8's own ladder: both charts at 140 columns and [`FULL_TIER_MIN_HEIGHT`]
/// rows, the CPU chart alone with a one-line memory summary from 100
/// columns, a single sparkline-and-gauge row below that (down to
/// [`flock::MIN_WIDTH`], the whole app's own floor, refused before this
/// pane ever opens), and nothing at all under [`MIN_HEIGHT_FOR_CHARTS`]
/// rows regardless of width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartTier {
    /// Both charts, full height: [`draw_charts`].
    Full,
    /// The CPU chart and a one-line memory summary: [`draw_cpu_only`].
    CpuOnly,
    /// 1a's own CPU sparkline and memory gauge, one row: [`draw_sparkline_row`].
    Sparkline,
    /// No chart content at all; the row belongs to the config column instead.
    None,
}

fn chart_tier(width: u16, height: u16) -> ChartTier {
    if height < MIN_HEIGHT_FOR_CHARTS {
        return ChartTier::None;
    }
    let by_width = if width >= 140 {
        ChartTier::Full
    } else if width >= 100 {
        ChartTier::CpuOnly
    } else if width >= flock::MIN_WIDTH {
        ChartTier::Sparkline
    } else {
        ChartTier::None
    };
    if by_width == ChartTier::Full && height < FULL_TIER_MIN_HEIGHT {
        ChartTier::CpuOnly
    } else {
        by_width
    }
}

/// Rows `CPU_HEADER_ROW` to `AXIS_ROW`: the CPU history, the memory
/// history, and the axis they share.
///
/// `body_cells` is computed once, here, and handed to both charts: the
/// other half of why a memory step and a CPU spike land on the same
/// column is [`GUTTER`] being one constant rather than two calculations
/// that could drift apart.
fn draw_charts(
    app: &App,
    sheep_id: u32,
    max_memory: Option<u64>,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let body_cells = chart_body_cells(area.width);
    let cpu_history = app.cpu_history(sheep_id);
    let rss_history = app.rss_history(sheep_id);

    let cpu_header = cpu_header_text(cpu_history.len(), body_cells);
    write_row(
        buffer,
        area,
        CPU_HEADER_ROW,
        &format!("\u{2588}\u{2588} CPU   {cpu_header}"),
        palette.muted(),
    );
    for (i, row_text) in cpu_chart_rows(cpu_history, body_cells)
        .into_iter()
        .enumerate()
    {
        write_row(
            buffer,
            area,
            CPU_CHART_ROW + i as u16,
            &row_text,
            Style::default(),
        );
    }

    let mem_window = &rss_history[rss_history.len().saturating_sub(body_cells)..];
    let mem_window_peak = mem_window.iter().copied().max().unwrap_or(0);
    let mem_header = mem_header_text(max_memory, mem_window_peak);
    write_row(
        buffer,
        area,
        MEM_HEADER_ROW,
        &format!("\u{2588}\u{2588} MEM   {mem_header}"),
        palette.muted(),
    );
    let (mem_rows, marked) = mem_chart_rows(rss_history, max_memory, body_cells);
    for (i, row_text) in mem_rows.into_iter().enumerate() {
        let style = if marked == Some(i) {
            palette.attention()
        } else {
            Style::default()
        };
        write_row(buffer, area, MEM_CHART_ROW + i as u16, &row_text, style);
    }

    write_row(
        buffer,
        area,
        AXIS_ROW,
        &axis_row(body_cells),
        palette.muted(),
    );
}

/// [`ChartTier::CpuOnly`]'s own rows: the CPU chart, unchanged, and in
/// [`MEM_HEADER_ROW`]'s own slot, [`mem_line_text`]'s single line in place
/// of the memory chart's header and five rows — memory still has a gauge
/// to fall back on; the CPU chart is the more diagnostic of the two, so it
/// is the one that stays.
fn draw_cpu_only(
    app: &App,
    sheep_id: u32,
    max_memory: Option<u64>,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let body_cells = chart_body_cells(area.width);
    let cpu_history = app.cpu_history(sheep_id);
    let rss_history = app.rss_history(sheep_id);

    let cpu_header = cpu_header_text(cpu_history.len(), body_cells);
    write_row(
        buffer,
        area,
        CPU_HEADER_ROW,
        &format!("\u{2588}\u{2588} CPU   {cpu_header}"),
        palette.muted(),
    );
    for (i, row_text) in cpu_chart_rows(cpu_history, body_cells)
        .into_iter()
        .enumerate()
    {
        write_row(
            buffer,
            area,
            CPU_CHART_ROW + i as u16,
            &row_text,
            Style::default(),
        );
    }

    let current_rss = rss_history.last().copied().unwrap_or(0);
    write_row(
        buffer,
        area,
        MEM_HEADER_ROW,
        &mem_line_text(current_rss, max_memory),
        Style::default(),
    );
    write_row(
        buffer,
        area,
        AXIS_ROW,
        &axis_row(body_cells),
        palette.muted(),
    );
}

/// [`ChartTier::CpuOnly`]'s own memory line: `rss` against its ceiling
/// when a ceiling exists, per the arithmetic in decision 8:
/// `rss 48.3M of 52M` plus a 10-cell gauge. With no ceiling set, the
/// gauge has nothing to fill against, the same no-limit case
/// [`cell::gauge`]'s own doc already covers.
fn mem_line_text(current_rss: u64, max_memory: Option<u64>) -> String {
    let gauge = cell::gauge(current_rss, max_memory, 10);
    match max_memory {
        Some(limit) => format!(
            "rss {} of {} {gauge}",
            crate::output::human_bytes(current_rss),
            crate::output::human_bytes(limit),
        ),
        None => format!("rss {} {gauge}", crate::output::human_bytes(current_rss)),
    }
}

/// [`ChartTier::Sparkline`]'s own row: 1a's own `CPU 20s` sparkline and
/// `MEM/CEIL` gauge, the pair [`super::flock`]'s own flat-view columns
/// already draw, on the one row this tier has left once both charts have
/// given up their own sixteen.
fn draw_sparkline_row(
    app: &App,
    info: &shep_core::protocol::ProcessInfo,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    let spark = cell::sparkline(app.cpu_history(info.id), 10, app.cpu_ceiling());
    let gauge = cell::gauge(info.memory_bytes.unwrap_or(0), info.max_memory, 10);
    write_row(
        buffer,
        area,
        CPU_HEADER_ROW,
        &format!("CPU 20s   {spark}      MEM/CEIL   {gauge}"),
        palette.muted(),
    );
}

/// Writes one styled line into `buffer`, `row` cells below `area`'s own
/// top. Callers only reach here once [`draw`] has already checked `area`
/// is tall enough for `row`.
fn write_row(buffer: &mut Buffer, area: Rect, row: u16, text: &str, style: Style) {
    let line = Line::from(Span::styled(text.to_string(), style));
    buffer.set_line(area.x, area.y + row, &line, area.width);
}

/// The chart body's width in cells: `area`'s own `width` less [`GUTTER`]
/// and [`MARGIN`], per decision 8's arithmetic:
///
/// ```text
/// 160 = 8 gutter + 140 body + 12 margin
/// 140 = 8 gutter + 120 body + 12 margin
/// ```
///
/// Capped at [`HISTORY`]: past 160 columns the arithmetic above would ask
/// for more samples than the buffer ever holds, and an uncapped body keeps
/// [`cpu_header_text`] reading `collecting` forever even once the buffer is
/// full. Which charts draw at which width past that point is task 11's own
/// tier; this is only the ceiling the header's own claim has to respect.
fn chart_body_cells(width: u16) -> usize {
    usize::from(width)
        .saturating_sub(GUTTER + MARGIN)
        .min(HISTORY)
}

/// `d` as `MmSSs`: `4m40s`, not `4m 40s` or a dropped `0m`. Neither
/// `output::human_duration`'s spacing nor its dropped-zero minor unit
/// would pass the window label's own test at 140 columns, where `4m00s`
/// has to keep its zero.
fn window_label(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}m{:02}s", secs / 60, secs % 60)
}

/// Row `CPU_HEADER_ROW`'s second half: the window this chart actually
/// drew, computed from `body_cells` rather than a literal, and how much of
/// it a buffer shorter than `body_cells` has filled so far.
fn cpu_header_text(history_len: usize, body_cells: usize) -> String {
    let full = window_label(window(body_cells));
    if history_len < body_cells {
        format!(
            "collecting \u{b7} {} of {full}",
            window_label(window(history_len))
        )
    } else {
        format!("{full}, one 2s sample per column")
    }
}

/// The CPU chart's `CPU_ROWS` rows: [`cell::chart`] scaled to
/// [`scale_top`] of the drawn window's own peak, floored at
/// [`CPU_CEILING_FLOOR`] rather than the flock-wide
/// [`App::cpu_ceiling`](super::super::app::App::cpu_ceiling), which this
/// one-sheep pane has no row to be comparable with.
fn cpu_chart_rows(history: &[f32], body_cells: usize) -> Vec<String> {
    let window_slice = &history[history.len().saturating_sub(body_cells)..];
    let peak = window_slice.iter().copied().fold(0.0_f32, f32::max);
    let ceiling = scale_top(f64::from(peak), f64::from(CPU_CEILING_FLOOR));
    let bars = cell::chart(history, ceiling as f32, body_cells, CPU_ROWS);
    gutter_lines(bars, ceiling, GutterCadence::Alternating, |value| {
        format!("{value:.0}%")
    })
}

/// Row `MEM_HEADER_ROW`'s second half: a real ceiling names itself; with
/// none, the header states the substitute denominator instead, per the
/// design rule that every measurement states what it is measured against.
fn mem_header_text(max_memory: Option<u64>, window_peak: u64) -> String {
    match max_memory {
        Some(limit) => format!(
            "rss   same window   \u{b7}   ceiling {}",
            crate::output::human_bytes(limit)
        ),
        None => format!(
            "rss   same window   \u{b7}   no limit set \u{b7} scaled to peak {}",
            crate::output::human_bytes(window_peak)
        ),
    }
}

/// The memory chart's `MEM_ROWS` rows, and which one (if any) is the row
/// nearest a real `max_memory` ceiling: that row is [`draw_charts`]'s cue
/// to paint it in `--butter` instead of the chart's own colour.
///
/// Scales to `max(max_memory, window peak)` rather than `max_memory`
/// alone, so a sheep already over its limit still draws the spike rather
/// than clipping it off the top.
fn mem_chart_rows(
    history: &[u64],
    max_memory: Option<u64>,
    body_cells: usize,
) -> (Vec<String>, Option<usize>) {
    let window_slice = &history[history.len().saturating_sub(body_cells)..];
    let window_peak = window_slice.iter().copied().max().unwrap_or(0);
    let peak_for_scale = max_memory.map_or(window_peak, |limit| limit.max(window_peak));
    let ceiling = scale_top(peak_for_scale as f64, 0.0);
    let samples: Vec<f32> = history.iter().map(|&bytes| bytes as f32).collect();
    let bars = cell::chart(&samples, ceiling as f32, body_cells, MEM_ROWS);
    let mut lines = gutter_lines(bars, ceiling, GutterCadence::EveryRow, |value| {
        crate::output::human_bytes(value as u64)
    });

    let marked = max_memory.map(|limit| {
        if ceiling <= 0.0 {
            return 0;
        }
        let band = ceiling / MEM_ROWS as f64;
        ((ceiling - limit as f64) / band)
            .floor()
            .clamp(0.0, (MEM_ROWS - 1) as f64) as usize
    });
    if let Some(row) = marked {
        // `GUTTER` is ASCII throughout (digits, `%`, `M`, spaces), so this
        // is a valid byte index even though the chart body past it is not.
        let gutter = lines[row][..GUTTER].to_string();
        lines[row] = format!("{gutter}{} ceiling", "\u{254c}".repeat(body_cells));
    }
    (lines, marked)
}

/// A gutter's labelling cadence: how many of its rows carry a value versus
/// a bare tick. The frame gives the two charts different cadences (decision
/// 8): 8 rows is enough to crowd if every one is labelled, 5 is few enough
/// to label completely.
#[derive(Clone, Copy)]
enum GutterCadence {
    /// A label on even rows, a bare `|` tick on odd ones. The CPU chart's
    /// 8 rows.
    Alternating,
    /// A label on every row. The memory chart's 5 rows.
    EveryRow,
}

/// Prefixes each of `bars`' lines with [`GUTTER`] cells: a value on the
/// rows `cadence` picks, right-aligned and formatted by `format_value`,
/// with a bare `|` tick on any row `cadence` skips; the bottom row is
/// always the literal `0` rather than `format_value(0.0)`, since a unit on
/// a value that is always zero states nothing a bare `0` doesn't.
/// [`scale_top`]'s own ladder is why a labelled row doesn't need to be the
/// top or bottom to land on a round number.
fn gutter_lines(
    bars: Vec<String>,
    ceiling: f64,
    cadence: GutterCadence,
    format_value: impl Fn(f64) -> String,
) -> Vec<String> {
    let rows = bars.len();
    let last = rows.saturating_sub(1);
    bars.into_iter()
        .enumerate()
        .map(|(i, bar)| {
            let labelled = match cadence {
                GutterCadence::Alternating => i % 2 == 0,
                GutterCadence::EveryRow => true,
            };
            let gutter = if i == last {
                format!("{:>7} ", "0")
            } else if labelled {
                #[allow(clippy::cast_precision_loss)] // display only, a gutter label
                let value = ceiling * (rows - i) as f64 / rows as f64;
                format!("{:>7} ", format_value(value))
            } else {
                format!("{:>7} ", "|")
            };
            format!("{gutter}{bar}")
        })
        .collect()
}

/// The shared x axis: `body_cells` cells of rule, `now` ending on the last
/// one.
fn axis_row(body_cells: usize) -> String {
    let label = "now";
    let dashes = body_cells.saturating_sub(label.len());
    format!("{}{}{label}", " ".repeat(GUTTER), "\u{2500}".repeat(dashes))
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use shep_core::protocol::Response;

    use super::super::super::app::{Body, Control, KeyPress, Msg, Sent};
    use super::super::super::frames::render_text;
    use super::super::super::level::Level;
    use super::super::fixtures;
    use super::*;

    /// The bug the reviewer reproduced: pane pinned to sheep 1, sheep 1
    /// deleted, `pane.sheep()` still reads `Sheep(1)` while `app.selected()`
    /// has moved to `Sheep(2)` (`alpha`'s alphabetical neighbour, `bravo`,
    /// the only row left once the reseat runs). Reading `App::selected_row`
    /// here would draw `bravo`'s facts under a band still naming `alpha`.
    #[test]
    fn the_band_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, 80, 3);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("bravo"),
            "must not draw the sheep that replaced the pinned one: {text:?}"
        );
        assert!(
            text.contains("sheep 1: it is no longer in the flock"),
            "got {text:?}"
        );
    }

    /// The chart-drawing twin of the test above: pane pinned to alpha,
    /// alpha leaves the flock, bravo (with its own, distinct `max_memory`)
    /// takes the reseated selection. `draw`'s own gate reads
    /// `App::sheep_pane_row`, which is `None` once alpha is gone, so
    /// nothing charts at all; reading `App::selected_row` instead would
    /// draw bravo's ceiling under a pane still naming alpha. The area here
    /// is tall and wide enough to reach `draw_charts`, unlike the test
    /// above's 80x3.
    #[test]
    fn the_charts_do_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online)
                    .max_memory(Some(64 << 20))
                    .build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        let _ = app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(2, "bravo", ProcStatus::Online)
                    .max_memory(Some(64 << 20))
                    .build(),
            ],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, 40, MIN_HEIGHT_FOR_CHARTS);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("ceiling"),
            "bravo's own max_memory must not draw once the pinned sheep is \
             gone: {text:?}"
        );
    }

    /// Thin wrapper over the real header function: a full buffer, so the
    /// "drawn window" branch runs rather than the "collecting" one.
    fn header_at(width: u16) -> String {
        let body = chart_body_cells(width);
        cpu_header_text(body, body)
    }

    /// Thin wrapper over the real header function: `body_cells` passed
    /// straight through, the way [`draw_charts`] hands it the pane's own
    /// computed value rather than a raw terminal width.
    fn header_with_samples(history_len: usize, body_cells: usize) -> String {
        cpu_header_text(history_len, body_cells)
    }

    /// Thin wrapper over the real row function, over a fixture with a real
    /// shape: a flat history would prove nothing about the ceiling row's
    /// placement.
    fn mem_rows_with_limit(max_memory: Option<u64>) -> (Vec<String>, Option<usize>) {
        let history = [20 << 20, 25 << 20, 30 << 20, 40 << 20, 48 << 20];
        mem_chart_rows(&history, max_memory, 20)
    }

    /// Thin wrapper over the real header function.
    fn mem_header_with_limit(max_memory: Option<u64>, window_peak: u64) -> String {
        mem_header_text(max_memory, window_peak)
    }

    /// The header states the window it actually drew, not a literal. At two
    /// widths, because a literal passes at one of them.
    #[test]
    fn the_cpu_header_states_the_drawn_window() {
        assert!(header_at(160).contains("4m40s, one 2s sample per column"));
        assert!(header_at(140).contains("4m00s, one 2s sample per column"));
    }

    /// The buffer starts empty on every launch and dies with the process, so
    /// a chart that is not yet full says how full it is.
    #[test]
    fn a_partial_buffer_says_how_much_it_has() {
        assert!(header_with_samples(35, 140).contains("collecting \u{b7} 1m10s of 4m40s"));
    }

    /// With a limit set the ceiling is the limit, drawn as its own row and
    /// labelled, and specifically the row this fixture's own scaling puts
    /// it at: row 0 (the top) would be wrong here, so pinning presence alone
    /// would not catch a placement bug that always drew row 0.
    #[test]
    fn the_memory_chart_labels_a_real_ceiling() {
        let (rows, marked) = mem_rows_with_limit(Some(52 << 20));
        assert_eq!(
            marked,
            Some(2),
            "a 52M limit against this fixture's 48M peak scales to a 100M \
             ceiling, 20M per row, so the marked row is 2, not the top"
        );
        assert!(rows[2].contains("ceiling"), "got {rows:?}");
    }

    /// With no limit there is no ceiling row and the header says what it
    /// scaled to instead, per the design rule that every measurement states
    /// its denominator.
    #[test]
    fn the_memory_chart_states_its_substitute_denominator() {
        let header = mem_header_with_limit(None, 48 << 20);
        assert!(header.contains("no limit set"));
        assert!(header.contains("scaled to peak"));
        assert!(
            !mem_rows_with_limit(None)
                .0
                .iter()
                .any(|row| row.contains("ceiling"))
        );
    }

    /// Past 160 columns [`chart_body_cells`] would ask for more samples than
    /// [`HISTORY`] ever holds; capped, or [`cpu_header_text`] would read
    /// `collecting` forever even once the buffer is full.
    #[test]
    fn chart_body_stays_within_the_history_buffer() {
        let body = chart_body_cells(300);
        assert_eq!(body, HISTORY);
        assert!(
            !cpu_header_text(HISTORY, body).contains("collecting"),
            "a full buffer past the cap must not still read collecting"
        );
    }

    /// Two labels bought nothing once `scale_top`'s own ladder makes every
    /// division round too: the gutter carries a value every other row,
    /// alternating with a bare tick, not just at the top and the bottom.
    #[test]
    fn the_gutter_labels_more_than_the_ends() {
        let history = [10.0_f32, 20.0, 30.0, 40.0, 45.0];
        let rows = cpu_chart_rows(&history, 20);
        let labelled: Vec<&str> = rows
            .iter()
            .map(|row| row[..GUTTER].trim())
            .filter(|cell| !cell.is_empty())
            .collect();
        assert!(
            labelled.len() > 2,
            "expected more than just the top and bottom label: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row[..GUTTER].contains('|')),
            "expected a bare tick between labels: {rows:?}"
        );
    }

    /// The frame gives the two gutters different cadences (the memory
    /// chart has few enough rows to label every one; the CPU chart has
    /// enough that alternating avoids crowding), so a fix that pins one
    /// must not flatten the other's.
    #[test]
    fn the_gutter_cadence_differs_by_chart() {
        let (mem_rows, _) = mem_rows_with_limit(None);
        let labelled_mem = mem_rows
            .iter()
            .filter(|row| {
                let cell = row[..GUTTER].trim();
                !cell.is_empty() && cell != "|"
            })
            .count();
        assert_eq!(
            labelled_mem, MEM_ROWS,
            "every memory row should carry a label: {mem_rows:?}"
        );

        let cpu_history = [10.0_f32, 20.0, 30.0, 40.0, 45.0];
        let cpu_rows = cpu_chart_rows(&cpu_history, 20);
        let ticked_cpu = cpu_rows
            .iter()
            .filter(|row| row[..GUTTER].contains('|'))
            .count();
        assert!(
            ticked_cpu > 0,
            "the CPU gutter should still alternate with bare ticks: {cpu_rows:?}"
        );
        assert!(
            ticked_cpu < CPU_ROWS,
            "the CPU gutter should not label every row: {cpu_rows:?}"
        );
    }

    /// One sheep, run through two real polls so both `cpu_history` and
    /// `rss_history` hold a differenced, nonzero last sample: the alignment
    /// test below needs the rightmost column of both charts' bottom row to
    /// be real content, not left-padding a too-short history would leave
    /// blank there too.
    fn app_with_two_polls(id: u32, name: &str, max_memory: Option<u64>) -> App {
        let t0 = std::time::Instant::now();
        let mut app = App::new(
            fixtures::plain(),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(id, name, ProcStatus::Online)
                    .cpu_ms(Some(0))
                    .memory_bytes(Some(10 << 20))
                    .max_memory(max_memory)
                    .build(),
            ],
            at: t0,
        });
        let t1 = t0 + Duration::from_secs(2);
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(id, name, ProcStatus::Online)
                    .cpu_ms(Some(2000))
                    .memory_bytes(Some(10 << 20))
                    .max_memory(max_memory)
                    .build(),
            ],
            at: t1,
        });
        app
    }

    /// The whole reason this frame was picked over side-by-side charts: a
    /// memory step and a CPU spike land on the same column because both
    /// bodies are drawn to the same `body_cells`, not two calculations that
    /// could quietly drift apart. Rendered through [`draw_charts`] itself,
    /// not two calls typed with the same literal by hand: that would still
    /// pass if the two calls' own `body_cells` argument drifted apart at the
    /// call site, which is exactly the mutation this test exists to catch.
    #[test]
    fn the_two_charts_share_one_body_width() {
        let app = app_with_two_polls(1, "alpha", None);
        let area = Rect::new(0, 0, 40, MIN_HEIGHT_FOR_CHARTS);
        let mut buffer = Buffer::empty(area);
        draw_charts(&app, 1, None, area, &mut buffer, fixtures::plain());
        let text = render_text(&buffer);
        let lines: Vec<&str> = text.lines().collect();
        let cpu_last_row = lines[usize::from(CPU_CHART_ROW) + CPU_ROWS - 1];
        let mem_last_row = lines[usize::from(MEM_CHART_ROW) + MEM_ROWS - 1];
        let cpu_end = cpu_last_row.trim_end().chars().count();
        let mem_end = mem_last_row.trim_end().chars().count();
        assert_eq!(
            cpu_end, mem_end,
            "CPU chart's bottom row ends at column {cpu_end}, memory's at \
             {mem_end}: {cpu_last_row:?} vs {mem_last_row:?}"
        );
    }

    use shep_core::config::AppConfig;

    /// `web`, with `max_memory` parked until a respawn and two env keys.
    fn web_view() -> SheepConfigView {
        let mut config = AppConfig {
            name: "web".to_owned(),
            ..AppConfig::default()
        };
        config
            .env
            .insert("DB_HOST".to_owned(), "db.internal".to_owned());
        config
            .env
            .insert("API_KEY".to_owned(), "{{secret:API}}".to_owned());
        SheepConfigView::new(config, Vec::new(), vec!["max_memory".to_owned()])
    }

    /// A bare config carrying exactly `pairs` as its env, nothing pending or
    /// overridden: what [`a_sealed_key_is_marked_and_a_plain_one_is_not`]
    /// needs to tell a plain key from a sealed one without `web_view`'s own
    /// pending field in the way.
    fn view_with_env(pairs: &[(&str, &str)]) -> SheepConfigView {
        let mut config = AppConfig {
            name: "test".to_owned(),
            ..AppConfig::default()
        };
        for (key, value) in pairs {
            config.env.insert((*key).to_owned(), (*value).to_owned());
        }
        SheepConfigView::new(config, Vec::new(), Vec::new())
    }

    /// One line as a plain string, styles dropped: the same flattening
    /// `view::pane`'s own `text_of` does, one line at a time.
    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The groups [`column_body`] actually emitted, in the order it emitted
    /// them: read off the same walk [`draw_column`] draws from, not
    /// recomputed independently, so a bug in that walk shows up here too.
    fn group_labels_of(view: &SheepConfigView) -> Vec<String> {
        column_body(view, fixtures::plain()).1
    }

    /// The rendered text of the one row naming `key`.
    fn field_row_of(view: &SheepConfigView, key: &str) -> String {
        column_body_lines(view, fixtures::plain())
            .iter()
            .map(line_text)
            .find(|line| line.trim_start_matches('!').starts_with(key))
            .unwrap_or_else(|| panic!("no row for {key}"))
    }

    /// The env section's own rows, everything after the `env` label line.
    fn env_rows_of(view: &SheepConfigView) -> Vec<String> {
        let lines: Vec<String> = column_body_lines(view, fixtures::plain())
            .iter()
            .map(line_text)
            .collect();
        let label = lines
            .iter()
            .position(|line| line.trim() == "env")
            .expect("column_body always closes with an env label");
        lines[label + 1..].to_vec()
    }

    /// The one row naming `key`, out of `rows`.
    fn row_for<'a>(key: &str, rows: &'a [String]) -> &'a str {
        rows.iter()
            .find(|row| row.contains(key))
            .unwrap_or_else(|| panic!("no row for {key} in {rows:?}"))
    }

    /// The whole column's text, `None` standing in for a pane whose config
    /// has not landed yet.
    fn column_of(config: Option<SheepConfigView>) -> String {
        let lines = match &config {
            Some(view) => column_body_lines(view, fixtures::plain()),
            None => vec![waiting_line(fixtures::plain())],
        };
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    /// The group labels [`super::super::pane::ConfigPane`]'s own field set
    /// carries for `web_view`'s config, in schema order, deduplicated
    /// consecutively the same way [`column_body`]'s own walk dedupes. The
    /// source of truth [`the_groups_are_the_schemas_eight_in_its_own_order`]
    /// checks the column against, instead of a copy of the order typed by
    /// hand: `column_body` layers its own `env`-skipping logic on top of the
    /// same [`pane::sheep_fields`] this reads, so a real cross-check is what
    /// would catch that column-specific divergence and a literal cannot.
    fn config_pane_group_labels(view: &SheepConfigView) -> Vec<String> {
        let pane = crate::lookout::pane::ConfigPane::sheep(view.clone());
        let mut labels = Vec::new();
        let mut current: Option<&str> = None;
        for field in pane.fields().fields() {
            if field.group.as_deref() != current {
                current = field.group.as_deref();
                labels.push(current.unwrap_or_default().to_owned());
            }
        }
        labels
    }

    /// Eight groups, in the schema's own order, the same order
    /// [`ConfigPane`](super::super::pane::ConfigPane)'s own field set gives
    /// for the same config. The frame lists seven and puts `restart` second;
    /// `cron` is missing from it entirely.
    #[test]
    fn the_groups_are_the_schemas_eight_in_its_own_order() {
        let view = web_view();
        assert_eq!(group_labels_of(&view), config_pane_group_labels(&view));
    }

    /// A field parked until the next respawn is marked and says so.
    #[test]
    fn a_pending_field_is_marked_and_annotated() {
        let row = field_row_of(&web_view(), "max_memory");
        assert!(row.starts_with('!'), "{row:?}");
        assert!(row.contains("awaits respawn"), "{row:?}");
    }

    /// The wire clears env before the struct is built, so no pane can show a
    /// value. This test exists so a later change that starts carrying them
    /// fails here rather than shipping.
    #[test]
    fn no_env_value_reaches_the_column() {
        let rendered = env_rows_of(&web_view()).join("");
        assert!(!rendered.contains("hunter2"), "{rendered:?}");
    }

    /// A key the store fills renders sealed; a Flockfile key does not.
    #[test]
    fn a_sealed_key_is_marked_and_a_plain_one_is_not() {
        let rows = env_rows_of(&view_with_env(&[
            ("PLAIN", "v"),
            ("SEALED", "{{secret:PW}}"),
        ]));
        assert!(row_for("SEALED", &rows).contains("sealed"));
        assert!(!row_for("PLAIN", &rows).contains("sealed"));
    }

    /// Until the reply lands there is no config, and an empty group list
    /// would read as a sheep that has none.
    #[test]
    fn a_pane_without_its_config_yet_says_so() {
        assert!(column_of(None).contains("reading config"));
    }

    /// `exp_backoff_restart_delay` is the Flockfile schema's longest field
    /// name: 25 characters, 26 with the pending `!` flag. `COLUMN_NAME_W`
    /// has to fit that exactly, not "wide enough" by a comment's own say-so.
    #[test]
    fn the_longest_pending_field_name_is_not_truncated() {
        let config = AppConfig {
            name: "web".to_owned(),
            ..AppConfig::default()
        };
        let view = SheepConfigView::new(
            config,
            Vec::new(),
            vec!["exp_backoff_restart_delay".to_owned()],
        );
        let row = field_row_of(&view, "exp_backoff_restart_delay");
        assert!(
            row.starts_with("!exp_backoff_restart_delay  "),
            "the flagged name and its separator must survive whole: {row:?}"
        );
    }

    /// [`field_value_text`] resolves a bare `MemSize` number the same way
    /// [`ConfigPane::display_value`](super::super::pane::ConfigPane::display_value)
    /// does for the editing pane: both read [`pane::resolved_display`], so a
    /// `max_memory` that reads `52428800` on one screen cannot read
    /// `52428800 B` on the other.
    #[test]
    fn the_column_and_the_editing_pane_resolve_the_same_mem_size_units() {
        use shep_core::values::MemSize;

        let mut config = AppConfig {
            name: "web".to_owned(),
            ..AppConfig::default()
        };
        // Not a multiple of any binary unit, so `MemSize`'s own `Display`
        // falls through to the bare-digit branch `resolved_display` exists
        // to annotate; a round number like `50M` would serialize with its
        // unit already and prove nothing.
        config.max_memory = Some(MemSize::from_bytes(1_234_567));
        let column_view = SheepConfigView::new(config.clone(), Vec::new(), Vec::new());
        let editing_view = SheepConfigView::new(config, Vec::new(), Vec::new());

        let row = field_row_of(&column_view, "max_memory");
        let pane = crate::lookout::pane::ConfigPane::sheep(editing_view);
        let resolved = pane.display_value("max_memory");
        assert!(
            resolved.ends_with(" B"),
            "setup: the fixture must actually exercise the unit suffix: {resolved:?}"
        );
        assert!(
            row.contains(&resolved),
            "column row {row:?} does not carry the editing pane's own {resolved:?}"
        );
    }

    /// [`Field::secret`] guards the sheep column the same way
    /// [`ConfigPane`](super::super::pane::ConfigPane)'s own row draws it: the
    /// value is never rendered, only that there is one. No Flockfile field
    /// carries the flag today, so this is unit-level, over a fabricated
    /// field rather than a real config.
    #[test]
    fn a_secret_field_renders_set_and_never_its_value() {
        use crate::lookout::field::FieldKind;

        let field = Field {
            key: "webhook".to_owned(),
            help: String::new(),
            group: None,
            kind: FieldKind::Text,
            value_kind: None,
            default: None,
            secret: true,
            editable: true,
        };
        let fields = FieldSet::from_fields(vec![field.clone()], &[]);
        let mut values = Map::new();
        values.insert("webhook".to_owned(), Value::String("hunter2".to_owned()));
        assert_eq!(field_value_text(&fields, &field, &values), "<set>");
    }

    /// The regression Tasks 7 and 8 both shipped once each: a pane pinned to
    /// a sheep the flock table has since reseated its selection away from.
    /// The column reads `SheepPane::config` — set only by `adopt_config` and
    /// `set_sheep`, never by the reseat — so it has no equivalent bug
    /// surface to begin with; this pins that a later change cannot grow one
    /// by threading `App::selected` into the column instead.
    #[test]
    fn the_column_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        let alpha_config = AppConfig {
            name: "alpha".to_owned(),
            ..AppConfig::default()
        };
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "alpha".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(SheepConfigView::new(
                alpha_config,
                Vec::new(),
                Vec::new(),
            )))),
        });
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, 80, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            text.contains("name                        alpha"),
            "must still draw the pinned sheep's own `name` field: {text:?}"
        );
        assert!(!text.contains("bravo"), "{text:?}");
    }

    /// `draw_feed` draws whatever `App::feed` holds, filtered through the
    /// pane's own pinned `BleatsPane`, and never reaches for `App::selected`
    /// itself: this pins that half. It does *not* pin `App::feed_row`'s own
    /// scoping to the pane's pinned sheep rather than the reseated
    /// selection — a fixture with no polling loop cannot re-fetch the tail
    /// after the reseat below, so the tail this test asserts on is exactly
    /// the one `Msg::Bleats` injected before it, whatever `feed_row` would
    /// answer now. `the_feed_row_follows_the_sheep_panes_own_pinned_sheep_when_the_selection_moves`
    /// in `app.rs` is what pins that half; a mutation removing `feed_row`'s
    /// own `Body::Sheep` branch left this test green.
    ///
    /// The area is 160x46, at [`FEED_X`] plus [`FEED_WIDTH`] and
    /// [`MIN_HEIGHT_FOR_COLUMN`] exactly: `draw`'s own gate skips
    /// `draw_feed` below either floor, and a narrower or shorter area would
    /// pass this test whether or not `draw_feed` ever ran, the same way a
    /// too-small area let Task 8's own chart bug through once.
    #[test]
    fn the_feed_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        app.update(Msg::Bleats {
            tail: Tail {
                lines: vec![TailLine {
                    stream: Stream::Out,
                    text: "alpha wrote this".to_string(),
                }],
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            text.contains("alpha wrote this"),
            "the pinned sheep's own line must still draw: {text:?}"
        );
    }

    /// [`feed_header_text`]'s own chip: `[level≥warn]`, the same word
    /// [`Level`]'s own doc says the filter row
    /// renders it as, once a minimum is set on the embedded feed.
    #[test]
    fn the_headers_level_chip_names_the_minimum() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_min_level(Some(Level::Warn));
        let text = feed_header_text(&feed, &Tail::default());
        assert!(text.contains("[level≥warn]"), "got {text:?}");
        assert!(text.contains("/ narrow"), "got {text:?}");
    }

    /// A regex matcher's chip names itself a regex, the same suffix
    /// `bleats_full.rs`'s own `chip_labels` carries: an operator who
    /// narrows the embedded feed with `/…/` needs the same tell this
    /// column's full-screen twin already gives.
    #[test]
    fn the_headers_match_chip_names_a_regex() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("/po+l/".to_string());
        let text = feed_header_text(&feed, &Tail::default());
        assert!(text.contains("match /po+l/ (regex)"), "got {text:?}");
    }

    /// A pattern that fails to compile says so on the chip, rather than the
    /// embedded feed just going quiet with no explanation until the
    /// operator presses `b` to reach the full-screen pane's own chip.
    #[test]
    fn the_headers_match_chip_names_an_invalid_regex() {
        let mut feed = BleatsPane::new(RowKey::Sheep(1));
        feed.set_match("/pool(/".to_string());
        let text = feed_header_text(&feed, &Tail::default());
        assert!(
            text.contains("invalid regex, matches nothing"),
            "got {text:?}"
        );
    }

    /// The `N earlier` clause counts survivors the body has no room to show,
    /// never the raw line count: with `COLUMN_BODY_ROWS` rows to draw into
    /// and one more line than that, exactly one line is hidden.
    #[test]
    fn the_headers_earlier_clause_counts_hidden_survivors_not_raw_lines() {
        let feed = BleatsPane::new(RowKey::Sheep(1));
        let tail = Tail {
            lines: (0..COLUMN_BODY_ROWS + 1)
                .map(|n| TailLine {
                    stream: Stream::Out,
                    text: format!("line-{n}"),
                })
                .collect(),
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 0,
            note: None,
        };
        let text = feed_header_text(&feed, &tail);
        assert!(text.contains("1 earlier"), "got {text:?}");
    }

    /// The feed shows the window's newest lines, oldest at the top, and
    /// truncates a line too long for [`FEED_WIDTH`] rather than wrapping it
    /// onto a second row: this column has no row budget to spend on one.
    #[test]
    fn the_feed_shows_the_newest_lines_and_truncates_a_long_one() {
        let mut app = fixtures::app_with(
            vec![ProcessInfo::builder(1, "alpha", ProcStatus::Online).build()],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Bleats {
            tail: Tail {
                lines: (0..COLUMN_BODY_ROWS + 5)
                    .map(|n| TailLine {
                        stream: Stream::Out,
                        text: format!("line-{n}"),
                    })
                    .chain(std::iter::once(TailLine {
                        stream: Stream::Err,
                        text: "x".repeat(200),
                    }))
                    .collect(),
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            !text.contains("line-0\n") && !text.contains("line-0 "),
            "the oldest lines scroll off the top: {text:?}"
        );
        assert!(
            text.contains(&format!("{}\u{2026}", "x".repeat(77))),
            "the long line truncates with an ellipsis at the column's own \
             width (78 cells: 77 characters plus the ellipsis), not before \
             it and not after: {text:?}"
        );
        assert!(
            !text.contains(&"x".repeat(78)),
            "and no further: a truncated line, not a wrapped one: {text:?}"
        );
    }

    /// The divider sits one cell past the config column, at
    /// [`COLUMN_WIDTH`], for every row the two sides draw into.
    #[test]
    fn the_divider_runs_the_full_height_of_the_column_and_feed() {
        let mut app = fixtures::app_with(
            vec![ProcessInfo::builder(1, "alpha", ProcStatus::Online).build()],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        for row in COLUMN_HEADER_ROW..=COLUMN_LAST_ROW {
            assert_eq!(
                buffer[(COLUMN_WIDTH, row)].symbol(),
                "\u{2502}",
                "row {row} has no divider"
            );
        }
    }

    /// The arithmetic, asserted rather than trusted:
    ///     160 = 8 gutter + 140 body + 12 margin
    ///     body = min(width - 20, HISTORY)
    /// A scene one cell short of its own column set silently drops the
    /// thing it exists to show, which is why this is a test and not a
    /// comment.
    #[test]
    fn the_chart_body_is_the_width_less_its_gutter_and_margin() {
        assert_eq!(chart_body_cells(160), 140);
        assert_eq!(chart_body_cells(140), 120);
    }

    /// Past the design target the buffer runs out before the columns do, so
    /// the margin grows rather than leaving cells that can never fill.
    #[test]
    fn a_wider_terminal_grows_the_margin_rather_than_the_body() {
        assert_eq!(chart_body_cells(200), 140);
    }

    /// A sheep pane over several real polls, cpu and rss both rising: task
    /// 4's counter differencing needs a poll to differ against, so a scene
    /// built from a single snapshot would demonstrate nothing while looking
    /// fine, the same trap `Scene::CfgDrift` was rewritten to avoid.
    fn render_at(width: u16, height: u16) -> String {
        let t0 = std::time::Instant::now();
        let mut app = App::new(
            fixtures::plain(),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(7, "web", ProcStatus::Online)
                    .cpu_ms(Some(0))
                    .memory_bytes(Some(10 << 20))
                    .max_memory(Some(64 << 20))
                    .build(),
            ],
            at: t0,
        });
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for i in 1..6u64 {
            let at = t0 + Duration::from_secs(2 * i);
            app.update(Msg::Snapshot {
                rows: vec![
                    ProcessInfo::builder(7, "web", ProcStatus::Online)
                        .cpu_ms(Some(i * 400))
                        .memory_bytes(Some((10 + i * 4) << 20))
                        .max_memory(Some(64 << 20))
                        .build(),
                ],
                at,
            });
        }
        let Body::Sheep(pane) = app.body() else {
            panic!("setup: the pane opened on web")
        };
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        render_text(&buffer)
    }

    /// Memory goes first and CPU stays: a CPU chart is the more diagnostic
    /// of the two, and memory still has a gauge to fall back on.
    #[test]
    fn below_a_hundred_and_forty_columns_only_the_cpu_chart_draws() {
        let rendered = render_at(139, 48);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(rendered.contains("rss "), "got {rendered:?}");
        assert!(
            !rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
    }

    /// Below 100 both go and the pane falls back to 1a's pair.
    #[test]
    fn below_a_hundred_columns_both_charts_become_the_sparkline_pair() {
        let rendered = render_at(99, 48);
        assert!(
            !rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(
            !rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
        assert!(rendered.contains("CPU 20s"), "got {rendered:?}");
    }

    /// Rows too: the charts hold 2 to 17, and the config and feed columns
    /// are what the pane is for, so they give ground last.
    #[test]
    fn a_short_terminal_drops_the_charts_before_the_columns() {
        assert!(!render_at(160, 25).contains("\u{2588}\u{2588} MEM"));
        assert!(render_at(160, 25).contains("\u{2588}\u{2588} CONFIG & ENV"));
        assert!(!render_at(160, 19).contains("\u{2588}\u{2588} CPU"));
        assert!(render_at(160, 19).contains("\u{2588}\u{2588} CONFIG & ENV"));
    }

    /// The Full tier's own floor: at exactly 140 columns both charts still
    /// draw. `below_a_hundred_and_forty_columns_only_the_cpu_chart_draws`
    /// pins the cell below this one; nothing pinned the boundary itself,
    /// so a one-cell-generous mutation of `width >= 140` could hold the
    /// Full tier open past its own name and every other test would stay
    /// green.
    #[test]
    fn at_a_hundred_and_forty_columns_both_charts_still_draw() {
        let rendered = render_at(140, 48);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(
            rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
    }

    /// The CpuOnly tier's own floor: at exactly 100 columns the CPU chart
    /// still draws rather than falling to the sparkline pair.
    #[test]
    fn at_a_hundred_columns_the_cpu_chart_still_draws() {
        let rendered = render_at(100, 48);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
        assert!(!rendered.contains("CPU 20s"), "got {rendered:?}");
    }

    /// [`MIN_HEIGHT_FOR_CHARTS`]'s own floor: at exactly that height a
    /// chart still draws rather than the pane falling straight to
    /// [`ChartTier::None`].
    #[test]
    fn at_the_chart_height_floor_a_chart_still_draws() {
        let rendered = render_at(160, MIN_HEIGHT_FOR_CHARTS);
        assert!(
            rendered.contains("\u{2588}\u{2588} CPU"),
            "got {rendered:?}"
        );
    }

    /// [`FULL_TIER_MIN_HEIGHT`]'s own floor: at exactly that height the
    /// Full tier still holds rather than being downgraded to
    /// [`ChartTier::CpuOnly`].
    #[test]
    fn at_the_full_tier_height_floor_the_memory_chart_still_draws() {
        let rendered = render_at(160, FULL_TIER_MIN_HEIGHT);
        assert!(
            rendered.contains("\u{2588}\u{2588} MEM"),
            "got {rendered:?}"
        );
    }
}
