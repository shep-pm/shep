//! Drawing a [`ConfigPane`]: a title naming the target, the field set's
//! groups as section headers, one row per field, and a cost column saying
//! what changing that field would cost.
//!
//! The layout is [`super::settings`]'s: both screens own the whole body
//! between the title and the status bar, both have more rows than a
//! terminal has lines, and both pay for chrome the viewport cannot see.
//! The scroll walk is shared ([`super::scroll::to_cursor`]); the layout
//! below is this pane's own, since a field list under eight headers and a
//! settings screen with a dogs table share almost no lines.
//!
//! A sheep pane is 40 rows plus a title, eight headers and seven blank
//! separators: sixteen lines of chrome before a marker is paid for.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

mod chrome;
pub(super) mod close;
mod env;
mod field_row;
#[cfg(test)]
mod fixtures;
mod layout;
mod list;

use self::chrome::{
    column_header_line, hairline_line, has_groups, legend_line, tab_row_line, title_band_line,
    title_line,
};
use self::close::draw_close_dialog;
use self::env::pending_and_env_lines;
use self::field_row::{field_line, impact_tag, push_wrapped_blurb, section_header, type_label};
use self::layout::{
    BLURB_WRAP, PANEL_MAX, body_width, clipped, lands_fits_beside_panel, line_columns, mask_secret,
    panel_width, wrap,
};
use self::list::list_lines;

use super::super::app::App;
use super::super::field::Field;
use super::super::pane::{ConfigPane, PaneRow, PaneTarget};
use super::super::theme::Palette;
use super::super::validation;
use super::flock::fit;
use super::overlay;
use super::scroll::Attempt;
use crate::vocabulary::Role;

/// The active group's fields, laid out through [`super::scroll::to_cursor`]
/// exactly as [`body_from`] already does for the groupless fallback, then
/// [`pending_and_env_lines`] appended after.
fn grouped_body_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    show_lands: bool,
) -> Vec<Line<'static>> {
    // The cursor's own row has to draw somewhere in this budget, per the
    // rule every screen in this file holds. When it is on an env row or
    // `+ add a key`, the field body has nothing to align to and its own
    // windowing would spend the whole budget on fields none of which is
    // selected, leaving nothing for the row that actually is. So
    // `pending_and_env_lines` goes first then, and the field body takes
    // whatever it leaves, rather than the other way around.
    if matches!(pane.cursor(), Some(PaneRow::Env(_) | PaneRow::AddEnv)) {
        let tail = pending_and_env_lines(pane, palette, width, budget, show_lands);
        let remaining = budget.saturating_sub(tail.len());
        let mut lines = if !pane.field_rows().is_empty() && remaining > 0 {
            body_from(pane, palette, width, remaining, 0, false, show_lands).lines
        } else {
            Vec::new()
        };
        lines.extend(tail);
        return lines;
    }
    let mut lines = if !pane.fields().is_empty() && budget > 0 {
        let field_rows = pane.field_rows();
        let cursor_row = field_rows
            .iter()
            .position(|row| Some(*row) == pane.cursor())
            .unwrap_or(0);
        super::scroll::to_cursor(
            cursor_row,
            pane.view().offset(),
            |offset| body_from(pane, palette, width, budget, offset, false, show_lands),
            || cursor_only(pane, palette, width, budget, cursor_row, show_lands),
        )
    } else {
        Vec::new()
    };
    let remaining = budget.saturating_sub(lines.len());
    lines.extend(pending_and_env_lines(
        pane, palette, width, remaining, show_lands,
    ));
    lines
}

/// The pane's lines when its fields carry groups (every sheep): the butter
/// title band naming the target once, the tab row, a hairline, the menu or
/// help line when one is up else the column header row, the body, a
/// hairline, and the legend.
fn grouped_pane_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    grouped_pane_lines_with_panel(pane, palette, width, budget, None)
}

/// [`grouped_pane_lines`], with the cursor's own field's [`panel_lines`]
/// drawn beside the body only: the title band, tab row, hairline and
/// header row above it, and the trailing hairline and legend below it,
/// are chrome, not per-field, so widening them into the panel's own
/// column would only paint over where the panel sits.
///
/// Chrome is laid out at the real `width`, not clamped to the design
/// target: a terminal wider than 160 stretches the title band's own
/// reverse-video band and the hairlines the rest of the way, rather than
/// leaving blank space to the right of a pane pinned at 160. `panel_width`
/// governs how much of that width the panel itself claims; nothing here
/// grows the panel past [`PANEL_MAX`].
///
/// Only reached when [`panel_width`] returns `Some`; a narrower terminal
/// never calls this and keeps today's single column.
fn grouped_pane_with_panel_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    panel_w: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let panel = panel_lines(pane, palette, panel_w);
    grouped_pane_lines_with_panel(pane, palette, width, budget, Some((panel, panel_w)))
}

/// The body shared by [`grouped_pane_lines`] and
/// [`grouped_pane_with_panel_lines`]: `panel` is `None` for the former,
/// which lays the body out at `width` exactly as it always has, and
/// `Some((lines, panel_width))` for the latter, which lays the body out at
/// `width - panel_width` instead and merges `lines` beside it, since that
/// is the one region tall enough and narrow enough to hold both.
fn grouped_pane_lines_with_panel(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    panel: Option<(Vec<Line<'static>>, u16)>,
) -> Vec<Line<'static>> {
    let has_panel = panel.is_some();
    let show_lands = lands_fits_beside_panel(width, has_panel);
    let left_width = panel
        .as_ref()
        .map_or(width, |(_, panel_width)| width.saturating_sub(*panel_width));
    let mut lines = vec![title_band_line(pane, palette, width)];
    let mut remaining = budget - 1;
    if remaining == 0 {
        return lines;
    }
    // Everything from here down is best-effort, and every push below is
    // gated on `remaining > 1` rather than `> 0`: the body's own cursor
    // outranks every one of these lines, per `cursor_only`'s own doc that
    // the selected row is drawn at every height the pane claims to
    // support, so a chrome line is only added when doing so still leaves
    // at least one line for the body once the budget reaches it.
    if remaining > 1 {
        lines.push(tab_row_line(pane, palette, width));
        remaining -= 1;
    }
    if remaining > 1 {
        lines.push(hairline_line(palette, width));
        remaining -= 1;
    }
    if remaining > 1 {
        lines.push(column_header_line(palette, left_width, show_lands));
        remaining -= 1;
    }
    push_wrapped_blurb(&mut lines, &mut remaining, pane, palette, width);
    // Unreachable given `push_wrapped_blurb`'s own floor of `<= 1`. Kept as
    // the explicit statement of that invariant;
    // `the_blurb_never_spends_the_row_the_cursor_needs` is what actually
    // fails if the floor is ever loosened.
    if remaining == 0 {
        return lines;
    }
    // The trailing hairline and legend are reserved ahead of the body,
    // same rule every other footer in this module follows (see
    // `body_from`'s own doc on markers): a line nothing counted is a line
    // that can overrun. Never the body's last line, though, for the same
    // reason as above.
    let footer_lines = if remaining > 1 {
        2.min(remaining - 1)
    } else {
        0
    };
    let body_budget = remaining - footer_lines;
    if body_budget > 0 {
        let body = grouped_body_lines(pane, palette, left_width, body_budget, show_lands);
        lines.extend(match panel {
            Some((panel, _)) => merge_beside_panel(body, panel, left_width, body_budget),
            None => body,
        });
    }
    if footer_lines >= 1 {
        lines.push(hairline_line(palette, width));
    }
    if footer_lines >= 2 {
        lines.push(legend_line(palette, width));
    }
    lines
}

/// `left`'s lines and `panel`'s lines, side by side: `left` padded out to
/// exactly `left_width` columns with a blank span (never truncated, since
/// every left line is already laid out to fit inside its own width), then
/// whichever `panel` row shares that index appended after it.
///
/// `left` and `panel` are rarely the same length: a short field list (a
/// dog's group-free body, or a group with few fields) can run out before
/// `NEIGHBOURS` does, and a field with nothing to say in three of its six
/// regions can leave `panel` shorter than the field list above it. Either
/// side short of the other draws blank rather than dropping the longer
/// side's own rows, up to `budget`, the same vertical ceiling
/// [`grouped_body_lines`] was already laid out against: this only ever
/// lengthens `left`'s own count, never grows past what the caller already
/// reserved room for.
fn merge_beside_panel(
    left: Vec<Line<'static>>,
    panel: Vec<Line<'static>>,
    left_width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let left_width = usize::from(left_width);
    // `left` never exceeds `budget` on its own, since `grouped_body_lines`
    // already laid it out against the same ceiling; the `min` only ever
    // trims `panel`'s own overrun.
    let rows = left.len().max(panel.len()).min(budget);
    let mut left = left.into_iter();
    let mut panel = panel.into_iter();
    (0..rows)
        .map(|_| {
            let line = left.next().unwrap_or_default();
            let used = line_columns(&line);
            let mut spans = line.spans;
            if used < left_width {
                spans.push(Span::raw(" ".repeat(left_width - used)));
            }
            if let Some(panel_line) = panel.next() {
                spans.extend(panel_line.spans);
            }
            Line::from(spans)
        })
        .collect()
}

/// The trailing "shep publishes..." line a dog-target render reserves out
/// of its own budget before the body claims what is left: `None` for a
/// sheep, which owns its own reload rather than handing that decision to a
/// dog's own binary, and for a dog with no budget left to spend on it.
///
/// Text only; the caller decides whether reserving it costs one line of
/// `body_budget`, since [`pane_lines`]'s plain branch and
/// [`ungrouped_pane_with_panel_lines`] both need that decision made before
/// this call, not after.
fn dog_footer_text(pane: &ConfigPane, body_budget: usize) -> Option<String> {
    let PaneTarget::Dog { name, .. } = pane.target() else {
        return None;
    };
    (body_budget > 0).then(|| format!("shep publishes the change; {name} decides what to reload"))
}

/// Pushes `footer`'s line onto `lines`, muted and fit to `width`: the tail
/// half of [`dog_footer_text`], shared the same way the reservation half is.
fn push_footer_line(
    lines: &mut Vec<Line<'static>>,
    footer: Option<String>,
    width: u16,
    palette: Palette,
) {
    if let Some(text) = footer {
        lines.push(Line::from(Span::styled(
            format!("  {}", fit(&text, body_width(width))),
            palette.muted(),
        )));
    }
}

/// Every line of the pane, top to bottom, laid out for a terminal `height`
/// rows tall.
///
/// `height` counts lines, and no more than that ever come back. Zero means
/// unlimited, which is what a test with no terminal behind it gets. See
/// [`super::scroll`] for why the viewport's offset is a starting point
/// here, not an answer.
///
/// The close dialog is not drawn here: it overlays the whole field list
/// (`draw_pane`), rather than taking the one line under the title the
/// apply menu this pane replaced used to.
#[must_use]
pub fn pane_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let budget = if height == 0 {
        usize::MAX
    } else {
        usize::from(height)
    };
    if budget == 0 {
        return Vec::new();
    }
    if let Some(list) = pane.list() {
        return list_lines(pane, list, palette, width, budget);
    }
    if has_groups(pane) {
        if let Some(panel_w) = panel_width(width) {
            return grouped_pane_with_panel_lines(pane, palette, width, panel_w, budget);
        }
        return grouped_pane_lines(pane, palette, width, budget);
    }
    let panel = panel_width(width).map(|panel_w| (panel_lines(pane, palette, panel_w), panel_w));
    ungrouped_pane_lines_with_panel(pane, palette, width, budget, panel)
}

/// The body [`pane_lines`] draws for a pane with no groups, which is a
/// dog's own shape: `panel` is [`None`] below [`panel_width`]'s floor, which
/// lays the body out at `width` in one column, and `Some((lines,
/// panel_width))` above it, which lays the body out at `width -
/// panel_width` and merges `lines` beside it.
///
/// One parameter rather than two near-identical functions, the same
/// treatment [`grouped_pane_lines_with_panel`] gives the grouped pair.
///
/// No tab row and no legend: a dog's schema carries no group to name, and
/// adding the panel is not a reason to invent one. The title is laid out at
/// the real `width`, and the trailing "shep publishes..." footer is chrome,
/// reserved out of the budget before the body claims what is left.
fn ungrouped_pane_lines_with_panel(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    panel: Option<(Vec<Line<'static>>, u16)>,
) -> Vec<Line<'static>> {
    let show_lands = lands_fits_beside_panel(width, panel.is_some());
    let left_width = panel
        .as_ref()
        .map_or(width, |(_, panel_width)| width.saturating_sub(*panel_width));
    let mut lines = vec![title_line(pane, palette, width)];
    // The title is unconditional, so the body is laid out against what is
    // left after it. An empty form (unreachable for a sheep, whose schema
    // is a committed file with 40 properties, but a dog answers `--schema`
    // for itself) leaves the title as the whole pane.
    let mut body_budget = budget - 1;
    // The one line a dog pane has that a sheep pane does not: shep does not
    // know what a dog's field costs, so every row's COST cell is empty.
    // Reserved out of the budget before rows are laid out, for the same
    // reason the top line is: a footer appended afterwards is a line
    // nothing counted.
    //
    // Reserved before the BLURB too, not between the blurb and the rows:
    // the blurb's floor below keeps one line back for the cursor's own
    // row, and a footer reserved afterward would take exactly that line.
    // The blurb is the line to lose, since it describes the row rather
    // than being it.
    let footer = dog_footer_text(pane, body_budget);
    if footer.is_some() {
        body_budget -= 1;
    }
    // The selected field's own help text, on the lines under the title.
    // Subtracted from the budget rather than appended, per `body_from`'s
    // own doc on markers. See `top_lines`.
    //
    // `push_wrapped_blurb`'s own floor of `<= 1`, not `== 0`: a long
    // wrapped help must not spend the last line reserved for the cursor's
    // own row.
    push_wrapped_blurb(&mut lines, &mut body_budget, pane, palette, width);
    if !pane.fields().is_empty() && body_budget > 0 {
        let total = pane.rows().len();
        let cursor_row = pane.view().cursor().min(total - 1);
        let body = super::scroll::to_cursor(
            cursor_row,
            pane.view().offset(),
            |offset| {
                body_from(
                    pane,
                    palette,
                    left_width,
                    body_budget,
                    offset,
                    true,
                    show_lands,
                )
            },
            || {
                cursor_only(
                    pane,
                    palette,
                    left_width,
                    body_budget,
                    cursor_row,
                    show_lands,
                )
            },
        );
        lines.extend(match panel {
            Some((panel, _)) => merge_beside_panel(body, panel, left_width, body_budget),
            None => body,
        });
    }
    push_footer_line(&mut lines, footer, width, palette);
    lines
}

/// Lays the body out from field `offset`, spending at most `budget` lines.
///
/// Every line pushed is counted, including section headers, blank
/// separators between them and both markers. The two markers are reserved
/// before a row is admitted rather than appended afterwards, so a height
/// that binds cuts a row instead of the sentence saying a row was cut.
fn body_from(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    offset: usize,
    show_group_headers: bool,
    show_lands: bool,
) -> Attempt {
    let rows = pane.field_rows();
    let total = rows.len();
    // The cursor may be on an env row rather than a field: nothing in
    // this body is selected then, and row 0 stands in so the first
    // attempt (offset 0) always finds it and never scrolls hunting for a
    // row that is not here. See `grouped_body_lines`'s own doc.
    let cursor_row = rows
        .iter()
        .position(|row| Some(*row) == pane.cursor())
        .unwrap_or(0)
        .min(total.saturating_sub(1));
    // The `... N above` marker is inserted at the top once everything under
    // it is laid out, so its line is held back from the very first check.
    let above = usize::from(offset > 0);
    // Whether a row at `index` still leaves room for `need` more lines. The
    // `... N below` marker is only owed when a row follows this one: a row
    // that fills the last line with nothing under it needs no marker.
    let room = |taken: usize, need: usize, index: usize| {
        taken + need + above + usize::from(index + 1 < total) <= budget
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_group: Option<&str> = None;
    // A group's header, and the blank line ahead of it for every group after
    // the first, held here rather than pushed straight away: it is pushed
    // alongside the first row of its group that survives the offset skip,
    // so a window opening in the middle of `control` still says `control`.
    // Never populated when `show_group_headers` is false: the grouped
    // layout's own tab row already names the one group `rows` ever holds,
    // so a second header line here would only cost the tight-height cursor
    // guarantee a line it has no header content to spend.
    let mut pending_header: Vec<Line<'static>> = Vec::new();
    let mut drawn = 0usize;

    for (index, row) in rows.iter().enumerate() {
        let PaneRow::Field(field_index) = *row else {
            continue;
        };
        let group = pane
            .fields()
            .fields()
            .get(field_index)
            .and_then(|field| field.group.as_deref());
        if show_group_headers && current_group != group {
            let mut header = Vec::new();
            if current_group.is_some() {
                header.push(Line::default());
            }
            if let Some(group) = group {
                header.push(section_header(group, palette));
            }
            pending_header = header;
            current_group = group;
        }
        if index < offset {
            continue;
        }
        if !room(lines.len(), pending_header.len() + 1, index) {
            break;
        }
        lines.append(&mut pending_header);
        lines.push(field_line(
            pane,
            field_index,
            pane.cursor() == Some(*row),
            width,
            palette,
            show_lands,
        ));
        drawn += 1;
    }

    // Counted off what this pass actually drew, not off the viewport's own
    // arithmetic: the viewport hides rows against a line budget it cannot
    // see spent, so its answer and this one disagree the moment the chrome
    // costs anything.
    let hidden_below = total.saturating_sub(offset + drawn);
    if hidden_below > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ... {hidden_below} below"),
            palette.muted(),
        )));
    }
    if offset > 0 {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("  ... {offset} above"),
                palette.muted(),
            )),
        );
    }

    Attempt {
        cursor_drawn: drawn > 0 && (offset..offset + drawn).contains(&cursor_row),
        lines,
    }
}

/// The cursor's own row, alone, for a body too short to hold the chrome its
/// group costs.
///
/// The last resort, reached only when every offset down to the cursor's own
/// left it undrawn. A group's first row costs a blank line and a header
/// above it before it may be drawn at all, and the two markers on top of
/// that: four lines for one row, where `view::MIN_HEIGHT` leaves this pane
/// three after its title. A pane that declares a minimum height should draw
/// something at it, and the selected row is the something.
///
/// Markers are added around it while they fit, the cursor's row first: it is
/// the one line this function exists to guarantee.
fn cursor_only(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
    cursor_row: usize,
    show_lands: bool,
) -> Vec<Line<'static>> {
    let rows = pane.field_rows();
    let mut lines = Vec::new();
    if let Some(PaneRow::Field(index)) = rows.get(cursor_row).copied() {
        lines.push(field_line(pane, index, true, width, palette, show_lands));
    }
    let hidden_below = rows.len().saturating_sub(cursor_row + 1);
    if cursor_row > 0 && lines.len() < budget {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("  ... {cursor_row} above"),
                palette.muted(),
            )),
        );
    }
    if hidden_below > 0 && lines.len() < budget {
        lines.push(Line::from(Span::styled(
            format!("  ... {hidden_below} below"),
            palette.muted(),
        )));
    }
    lines
}

/// A row of `prefix` (already at its own fixed width) followed by `text`,
/// clipped to whatever of `width` the prefix leaves: the guard that keeps
/// a live value or a schema-authored sentence from pushing the row past
/// the panel's own column.
fn bounded_row(
    prefix: Span<'static>,
    prefix_cols: u16,
    text: &str,
    text_style: Style,
    width: u16,
) -> Line<'static> {
    let budget = width.saturating_sub(prefix_cols);
    Line::from(vec![
        prefix,
        Span::styled(clipped(text, budget), text_style),
    ])
}

/// The right-hand explanation panel: everything about `field` alone, drawn
/// at `width` columns, clamped to [`PANEL_MAX`] as a defensive floor under
/// a caller that has not already run `width` through [`panel_width`]'s own
/// ladder.
///
/// Six regions, top to bottom, each omitted entirely when its own source is
/// empty rather than drawn with nothing under its heading: the `FOCUSED`
/// chip and the blurb are unconditional (a field always has a key, a kind
/// and a help string), `now`/`default`/`example` show a placeholder rather
/// than disappearing since `now` is never empty, the impact tag is omitted
/// for a dog (whose own binary decides what a change costs, not this
/// pane), and `VALIDATION`/`NEIGHBOURS` are each omitted on their own when
/// [`validation::bullets`] or [`Field::neighbours`] has nothing to say.
pub(super) fn panel_for_field(
    field: &Field,
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
) -> Vec<Line<'static>> {
    let width = width.min(PANEL_MAX);
    let mut lines = Vec::new();

    // 1: the FOCUSED chip, the field name, its type.
    lines.push(Line::from(vec![
        Span::styled(" FOCUSED ", palette.band(Role::Butter)),
        Span::raw(format!(" {}", field.key)),
        Span::styled(format!("  {}", type_label(field)), palette.muted()),
    ]));

    // 2: the blurb, wrapped. The leading two-space indent below is part of
    // the row's own width, so the wrap budget is `width` minus those two
    // columns, not `width` itself: at the design target's fixed 72 columns
    // `BLURB_WRAP` (66) left enough headroom that this never showed, but
    // the ladder's own floor of 50 does not.
    for row in wrap(
        &field.help,
        usize::from(BLURB_WRAP.min(width.saturating_sub(2))),
    ) {
        lines.push(Line::from(Span::raw(format!("  {row}"))));
    }

    // 3: now, default, example. `now` is never empty; the other two show a
    // placeholder rather than dropping their own row, so the panel always
    // names all three questions even when the schema answers only one.
    // `now` is masked the same way `field_line` masks its own value cell:
    // it is `pane.display_value`, the live config, and a dog's schema can
    // mark that secret. `default` and `example` are never masked: both come
    // from the schema itself (`init.default`/`init.example`), authored by
    // whoever wrote the schema, not by an operator, so neither can carry a
    // value this pane owes any secrecy to.
    lines.push(Line::default());
    let now = mask_secret(field.secret, pane.display_value(&field.key));
    let default = field.default.clone().unwrap_or_else(|| "(none)".to_owned());
    let example = field.example.clone().unwrap_or_else(|| "(none)".to_owned());
    for (label, value) in [("now", now), ("default", default), ("example", example)] {
        let prefix = format!("  {label:<8}");
        let prefix_cols = u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX);
        lines.push(bounded_row(
            Span::styled(prefix, palette.muted()),
            prefix_cols,
            &value,
            Style::default(),
            width,
        ));
    }

    // 4: the impact tag and its sentence, omitted for a dog.
    if let Some(group) = pane.cost(&field.key) {
        let (glyph, word, sentence) = impact_tag(group);
        lines.push(Line::default());
        let prefix = format!("  {glyph} {word}  ");
        let prefix_cols = u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX);
        lines.push(bounded_row(
            Span::raw(prefix),
            prefix_cols,
            sentence,
            Style::default(),
            width,
        ));
        let hint = "pick the timing when you close the pane";
        let hint_cols = u16::try_from(hint.chars().count()).unwrap_or(width);
        let pad = width.saturating_sub(hint_cols);
        lines.push(Line::from(Span::raw(format!(
            "{}{hint}",
            " ".repeat(usize::from(pad))
        ))));
    }

    // 5: VALIDATION, omitted when the field has nothing accepted or refused.
    let bullets = validation::bullets(field);
    if !bullets.is_empty() {
        lines.push(Line::default());
        lines.push(hairline_line(palette, width));
        lines.push(Line::from(Span::styled("  VALIDATION", palette.muted())));
        for text in &bullets.accepts {
            lines.push(bounded_row(
                Span::styled("  \u{2588} ", palette.band(Role::Meadow)),
                4,
                text,
                Style::default(),
                width,
            ));
        }
        for text in &bullets.refuses {
            lines.push(bounded_row(
                Span::styled("  \u{2588} ", palette.band(Role::Bark)),
                4,
                &format!("refused: {text}"),
                Style::default(),
                width,
            ));
        }
    }

    // 6: NEIGHBOURS, omitted when the field names none.
    if !field.neighbours.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("  NEIGHBOURS", palette.muted())));
        for neighbour in &field.neighbours {
            let prefix = format!("  {}  ", neighbour.field);
            let prefix_cols = u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX);
            lines.push(bounded_row(
                Span::raw(prefix),
                prefix_cols,
                &neighbour.note,
                Style::default(),
                width,
            ));
        }
    }

    lines
}

/// The explanation panel for whichever field the pane's own cursor is on,
/// or nothing when the cursor is not on a field at all (the env rows and
/// the pending-edits section carry no [`Field`] of their own).
///
/// The panel follows the cursor: it describes the row [`ConfigPane::cursor`]
/// names, never the row this call happened to be drawn from, so a caller
/// that redraws on every cursor move sees new content on the same
/// keypress that moved it.
#[must_use]
pub(super) fn panel_lines(pane: &ConfigPane, palette: Palette, width: u16) -> Vec<Line<'static>> {
    let Some(PaneRow::Field(index)) = pane.cursor() else {
        return Vec::new();
    };
    let Some(field) = pane.fields().fields().get(index) else {
        return Vec::new();
    };
    panel_for_field(field, pane, palette, width)
}

/// Draws the pane into `area`, straight into `buffer`.
pub fn draw_pane(app: &App, pane: &ConfigPane, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = pane_lines(pane, app.palette(), area.width, area.height);
    for (offset, line) in lines.iter().enumerate().take(usize::from(area.height)) {
        let offset = u16::try_from(offset).unwrap_or(0);
        buffer.set_line(area.x, area.y + offset, line, area.width);
    }
    if let Some(dialog) = app.close_dialog() {
        // The pane draws first and is then muted whole, so 1e's own render
        // is untouched and its four pinned snapshots do not move.
        overlay::mute(buffer, area, app.palette());
        draw_close_dialog(dialog, app.palette(), app.now(), area, buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::layout::{FULL_WIDTH, LANDS_WITH_PANEL_MIN};

    use super::super::MIN_TERM_WIDTH;
    use super::super::fixtures;
    use super::fixtures::{
        marked, pane_to, screen_at, secret_dog_pane_with_an_edit, text_of, web_pane,
    };
    use super::*;
    use crate::lookout::app::{Effect, KeyPress, Msg};
    use crate::output::width::visible_width;

    /// The whole pane at a comfortable width, unbounded. The snapshot is the
    /// assertion: it pins the title, the four section headers in order, all
    /// 40 rows, the two flags and the cost cell beside each one.
    #[test]
    fn a_sheep_pane_at_a_comfortable_width() {
        let lines = pane_lines(&web_pane(), fixtures::plain(), 120, 0);
        insta::assert_snapshot!("sheep_pane_wide", text_of(&lines).join("\n"));
    }

    #[test]
    fn a_sheep_pane_scrolled_to_the_last_field_of_a_group_shows_it() {
        // `cron` has only two fields, both of which always fit; `process`,
        // the group a fresh pane opens on, has ten, which is what forces
        // the scroll this test is about.
        let mut pane = web_pane();
        pane.set_rows(8);
        // Not `move_to_last`: that now lands on `+ add a key`, past every
        // field. `move_to_key` reaches `user`, `process`'s own last field,
        // directly.
        pane.move_to_key("user");
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 9));
        assert!(text.len() <= 9, "{text:?}");
        assert!(text.iter().any(|line| line.contains("above")), "{text:?}");
        assert!(
            text.iter().any(|line| line.contains("user")),
            "process's last field is visible: {text:?}"
        );
        assert!(!text.iter().any(|line| line.contains("below")), "{text:?}");
    }

    /// `Buffer::set_line` clips in silence, so an overrun renders as a
    /// truncated cost cell with nothing saying it was cut.
    #[test]
    fn every_pane_line_fits_the_width_it_was_drawn_for() {
        let pane = web_pane();
        for width in super::super::MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// Guards against the class of bug where chrome eats the budget and
    /// the selected row is never drawn, while every static frame still
    /// looks right. Six is `view::MIN_HEIGHT`; three lines of body under
    /// the title is less than a group's first row costs, so those steps
    /// go through `cursor_only`.
    #[test]
    fn the_cursor_survives_every_step_of_a_walk_down_and_back_up() {
        for height in [6u16, 7, 8, 10, 14, 20, 45] {
            let mut app = fixtures::app_in_sheep_pane();
            let total = app.config_pane().unwrap().rows().len();
            for step in 0..=total {
                let text = screen_at(&mut app, height);
                assert_eq!(marked(&text), 1, "{height} rows, {step} down:\n{text}");
                app.update(Msg::Key(KeyPress::SelectDown));
            }
            for step in 0..=total {
                let text = screen_at(&mut app, height);
                assert_eq!(marked(&text), 1, "{height} rows, {step} up:\n{text}");
                app.update(Msg::Key(KeyPress::SelectUp));
            }
        }
    }

    /// A roomy terminal (`view::ROOMY_HEIGHT` and up) spends a blank row
    /// under the title. `body_rows` has to know about that row, or the
    /// `Rect` it hands the pane reaches one row past the status bar and the
    /// last row the pane draws, here the cursor's own row after jumping to
    /// the last field, gets overwritten rather than shown. Caught the
    /// scrolled offset itself agreeing on a stale, too-tall budget: `marked`
    /// went to 0 at exactly `ROOMY_HEIGHT` and the row above it, with a
    /// `body_rows` that did not subtract the blank row.
    #[test]
    fn the_cursor_survives_a_jump_to_the_last_field_at_a_roomy_height() {
        // 30 is `view::ROOMY_HEIGHT`, private to that module.
        for height in [24u16, 29, 30, 31, 45] {
            let mut app = fixtures::app_in_sheep_pane();
            app.update(Msg::Key(KeyPress::SelectLast));
            let text = screen_at(&mut app, height);
            assert_eq!(marked(&text), 1, "{height} rows:\n{text}");
        }
    }

    /// The marker that says rows were cut would itself become the row
    /// that gets cut.
    ///
    /// Both widths, because they take different code. 120 has a panel, so
    /// every height walks `grouped_pane_lines_with_panel` with `top_lines`
    /// returning empty and never reaches the blurb loop's own arithmetic.
    /// 89 is one column under `panel_width`'s floor, so the blurb draws and
    /// the sweep crosses `remaining` entering that loop at 0, 1 and 2
    /// without having to name which height produces which value.
    ///
    /// One test over a width list, not two copies of it: they were twelve
    /// identical lines apart from the literal, and the assertion message
    /// names the width so a failure still says which case broke.
    ///
    /// What this does NOT see is a row too FEW, since the bound is an upper
    /// one and losing the cursor's row only shortens the output.
    /// `the_blurb_never_spends_the_row_the_cursor_needs` is that half, and
    /// it exists because a real defect hid in this gap.
    #[test]
    fn the_body_never_outgrows_the_height_it_was_given() {
        let mut pane = web_pane();
        for width in [120u16, 89] {
            for height in 1..=60u16 {
                pane.set_rows(usize::from(height.saturating_sub(1)));
                for cursor in [0usize, 7, 20, 38] {
                    pane.move_to_first();
                    pane.move_by(isize::try_from(cursor).unwrap());
                    let text = text_of(&pane_lines(&pane, fixtures::plain(), width, height));
                    assert!(
                        text.len() <= usize::from(height),
                        "width {width}, height {height}, cursor {cursor}: {text:?}"
                    );
                }
            }
        }
    }

    /// `push_wrapped_blurb` breaks on a budget of `<= 1` rather than
    /// `== 0`, so a long wrapped help must not spend the last line the
    /// cursor's own row needs. Nothing pinned it before the helper existed:
    /// the height sweep above passes with the floor at either value,
    /// because losing the field row only makes the output shorter and that
    /// assertion is an upper bound.
    ///
    /// So this asserts the pair instead: wherever the blurb reached the
    /// screen, the cursor's own row did too. Both pane shapes, grouped and
    /// ungrouped, even though one function now serves both, because this is
    /// the end-to-end check through the real `pane_lines` entry point
    /// rather than a check of the helper alone.
    ///
    /// The final count is the vacuity guard: a blurb that never drew would
    /// skip every height and pass.
    #[test]
    fn the_blurb_never_spends_the_row_the_cursor_needs() {
        for (which, mut pane, key) in [
            ("grouped", web_pane(), "autorestart"),
            ("ungrouped", bark_pane(), "history_bytes"),
        ] {
            pane.move_to_key(key);
            let anchor = blurb_anchor(&pane);
            let mut checked = 0;
            for height in 1..=20u16 {
                let rows = text_of(&pane_lines(&pane, fixtures::plain(), 89, height));
                if !rows.iter().any(|row| row.contains(&anchor)) {
                    continue;
                }
                checked += 1;
                // Excludes the blurb row itself: a help string that ever
                // came to mention its own field's name would let a cut
                // cursor row hide behind the blurb row satisfying `key` by
                // coincidence.
                assert!(
                    rows.iter()
                        .any(|row| row.contains(key) && !row.contains(&anchor)),
                    "{which} at height {height}: the blurb drew and the cursor's row did not: {rows:?}"
                );
            }
            assert!(
                checked > 0,
                "{which}: the blurb never drew, so nothing was checked"
            );
        }
    }

    /// Nothing is armed and nothing is in flight, so the slot under the
    /// title carries no question. A filed edit shows in its own row's
    /// value cell, which is where the operator is already looking.
    #[test]
    fn a_filed_edit_draws_no_question_under_the_title() {
        let mut pane = web_pane();
        pane.move_to_key("autorestart");
        pane.cycle();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        assert!(
            !text.iter().any(|line| line.contains("enter confirms")),
            "{text:?}"
        );
        assert!(
            !text.iter().any(|line| line.contains("set autorestart")),
            "{text:?}"
        );
    }

    /// At a width with no explanation panel, the field under the cursor
    /// still has its help text on screen, with no key pressed.
    ///
    /// 89 columns is the widest terminal `panel_width` refuses: the panel
    /// clamps to `PANEL_MIN` 50 and 89 - 50 is 39, one short of `LEFT_MIN`.
    ///
    /// `contains` cannot see indentation drift, so this compares against
    /// the header row's own margin instead of a literal 2, since every
    /// row in the pane shares one.
    #[test]
    fn the_blurb_shares_the_panes_own_indent() {
        let pane = web_pane();
        let lines = pane_lines(&pane, fixtures::plain(), 89, 40);
        let rows = text_of(&lines);
        let indent = |row: &str| row.len() - row.trim_start().len();
        let header = rows
            .iter()
            .find(|row| row.contains("FIELD") && row.contains("VALUE"))
            .expect("no column header");
        let anchor = blurb_anchor(&pane);
        let blurb = rows
            .iter()
            .find(|row| row.contains(&anchor))
            .expect("no blurb row");
        assert_eq!(
            indent(blurb),
            indent(header),
            "blurb {:?} against header {:?}",
            blurb.get(..12),
            header.get(..12)
        );
    }

    #[test]
    fn the_blurb_draws_at_a_width_with_no_panel() {
        let pane = web_pane();
        assert!(panel_width(89).is_none(), "89 must have no panel");
        let lines = pane_lines(&pane, fixtures::plain(), 89, 40);
        let anchor = blurb_anchor(&pane);
        let rows = text_of(&lines);
        assert!(
            rows.iter().any(|row| row.contains(&anchor)),
            "no blurb at 89 columns: {rows:?}"
        );
    }

    /// And it describes the row the cursor is on, not the first field.
    #[test]
    fn the_blurb_follows_the_cursor_with_no_panel() {
        let mut pane = web_pane();
        let first = blurb_anchor(&pane);
        pane.move_by(1);
        let second = blurb_anchor(&pane);
        // On the anchors, not the help strings: two fields can have
        // different help and share a longest word, and then the absence
        // assertion below cannot fail. Guarding the help alone would look
        // like it covered this.
        //
        // Substring, not just inequality: "memory" != "max_memory" passes
        // `assert_ne!` while `second`'s own row still contains `first`,
        // which would fail the absence assertion below on a fixture
        // mismatch rather than a real regression.
        assert!(
            !first.contains(&second) && !second.contains(&first),
            "the fixture needs two fields whose longest help words are not substrings of each other: {first:?} / {second:?}"
        );
        let lines = pane_lines(&pane, fixtures::plain(), 89, 40);
        let rows = text_of(&lines);
        // The specific blurb row, not the whole screen: `first` scanned
        // against every row would false-fail on a coincidental substring
        // in an unrelated one, a field name or a value cell.
        let blurb_row = rows
            .iter()
            .find(|row| row.contains(&second))
            .unwrap_or_else(|| panic!("the cursor moved and the blurb did not: {rows:?}"));
        assert!(
            !blurb_row.contains(&first),
            "the previous field's blurb is still on screen: {rows:?}"
        );
    }

    /// The longest word in the cursor's field help, which is what the blurb
    /// tests match on.
    ///
    /// Not the whole help string: the blurb wraps to `BLURB_WRAP`, so a
    /// help text longer than the wrap budget appears in no single row and a
    /// `contains` against all of it fails for a reason unrelated to what
    /// these tests pin. Not the first word either, since "Set" or "The"
    /// appears in other rows. The longest word is the one least likely to
    /// be split by a wrap or shared with another row.
    fn blurb_anchor(pane: &ConfigPane) -> String {
        let help = field_help_under_cursor(pane);
        help.split_whitespace()
            .max_by_key(|word| word.len())
            .expect("the field help is empty")
            .to_owned()
    }

    /// The `help` string of the field under the cursor, whichever it is.
    fn field_help_under_cursor(pane: &ConfigPane) -> String {
        let Some(PaneRow::Field(index)) = pane.cursor() else {
            panic!("the cursor is not on a field");
        };
        pane.fields().fields()[index].help.clone()
    }

    /// The hard constraint this item's brief calls out: the blurb rows
    /// under the title are counted against the same budget every other
    /// line in the pane is, at every width and height the pane claims to
    /// draw at.
    ///
    /// "A line drawn into the fixed slot" until 072b6ff8: the slot held one
    /// line while `h` toggled it, and holds as many as the help text wraps
    /// to now that it is unconditional.
    #[test]
    fn help_text_still_respects_the_width_and_height_budgets() {
        let mut pane = web_pane();
        pane.move_to_key("max_memory");
        for width in super::super::MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width}: {line:?}"
                );
            }
        }
        for height in 1..=30u16 {
            let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, height));
            assert!(
                text.len() <= usize::from(height),
                "height {height}: {text:?}"
            );
        }
    }

    /// Both directions: `watch` is `Live`, draws `now`, and can still
    /// park. `autostart` is `NextSpawn`, draws `next start`, and takes
    /// effect at muster, not at the next spawn. The column is never
    /// corrected after a reply, since a reply covers one row of
    /// forty; it stays a prediction everywhere, the bar reports the
    /// outcome, and the row's `!` flag carries it afterwards.
    #[test]
    fn the_cost_column_predicts_and_the_status_bar_reports() {
        for (key, column, pending, sentence) in [
            ("watch", "now", true, "waits for `shep reload web`"),
            ("autostart", "next start", false, "set to false"),
        ] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);

            // Below the panel's own floor: the panel suppresses the row's own
            // COST cell, which is exactly what this test reads.
            let text = text_of(&pane_lines(
                app.config_pane().unwrap(),
                fixtures::plain(),
                89,
                0,
            ));
            // A group header line is bare, just its name; a field row always
            // carries a value and a cost cell after the key, so the header
            // never matches once a second token is required. `key` can equal
            // its own group's name (the `watch` field, the `watch` group).
            let row = text
                .iter()
                .find(|line| {
                    let mut words = line.split_whitespace();
                    let mut rest = line.get(3..).map(str::split_whitespace);
                    (words.next() == Some(key) && words.next().is_some())
                        || rest
                            .as_mut()
                            .is_some_and(|w| w.next() == Some(key) && w.next().is_some())
                })
                .unwrap_or_else(|| panic!("{key} is drawn at 89 columns: {text:?}"));
            assert!(row.contains(column), "{key}: {row:?}");

            app.update(Msg::Key(KeyPress::Cycle));
            // `app_in_sheep_pane_with_control` parks `kill_signal`
            // unconditionally, so `esc` only asks; `c` is what actually
            // gets the write onto the wire.
            let _ = app.update(Msg::Key(KeyPress::Escape));
            let effect = if app.close_dialog().is_some() {
                app.update(Msg::Key(KeyPress::Continue))
            } else {
                Effect::None
            };
            let Effect::SendAll(mut batch) = effect else {
                panic!("{key}: closing the pane sends");
            };
            app.update(Msg::Replied {
                sent: batch.remove(0),
                result: Ok(shep_core::protocol::Response::SheepFieldSet {
                    name: "web".to_string(),
                    key: key.to_string(),
                    pending,
                    warning: None,
                }),
            });
            let bar = crate::lookout::view::status::status_line(&app, 200).to_string();
            assert!(bar.contains(sentence), "{key}: {bar:?}");
        }
    }

    /// The only place on screen that says which field the buffer belongs
    /// to.
    #[test]
    fn an_open_editor_draws_its_buffer_in_the_fields_own_row() {
        let mut pane = web_pane();
        pane.move_to_key("cwd");
        pane.begin_typing();
        for typed in "/srv".chars() {
            pane.type_char(typed);
        }
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        let row = text
            .iter()
            .find(|line| line.contains(" cwd"))
            .expect("cwd is drawn at 120 columns");
        assert!(row.contains("/srv\u{258f}"), "{row:?}");
        assert!(
            !text.iter().any(|line| line.contains("set cwd")),
            "an editor is not a confirm: {text:?}"
        );
    }

    /// A dog pane over the bark dog, with a sink in its section.
    fn bark_pane() -> ConfigPane {
        let schema = crate::dog::builtin_schema("bark").expect("bark is a built-in");
        ConfigPane::dog(
            "bark".into(),
            None,
            schema,
            "poll = \"60s\"\nhistory_bytes = 4096\n\n[sinks.ops]\nkind = \"slack\"\nurl = \"https://hooks.example/x\"\n"
                .into(),
        )
    }

    /// The whole dog pane at a comfortable width. The snapshot is the
    /// assertion: the title says `dog config`, the rows are flat with no
    /// section headers, every COST cell is empty, and the foot says once
    /// that the dog decides.
    ///
    /// The three assertions ahead of it fail loudly rather than as a
    /// snapshot diff: a webhook URL on screen is the leak the whole secret
    /// contract exists to prevent.
    #[test]
    fn a_dog_pane_at_a_comfortable_width() {
        let text = text_of(&pane_lines(&bark_pane(), fixtures::plain(), 120, 0));
        assert!(
            !text.iter().any(|line| line.contains("hooks.example")),
            "a secret is never rendered: {text:?}"
        );
        assert!(text.iter().any(|line| line.contains("<set>")), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.contains("decides what to reload")),
            "{text:?}"
        );
        insta::assert_snapshot!("dog_pane_wide", text.join("\n"));
    }

    #[test]
    fn a_dog_panes_footer_is_paid_for_out_of_the_height_it_was_given() {
        let mut pane = bark_pane();
        for height in 1..=20u16 {
            pane.set_rows(usize::from(height.saturating_sub(1)));
            for cursor in [0usize, 2, 4] {
                pane.move_to_first();
                pane.move_by(isize::try_from(cursor).unwrap());
                let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, height));
                assert!(
                    text.len() <= usize::from(height),
                    "height {height}, cursor {cursor}: {text:?}"
                );
            }
        }
    }

    /// The footer is the newest line and the longest, and
    /// `Buffer::set_line` clips in silence.
    #[test]
    fn every_dog_pane_line_fits_the_width_it_was_drawn_for() {
        let pane = bark_pane();
        for width in super::super::MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// `panel_for_field`'s `now` row is the panel's own live-config read,
    /// same as `field_line`'s value cell, and has to mask a secret the same
    /// way. Cursor left off `token` on purpose: every new test in the
    /// previous round left it on field 0, which is why the panel's `now`
    /// row leaked a real webhook URL and nothing caught it.
    #[test]
    fn the_explanation_panel_masks_a_secret_fields_value() {
        let pane = secret_dog_pane_with_an_edit();
        let field = pane.fields().by_key("token").expect("token is a field");
        let lines = panel_for_field(field, &pane, fixtures::plain(), 120);
        let text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            !text.contains("ab12cd34"),
            "the current value leaked: {text}"
        );
        assert!(text.contains("<set>"), "{text}");
    }

    #[test]
    fn the_panel_describes_the_focused_field_only() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        assert!(panel.iter().any(|row| row.contains("cwd")), "{panel:?}");
        assert!(
            !panel.iter().any(|row| row.contains("kill_timeout")),
            "the panel is showing a field that is not focused: {panel:?}"
        );
    }

    #[test]
    fn the_panel_names_now_default_and_example() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        for label in ["now", "default", "example"] {
            assert!(
                panel.iter().any(|row| row.trim_start().starts_with(label)),
                "no {label} row: {panel:?}"
            );
        }
    }

    /// The panel follows the cursor, and does so on the keypress that moves
    /// it rather than on the next redraw.
    #[test]
    fn the_panel_follows_the_selection() {
        let mut app = fixtures::app_in_sheep_pane();
        let first = fixtures::config_pane_panel_for_tests(&app, 160);
        app.update(Msg::Key(KeyPress::SelectDown));
        let second = fixtures::config_pane_panel_for_tests(&app, 160);
        assert_ne!(first, second);
    }

    #[test]
    fn a_duration_field_takes_its_bullets_from_the_type_table() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::Group(4)));
        let panel = fixtures::config_pane_panel_focused_on(&app, "min_uptime", 160);
        assert!(
            panel.iter().any(|row| row.contains("milliseconds")),
            "{panel:?}"
        );
    }

    /// A field with no accepted forms and no neighbours renders no heading,
    /// not an empty one. Same rule as the detail pane's cfg cell.
    #[test]
    fn a_field_with_nothing_to_say_renders_no_headings() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_focused_on(&app, "fold", 160);
        assert!(
            !panel.iter().any(|row| row.contains("VALIDATION")),
            "{panel:?}"
        );
        assert!(
            !panel.iter().any(|row| row.contains("NEIGHBOURS")),
            "{panel:?}"
        );
    }

    /// Colour is never the only carrier: a refused form has to read as
    /// refused with every colour stripped.
    ///
    /// `name` rather than `cwd`: `cwd`'s own "cannot enter" claim moved to
    /// an accepted form once a bad `cwd` started earning a warning instead
    /// of a refusal (`SetSheepField`'s `warning`), and `name`'s refusals are
    /// still a hard `normalize` rule with nothing advisory about them.
    #[test]
    fn a_refused_form_reads_as_refused_without_colour() {
        let mut app = fixtures::app_with_plain_palette_in_sheep_pane();
        fixtures::select_field(&mut app, "name");
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        let refusal = panel
            .iter()
            .find(|row| row.contains("path separator"))
            .expect("name states a refusal");
        assert!(refusal.contains("refused"), "{refusal}");
    }

    #[test]
    fn the_blurb_wraps_rather_than_truncating() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        assert!(
            panel.iter().all(|row| row.chars().count() <= 72),
            "a panel row overflows its column: {panel:?}"
        );
        assert!(panel.iter().filter(|row| !row.trim().is_empty()).count() > 3);
    }

    /// Nothing on the live screen drew the panel, because nothing called
    /// it outside a test fixture. This pins the wiring itself, through the
    /// same [`pane_lines`] the real draw path calls, not through a
    /// fixture built to reach [`panel_lines`] directly.
    ///
    /// The panel draws across a range, from the design target down to
    /// [`panel_width`]'s own floor at 90 columns, not only at the design
    /// target. What this pins is the floor: nothing below it.
    #[test]
    fn the_panel_draws_beside_the_field_list_at_the_design_target() {
        let app = fixtures::app_in_sheep_pane();
        let pane = app.config_pane().expect("the pane is open");
        let at_target = text_of(&pane_lines(pane, fixtures::plain(), 160, 48));
        assert!(
            at_target.iter().any(|line| line.contains("FOCUSED")),
            "the panel never drew at the design target: {at_target:?}"
        );
        let below_floor = text_of(&pane_lines(pane, fixtures::plain(), 89, 48));
        assert!(
            !below_floor.iter().any(|line| line.contains("FOCUSED")),
            "the panel must not draw below its own floor: {below_floor:?}"
        );
    }

    /// No row may cross the panel boundary or run past the terminal it was
    /// drawn for, at the design target and at a terminal wider than it: the
    /// chrome now stretches to fill a wide terminal instead of capping at
    /// 160 with blank space to the right, so the ceiling this checks
    /// against is `width` itself, not a fixed 160.
    #[test]
    fn no_row_spills_across_the_panel_boundary_or_past_its_own_width() {
        let app = fixtures::app_in_sheep_pane();
        let pane = app.config_pane().expect("the pane is open");
        for width in [160, 200] {
            for line in pane_lines(pane, fixtures::plain(), width, 48) {
                let cols = line_columns(&line);
                assert!(
                    cols <= usize::from(width),
                    "a row is {cols} columns wide at terminal width {width}: {line:?}"
                );
            }
        }
    }

    /// A dog pane has no groups, so it takes `pane_lines`'s other branch,
    /// which got the panel after the grouped one did and is the branch that
    /// went without it for a while. Same layout, same panel, its empty
    /// regions simply empty.
    #[test]
    fn a_dogs_panel_draws_beside_its_field_list_at_the_design_target() {
        let app = fixtures::app_in_dog_pane();
        assert!(
            fixtures::config_pane_draws_a_panel(&app, 160),
            "the panel never drew for a dog at the design target"
        );
        assert!(
            !fixtures::config_pane_draws_a_panel(&app, 89),
            "the panel must not draw below its own floor"
        );
    }

    /// A dog's `cost` is always `None`, so its impact region is empty: no
    /// sentence, and no reverse-video tag rendered with nothing to say.
    /// Nothing rather than a zero, the same rule a field with no
    /// validation bullets already follows.
    #[test]
    fn a_dogs_panel_has_no_impact_region() {
        let app = fixtures::app_in_dog_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        assert!(
            panel.iter().any(|line| line.contains("FOCUSED")),
            "the panel must actually be drawn for its own absence to mean anything: {panel:?}"
        );
        assert!(
            !panel
                .iter()
                .any(|line| line.contains("pick the timing when you close the pane")),
            "a dog's cost is always None, so no impact sentence should render: {panel:?}"
        );
    }

    /// A dog's schema carries no group, so `pane_lines` draws no tab row
    /// for it even beside the panel.
    #[test]
    fn a_dogs_panel_layout_draws_no_tab_row() {
        let app = fixtures::app_in_dog_pane();
        assert!(
            fixtures::config_pane_draws_a_panel(&app, 160),
            "the panel must actually be drawn for its own absence to mean anything"
        );
        assert!(
            !fixtures::config_pane_draws_a_tab_row(&app, 160),
            "a dog has no groups to tab through"
        );
    }

    // --- the responsive ladder: what each width drops ---

    /// [`pane_lines`]'s own rows for the sheep [`fixtures::app_in_sheep_pane`]
    /// opens, rendered plain: the config pane's own equivalent of
    /// [`fixtures::draw_lines`], which draws the bleats pane instead.
    fn config_pane_lines_for_tests(app: &App, width: u16, height: u16) -> Vec<Line<'static>> {
        let pane = app.config_pane().expect("the pane is open");
        pane_lines(pane, fixtures::plain(), width, height)
    }

    /// The drop order the whole ladder rests on: where both the panel and
    /// `LANDS` fit, both draw; where they cannot, `LANDS` gives way first.
    /// Corrected 2026-09-11, reversing this test's own original name and
    /// claim, which asserted the two were never both present. Asserted at
    /// every width rather than at three of them, through the real render
    /// path ([`pane_lines`]/[`config_pane_lines_for_tests`]) rather than
    /// [`widths`] called directly: `widths`'s own `show_lands` parameter is
    /// wired from the caller's own [`lands_fits_beside_panel`] state, so
    /// calling it directly with a fixed `show_lands` cannot see whether
    /// that wiring is actually in place.
    #[test]
    fn lands_gives_way_to_the_panel_below_the_design_target_and_joins_it_above() {
        let app = fixtures::app_in_sheep_pane();
        for width in MIN_TERM_WIDTH..=200 {
            let rows = fixtures::render_all(&config_pane_lines_for_tests(&app, width, 48));
            let panel = rows.contains("FOCUSED");
            let lands = rows.contains("LANDS");
            let has_panel = panel_width(width).is_some();
            match (has_panel, width >= LANDS_WITH_PANEL_MIN) {
                // 160 and up: the panel and the column share the row.
                (true, true) => assert!(
                    panel && lands,
                    "the panel and LANDS must both draw at {width} columns"
                ),
                // 90 to 159: the panel draws, LANDS gives way to it.
                (true, false) => assert!(
                    panel && !lands,
                    "LANDS must give way to the panel at {width} columns"
                ),
                // below 90: no panel, so LANDS carries cost alone, subject
                // to `widths`'s own pre-existing narrow-terminal cascade,
                // which this fix leaves untouched.
                (false, _) => {
                    assert!(!panel, "the panel must not draw at {width} columns");
                    let expected_lands = body_width(width) >= FULL_WIDTH;
                    assert_eq!(
                        lands, expected_lands,
                        "LANDS mismatch at {width} columns, no panel"
                    );
                }
            }
        }
    }

    /// Every width the pane can be drawn at draws inside itself. This is
    /// the same sweep the existing pane tests run and it stays.
    #[test]
    fn every_row_fits_the_width_it_was_drawn_for() {
        for width in MIN_TERM_WIDTH..=200 {
            let app = fixtures::app_in_sheep_pane();
            for row in config_pane_lines_for_tests(&app, width, 48) {
                assert!(
                    fixtures::render_all(std::slice::from_ref(&row))
                        .chars()
                        .count()
                        <= usize::from(width),
                    "a row overflows at {width}"
                );
            }
        }
    }

    #[test]
    fn a_short_body_sheds_the_legend_before_the_tab_row() {
        let app = fixtures::app_in_sheep_pane();
        let rows = fixtures::render_all(&config_pane_lines_for_tests(&app, 160, 6));
        assert!(rows.contains("tab next group"), "the tab row must survive");
        assert!(!rows.contains("changed by you"), "the legend must go first");
    }
}
