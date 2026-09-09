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
use shep_core::config::{ApplyGroup, GROUP_ORDER};

use super::super::app::{App, PaneMenu};
use super::super::pane::{
    ConfigPane, EnvPane, EnvRow, ListPane, ListRow, Lock, PaneEdit, PaneRow, PaneTarget,
};
use super::super::theme::Palette;
use super::cell;
use super::flock::{fit, mark};
use super::scroll::Attempt;
use crate::vocabulary::Role;

/// The columns every line spends on the selection mark and the space after
/// it, before any cell is drawn. [`super::settings::GUTTER`]'s twin, and it
/// exists for the reason that one does: a budget that forgets it is a budget
/// every line overruns.
const GUTTER: u16 = 2;

/// The KEY cell at its full width, flag character included. Twenty-six is
/// `exp_backoff_restart_delay` plus its flag, the longest key the Flockfile
/// schema declares, so no field name is truncated at a width that can
/// afford the whole column.
const KEY_W: u16 = 26;

/// The floor KEY shrinks to before the COST column is dropped instead.
const KEY_MIN: u16 = 8;

/// The floor VALUE shrinks to. Below this the pane drops COST, and below
/// that it draws KEY alone.
const VALUE_MIN: u16 = 8;

/// The position cell on the list sub-screen. Three columns holds an index
/// into an array longer than any Flockfile has, and no more: an element is
/// what the row is about.
const POSITION_W: u16 = 3;

/// The COST cell. Ten columns, which is exactly `next start`, the longest
/// word [`cost_label`] prints.
const COST_W: u16 = 10;

/// The narrowest body that still draws KEY, VALUE and COST.
const FULL_WIDTH: u16 = KEY_W + 2 + VALUE_MIN + 2 + COST_W;

/// The narrowest body that still draws a VALUE beside the KEY.
const VALUE_WIDTH: u16 = KEY_MIN + 2 + VALUE_MIN;

/// The width the rows are laid out in: the terminal minus [`GUTTER`].
const fn body_width(width: u16) -> u16 {
    width.saturating_sub(GUTTER)
}

/// What a change to a field in `group` costs, in an operator's words.
///
/// A prediction from the field's class, not the outcome of a specific
/// write: `watch` says `now` but can park, and `autostart` says `next
/// start` but takes effect at muster. The status bar reports what
/// actually happened; the row's `!` flag is the durable answer.
///
/// [`ApplyGroup`] is `#[non_exhaustive]`, so an untaught field falls back
/// to `respawn`, the most conservative of the four.
const fn cost_label(group: ApplyGroup) -> &'static str {
    match group {
        ApplyGroup::Live => "now",
        ApplyGroup::NextSpawn => "next start",
        ApplyGroup::Structural => "read-only",
        ApplyGroup::NeedsRespawn | _ => "respawn",
    }
}

/// The three cell widths for a body of `width`: KEY, VALUE, COST. A zero
/// means the column is not drawn at all.
///
/// The three always sum, with their two-space separators, to exactly
/// `width`, so no line can overrun the terminal it was laid out for.
/// COST goes first when the terminal narrows: arming a field repeats its
/// cost verbatim in the status bar, which is the same reasoning
/// [`super::settings`] gives for dropping its own cost cell first.
fn widths(width: u16) -> (u16, u16, u16) {
    if width >= FULL_WIDTH {
        let rest = width - COST_W - 2;
        let key = KEY_W.min(rest - VALUE_MIN - 2);
        (key, rest - key - 2, COST_W)
    } else if width >= VALUE_WIDTH {
        let key = KEY_W.min(width - VALUE_MIN - 2);
        (key, width - key - 2, 0)
    } else {
        (width, 0, 0)
    }
}

/// A section header, indented to match every field row's own mark-and-gap
/// prefix, the same as [`super::settings`]'s own.
fn section_header(label: &str, palette: Palette) -> Line<'static> {
    Line::from(Span::styled(format!("  {label}"), palette.muted()))
}

/// The pane's own title: which sheep or dog is being edited.
///
/// The dashboard's title line above this one names `$SHEP_HOME` and
/// nothing else, so this names whose 40 fields are on screen.
///
/// Carries no control-dependent word: what the keys do belongs in the key
/// hint (`view::status::pane_hint`), which already reads the gate.
fn title_line(pane: &ConfigPane, palette: Palette, width: u16) -> Line<'static> {
    let kind = match pane.target() {
        PaneTarget::Sheep { .. } => "sheep config",
        PaneTarget::Dog { .. } => "dog config",
    };
    Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                &format!("{}  ({kind})", pane.target().name()),
                body_width(width)
            )
        ),
        palette.muted(),
    ))
}

/// One field's row: the selection mark, a lock glyph, a flag, the key, the
/// value and what changing it costs.
///
/// The flag matches `shep flock`'s CFG column: `!` for a field parked
/// until a respawn, `*` for one an operator has overridden. Pending wins
/// when both apply, since the value on screen is not what the running
/// child holds.
///
/// The lock is a glyph, not a style, since a style says nothing in
/// `plain`. It sits between the mark and the flag rather than in the cost
/// cell, since cost is the first column [`widths`] drops. See [`Lock`].
///
/// A field with a filed edit shows `old -> new` in the value cell, in
/// butter, and keeps its own flag and lock exactly as computed: `!` and `*`
/// are the shepherd's own words for a different fact, and an edit an
/// operator has not sent yet earns neither.
fn field_line(
    pane: &ConfigPane,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let Some(field) = pane.fields().fields().get(index) else {
        return Line::default();
    };
    let (key_w, value_w, cost_w) = widths(body_width(width));
    let flag = match (pane.is_pending(&field.key), pane.is_overridden(&field.key)) {
        (true, _) => '!',
        (false, true) => '*',
        (false, false) => ' ',
    };
    // A secret's value is never rendered, only whether there is one. The
    // Flockfile schema marks nothing secret today; a dog's own schema can,
    // and this pane draws both. `display_value`, not `value`: a
    // `MemSize`/`UpDuration` field's bare number is resolved for the row,
    // not for whatever an editor would seed.
    let raw = pane.display_value(&field.key);
    // An editor open on this field replaces the cell with what is being
    // typed, cursor included. The same `\u{258f}` every text box in
    // lookout draws; a character rather than a reversed cell because the
    // ANSI gallery renders foregrounds only.
    let typing = pane
        .typing()
        .filter(|typing| typing.key == field.key)
        .map(|typing| &typing.buffer);
    let value = match typing {
        Some(buffer) => format!("{buffer}\u{258f}"),
        None if field.secret && raw != "(unset)" => "<set>".to_owned(),
        None => raw,
    };

    let lock = match pane.lock(&field.key) {
        // Fixed: no surface edits this one, not just this pane.
        Some(Lock::Refused) => '=',
        // Shown only. A Flockfile still writes it, and the cost cell beside
        // it reports what doing so would cost.
        Some(Lock::NoWidget) => '~',
        None => ' ',
    };
    let mut prefix = format!("{}{lock}", mark(selected));
    prefix.push_str(&fit(&format!("{flag}{}", field.key), key_w));
    let cost_cell = (cost_w > 0).then(|| fit(pane.cost(&field.key).map_or("", cost_label), cost_w));

    // A filed edit, with nothing being typed over it right now: the value
    // cell becomes `old -> new` instead of the stored value alone. Never
    // reached for a locked field, since no key files an edit for one, so
    // `field.editable` is not re-checked here.
    let edited = typing
        .is_none()
        .then(|| pane.edited_value(&field.key))
        .flatten();
    let Some(new_value) = edited else {
        let mut text = prefix;
        if value_w > 0 {
            text.push_str("  ");
            text.push_str(&fit(&value, value_w));
        }
        if let Some(cost) = &cost_cell {
            text.push_str("  ");
            text.push_str(cost);
        }
        // Muting reinforces the glyph but carries no fact alone: a `plain`
        // palette renders muted as nothing, so style could not tell a
        // locked row from an editable one on its own.
        return if field.editable {
            Line::from(Span::raw(text))
        } else {
            Line::from(Span::styled(text, palette.muted()))
        };
    };
    let new_value = if field.secret && new_value != "(unset)" {
        "<set>".to_owned()
    } else {
        new_value
    };
    let mut spans = vec![Span::raw(prefix)];
    if value_w > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            fit(&format!("{value} -> {new_value}"), value_w),
            palette.attention(),
        ));
    }
    if let Some(cost) = cost_cell {
        spans.push(Span::raw("  "));
        spans.push(Span::raw(cost));
    }
    Line::from(spans)
}

/// The apply menu's own sentence.
///
/// Says "saved" in its first three words on purpose. Every one of these
/// fields is already in the override store, so this is not a save prompt
/// and must not read as one: leaving costs nothing, and the last clause
/// says that too.
fn menu_text(menu: PaneMenu) -> String {
    let reload = menu.reload().label();
    match menu.parked() {
        1 => format!(
            "1 saved field waits on the running sheep: L reload ({reload}), R restart, esc leave it parked"
        ),
        parked => format!(
            "{parked} saved fields wait on the running sheep: L reload ({reload}), R restart, esc leave them parked"
        ),
    }
}

/// The one line the field list reserves under its title: the apply menu
/// while it is up, else the selected field's own help text while `h` has
/// it open. [`None`] when neither applies.
///
/// The menu outranks help: it is what the operator's next keystroke
/// answers, and help is dismissed by a keystroke of the operator's own
/// choosing, so it can wait for the slot back.
///
/// One line, and one already counted, so a menu costs the field list
/// nothing: [`super::scroll`]'s walk sees the same budget either way.
fn top_line(
    pane: &ConfigPane,
    menu: Option<&PaneMenu>,
    palette: Palette,
) -> Option<(String, Style)> {
    if let Some(menu) = menu {
        return Some((menu_text(*menu), palette.attention()));
    }
    if pane.help_open()
        && let Some(PaneRow::Field(index)) = pane.cursor()
        && let Some(field) = pane.fields().fields().get(index)
    {
        return Some((field.help.clone(), palette.muted()));
    }
    None
}

/// The env sub-screen: the sheep's env key names, and a row to add one on.
///
/// Values are never drawn: `Request::SheepConfig` answers with keys alone,
/// so every key reads `<set>` regardless of what the pane knows. That is a
/// property of the wire, not a truncation, and the title says so.
///
/// Laid out through [`super::scroll::to_cursor`], the same walk the field
/// list uses (one uniform row, no group headers), so the cursor is drawn
/// at every height this pane claims to support.
fn env_lines(
    pane: &ConfigPane,
    env: &EnvPane,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                &format!("{}  env (write-only)", pane.target().name()),
                body_width(width)
            )
        ),
        palette.muted(),
    ))];
    let body_budget = budget - 1;
    if body_budget == 0 {
        return lines;
    }
    let rows = env.rows();
    let cursor_row = env.view().cursor().min(rows.len().saturating_sub(1));
    lines.extend(super::scroll::to_cursor(
        cursor_row,
        env.view().offset(),
        |offset| env_body_from(env, palette, width, body_budget, offset),
        || vec![env_line(env, cursor_row, true, width, palette)],
    ));
    lines
}

/// One row of the env sub-screen: the selection mark, the key, and `<set>`.
///
/// The `+ new` row carries no value cell: there is no key yet for one to
/// belong to.
fn env_line(
    env: &EnvPane,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let (key_w, value_w, _) = widths(body_width(width));
    let typed = selected
        .then(|| env.typing())
        .flatten()
        .map(|(_, buffer)| buffer);
    let (key, value) = match env.rows().get(index).copied() {
        Some(EnvRow::Key(key_index)) => (
            env.keys().get(key_index).cloned().unwrap_or_default(),
            typed.map_or_else(|| "<set>".to_owned(), |buffer| format!("{buffer}\u{258f}")),
        ),
        // On `+ new` the buffer is the whole `KEY=value`, so it replaces
        // the key cell rather than the value cell.
        Some(EnvRow::New) => match typed {
            Some(buffer) => (format!("{buffer}\u{258f}"), String::new()),
            None => ("+ new".to_owned(), String::new()),
        },
        None => return Line::default(),
    };
    let mut text = format!("{} ", mark(selected));
    text.push_str(&fit(&key, key_w));
    if value_w > 0 && !value.is_empty() {
        text.push_str("  ");
        text.push_str(&fit(&value, value_w));
    }
    if matches!(env.rows().get(index), Some(EnvRow::New)) {
        return Line::from(Span::styled(text, palette.muted()));
    }
    Line::from(Span::raw(text))
}

/// Lays the sub-screen's body out from row `offset`, spending at most
/// `budget` lines. Both markers are reserved before a row is admitted, the
/// same rule [`body_from`] follows.
fn env_body_from(
    env: &EnvPane,
    palette: Palette,
    width: u16,
    budget: usize,
    offset: usize,
) -> Attempt {
    let rows = env.rows();
    let total = rows.len();
    let cursor_row = env.view().cursor().min(total.saturating_sub(1));
    let above = usize::from(offset > 0);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut drawn = 0usize;
    for index in offset..total {
        if lines.len() + 1 + above + usize::from(index + 1 < total) > budget {
            break;
        }
        lines.push(env_line(env, index, index == cursor_row, width, palette));
        drawn += 1;
    }
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

/// The list sub-screen: one array field's elements, and a row to add one on.
///
/// Values are drawn, unlike [`env_lines`]: an array arrives with the
/// config, so hiding it would leave the screen unable to say which element
/// the cursor is on.
///
/// Laid out through [`super::scroll::to_cursor`], the same walk the field
/// list uses, so the cursor is drawn at every height this pane claims to
/// support.
fn list_lines(
    pane: &ConfigPane,
    list: &ListPane,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                &format!("{}  {} (list)", pane.target().name(), list.key()),
                body_width(width)
            )
        ),
        palette.muted(),
    ))];
    let body_budget = budget - 1;
    if body_budget == 0 {
        return lines;
    }
    let secret = pane.fields().by_key(list.key()).is_some_and(|f| f.secret);
    let rows = list.rows();
    let cursor_row = list.view().cursor().min(rows.len().saturating_sub(1));
    lines.extend(super::scroll::to_cursor(
        cursor_row,
        list.view().offset(),
        |offset| list_body_from(list, palette, width, body_budget, offset, secret),
        || vec![list_line(list, cursor_row, true, width, palette, secret)],
    ));
    lines
}

/// One row of the list sub-screen: the selection mark, the element's
/// position, and the element.
///
/// The position is drawn because `K` and `J` move an element by one, so a
/// row that did not say where it was would leave an operator counting.
/// `secret` masks an unset element the same way [`field_line`] masks a
/// `x-shep-secret` field; nothing in today's schemas sets it on an array,
/// but a future one could.
fn list_line(
    list: &ListPane,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
    secret: bool,
) -> Line<'static> {
    let body = body_width(width);
    let position_w = POSITION_W.min(body);
    let value_w = body.saturating_sub(position_w + 2);
    let typed = selected
        .then(|| list.typing())
        .flatten()
        .map(|(_, buffer)| buffer);
    let (position, value) = match list.rows().get(index).copied() {
        Some(ListRow::Item(item)) => (
            format!("{item}"),
            typed.map_or_else(
                || {
                    let raw = list
                        .elements()
                        .get(item)
                        .cloned()
                        .unwrap_or_else(|| "(unset)".to_owned());
                    if secret && raw != "(unset)" {
                        "<set>".to_owned()
                    } else {
                        raw
                    }
                },
                |buffer| format!("{buffer}\u{258f}"),
            ),
        ),
        Some(ListRow::New) => match typed {
            Some(buffer) => (String::new(), format!("{buffer}\u{258f}")),
            None => (String::new(), "+ new".to_owned()),
        },
        None => return Line::default(),
    };
    let mut text = format!("{} ", mark(selected));
    text.push_str(&fit(&position, position_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(&value, value_w));
    }
    if matches!(list.rows().get(index), Some(ListRow::New)) {
        return Line::from(Span::styled(text, palette.muted()));
    }
    Line::from(Span::raw(text))
}

/// Lays the sub-screen's body out from row `offset`, spending at most
/// `budget` lines. Both markers are reserved before a row is admitted, the
/// same rule [`body_from`] follows.
fn list_body_from(
    list: &ListPane,
    palette: Palette,
    width: u16,
    budget: usize,
    offset: usize,
    secret: bool,
) -> Attempt {
    let rows = list.rows();
    let total = rows.len();
    let cursor_row = list.view().cursor().min(total.saturating_sub(1));
    let above = usize::from(offset > 0);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut drawn = 0usize;
    for index in offset..total {
        if lines.len() + 1 + above + usize::from(index + 1 < total) > budget {
            break;
        }
        lines.push(list_line(
            list,
            index,
            index == cursor_row,
            width,
            palette,
            secret,
        ));
        drawn += 1;
    }
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

/// Whether `pane`'s own fields carry a group at all.
///
/// True for every sheep, whose Flockfile schema tags all 41 fields. False
/// for a dog, whose schema declares none (see [`ConfigPane::dog`]), which
/// is what keeps the tab row and the pending/env sections below a
/// sheep-only elaboration: a control that filters nothing would be a
/// tab row lying about what it does.
fn has_groups(pane: &ConfigPane) -> bool {
    pane.fields()
        .fields()
        .iter()
        .any(|field| field.group.is_some())
}

/// The pane's own reverse-video summary band: the target's name and how
/// many edits are filed and unsent.
///
/// Never the shepherd's own word for a field that is written and parked:
/// `pending` is what `shep flock`'s CFG column and this same pane's `!`
/// flag already mean, and a second meaning for the same word on the
/// screen an operator moves to next is exactly the confusion this counts
/// around instead.
fn title_band_line(pane: &ConfigPane, palette: Palette, width: u16) -> Line<'static> {
    let count = pane.edits().len();
    let edits = match count {
        1 => "1 edit".to_owned(),
        n => format!("{n} edits"),
    };
    let text = format!("{}  {edits}", pane.target().name());
    Line::from(Span::styled(
        cell::band(&text, usize::from(width)),
        palette.band(Role::Butter),
    ))
}

/// The tab row: every group in [`GROUP_ORDER`], the active one drawn as a
/// paper-2 chip in ink and the rest in ink-3, then the two keys that move
/// between them.
fn tab_row_line(pane: &ConfigPane, palette: Palette, width: u16) -> Line<'static> {
    let active = pane.group();
    let chip = |group: &str| format!(" {group} ");
    let chips = GROUP_ORDER
        .iter()
        .map(|group| chip(group))
        .collect::<Vec<_>>()
        .join(" ");
    let full = format!("{chips}    tab next group   1\u{2026}8 jump");
    let text = fit(&full, body_width(width));
    let marker = chip(active);
    let mut spans = vec![Span::raw("  ")];
    if let Some(start) = text.find(&marker) {
        let end = start + marker.len();
        if start > 0 {
            spans.push(Span::styled(text[..start].to_owned(), palette.muted()));
        }
        spans.push(Span::styled(text[start..end].to_owned(), palette.ground()));
        if end < text.len() {
            spans.push(Span::styled(text[end..].to_owned(), palette.muted()));
        }
    } else {
        // The active chip fell off the fitted text at a width too narrow
        // to hold it: the row still shows, muted throughout, rather than
        // claiming a chip it cannot draw.
        spans.push(Span::styled(text, palette.muted()));
    }
    Line::from(spans)
}

/// A rule of box-drawing horizontals, the full width of the body.
fn hairline_line(palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", cell::rule(usize::from(body_width(width)))),
        palette.line(),
    ))
}

/// The column header row: `KEY`, `VALUE` and `COST`, aligned over
/// [`field_line`]'s own cells. Drawn in place of [`top_line`] when neither
/// the apply menu nor `h`'s help text is up, so the slot under the tab row
/// always says something.
fn column_header_line(palette: Palette, width: u16) -> Line<'static> {
    let (key_w, value_w, cost_w) = widths(body_width(width));
    let mut text = String::from("  ");
    text.push_str(&fit("KEY", key_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit("VALUE", value_w));
    }
    if cost_w > 0 {
        text.push_str("  ");
        text.push_str(&fit("COST", cost_w));
    }
    Line::from(Span::styled(text, palette.muted()))
}

/// The pane's own key hints, drawn once at the foot rather than repeated
/// per group: the status bar's own hint already names `esc`/`tab`/space`
/// for the screen as a whole, and this line is the pane's local footnote
/// on top of it.
fn legend_line(palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                "enter edit   space cycle   tab group   u undo   esc save & close",
                body_width(width)
            )
        ),
        palette.muted(),
    ))
}

/// One entry of the pending-edits section: the field or env key it is
/// filed under, and what it will send.
///
/// A field edit shows `old -> new`, the same cell [`field_line`] draws for
/// one still visible in the active group; an env edit shows only that it
/// is set or removed, since the pane never holds an env value to show
/// either side of.
fn pending_edit_line(
    pane: &ConfigPane,
    edit: &PaneEdit,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let text = match edit {
        PaneEdit::Set { key, .. } => {
            let old = pane.display_value(key);
            let new = pane.edited_value(key).unwrap_or_default();
            format!("{key}  {old} -> {new}")
        }
        PaneEdit::SetEnv { key, value } => match value {
            Some(_) => format!("env.{key}  -> <set>"),
            None => format!("env.{key}  removed"),
        },
    };
    Line::from(Span::styled(
        format!("    {}", fit(&text, body_width(width).saturating_sub(4))),
        palette.attention(),
    ))
}

/// The pending-edits and env sections that follow the active group's own
/// fields: every filed edit regardless of which group it belongs to, then
/// the sheep's own env key names and a hint to add one.
///
/// Appended rather than scrolled: both are short by construction, an
/// operator files a handful of edits and a Flockfile carries a handful of
/// env keys, so neither earns the `... N below` machinery the field list's
/// 41 rows need.
fn pending_and_env_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if budget == 0 {
        return lines;
    }
    let mut remaining = budget;

    if !pane.edits().is_empty() && remaining > 0 {
        lines.push(Line::default());
        remaining -= 1;
        if remaining > 0 {
            lines.push(section_header("pending edits", palette));
            remaining -= 1;
        }
        for (_, entry) in pane.edits().iter() {
            if remaining == 0 {
                break;
            }
            lines.push(pending_edit_line(pane, entry.edit(), width, palette));
            remaining -= 1;
        }
    }

    if remaining > 0 {
        lines.push(Line::default());
        remaining -= 1;
    }
    if remaining > 0 {
        lines.push(section_header("env", palette));
        remaining -= 1;
    }
    for name in pane.env_key_names() {
        if remaining == 0 {
            break;
        }
        lines.push(Line::from(Span::raw(format!("    {name}  <set>"))));
        remaining -= 1;
    }
    if remaining > 0 {
        lines.push(Line::from(Span::styled(
            "    + add a key".to_owned(),
            palette.muted(),
        )));
    }
    lines
}

/// The active group's fields, laid out through [`super::scroll::to_cursor`]
/// exactly as [`body_from`] already does for the groupless fallback, then
/// [`pending_and_env_lines`] appended after.
fn grouped_body_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut lines = if !pane.fields().is_empty() && budget > 0 {
        let total = pane.rows().len();
        let cursor_row = pane.view().cursor().min(total.saturating_sub(1));
        super::scroll::to_cursor(
            cursor_row,
            pane.view().offset(),
            |offset| body_from(pane, palette, width, budget, offset, false),
            || cursor_only(pane, palette, width, budget, cursor_row),
        )
    } else {
        Vec::new()
    };
    let remaining = budget.saturating_sub(lines.len());
    lines.extend(pending_and_env_lines(pane, palette, width, remaining));
    lines
}

/// The pane's lines when its fields carry groups (every sheep): the butter
/// title band, the provenance row naming the target, the tab row, a
/// hairline, the menu or help line when one is up else the column header
/// row, the body, a hairline, and the legend.
fn grouped_pane_lines(
    pane: &ConfigPane,
    menu: Option<&PaneMenu>,
    palette: Palette,
    width: u16,
    budget: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![title_band_line(pane, palette, width)];
    let mut remaining = budget - 1;
    if remaining == 0 {
        return lines;
    }
    lines.push(title_line(pane, palette, width));
    remaining -= 1;
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
        if let Some((text, style)) = top_line(pane, menu, palette) {
            lines.push(Line::from(Span::styled(
                format!("  {}", fit(&text, body_width(width))),
                style,
            )));
        } else {
            lines.push(column_header_line(palette, width));
        }
        remaining -= 1;
    }
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
        lines.extend(grouped_body_lines(pane, palette, width, body_budget));
    }
    if footer_lines >= 1 {
        lines.push(hairline_line(palette, width));
    }
    if footer_lines >= 2 {
        lines.push(legend_line(palette, width));
    }
    lines
}

/// Every line of the pane, top to bottom, laid out for a terminal `height`
/// rows tall.
///
/// `height` counts lines, and no more than that ever come back. Zero means
/// unlimited, which is what a test with no terminal behind it gets. See
/// [`super::scroll`] for why the viewport's offset is a starting point
/// here, not an answer.
///
/// `menu` is the apply offer, which takes the one line under the title
/// rather than a line of its own.
#[must_use]
pub fn pane_lines(
    pane: &ConfigPane,
    menu: Option<&PaneMenu>,
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
    if let Some(env) = pane.env() {
        return env_lines(pane, env, palette, width, budget);
    }
    if has_groups(pane) {
        return grouped_pane_lines(pane, menu, palette, width, budget);
    }
    let mut lines = vec![title_line(pane, palette, width)];
    // The title is unconditional, so the body is laid out against what is
    // left after it. An empty form (unreachable for a sheep, whose schema
    // is a committed file with 40 properties, but a dog answers `--schema`
    // for itself) leaves the title as the whole pane.
    let mut body_budget = budget - 1;
    // The apply menu, or `h`'s help text, on the line under the title.
    // Subtracted from the budget rather than appended, per `body_from`'s
    // own doc on markers. See `top_line`.
    if let Some((text, style)) = top_line(pane, menu, palette)
        && body_budget > 0
    {
        lines.push(Line::from(Span::styled(
            format!("  {}", fit(&text, body_width(width))),
            style,
        )));
        body_budget -= 1;
    }
    // The one line a dog pane has that a sheep pane does not: shep does not
    // know what a dog's field costs, so every row's COST cell is empty.
    // Reserved out of the budget before rows are laid out, for the same
    // reason the top line is: a footer appended afterwards is a line
    // nothing counted.
    let footer = match pane.target() {
        PaneTarget::Sheep { .. } => None,
        PaneTarget::Dog { name, .. } => (body_budget > 0)
            .then(|| format!("shep publishes the change; {name} decides what to reload")),
    };
    if footer.is_some() {
        body_budget -= 1;
    }
    if !pane.fields().is_empty() && body_budget > 0 {
        let total = pane.rows().len();
        let cursor_row = pane.view().cursor().min(total - 1);
        lines.extend(super::scroll::to_cursor(
            cursor_row,
            pane.view().offset(),
            |offset| body_from(pane, palette, width, body_budget, offset, true),
            || cursor_only(pane, palette, width, body_budget, cursor_row),
        ));
    }
    if let Some(text) = footer {
        lines.push(Line::from(Span::styled(
            format!("  {}", fit(&text, body_width(width))),
            palette.muted(),
        )));
    }
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
) -> Attempt {
    let rows = pane.rows();
    let total = rows.len();
    let cursor_row = pane.view().cursor().min(total.saturating_sub(1));
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
        let PaneRow::Field(field_index) = *row;
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
) -> Vec<Line<'static>> {
    let rows = pane.rows();
    let mut lines = Vec::new();
    if let Some(PaneRow::Field(index)) = rows.get(cursor_row).copied() {
        lines.push(field_line(pane, index, true, width, palette));
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

/// Draws the pane into `area`, straight into `buffer`.
pub fn draw_pane(app: &App, pane: &ConfigPane, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = pane_lines(
        pane,
        app.pane_menu().as_ref(),
        app.palette(),
        area.width,
        area.height,
    );
    for (offset, line) in lines.iter().enumerate().take(usize::from(area.height)) {
        let offset = u16::try_from(offset).unwrap_or(0);
        buffer.set_line(area.x, area.y + offset, line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::fixtures;
    use super::*;
    use crate::lookout::app::{Effect, KeyPress, Msg};
    use crate::lookout::frames::render_text;
    use crate::lookout::pane::ReloadKind;
    use crate::output::width::visible_width;

    /// The pane the rest of this module renders: `web`, with two overridden
    /// fields, one pending and two env keys.
    fn web_pane() -> ConfigPane {
        ConfigPane::sheep(fixtures::sheep_config_view())
    }

    /// Every line as a plain string, styles dropped.
    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    /// The whole pane at a comfortable width, unbounded. The snapshot is the
    /// assertion: it pins the title, the four section headers in order, all
    /// 40 rows, the two flags and the cost cell beside each one.
    #[test]
    fn a_sheep_pane_at_a_comfortable_width() {
        let lines = pane_lines(&web_pane(), None, fixtures::plain(), 120, 0);
        insta::assert_snapshot!("sheep_pane_wide", text_of(&lines).join("\n"));
    }

    #[test]
    fn a_sheep_pane_scrolled_to_the_last_field_of_a_group_shows_it() {
        // `cron` has only two fields, both of which always fit; `process`,
        // the group a fresh pane opens on, has ten, which is what forces
        // the scroll this test is about.
        let mut pane = web_pane();
        pane.set_rows(8);
        pane.move_to_last();
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 9));
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
            for line in text_of(&pane_lines(&pane, None, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// Checks `args` too, since shep writes it happily: muting must mark
    /// both reasons a row can be locked, not just the structural one.
    #[test]
    fn a_structural_field_renders_muted_and_the_cost_column_says_why() {
        let lines = all_group_lines(fixtures::coloured());
        let instances = lines
            .iter()
            .find(|line| text_of(core::slice::from_ref(line))[0].contains("instances"))
            .expect("every field is drawn at 120 columns");
        assert!(
            text_of(core::slice::from_ref(instances))[0].contains("read-only"),
            "{instances:?}"
        );
        assert_eq!(
            instances.spans[0].style,
            fixtures::coloured().muted(),
            "a refused row is muted"
        );

        let probe = lines
            .iter()
            .find(|line| text_of(core::slice::from_ref(line))[0].contains("~ liveness_probe"))
            .expect("a field with no widget is drawn too");
        let rendered = text_of(core::slice::from_ref(probe))[0].clone();
        assert!(
            rendered.contains("now") && !rendered.contains("read-only"),
            "shep writes `liveness_probe`, so its cost is a real cost: {rendered:?}"
        );
        assert_eq!(
            probe.spans[0].style,
            fixtures::coloured().muted(),
            "muting says `not from here`, which is true of both kinds"
        );
    }

    /// Every field row over all eight groups, walked with `next_group`,
    /// styled by `palette`. What a test that used to find any field at
    /// 120 columns in one shot now needs, since the active group is the
    /// only one a single render draws.
    fn all_group_lines(palette: Palette) -> Vec<Line<'static>> {
        let mut pane = web_pane();
        let mut lines = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            lines.extend(pane_lines(&pane, None, palette, 120, 0));
            pane.next_group();
        }
        lines
    }

    /// A field row split into its four fixed leading parts: the selection
    /// mark, the lock glyph, the flag and the key. Positional rather than a
    /// `starts_with`, which only ever matched unselected rows and so could
    /// not see a glyph on the one row the cursor was on.
    fn parts(line: &str) -> Option<(char, char, char, String)> {
        let mut chars = line.chars();
        let mark = chars.next()?;
        let lock = chars.next()?;
        let flag = chars.next()?;
        let key = chars.as_str().split_whitespace().next()?.to_string();
        (mark == '>' || mark == ' ').then_some((mark, lock, flag, key))
    }

    /// Every field row, as its four leading parts. Headers, markers, blanks
    /// and the title are dropped: none of them names a field.
    fn rows_of(text: &[String]) -> Vec<(char, char, char, String)> {
        let keys: Vec<String> = web_pane()
            .fields()
            .fields()
            .iter()
            .map(|field| field.key.clone())
            .collect();
        text.iter()
            .filter_map(|line| parts(line))
            .filter(|(_, _, _, key)| keys.contains(key))
            .collect()
    }

    #[test]
    fn the_flags_mark_exactly_the_overridden_and_pending_fields() {
        let text = text_of(&all_group_lines(fixtures::plain()));
        let flagged = |wanted: char| -> Vec<String> {
            rows_of(&text)
                .into_iter()
                .filter(|(_, _, flag, _)| *flag == wanted)
                .map(|(_, _, _, key)| key)
                .collect()
        };
        assert_eq!(flagged('*'), ["reuse_port", "max_restarts"]);
        assert_eq!(flagged('!'), ["kill_signal"]);
        assert_eq!(rows_of(&text).len(), 41, "every field is drawn at 120");
    }

    /// `=` is shep refusing the write outright; `~` is only this pane
    /// having no widget for the shape. The two probes shep writes happily
    /// carry `~`, so their cost cell must say `respawn` or `now`, never
    /// `read-only`.
    #[test]
    fn a_refused_field_and_one_the_pane_has_no_widget_for_get_different_glyphs() {
        let text = text_of(&all_group_lines(fixtures::plain()));
        let glyphed = |wanted: char| -> Vec<String> {
            rows_of(&text)
                .into_iter()
                .filter(|(_, lock, _, _)| *lock == wanted)
                .map(|(_, _, _, key)| key)
                .collect()
        };
        assert_eq!(glyphed('='), ["instances", "name"]);
        assert_eq!(glyphed('~'), ["liveness_probe", "readiness_probe"]);
        assert_eq!(glyphed(' ').len(), 41 - 2 - 2);
    }

    /// `kill_timeout` and `exp_backoff_restart_delay` default to 1600ms
    /// and 100ms, which `UpDuration::Display` prints as bare digits with
    /// no unit; `listen_timeout` defaults to 3s, which it already prints
    /// with one.
    #[test]
    fn a_bare_duration_or_mem_size_shows_its_resolved_unit_in_the_row() {
        let text = text_of(&all_group_lines(fixtures::plain()));
        let row = |key: &str| {
            text.iter()
                .find(|line| line.contains(key))
                .unwrap_or_else(|| panic!("{key} is drawn at 120 columns: {text:?}"))
        };
        assert!(
            row("kill_timeout").contains("1600ms"),
            "{:?}",
            row("kill_timeout")
        );
        assert!(
            row("exp_backoff_restart_delay").contains("100ms"),
            "{:?}",
            row("exp_backoff_restart_delay")
        );
        assert!(
            row("listen_timeout").contains("3s"),
            "{:?}",
            row("listen_timeout")
        );
    }

    /// `MIN_TERM_WIDTH` drops the cost cell and `plain` renders muted as
    /// nothing, so the glyph is the whole signal in exactly the case an
    /// operator is most likely to be in.
    #[test]
    fn the_two_glyphs_survive_the_narrowest_width_and_a_palette_with_no_colour() {
        let width = super::super::MIN_TERM_WIDTH;
        let mut pane = web_pane();
        let mut rows = Vec::new();
        let mut all_text = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            pane.move_to_last();
            let text = text_of(&pane_lines(&pane, None, fixtures::plain(), width, 0));
            rows.extend(rows_of(&text));
            all_text.extend(text);
            pane.next_group();
        }
        assert!(
            !all_text
                .iter()
                .any(|line| parts(line).is_some() && line.contains("read-only")),
            "no field row can afford the cost cell at {width}: {all_text:?}"
        );
        let glyph = |key: &str| rows.iter().find(|(_, _, _, k)| k == key).map(|r| r.1);
        assert_eq!(glyph("instances"), Some('='));
        assert_eq!(glyph("liveness_probe"), Some('~'));
        assert_eq!(glyph("watch"), Some(' '));
    }

    /// The whole frame at `height`, through the same `note_body_rows` and
    /// `draw` the event loop runs before each one.
    fn screen_at(app: &mut crate::lookout::app::App, height: u16) -> String {
        let area = Rect::new(0, 0, 120, height);
        app.note_body_rows(super::super::body_rows(area));
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal
            .draw(|frame| super::super::draw(app, frame))
            .unwrap();
        render_text(terminal.backend().buffer())
    }

    /// How many rows the frame marks as selected. One, always.
    fn marked(text: &str) -> usize {
        text.lines().filter(|line| line.starts_with('>')).count()
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

    /// The marker that says rows were cut would itself become the row
    /// that gets cut.
    #[test]
    fn the_body_never_outgrows_the_height_it_was_given() {
        let mut pane = web_pane();
        for height in 1..=60u16 {
            pane.set_rows(usize::from(height.saturating_sub(1)));
            for cursor in [0usize, 7, 20, 38] {
                pane.move_to_first();
                pane.move_by(isize::try_from(cursor).unwrap());
                let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, height));
                assert!(
                    text.len() <= usize::from(height),
                    "height {height}, cursor {cursor}: {text:?}"
                );
            }
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
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 0));
        assert!(
            !text.iter().any(|line| line.contains("enter confirms")),
            "{text:?}"
        );
        assert!(
            !text.iter().any(|line| line.contains("set autorestart")),
            "{text:?}"
        );
    }

    /// `max_memory`'s own blurb, read off the same schema `Field::help`
    /// is built from.
    #[test]
    fn help_open_draws_the_selected_fields_own_text_under_the_title() {
        let mut pane = web_pane();
        pane.move_to_key("max_memory");
        pane.toggle_help();
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 0));
        assert!(
            text.iter()
                .any(|line| line.contains("Restart the app if it climbs above this much memory")),
            "{text:?}"
        );
    }

    #[test]
    fn toggling_help_again_dismisses_it() {
        let mut pane = web_pane();
        pane.move_to_key("max_memory");
        pane.toggle_help();
        pane.toggle_help();
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 0));
        assert!(
            !text.iter().any(|line| line.contains("Restart the app")),
            "{text:?}"
        );
    }

    /// Help keeps the shared slot through an edit: nothing competes for
    /// it any more, so a filed edit must not blank a note the operator has
    /// not dismissed.
    #[test]
    fn open_help_survives_a_filed_edit() {
        let mut pane = web_pane();
        pane.move_to_key("autorestart");
        pane.toggle_help();
        pane.cycle();
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 0));
        assert!(
            text.iter()
                .any(|line| line.contains("Restarts the process automatically")),
            "{text:?}"
        );
    }

    /// The hard constraint this item's brief calls out: a line drawn into
    /// the fixed slot under the title is still one line counted against
    /// the same budget every other line in the pane is, at every width
    /// and height the pane claims to draw at.
    #[test]
    fn help_text_still_respects_the_width_and_height_budgets() {
        let mut pane = web_pane();
        pane.move_to_key("max_memory");
        pane.toggle_help();
        for width in super::super::MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, None, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width}: {line:?}"
                );
            }
        }
        for height in 1..=30u16 {
            let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, height));
            assert!(
                text.len() <= usize::from(height),
                "height {height}: {text:?}"
            );
        }
    }

    /// The menu, opened the way an operator opens it: `esc` on a pane the
    /// running sheep has not caught up with.
    fn app_at_the_menu() -> crate::lookout::app::App {
        let mut app = fixtures::app_in_sheep_pane_with_two_parked_fields();
        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.pane_menu().is_some(), "esc offered the menu");
        app
    }

    #[test]
    fn the_menu_takes_the_line_under_the_title_and_names_all_three_keys() {
        let mut app = app_at_the_menu();
        let text = screen_at(&mut app, 30);
        let line = text
            .lines()
            .find(|line| line.contains("saved fields"))
            .expect("the menu is drawn");
        assert!(line.contains("2 saved fields wait"), "{line:?}");
        for clause in [
            "L reload (overlapping)",
            "R restart",
            "esc leave them parked",
        ] {
            assert!(line.contains(clause), "{clause} missing from {line:?}");
        }
    }

    #[test]
    fn one_parked_field_reads_in_the_singular() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        app.update(Msg::Key(KeyPress::Escape));
        let text = screen_at(&mut app, 30);
        assert!(text.contains("1 saved field waits"), "{text}");
        assert!(text.contains("esc leave it parked"), "{text}");
    }

    /// The menu must not read as a save prompt: every field it counts is
    /// already in the override store.
    #[test]
    fn the_menu_never_says_anything_is_at_risk() {
        let mut app = app_at_the_menu();
        let screen = screen_at(&mut app, 30);
        let line = screen
            .lines()
            .find(|line| line.contains("saved fields"))
            .expect("the menu is drawn")
            .to_lowercase();
        for word in ["discard", "unsaved", "are you sure", "lose"] {
            assert!(!line.contains(word), "{word} has no business in {line:?}");
        }
    }

    /// The menu shares the slot the confirm and the help text already
    /// share, so the field list is laid out against the same budget with
    /// it open as without.
    #[test]
    fn the_menu_costs_the_field_list_no_line() {
        let mut open = fixtures::app_in_sheep_pane_with_two_parked_fields();
        let mut closed = fixtures::app_in_sheep_pane_with_two_parked_fields();
        open.update(Msg::Key(KeyPress::Escape));
        for height in [super::super::flock::MIN_HEIGHT, 7, 8, 12, 20, 30] {
            let with = screen_at(&mut open, height);
            let without = screen_at(&mut closed, height);
            assert_eq!(
                with.lines().count(),
                without.lines().count(),
                "height {height}"
            );
            assert_eq!(marked(&with), 1, "height {height}:\n{with}");
        }
    }

    #[test]
    fn the_menu_line_fits_every_width_the_pane_claims_to_support() {
        let mut app = app_at_the_menu();
        for width in super::super::MIN_TERM_WIDTH..=200 {
            let area = Rect::new(0, 0, width, 30);
            app.note_body_rows(super::super::body_rows(area));
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal
                .draw(|frame| super::super::draw(&app, frame))
                .unwrap();
            for line in render_text(terminal.backend().buffer()).lines() {
                assert!(
                    visible_width(line) <= usize::from(width),
                    "width {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn a_sheep_pane_with_the_apply_menu_open() {
        let menu = PaneMenu::new(2, ReloadKind::Serial, Instant::now());
        let lines = pane_lines(&web_pane(), Some(&menu), fixtures::plain(), 120, 0);
        insta::assert_snapshot!("sheep_pane_apply_menu", text_of(&lines).join("\n"));
    }

    /// The pane's cursor, walked onto `key` the way an operator walks it.
    /// A thin wrapper: [`fixtures::select_field`] is this exact walk, and
    /// this module had its own copy before the tab row gave a field's
    /// group somewhere to switch to first.
    fn pane_to(app: &mut crate::lookout::app::App, key: &str) {
        fixtures::select_field(app, key);
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

            let text = text_of(&pane_lines(
                app.config_pane().unwrap(),
                None,
                fixtures::plain(),
                120,
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
                .unwrap_or_else(|| panic!("{key} is drawn at 120 columns: {text:?}"));
            assert!(row.contains(column), "{key}: {row:?}");

            app.update(Msg::Key(KeyPress::Cycle));
            let Effect::SendAll(mut batch) = app.update(Msg::Key(KeyPress::Escape)) else {
                panic!("{key}: closing the pane sends");
            };
            app.update(Msg::Replied {
                sent: batch.remove(0),
                result: Ok(shep_core::protocol::Response::SheepFieldSet {
                    name: "web".to_string(),
                    key: key.to_string(),
                    pending,
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
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 0));
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
        let text = text_of(&pane_lines(&bark_pane(), None, fixtures::plain(), 120, 0));
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
                let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, height));
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
            for line in text_of(&pane_lines(&pane, None, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// The env sub-screen at a comfortable width. The snapshot is the
    /// assertion: the title says write-only, both of the fixture's keys are
    /// listed with `<set>` rather than a value, and the `+ new` row is last.
    #[test]
    fn the_env_sub_screen_at_a_comfortable_width() {
        let mut pane = web_pane();
        pane.open_env();
        let lines = pane_lines(&pane, None, fixtures::plain(), 120, 0);
        insta::assert_snapshot!("env_sub_screen", text_of(&lines).join("\n"));
    }

    /// A filed env write draws nothing at all on this screen, and above
    /// all not the value that was typed into it.
    #[test]
    fn a_filed_env_write_never_reaches_the_screen() {
        let mut pane = web_pane();
        pane.open_env();
        pane.env_mut().unwrap().begin_typing();
        for typed in "hunter2".chars() {
            pane.env_mut().unwrap().type_char(typed);
        }
        let (key, value) = pane.env_mut().unwrap().apply_typing().unwrap();
        pane.file_env(key, value.map(Into::into));
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 0));
        assert!(!text.join("\n").contains("hunter2"), "{text:?}");
    }

    /// The shepherd sends keys with no values at all, so a rendered value
    /// could only have been invented.
    #[test]
    fn the_env_sub_screen_never_renders_a_value() {
        let mut pane = ConfigPane::sheep({
            let mut config = shep_core::config::AppConfig {
                name: "web".to_string(),
                ..Default::default()
            };
            config
                .env
                .insert("DB_PASSWORD".to_string(), "hunter2".to_string());
            shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new())
        });
        pane.open_env();
        let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, 0)).join("\n");
        assert!(text.contains("DB_PASSWORD"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("<set>"), "{text}");
    }

    #[test]
    fn the_env_cursor_survives_every_step_of_a_walk_down_and_back_up() {
        let mut pane = ConfigPane::sheep({
            let mut config = shep_core::config::AppConfig {
                name: "web".to_string(),
                ..Default::default()
            };
            for index in 0..20 {
                config
                    .env
                    .insert(format!("KEY_{index:02}"), "x".to_string());
            }
            shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new())
        });
        pane.open_env();
        for height in [1u16, 2, 3, 6, 8, 14, 30] {
            pane.env_mut().unwrap().move_to_first();
            let body = usize::from(height.saturating_sub(1));
            pane.env_mut().unwrap().set_rows(body);
            let total = pane.env().unwrap().rows().len();
            for step in 0..=total {
                let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, height));
                assert!(
                    text.len() <= usize::from(height),
                    "height {height}, step {step}: {text:?}"
                );
                if height > 1 {
                    assert_eq!(
                        text.iter().filter(|line| line.starts_with('>')).count(),
                        1,
                        "height {height}, step {step}: {text:?}"
                    );
                }
                pane.env_mut().unwrap().move_by(1);
            }
            for step in 0..=total {
                let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, height));
                if height > 1 {
                    assert_eq!(
                        text.iter().filter(|line| line.starts_with('>')).count(),
                        1,
                        "height {height}, step {step} up: {text:?}"
                    );
                }
                pane.env_mut().unwrap().move_by(-1);
            }
        }
    }

    #[test]
    fn every_env_line_fits_the_width_it_was_drawn_for() {
        let mut pane = web_pane();
        pane.open_env();
        pane.env_mut().unwrap().begin_typing();
        for typed in "A_VERY_LONG_ENV_KEY_NAME=and a value longer still".chars() {
            pane.env_mut().unwrap().type_char(typed);
        }
        for width in super::super::MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, None, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    #[test]
    fn the_title_names_the_target_and_no_longer_calls_it_read_only() {
        let text = text_of(&pane_lines(&web_pane(), None, fixtures::plain(), 120, 0));
        assert!(text[0].contains("web"), "{:?}", text[0]);
        assert!(text[1].contains("web"), "{:?}", text[1]);
        assert!(text[1].contains("(sheep config)"), "{:?}", text[1]);
        assert!(!text[0].contains("read-only"), "{:?}", text[0]);
        assert!(!text[1].contains("read-only"), "{:?}", text[1]);
    }

    /// A sheep whose `args` are `args`, for the list sub-screen's own
    /// tests.
    fn web_with_args(args: &[&str]) -> shep_core::protocol::SheepConfigView {
        let config = shep_core::config::AppConfig {
            name: "web".to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            ..Default::default()
        };
        shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new())
    }

    /// The list sub-screen over `args`, as an operator reaches it: cursor
    /// onto the row, then open.
    fn rendered_list(pane: &ConfigPane, width: u16, height: u16) -> Vec<String> {
        let mut pane = pane.clone();
        pane.move_to_key("args");
        pane.open_list();
        text_of(&pane_lines(&pane, None, fixtures::plain(), width, height))
    }

    /// The dashboard with `web` selected and its list sub-screen open on
    /// `args`, reached the way an operator reaches it: `e`, the shepherd's
    /// reply, walk to the row, `Enter`.
    fn app_on_the_list_screen(args: &[&str]) -> crate::lookout::app::App {
        let mut app = fixtures::with_selection(
            shep_core::protocol::ProcessInfo::builder(
                9,
                "web",
                shep_core::status::ProcStatus::Online,
            )
            .pid(Some(48_000))
            .build(),
        );
        app.set_control_for_tests(crate::lookout::app::Control::Allowed);
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: crate::lookout::app::Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(shep_core::protocol::Response::SheepConfig(Box::new(
                web_with_args(args),
            ))),
        });
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app
    }

    /// Env is write-only because the shepherd sends no value; an array
    /// arrives with the config, so a screen that hid it could not say
    /// which element the cursor is on.
    #[test]
    fn the_list_screen_shows_its_values_unlike_env() {
        let pane = ConfigPane::sheep(web_with_args(&["--port"]));
        let lines = rendered_list(&pane, 120, 20);
        assert!(lines.iter().any(|l| l.contains("--port")), "{lines:?}");
        assert!(
            !lines.iter().any(|l| l.contains(r#"["--port"]"#)),
            "the element is a row of its own, not the field list's JSON cell: {lines:?}"
        );
    }

    /// A dog pane with one array field marked `x-shep-secret`, two elements
    /// already set, cursor opened onto the list sub-screen.
    fn secret_list_dog_pane() -> ConfigPane {
        let schema = serde_json::json!({
            "properties": {
                "tokens": {
                    "type": "array",
                    "items": { "type": "string" },
                    "x-shep-secret": true,
                }
            }
        });
        let mut pane = ConfigPane::dog(
            "watch".into(),
            None,
            schema,
            "tokens = [\"ab12cd34\", \"ef56gh78\"]\n".into(),
        );
        pane.move_to_key("tokens");
        pane.open_list();
        pane
    }

    /// No Flockfile field is secret today, but a dog's schema can mark one,
    /// and the list sub-screen has to mask it the same way the field list
    /// and the confirm sentence already do.
    #[test]
    fn the_list_screen_masks_a_secret_arrays_elements() {
        let text = text_of(&pane_lines(
            &secret_list_dog_pane(),
            None,
            fixtures::plain(),
            120,
            0,
        ));
        assert!(
            !text
                .iter()
                .any(|line| line.contains("ab12cd34") || line.contains("ef56gh78")),
            "a secret element is never rendered: {text:?}"
        );
        assert!(
            text.iter().filter(|line| line.contains("<set>")).count() == 2,
            "both elements mask to <set>: {text:?}"
        );
    }

    #[test]
    fn the_list_sub_screen_at_a_comfortable_width() {
        let lines = rendered_list(&web_pane(), 120, 0);
        insta::assert_snapshot!("list_sub_screen", lines.join("\n"));
    }

    /// The chrome this screen draws is counted like every other line: a
    /// title, and the confirm echo under it when something is armed.
    #[test]
    fn the_list_sub_screen_never_outgrows_the_height_or_the_width_it_was_given() {
        let mut pane = ConfigPane::sheep(web_with_args(&[
            "--port",
            "8080",
            "--host",
            "0.0.0.0",
            "--verbose",
            "--log",
            "debug",
        ]));
        pane.move_to_key("args");
        pane.open_list();
        pane.file_list_removal();
        for height in 1..=20u16 {
            pane.list_mut().unwrap().move_to_first();
            let total = pane.list().unwrap().rows().len();
            pane.list_mut()
                .unwrap()
                .set_rows(usize::from(height.saturating_sub(1)));
            for step in 0..=total {
                let text = text_of(&pane_lines(&pane, None, fixtures::plain(), 120, height));
                assert!(
                    text.len() <= usize::from(height),
                    "height {height}, step {step}: {text:?}"
                );
                pane.list_mut().unwrap().move_by(1);
            }
        }
        for width in super::super::MIN_TERM_WIDTH..=200 {
            for line in text_of(&pane_lines(&pane, None, fixtures::plain(), width, 0)) {
                assert!(
                    visible_width(&line) <= usize::from(width),
                    "width {width} drew {}: {line:?}",
                    visible_width(&line)
                );
            }
        }
    }

    /// The invariant `view::scroll` exists to hold, at the shortest height
    /// the pane claims to draw: one row is marked at every step of a walk.
    ///
    /// Reached through `screen_at`, the way the field list's own twin
    /// (`the_cursor_survives_every_step_of_a_walk_down_and_back_up`) reaches
    /// it, so the budget is the real `body_rows`: four rows at
    /// `MIN_HEIGHT`, not `MIN_HEIGHT` itself.
    #[test]
    fn the_list_cursor_survives_every_step_at_the_minimum_height() {
        let args = [
            "--port",
            "8080",
            "--host",
            "0.0.0.0",
            "--verbose",
            "--log",
            "debug",
            "--quiet",
        ];
        for height in [super::super::flock::MIN_HEIGHT, 7, 8, 12] {
            let mut app = app_on_the_list_screen(&args);
            let total = app.config_pane().unwrap().list().unwrap().rows().len();
            for step in 0..=total {
                let text = screen_at(&mut app, height);
                assert_eq!(marked(&text), 1, "height {height}, step {step}:\n{text}");
                app.update(Msg::Key(KeyPress::SelectDown));
            }
        }
    }

    /// Eight groups, in `GROUP_ORDER`, and every one of them reachable.
    /// This is the test that would have caught a filter axis nothing
    /// could set.
    #[test]
    fn tab_walks_every_group_and_each_one_shows_its_own_fields() {
        let mut app = fixtures::app_in_sheep_pane();
        let mut seen = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            let group = app.config_pane().unwrap().group().to_owned();
            let rendered = pane_lines(
                app.config_pane().unwrap(),
                app.pane_menu().as_ref(),
                fixtures::plain(),
                160,
                48,
            );
            let listed = fixtures::render_all(&rendered);
            assert!(listed.contains(&group), "the tab row does not name {group}");
            seen.push(group);
            app.update(Msg::Key(KeyPress::NextGroup));
        }
        assert_eq!(seen, GROUP_ORDER.to_vec());
    }

    #[test]
    fn tab_wraps_from_the_last_group_to_the_first() {
        let mut app = fixtures::app_in_sheep_pane();
        for _ in 0..GROUP_ORDER.len() {
            app.update(Msg::Key(KeyPress::NextGroup));
        }
        assert_eq!(app.config_pane().unwrap().group(), GROUP_ORDER[0]);
    }

    /// The digits reach the same eight groups `tab` does. A key that
    /// reaches nothing is exactly the shape this plan exists to avoid.
    #[test]
    fn the_digits_reach_the_same_groups_tab_does() {
        for (index, wanted) in GROUP_ORDER.iter().enumerate() {
            let mut app = fixtures::app_in_sheep_pane();
            let digit = u8::try_from(index + 1).unwrap();
            app.update(Msg::Key(KeyPress::Group(digit)));
            assert_eq!(&app.config_pane().unwrap().group(), wanted);
        }
    }

    #[test]
    fn the_list_shows_only_the_active_groups_fields() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::Group(1)));
        let rows = fixtures::config_pane_field_rows_for_tests(&app);
        assert!(rows.iter().any(|row| row.contains("cwd")), "{rows:?}");
        assert!(
            !rows.iter().any(|row| row.contains("kill_timeout")),
            "a shutdown field is showing under process: {rows:?}"
        );
    }

    /// The pending section spans every group, which is the whole reason
    /// it is a section rather than a marker.
    #[test]
    fn the_pending_section_lists_an_edit_from_another_group() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Group(1)));
        let rows = fixtures::config_pane_pending_rows_for_tests(&app);
        assert!(
            rows.iter().any(|row| row.contains("max_memory")),
            "{rows:?}"
        );
    }

    /// `pending` is the shepherd's word for a field already written and
    /// parked until a respawn, and it is what the flock table's own `cfg
    /// !2 pending` cell means. This pane's own count of unsent edits must
    /// not borrow it.
    #[test]
    fn the_title_band_counts_edits_without_calling_them_pending() {
        let app = fixtures::app_in_sheep_pane_with_two_edits();
        let band = fixtures::config_pane_title_band_for_tests(&app, 160);
        assert!(band.contains("2 edits"), "{band}");
        assert!(!band.contains("pending"), "{band}");
    }

    /// An edited row shows its own change rather than borrowing the `!`
    /// the shepherd's parked-field marker already owns.
    #[test]
    fn an_edited_row_shows_old_then_new_and_takes_no_marker() {
        let app = fixtures::app_in_sheep_pane_with_two_edits();
        let row = fixtures::config_pane_row_for_tests(&app, "max_memory");
        assert!(row.contains("->"), "{row}");
        assert!(!row.trim_start().starts_with('!'), "{row}");
    }
}
