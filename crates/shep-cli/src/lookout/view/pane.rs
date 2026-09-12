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

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use shep_core::config::{ApplyGroup, GROUP_ORDER};

use super::super::app::{App, CONFIRM_EXPIRY, CloseDialog};
use super::super::field::{Field, FieldKind, ValueKind};
use super::super::pane::{
    ConfigPane, EnvTyping, ListPane, ListRow, Lock, PaneEdit, PaneRow, PaneTarget, ReloadKind,
};
use super::super::theme::Palette;
use super::super::validation;
use super::cell;
use super::flock::{fit, mark};
use super::scroll::Attempt;
use crate::output::width::char_columns;
use crate::vocabulary::Role;

/// The columns every line spends on the selection mark and the space after
/// it, before any cell is drawn. [`super::settings::GUTTER`]'s twin, and it
/// exists for the reason that one does: a budget that forgets it is a budget
/// every line overruns.
const GUTTER: u16 = 2;

/// The close dialog's own interior width, once it is boxed: what
/// [`close_dialog_lines`] lays its rows out to when [`draw_close_dialog`]
/// draws the boxed form.
const BOX_WIDTH: u16 = 86;

/// 86 interior plus a border cell each side is 88, plus a margin cell each
/// side is 90. One column narrower and the border would have to clip, which
/// `docs/lookout/design-files/README.md:332` refuses ("The 1g and 1k
/// overlays need 90 and 132 columns; below that, draw them full-width with
/// no border box rather than clipping"), so 89 draws the borderless form
/// instead.
const BOX_FLOOR: u16 = BOX_WIDTH + 4;

/// The border's four corners, checked against `unicodedata.east_asian_width`
/// and found Neutral, same as [`BOX_LEFT`].
const BOX_TOP_LEFT: char = '▛';
const BOX_TOP_RIGHT: char = '▜';
const BOX_BOTTOM_LEFT: char = '▙';
const BOX_BOTTOM_RIGHT: char = '▟';

/// The left edge. Neutral, unlike the other three edge glyphs below.
const BOX_LEFT: char = '▐';

/// The top, bottom and right edges. All three are East-Asian Ambiguous,
/// checked the same way the rulings ask `▌` to be. Kept anyway: `─` already
/// draws every hairline rule in this pane at full width and `█░` fill every
/// gauge, both Ambiguous too, so "no Ambiguous glyph" was never this
/// codebase's bar. The right edge is the one with real exposure, since no
/// Neutral right-half block exists to swap `▌` for and a terminal that
/// doubles it shifts every interior row;
/// `the_border_vocabulary_is_the_one_that_was_checked` pins the set so a
/// later glyph change gets the same check rather than inheriting this
/// answer.
const BOX_TOP: char = '▀';
const BOX_BOTTOM: char = '▄';
const BOX_RIGHT: char = '▌';

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

/// The blurb's own wrap width, independent of whatever width the panel
/// itself draws at: prose reads better narrower than the panel's outer
/// ceiling allows, the same way a magazine column is narrower than the
/// page.
const BLURB_WRAP: u16 = 66;

/// The narrowest left column that still draws a marker, a key and a value.
const LEFT_MIN: u16 = 40;

/// The narrowest panel that holds a wrapped blurb and a validation list.
const PANEL_MIN: u16 = 50;

/// The panel at the design target, and the ceiling everywhere else.
const PANEL_MAX: u16 = 72;

/// How wide the explanation panel is, and [`None`] when it does not draw.
///
/// 45% of 160 is 72, so the design target from
/// `docs/brainstorming/specs/2026-09-08-lookout-1e-editing-pane-design.md`
/// falls out of the formula rather than being special cased. Below it the
/// clamp to [`PANEL_MAX`] holds the panel at the design target's own width
/// rather than growing it to fill a wider terminal: the left column takes
/// the remainder instead, the same way [`super::flock`]'s own table grows.
/// Above [`LEFT_MIN`] short of `width` the panel cannot hold a wrapped
/// blurb and a validation list beside a left column with room for a
/// marker, a key and a value, and [`pane_lines`] falls back to the single
/// column it always drew.
fn panel_width(width: u16) -> Option<u16> {
    let wanted = (u32::from(width) * 45 / 100) as u16;
    let panel = wanted.clamp(PANEL_MIN, PANEL_MAX);
    (width.saturating_sub(panel) >= LEFT_MIN).then_some(panel)
}

/// The narrowest terminal that draws `LANDS` beside the panel rather than
/// giving way to it. The design target itself: at 160 the frame's own
/// `FIELD / VALUE / LANDS` header and the `FOCUSED` panel share a row, per
/// `docs/lookout/design-files/rulings.md`'s 1e ruling and
/// `docs/brainstorming/specs/2026-09-08-lookout-1e-editing-pane-design.md`'s
/// width table.
const LANDS_WITH_PANEL_MIN: u16 = 160;

/// Whether `LANDS` draws on a terminal `width` columns wide that also draws
/// the panel (`has_panel`).
///
/// `LANDS` and the panel do different jobs: the column is what an operator
/// scans to read every field's cost at once, the panel is a sentence about
/// only the field under the cursor. Where both fit, both draw. Where they
/// cannot, `LANDS` gives way first, since the panel's own impact sentence
/// already names the focused field's cost and nothing else on screen names
/// the column's.
///
/// Always `true` when the panel is absent: nothing else on screen carries
/// cost then, so `LANDS` has no reason to hide.
const fn lands_fits_beside_panel(width: u16, has_panel: bool) -> bool {
    !has_panel || width >= LANDS_WITH_PANEL_MIN
}

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
///
/// `show_lands` is `false` only where the explanation panel is drawn and
/// the terminal is too narrow to hold both beside it (see
/// [`lands_fits_beside_panel`]): the panel still names the focused field's
/// own cost in its impact sentence, so nothing on screen goes unsaid.
/// `false` here behaves as if `width` had fallen under [`FULL_WIDTH`] on its
/// own, freeing what COST would have spent onto VALUE instead.
fn widths(width: u16, show_lands: bool) -> (u16, u16, u16) {
    if show_lands && width >= FULL_WIDTH {
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

/// Masks a secret field's rendered value: `(unset)` passes through, since
/// there is nothing to hide, and anything else becomes `<set>`.
///
/// The one rule every render path in this file applies to a secret before
/// its value reaches the screen, and every one of them calls this rather
/// than spelling the condition again: [`field_line`]'s stored cell and its
/// `old -> new` cell, [`list_line`]'s elements, and both halves of
/// [`pending_edit_line`]. Four inline copies of it agreed with each other
/// until they were routed through here, which is the state a fifth call
/// site would have had to keep up.
fn mask_secret(secret: bool, raw: String) -> String {
    if secret && raw != "(unset)" {
        "<set>".to_owned()
    } else {
        raw
    }
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
    show_lands: bool,
) -> Line<'static> {
    let Some(field) = pane.fields().fields().get(index) else {
        return Line::default();
    };
    let (key_w, value_w, cost_w) = widths(body_width(width), show_lands);
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
        None => mask_secret(field.secret, raw),
    };

    let lock = match pane.lock(&field.key) {
        // Fixed: no surface edits this one, not just this pane.
        Some(Lock::Refused) => '=',
        // Shown only. A Flockfile still writes it, and the cost cell beside
        // it reports what doing so would cost.
        Some(Lock::NoWidget) => '~',
        None => ' ',
    };
    let mut rest = String::from(lock);
    rest.push_str(&fit(&format!("{flag}{}", field.key), key_w));
    let cost_cell = (cost_w > 0).then(|| fit(pane.cost(&field.key).map_or("", cost_label), cost_w));

    // The selected row's own paint: a paper-2 ground the full width of the
    // line, and the mark in column 1 in butter rather than plain text.
    // `palette.ground()` is the same call the flock table's selected row
    // makes; `palette.attention()` is `theme.rs`'s own butter, the same
    // colour an edited value already borrows a few lines below.
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mark_style = if selected {
        palette.attention().patch(ground)
    } else {
        Style::default()
    };
    let mark_span = Span::styled(mark(selected), mark_style);

    // A filed edit, with nothing being typed over it right now: the value
    // cell becomes `old -> new` instead of the stored value alone. Never
    // reached for a locked field, since no key files an edit for one, so
    // `field.editable` is not re-checked here.
    let edited = typing
        .is_none()
        .then(|| pane.edited_value(&field.key))
        .flatten();
    let Some(new_value) = edited else {
        let mut text = rest;
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
        let rest_style = if field.editable {
            Style::default()
        } else {
            palette.muted()
        };
        return Line::from(vec![
            mark_span,
            Span::styled(text, rest_style.patch(ground)),
        ]);
    };
    let new_value = mask_secret(field.secret, new_value);
    let mut spans = vec![mark_span, Span::styled(rest, ground)];
    if value_w > 0 {
        spans.push(Span::styled("  ", ground));
        spans.push(Span::styled(
            fit(&format!("{value} -> {new_value}"), value_w),
            palette.attention().patch(ground),
        ));
    }
    if let Some(cost) = cost_cell {
        spans.push(Span::styled("  ", ground));
        spans.push(Span::styled(cost, ground));
    }
    Line::from(spans)
}

/// The one line the field list reserves under its title: the selected
/// field's own help text while `h` has it open. [`None`] otherwise.
///
/// The close dialog used to draw here too, as the apply menu this pane
/// replaced. It draws over the whole field list instead now
/// ([`draw_pane`]), since it answers a question about the pane's own
/// close rather than a per-field one.
fn top_line(pane: &ConfigPane, palette: Palette) -> Option<(String, Style)> {
    if pane.help_open()
        && let Some(PaneRow::Field(index)) = pane.cursor()
        && let Some(field) = pane.fields().fields().get(index)
    {
        return Some((field.help.clone(), palette.muted()));
    }
    None
}

/// `""` for one, `"S"` for every other count: the plural suffix
/// [`close_dialog_heading`] and [`close_dialog_naming_sentence`] both
/// append to a bare noun.
const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "S" }
}

/// `"NEEDS"` for one, `"NEED"` for every other count: the noun `plural`
/// inflects and the verb agreeing with it are two different words, and
/// every fixture in this file happens to file two edits, which is exactly
/// why a mismatched verb went unnoticed until a real screen showed one.
const fn needs_or_need(count: usize) -> &'static str {
    if count == 1 { "NEEDS" } else { "NEED" }
}

/// The column the heading's right clause starts at, when the row is wide
/// enough to hold both clauses: `docs/lookout/design-files/README.md`'s
/// own mock, which puts `catcher is online, pid 71578` there.
const HEADING_SHEEP_COLUMN: usize = 36;

/// The heading row: the question on the left, the sheep it is about on
/// the right.
///
/// The right clause goes first when `body` cannot hold both. The left
/// clause is the question itself, and the pane's own title band names the
/// sheep too but is dimmed behind the box, which is the whole reason the
/// right clause exists: an operator answering a question that restarts a
/// process should not have to read around the dialog to learn which one.
fn close_dialog_heading_row(dialog: &CloseDialog, body: u16) -> String {
    let left = close_dialog_heading(dialog);
    let right = close_dialog_sheep_clause(dialog);
    let left_w = columns(&left);
    // One space of separation at minimum, however far past the column the
    // left clause runs.
    let gap = HEADING_SHEEP_COLUMN.saturating_sub(left_w).max(1);
    if left_w + gap + columns(&right) > usize::from(body) {
        return left;
    }
    format!("{left}{}{right}", " ".repeat(gap))
}

/// `catcher is online, pid 71578`, or `catcher is online` for a sheep the
/// shepherd runs several of, where no single pid is the answer.
fn close_dialog_sheep_clause(dialog: &CloseDialog) -> String {
    let state = format!("{} is {}", dialog.target_name(), dialog.status());
    match dialog.pid() {
        Some(pid) => format!("{state}, pid {pid}"),
        None => state,
    }
}

/// How many columns `text` occupies, the same count [`fit`] and
/// [`clipped`] measure against.
fn columns(text: &str) -> usize {
    text.chars().map(char_columns).sum()
}

/// The question the heading asks, one of three depending on which half of
/// it fired.
fn close_dialog_heading(dialog: &CloseDialog) -> String {
    match (dialog.unsent(), dialog.parked()) {
        (0, parked) => format!("{parked} FIELD{} ALREADY WAITING", plural(parked)),
        (unsent, 0) => format!(
            "{unsent} EDIT{} {} A RESPAWN",
            plural(unsent),
            needs_or_need(unsent)
        ),
        (unsent, parked) => format!(
            "{unsent} EDIT{} {} A RESPAWN, {parked} FIELD{} ALREADY DID",
            plural(unsent),
            needs_or_need(unsent),
            plural(parked)
        ),
    }
}

/// The unsent fields, named in a sentence and truncated past three:
/// `cwd and err_file take hold when the process starts again.` or `cwd,
/// err_file and 3 more take hold when the process starts again.`
fn close_dialog_naming_sentence(fields: &[String]) -> String {
    let list = match fields {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [first, second, third] => format!("{first}, {second} and {third}"),
        [first, second, rest @ ..] => {
            format!("{first}, {second} and {} more", rest.len())
        }
    };
    let verb = if fields.len() == 1 { "takes" } else { "take" };
    format!("{list} {verb} hold when the process starts again.")
}

/// The reload row's own sentence, one of four off `dialog.reload()` and
/// `dialog.instances()`.
///
/// The `SO_REUSEPORT` caveat rides on both overlap lines: an app with no
/// readiness probe overlaps either way, and needs it exactly as much as a
/// `reuse_port` app does if it binds an address.
fn close_dialog_reload_sentence(dialog: &CloseDialog) -> String {
    let graceful = dialog.graceful_timeout();
    match (dialog.reload(), dialog.instances()) {
        (ReloadKind::Overlap, 1) => {
            "the replacement starts alongside and takes over. No gap, if the app sets \
             SO_REUSEPORT itself."
                .to_owned()
        }
        (ReloadKind::Overlap, _) => {
            "one instance at a time, each replacement alongside the one it replaces. No gap, \
             if the app sets SO_REUSEPORT itself."
                .to_owned()
        }
        (ReloadKind::Serial, 1) => format!(
            "drains it, then starts the replacement. Up to {graceful}, so slower than a \
             restart for the same gap."
        ),
        (ReloadKind::Serial, n) => format!(
            "one instance at a time, each drained before its replacement starts. Up to \
             {graceful} each, 1 of {n} down at a time."
        ),
    }
}

/// One option row, indented and styled the way every other pane line in
/// this file is.
fn close_dialog_option_line(text: String, palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", fit(&text, width)),
        palette.ground(),
    ))
}

/// `"L   reload           "`, aligned with the `R` and `c` rows' own
/// label columns (all three are the same width): what
/// [`close_dialog_reload_lines`] indents a continuation row under.
const RELOAD_LABEL: &str = "L   reload           ";

/// The reload row, wrapped rather than truncated.
///
/// [`fit`] truncates with an ellipsis, which is right for a table cell but
/// wrong here: the sentence's own tail is the `SO_REUSEPORT` caveat
/// (`close_dialog_reload_sentence`'s own doc), the correction the design
/// added after refusing an earlier, uncaveated "No downtime, slower". A
/// truncated row ships exactly the claim that correction exists to
/// prevent. `docs/lookout/design-files/README.md`'s own mock wraps this
/// row onto a continuation line indented under the label instead, for
/// both the overlap and the serial sentence, so this does too, at every
/// width: the box's own interior is a fixed 86 columns regardless of the
/// terminal's, so this wraps even at a comfortable terminal width.
fn close_dialog_reload_lines(
    dialog: &CloseDialog,
    palette: Palette,
    body: u16,
) -> Vec<Line<'static>> {
    let label_w = u16::try_from(RELOAD_LABEL.chars().count()).unwrap_or(0);
    let sentence = close_dialog_reload_sentence(dialog);
    let available = usize::from(body.saturating_sub(label_w));
    let indent = " ".repeat(usize::from(label_w));
    wrap(&sentence, available)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let text = if index == 0 {
                format!("{RELOAD_LABEL}{chunk}")
            } else {
                format!("{indent}{chunk}")
            };
            close_dialog_option_line(text, palette, body)
        })
        .collect()
}

/// What a row of the dialog is, so that a terminal too short for all of
/// them sheds the ones it can afford to lose.
///
/// Ordered by what losing the row costs, cheapest last: [`shed_dialog_rows`]
/// drops the greatest first. [`DialogRow::Key`] is the floor and is never
/// dropped, since a key an operator cannot see is an answer they cannot
/// give, and `esc` no longer writes on its own.
///
/// [`DialogRow::Naming`] goes before [`DialogRow::Continuation`], which
/// is the call this order exists to record. Losing the naming sentence
/// costs a whole, self-contained row whose fields are also named by the
/// heading's count and by the pane's own pending section behind the box.
/// Losing a continuation costs the second half of a row that is still on
/// screen: the reload sentence's tail is the `SO_REUSEPORT` caveat, which
/// is the condition on the only cost claim this dialog makes, and
/// `docs/lookout/design-files/rulings.md` refused the frame's own
/// uncaveated `No downtime, slower` over exactly that. At 90x8 this
/// ordering keeps the sentence whole.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DialogRow {
    /// `R`, `L`, `c` and `esc`: the four rows that name a key.
    Key,
    /// The heading, naming both halves and the sheep.
    Heading,
    /// The reload sentence's own wrapped tail.
    Continuation,
    /// The sentence naming the fields a respawn is what applies.
    Naming,
    /// `Everything else you changed is already live.`
    Live,
    /// A separator.
    Blank,
}

/// The dialog's rows: what a terminal under [`BOX_FLOOR`] columns gets
/// full width, and what the boxed form draws inside its own border.
///
/// `now` is the caller's own clock, against which the `esc` row states
/// what is left of [`CONFIRM_EXPIRY`] since `dialog.at()`: seconds and a
/// ten-cell gauge, saying the same thing twice on purpose, since the
/// design's own rule is that colour and glyph never carry anything the
/// words do not.
#[must_use]
pub(super) fn close_dialog_lines(
    dialog: &CloseDialog,
    palette: Palette,
    width: u16,
    now: Instant,
) -> Vec<Line<'static>> {
    close_dialog_rows(dialog, palette, width, now)
        .into_iter()
        .map(|(_, line)| line)
        .collect()
}

/// [`close_dialog_lines`], each row carrying what it would cost to lose.
fn close_dialog_rows(
    dialog: &CloseDialog,
    palette: Palette,
    width: u16,
    now: Instant,
) -> Vec<(DialogRow, Line<'static>)> {
    let body = body_width(width);
    // `band`, not `attention`: every other band on this dashboard reverses
    // its role's colour rather than merely tinting the text, and 12a's own
    // rule is that colour is always redundant with the words, so `NO_COLOR`
    // has to lose decoration, never information. `attention` alone drops
    // both under `NO_COLOR`, since it carries no modifier at all.
    let mut lines = vec![(
        DialogRow::Heading,
        Line::from(Span::styled(
            format!("  {}", fit(&close_dialog_heading_row(dialog, body), body)),
            palette.band(Role::Butter),
        )),
    )];
    if dialog.unsent() > 0 {
        let sentence = close_dialog_naming_sentence(dialog.unsent_fields());
        lines.push((
            DialogRow::Naming,
            close_dialog_option_line(sentence, palette, body),
        ));
        if dialog.live() > 0 {
            lines.push((
                DialogRow::Live,
                close_dialog_option_line(
                    "Everything else you changed is already live.".to_owned(),
                    palette,
                    body,
                ),
            ));
        }
    }
    lines.push((DialogRow::Blank, Line::from(Span::raw(""))));
    lines.push((
        DialogRow::Key,
        close_dialog_option_line(
            format!(
                "R   restart now      stop, then start. The stop takes up to {}.",
                dialog.kill_timeout()
            ),
            palette,
            body,
        ),
    ));
    lines.extend(
        close_dialog_reload_lines(dialog, palette, body)
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let kind = if index == 0 {
                    DialogRow::Key
                } else {
                    DialogRow::Continuation
                };
                (kind, line)
            }),
    );
    lines.push((
        DialogRow::Key,
        close_dialog_option_line(
            "c   continue         write them and leave it running. They wait for a respawn."
                .to_owned(),
            palette,
            body,
        ),
    ));
    lines.push((DialogRow::Blank, Line::from(Span::raw(""))));
    let elapsed = now.saturating_duration_since(dialog.at());
    let remaining = CONFIRM_EXPIRY.saturating_sub(elapsed);
    lines.push((
        DialogRow::Key,
        close_dialog_option_line(
            format!(
                "esc  keep editing, write nothing   \u{b7}   this prompt expires in {}s {}",
                remaining.as_secs(),
                cell::gauge(
                    u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX),
                    Some(u64::try_from(CONFIRM_EXPIRY.as_millis()).unwrap_or(u64::MAX)),
                    10
                )
            ),
            palette,
            body,
        ),
    ));
    lines
}

/// Drops rows from `rows` until it is no taller than `height`, in
/// [`DialogRow`]'s own order: the blank separators, then the two
/// explanatory sentences, then the reload sentence's wrapped tail, then
/// the heading.
///
/// The four [`DialogRow::Key`] rows survive every height, which is what
/// the design means by flooring the borderless form: below six rows the
/// field list this is drawn over cannot render either, and an operator
/// with no key on screen has no way to answer and, since `esc` stopped
/// writing on its own, no way to write. Shorter still than the four is a
/// terminal `view::draw` refuses outright, but [`Buffer`] indexes with a
/// panic rather than a clip, so the truncation at the end is the one that
/// keeps a resize from taking the dashboard down.
///
/// A shed continuation ends its sentence early, so the row above it takes
/// the ellipsis [`fit`] leaves on a cell cut for width. What the screen
/// cannot show, it says, the same rule the bleats feed follows when it
/// counts the lines it discarded.
///
/// `width` is the whole row's, [`GUTTER`] included, since that is what a
/// marked row must still fit inside.
fn shed_dialog_rows(rows: &mut Vec<(DialogRow, Line<'static>)>, height: u16, width: u16) {
    let height = usize::from(height);
    while rows.len() > height {
        let sheddable = rows
            .iter()
            .enumerate()
            .filter(|(_, (kind, _))| *kind != DialogRow::Key)
            .max_by_key(|(index, (kind, _))| (*kind, *index))
            .map(|(index, _)| index);
        let Some(index) = sheddable else { break };
        let cut = rows.remove(index).0 == DialogRow::Continuation;
        // The last continuation is always the one shed, so the row above
        // it is the rest of the same sentence: an earlier continuation, or
        // the `L` row itself.
        if cut && let Some((_, line)) = index.checked_sub(1).and_then(|above| rows.get_mut(above)) {
            mark_cut(line, width);
        }
    }
    rows.truncate(height);
}

/// Ends `line` with an ellipsis, inside `width` columns.
///
/// A no-op on a row that already carries one, which a row [`fit`] cut for
/// width does: one marker says the row was cut, and two say nothing more.
fn mark_cut(line: &mut Line<'static>, width: u16) {
    let Some(span) = line.spans.last_mut() else {
        return;
    };
    let mut text = span.content.trim_end().to_owned();
    if text.ends_with('\u{2026}') {
        return;
    }
    while columns(&text) + 1 > usize::from(width) {
        text.pop();
    }
    text.push('\u{2026}');
    span.content = text.into();
}

/// The list sub-screen: one array field's elements, and a row to add one on.
///
/// Values are drawn, unlike an env row: an array arrives with the
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
                    mask_secret(secret, raw)
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

/// The pane's own reverse-video summary band: which target's config this
/// is, and how many edits are filed and unsent.
///
/// The only line that names the target: [`title_line`] duplicated it
/// directly underneath, and the design's own row allocation gives the
/// sheep or dog's name exactly one row.
///
/// Never the shepherd's own word for a field that is written and parked:
/// `pending` is what `shep flock`'s CFG column and this same pane's `!`
/// flag already mean, and a second meaning for the same word on the
/// screen an operator moves to next is exactly the confusion this counts
/// around instead.
///
/// Nothing rather than a zero, the same rule `view::detail`'s `cfg` cell
/// follows: an untouched pane names no count at all, not `0 edits`.
fn title_band_line(pane: &ConfigPane, palette: Palette, width: u16) -> Line<'static> {
    let kind = match pane.target() {
        PaneTarget::Sheep { .. } => "sheep config",
        PaneTarget::Dog { .. } => "dog config",
    };
    let mut text = format!("{}  ({kind})", pane.target().name());
    let count = pane.edits().len();
    match count {
        0 => {}
        1 => text.push_str("  1 edit"),
        n => text.push_str(&format!("  {n} edits")),
    }
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
fn column_header_line(palette: Palette, width: u16, show_lands: bool) -> Line<'static> {
    let (key_w, value_w, cost_w) = widths(body_width(width), show_lands);
    let mut text = String::from("  ");
    text.push_str(&fit("FIELD", key_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit("VALUE", value_w));
    }
    if cost_w > 0 {
        text.push_str("  ");
        text.push_str(&fit("LANDS", cost_w));
    }
    Line::from(Span::styled(text, palette.muted()))
}

/// The pane's own marker legend, drawn once at the foot rather than
/// repeated per group: the status bar's own hint already names every key
/// this pane answers to (`view::status::pane_hint`), so this line explains
/// the glyphs instead, adapted from the design's own legend
/// (`docs/lookout/design-files/README.md`, the 1e frame's row 45) to the
/// four the pane actually draws (see [`field_line`]).
///
/// Names no key: `status.rs`'s `esc close` is the one place that wording
/// lives, and a second copy here would only need to be kept in sync with
/// it.
///
/// `* yours` rather than `* overridden`, because the status bar has said
/// `* yours` on every screen that draws the glyph since before this pane
/// existed. One glyph with two words for it was visible on one screen at
/// once.
fn legend_line(palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                "= read-only, set it in the Flockfile   ~ no widget for this shape   \
                 ! parked, awaits a respawn   * yours   -> changed by you, not yet written",
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
/// one still visible in the active group, and masks a secret field's
/// values the same way that cell does: nothing today reaches either path,
/// since the Flockfile schema marks no field secret and a dog never
/// carries a group, but a dog's own schema can mark one and this section
/// draws every group's edits regardless of which is active. An env edit
/// shows only that it is set or removed, since the pane never holds an
/// env value to show either side of.
fn pending_edit_line(
    pane: &ConfigPane,
    edit: &PaneEdit,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let text = match edit {
        PaneEdit::Set { key, .. } => {
            let secret = pane.fields().by_key(key).is_some_and(|field| field.secret);
            let old = pane.display_value(key);
            let new = pane.edited_value(key).unwrap_or_default();
            let old = mask_secret(secret, old);
            let new = mask_secret(secret, new);
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
    show_lands: bool,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if budget == 0 {
        return lines;
    }
    // The cursor's own row has to draw somewhere in this budget, the same
    // rule `body_from`/`cursor_only` hold for a field. Below the floor a
    // full section (headers, blanks, every key) would cut it along with
    // everything else, so at the floor this draws that one row alone and
    // nothing else, rather than truncating from the bottom and losing
    // whichever row the truncation happens to reach last.
    let pending_len = if pane.edits().is_empty() {
        0
    } else {
        2 + pane.edits().len()
    };
    let env_len = 2 + pane.env_key_names().len() + 1;
    if pending_len + env_len > budget
        && let Some(PaneRow::Env(_) | PaneRow::AddEnv) = pane.cursor()
    {
        return vec![cursor_env_row_line(pane, width, palette, show_lands)];
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
    for (index, name) in pane.env_key_names().iter().enumerate() {
        if remaining == 0 {
            break;
        }
        let selected = pane.cursor() == Some(PaneRow::Env(index));
        lines.push(env_row_line(
            pane, name, selected, width, palette, show_lands,
        ));
        remaining -= 1;
    }
    if remaining > 0 {
        let selected = pane.cursor() == Some(PaneRow::AddEnv);
        lines.push(add_env_row_line(pane, selected, width, palette, show_lands));
    }
    lines
}

/// The cursor's own row, alone: [`pending_and_env_lines`]'s floor fallback,
/// [`cursor_only`]'s twin for the env rows below the field list. Panics if
/// the cursor is not on [`PaneRow::Env`] or [`PaneRow::AddEnv`]; every
/// caller has already checked.
fn cursor_env_row_line(
    pane: &ConfigPane,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    match pane.cursor() {
        Some(PaneRow::Env(index)) => {
            let name = pane
                .env_key_names()
                .get(index)
                .expect("the cursor names a real env row");
            env_row_line(pane, name, true, width, palette, show_lands)
        }
        Some(PaneRow::AddEnv) => add_env_row_line(pane, true, width, palette, show_lands),
        Some(PaneRow::Field(_)) | None => {
            unreachable!("callers check the cursor is on an env row first")
        }
    }
}

/// One env key's own row: the selection mark, the key, and `(set)`.
///
/// Every key reads `(set)`, never its own value: `SheepConfigView::new`
/// clears `config.env` before this pane ever sees it, so no value for any
/// key reaches here, and this is the one word every row can honestly draw.
fn env_row_line(
    pane: &ConfigPane,
    name: &str,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    let (key_w, value_w, _) = widths(body_width(width), show_lands);
    let typing = selected
        .then(|| pane.env_typing())
        .flatten()
        .filter(|typing| typing.key() == Some(name))
        .map(EnvTyping::buffer);
    let value = typing.map_or_else(|| "(set)".to_owned(), |buffer| format!("{buffer}\u{258f}"));
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mark_style = if selected {
        palette.attention().patch(ground)
    } else {
        Style::default()
    };
    let mark_span = Span::styled(mark(selected), mark_style);
    // A single blank standing in for `field_line`'s lock glyph: an env row
    // has none, but `GUTTER` already spent that column out of
    // `body_width`, so this row has to spend it too rather than draw one
    // column short.
    //
    // Inside the key cell itself, `field_line` reserves a first character
    // for its flag (`!`/`*`/` `) before the key text; an env row has no
    // flag, but the reservation is the same cell `fit` pads to `key_w`, so
    // this row has to spend that character too, or its key text starts one
    // column left of every field's.
    let mut text = String::from(" ");
    text.push_str(&fit(&format!(" {name}"), key_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit(&value, value_w));
    }
    Line::from(vec![mark_span, Span::styled(text, ground)])
}

/// The `+ add a key` row: the selection mark, and either the caption or,
/// while it is being typed, the whole `KEY=value` buffer.
fn add_env_row_line(
    pane: &ConfigPane,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    let (key_w, _, _) = widths(body_width(width), show_lands);
    let typing = selected
        .then(|| pane.env_typing())
        .flatten()
        .filter(|typing| typing.key().is_none())
        .map(EnvTyping::buffer);
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mark_style = if selected {
        palette.attention().patch(ground)
    } else {
        Style::default()
    };
    let mark_span = Span::styled(mark(selected), mark_style);
    let (caption, muted) = match typing {
        Some(buffer) => (format!("{buffer}\u{258f}"), false),
        None => ("+ add a key".to_owned(), true),
    };
    // Same key-cell reservation as [`env_row_line`]: a leading space stands
    // in for `field_line`'s flag character so `+ add a key` and every field
    // above it start their text in the same column.
    let mut text = String::from(" ");
    text.push_str(&fit(&format!(" {caption}"), key_w));
    let style = if muted {
        palette.muted()
    } else {
        Style::default()
    };
    Line::from(vec![mark_span, Span::styled(text, style.patch(ground))])
}

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
        if let Some((text, style)) = top_line(pane, palette) {
            lines.push(Line::from(Span::styled(
                format!("  {}", fit(&text, body_width(width))),
                style,
            )));
        } else {
            lines.push(column_header_line(palette, left_width, show_lands));
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

/// The columns `line`'s spans occupy, added rather than measured against a
/// fixed cell: a blank separator or an unselected section header is
/// shorter than the left column's own width on its own, and only the
/// panel's own starting column cares where it ends.
fn line_columns(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(char_columns)
        .sum()
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
    // `h`'s help text, on the line under the title. Subtracted from the
    // budget rather than appended, per `body_from`'s own doc on markers.
    // See `top_line`.
    if let Some((text, style)) = top_line(pane, palette)
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
    let footer = dog_footer_text(pane, body_budget);
    if footer.is_some() {
        body_budget -= 1;
    }
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

/// What an operator would call `field`'s shape: not the schema keyword, the
/// grammar the widget and [`super::super::validation`] already treat it as.
fn type_label(field: &Field) -> &'static str {
    match field.value_kind {
        Some(ValueKind::UpDuration) => return "duration",
        Some(ValueKind::MemSize) => return "memory size",
        None => {}
    }
    match &field.kind {
        FieldKind::Bool => "bool",
        FieldKind::Integer => "integer",
        FieldKind::Text | FieldKind::Suggested(_) => "text",
        FieldKind::Choice(_) => "choice",
        FieldKind::Map => "map",
        FieldKind::List(_) => "list",
        FieldKind::Opaque => "opaque",
    }
}

/// The glyph, the word [`cost_label`] already prints in the LANDS/COST
/// cell, and one sentence naming what that word costs the operator, keyed
/// on [`ApplyGroup`] the same way [`cost_label`] is.
///
/// `●` for a change the running sheep picks up on its own; `▲` for one
/// that waits on a start the operator has to choose the timing of. Both
/// are checked against the same East-Asian-width rule `flock.rs:46`
/// applies to `mark`'s own glyph, in this module's own tests: a
/// double-width rendering would shift every column after it.
fn impact_tag(group: ApplyGroup) -> (char, &'static str, &'static str) {
    match group {
        ApplyGroup::Live => (
            '\u{25cf}',
            "now",
            "the running sheep picks it up without stopping",
        ),
        ApplyGroup::NextSpawn => (
            '\u{25b2}',
            "next start",
            "takes effect the next time the sheep starts",
        ),
        ApplyGroup::Structural => (
            '\u{25b2}',
            "read-only",
            "shep never writes this; set it in the Flockfile instead",
        ),
        ApplyGroup::NeedsRespawn | _ => {
            ('\u{25b2}', "respawn", "the sheep must stop and start again")
        }
    }
}

/// Wraps `text` at `width` columns, breaking on spaces. A single word
/// longer than `width` is placed on its own (overlong) line rather than
/// split mid-word: [`Field::help`] is prose, not data, and a rare overlong
/// word is a smaller wrong than a hyphen this module invented.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_owned()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_w: usize = word.chars().map(char_columns).sum();
        let current_w: usize = current.chars().map(char_columns).sum();
        if current.is_empty() {
            current.push_str(word);
        } else if current_w + 1 + word_w <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// `text`, truncated (never padded) to at most `width` columns, marking a
/// cut with a trailing `…` the way [`fit`] does.
///
/// Not [`fit`]: the panel's rows are prose, not a fixed-width table cell,
/// so a short value stays short rather than growing padded trailing spaces
/// across the column. This is the guard the panel's own width tests check:
/// a row built from a live value or a schema-authored sentence cannot push
/// the panel past its own column, even though nothing in today's schemas
/// is long enough to exercise the truncation itself.
fn clipped(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let columns: usize = text.chars().map(char_columns).sum();
    if columns <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_columns(c);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
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
        //
        // Two calls, not one: `Buffer::set_style` (ratatui-core 0.1.2,
        // `buffer/buffer.rs:405`) patches a cell rather than replacing it,
        // so a single `palette.muted()` call would leave the title band's
        // reverse video and the selected row's own ground sitting under the
        // new ink. `Style::reset()` clears both back to the terminal's own
        // default first; `palette.muted()` then repaints the one ink the
        // dialog leaves the pane in. Under `NO_COLOR` the second call is a
        // no-op (`Palette::muted` has no colour to give), so only the reset
        // runs and the pane behind goes completely flat, which is the right
        // outcome there: the border and the reverse-video heading are what
        // carry the separation on their own.
        buffer.set_style(area, Style::reset());
        buffer.set_style(area, app.palette().muted());
        draw_close_dialog(dialog, app.palette(), app.now(), area, buffer);
    }
}

/// Whether a dialog `width` columns wide draws boxed, or gives way to the
/// borderless form.
const fn dialog_is_boxed(width: u16) -> bool {
    width >= BOX_FLOOR
}

/// The dialog on top of the muted pane: boxed at [`BOX_FLOOR`] and above,
/// full width with no border below it, and full width with no border at
/// any width when the box is taller than the rows there are.
///
/// A box cannot shed rows the way the borderless form can, since its
/// border pair is what makes it a box, and half a box is worse than none.
/// So a terminal too short for the whole box gives way to the borderless
/// form, which is the same answer the width rule already gives one column
/// under [`BOX_FLOOR`].
fn draw_close_dialog(
    dialog: &CloseDialog,
    palette: Palette,
    now: Instant,
    area: Rect,
    buffer: &mut Buffer,
) {
    if dialog_is_boxed(area.width) {
        let lines = close_dialog_lines(dialog, palette, BOX_WIDTH, now);
        let box_height = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2);
        if box_height <= area.height {
            draw_boxed_close_dialog(&lines, palette, area, buffer);
            return;
        }
    }
    draw_borderless_close_dialog(dialog, palette, now, area, buffer);
}

/// The full-width, borderless form: bottom-anchored over the field list,
/// the same rows a terminal under [`BOX_FLOOR`] always drew before this
/// task, so a gallery scene one column below the floor still gets the form
/// it exists to show rather than a clipped box.
///
/// [`shed_dialog_rows`] is what keeps this inside `area`: a narrow
/// terminal wraps the reload sentence over more rows, so the form is
/// tallest exactly where there is least room for it.
fn draw_borderless_close_dialog(
    dialog: &CloseDialog,
    palette: Palette,
    now: Instant,
    area: Rect,
    buffer: &mut Buffer,
) {
    let mut rows = close_dialog_rows(dialog, palette, area.width, now);
    shed_dialog_rows(&mut rows, area.height, area.width);
    let top = area.y
        + area
            .height
            .saturating_sub(u16::try_from(rows.len()).unwrap_or(0));
    for (offset, (_, line)) in rows.iter().enumerate() {
        let offset = u16::try_from(offset).unwrap_or(0);
        blank_row(buffer, area.x, top + offset, area.width);
        buffer.set_line(area.x, top + offset, line, area.width);
    }
}

/// The boxed form: [`BOX_WIDTH`] cells wide, centred in `area`, its rows
/// vertically centred too.
///
/// `lines` comes from the caller, which has already measured them against
/// `area.height` to decide this form fits at all.
fn draw_boxed_close_dialog(
    lines: &[Line<'static>],
    palette: Palette,
    area: Rect,
    buffer: &mut Buffer,
) {
    let rows = u16::try_from(lines.len()).unwrap_or(0);
    let box_height = rows + 2;
    let margin = area.width.saturating_sub(BOX_WIDTH + 2) / 2;
    let box_x = area.x + margin;
    let box_y = area.y + area.height.saturating_sub(box_height) / 2;
    let line_style = palette.line();

    buffer.set_string(
        box_x,
        box_y,
        format!(
            "{BOX_TOP_LEFT}{}{BOX_TOP_RIGHT}",
            BOX_TOP.to_string().repeat(usize::from(BOX_WIDTH))
        ),
        line_style,
    );
    for (offset, line) in lines.iter().enumerate() {
        let offset = u16::try_from(offset).unwrap_or(0);
        let y = box_y + 1 + offset;
        buffer.set_string(box_x, y, BOX_LEFT.to_string(), line_style);
        blank_row(buffer, box_x + 1, y, BOX_WIDTH);
        buffer.set_line(box_x + 1, y, line, BOX_WIDTH);
        buffer.set_string(box_x + 1 + BOX_WIDTH, y, BOX_RIGHT.to_string(), line_style);
    }
    buffer.set_string(
        box_x,
        box_y + 1 + rows,
        format!(
            "{BOX_BOTTOM_LEFT}{}{BOX_BOTTOM_RIGHT}",
            BOX_BOTTOM.to_string().repeat(usize::from(BOX_WIDTH))
        ),
        line_style,
    );
}

/// `width` cells of plain space at `(x, y)`, reset back to the terminal's
/// own default: the dialog itself is never muted, only the pane behind it.
///
/// [`Buffer::set_line`] only ever writes as many cells as its `Line` carries
/// content for, so a blank separator row (`Line::from(Span::raw(""))`,
/// [`close_dialog_lines`]'s own two of them) writes nothing and would leave
/// whatever the field list drew there showing through, muted, in the
/// middle of what is meant to read as a solid dialog. Called ahead of every
/// row this module draws the dialog's own lines into, boxed or not.
fn blank_row(buffer: &mut Buffer, x: u16, y: u16, width: u16) {
    buffer.set_string(x, y, " ".repeat(usize::from(width)), Style::reset());
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::MIN_TERM_WIDTH;
    use super::super::fixtures;
    use super::super::flock::MIN_HEIGHT;
    use super::*;
    use crate::lookout::app::{Effect, KeyPress, Msg};
    use crate::lookout::frames::render_text;
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

    /// Bounded on purpose. A frame-wide `contains("respawn")` passes off
    /// 1e's own legend row, which is drawn underneath this dialog and says
    /// the word. Assert on the dialog's rows, never on the frame.
    #[test]
    fn the_dialog_names_both_halves_in_its_heading() {
        let dialog = fixtures::close_dialog_with(2, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let heading = text_of(&lines)[0].trim().to_string();
        assert!(
            heading.starts_with("2 EDITS NEED A RESPAWN, 1 FIELD ALREADY DID"),
            "{heading:?}"
        );
    }

    /// The other half of the heading, and the only place the dialog says
    /// which sheep it is about: the pane's own title band says so too, but
    /// it is dimmed behind the box by the time this question is asked.
    #[test]
    fn the_heading_names_the_sheep_its_state_and_its_pid() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let heading = text_of(&lines)[0].trim().to_string();
        assert!(heading.ends_with("web is online, pid 71578"), "{heading:?}");
    }

    /// The other half of `running_state`'s answer: several instances name
    /// no one pid, so the clause names the sheep and its state and stops
    /// there, rather than trailing a `pid` with nothing after it.
    #[test]
    fn a_sheep_with_no_single_pid_gets_a_heading_that_names_none() {
        let dialog = fixtures::close_dialog_without_a_pid();
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let heading = text_of(&lines)[0].trim().to_string();
        assert!(heading.ends_with("web is online"), "{heading:?}");
        assert!(!heading.contains("pid"), "{heading:?}");
    }

    /// The right clause is the first thing to go when the row cannot hold
    /// both: the left clause is the question itself. Swept at every width
    /// the dialog draws at, so a clause that overran the border or
    /// collided with the question would show up as a row wider than its
    /// own body.
    #[test]
    fn a_narrow_heading_drops_the_sheep_and_keeps_the_question() {
        for width in [MIN_TERM_WIDTH, 40, 51, 60, 89, BOX_WIDTH, 120, 160] {
            let dialog = fixtures::close_dialog_with(2, 1);
            let lines = close_dialog_lines(&dialog, fixtures::plain(), width, dialog.at());
            let heading = text_of(&lines)[0].clone();
            assert!(
                visible_width(&heading) <= usize::from(width),
                "{width}: {heading:?}"
            );
            let trimmed = heading.trim();
            assert!(
                trimmed.starts_with("2 EDITS") || trimmed.starts_with('2'),
                "the question survives at {width}: {heading:?}"
            );
            // 43 for the question, 2 for the gutter, 1 for the gap and 24
            // for `web is online, pid 71578`: the clause draws from 70
            // columns up and is gone below that, never truncated.
            let named = heading.contains("web is online, pid 71578");
            assert_eq!(named, width >= 70, "{width}: {heading:?}");
        }
    }

    /// The noun and the verb are two different words `plural` and
    /// `needs_or_need` each inflect on their own; every other fixture in
    /// this file files two edits, which is exactly why a verb that never
    /// agreed with a singular subject went unnoticed until a real screen
    /// showed one.
    #[test]
    fn a_single_edit_gets_a_singular_verb() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        assert!(
            text_of(&lines)[0]
                .trim()
                .starts_with("1 EDIT NEEDS A RESPAWN"),
            "{:?}",
            text_of(&lines)[0]
        );
    }

    #[test]
    fn a_serial_reload_does_not_promise_no_gap() {
        let dialog = fixtures::close_dialog_reloading(ReloadKind::Serial, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let reload = fixtures::row_starting_with(&lines, "L");
        assert!(reload.contains("slower than a restart"), "{reload}");
        assert!(!reload.contains("No gap"), "{reload}");
    }

    #[test]
    fn an_overlapping_reload_carries_the_reuse_port_caveat() {
        let dialog = fixtures::close_dialog_reloading(ReloadKind::Overlap, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let reload = fixtures::row_starting_with(&lines, "L");
        assert!(
            reload.contains("if the app sets SO_REUSEPORT itself"),
            "{reload}"
        );
    }

    /// The caveat's own last word, not a `contains` on a prefix of it: a
    /// row truncated with `fit`'s ellipsis would still pass
    /// `contains("No gap")`, which is exactly the bug this test exists to
    /// catch. Every width here is one the box or the borderless form
    /// actually draws at (the box's own interior is a fixed [`BOX_WIDTH`]
    /// regardless of the terminal, so a wide terminal still wraps).
    #[test]
    fn the_reload_sentence_wraps_rather_than_truncates_at_every_width() {
        for width in [BOX_WIDTH, BOX_FLOOR - 1, MIN_TERM_WIDTH, 160] {
            for kind in [ReloadKind::Overlap, ReloadKind::Serial] {
                for instances in [1, 3] {
                    let dialog = fixtures::close_dialog_reloading(kind, instances);
                    let sentence = close_dialog_reload_sentence(&dialog);
                    let last_word = sentence.split_whitespace().next_back().unwrap();
                    let lines = close_dialog_lines(&dialog, fixtures::plain(), width, dialog.at());
                    let joined = text_of(&lines).join(" ");
                    assert!(
                        joined.contains(last_word),
                        "width {width}, {kind:?}, {instances} instances: {joined}"
                    );
                }
            }
        }
    }

    /// `docs/terminology.md:20`. The frame calls instances lambs; four
    /// places in the bundle do.
    #[test]
    fn no_line_calls_an_instance_a_lamb() {
        for kind in [ReloadKind::Overlap, ReloadKind::Serial] {
            for instances in [1, 3] {
                let dialog = fixtures::close_dialog_reloading(kind, instances);
                let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
                for row in text_of(&lines) {
                    assert!(!row.contains("lamb"), "{row}");
                }
            }
        }
    }

    #[test]
    fn the_restart_row_states_the_sheeps_own_kill_timeout() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let restart = fixtures::row_starting_with(&lines, "R");
        assert!(restart.contains("5s"), "{restart}");
    }

    /// The sentence draws when the filed set holds a live field alongside
    /// the one that needs a respawn, so an operator reading the dialog is
    /// not left thinking nothing else they changed took effect.
    #[test]
    fn everything_else_is_already_live_draws_beside_a_live_edit() {
        let dialog = fixtures::close_dialog_with_live_edit(true);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        assert!(
            text_of(&lines)
                .iter()
                .any(|line| line.contains("Everything else you changed is already live")),
            "{:?}",
            text_of(&lines)
        );
    }

    /// The other direction: a filed set that is entirely `cwd` (needs a
    /// respawn, nothing else) draws no such claim. A sentence that always
    /// draws would pass the test above without saying anything.
    #[test]
    fn everything_else_is_already_live_does_not_draw_with_nothing_else_filed() {
        let dialog = fixtures::close_dialog_with_live_edit(false);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        assert!(
            !text_of(&lines)
                .iter()
                .any(|line| line.contains("Everything else you changed is already live")),
            "{:?}",
            text_of(&lines)
        );
    }

    /// The countdown is redundant on purpose: the seconds and the gauge say
    /// the same thing twice, so both have to move together as `now`
    /// advances toward `CONFIRM_EXPIRY`.
    #[test]
    fn the_gauge_shortens_as_now_advances() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let fresh = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let fresh_row = fixtures::row_starting_with(&fresh, "esc");
        let fresh_filled = fresh_row.matches('\u{2588}').count();

        let halfway = dialog.at() + CONFIRM_EXPIRY / 2;
        let later = close_dialog_lines(&dialog, fixtures::plain(), 120, halfway);
        let later_row = fixtures::row_starting_with(&later, "esc");
        let later_filled = later_row.matches('\u{2588}').count();

        assert_eq!(fresh_filled, 10, "{fresh_row}");
        assert_eq!(later_filled, 5, "{later_row}");
    }

    #[test]
    fn the_box_draws_at_its_floor_and_not_one_column_below() {
        assert!(dialog_is_boxed(BOX_FLOOR));
        assert!(!dialog_is_boxed(BOX_FLOOR - 1));
    }

    #[test]
    fn the_borderless_form_spans_the_whole_width_and_never_clips() {
        let rendered = fixtures::render_dialog(89, 48);
        let heading = fixtures::row_containing(&rendered, "NEEDS A RESPAWN");
        assert!(
            !heading.contains('▐'),
            "no border below the floor: {heading}"
        );
        assert!(
            visible_width(&heading) <= 89,
            "clipped or overran: {heading}"
        );
    }

    /// Every size the dashboard claims to support, through the real
    /// `view::draw` rather than a pane-local fixture: the fixture hands
    /// the pane a `Rect` as tall as the terminal, and the rows the dialog
    /// overran were the ones `draw` keeps back for the title band and the
    /// status bar.
    ///
    /// [`Buffer`]'s own `Index` panics rather than clipping, so a dialog
    /// taller than the rows it was given takes the whole dashboard down
    /// with it. Both forms are swept: the box gives way to the borderless
    /// form when it cannot fit, and the borderless form sheds rows.
    ///
    /// The four rows that name a key are what makes the answer reachable,
    /// and `esc` no longer writes on its own, so a dialog missing one of
    /// them leaves an operator with no way to answer and no way to write.
    #[test]
    fn the_dialog_fits_every_size_the_dashboard_supports_and_stays_answerable() {
        for width in [MIN_TERM_WIDTH, 40, 51, 89, 90, 120, 160] {
            for height in MIN_HEIGHT..=24 {
                let rendered = render_text(&fixtures::render_dialog(width, height));
                for needle in ["restart now", "reload", "continue", "keep editing"] {
                    assert!(
                        rendered.contains(needle),
                        "{width}x{height} lost {needle:?}:\n{rendered}"
                    );
                }
                // The frame above hides a one-row overrun: the status bar
                // is drawn after the pane and repaints the row a dialog
                // one too tall reached into. This draws the pane alone
                // into a buffer of exactly its own rows, where the same
                // overrun is the panic it really is.
                let _ = fixtures::draw_pane_with_dialog(width, height);
            }
        }
    }

    /// The reload sentence is one sentence however many rows it takes,
    /// and its tail is the `SO_REUSEPORT` caveat the rulings refused the
    /// frame's own uncaveated copy over. A continuation shed for height
    /// ends the row mid-clause (`No gap, if the app`, captured at 90x8),
    /// which is the same half of the sentence going missing that the
    /// width fix already dealt with once.
    ///
    /// Either the sentence survives whole or the cut is marked. Asserted
    /// on the joined rows, never on a prefix: a `contains` on the first
    /// row passes on exactly the broken output.
    #[test]
    fn a_shed_reload_continuation_leaves_the_cut_marked() {
        let app = fixtures::app_with_close_dialog();
        let dialog = app.close_dialog().expect("the dialog is up");
        let whole = close_dialog_reload_sentence(dialog);
        for width in [MIN_TERM_WIDTH, 40, 51, 89, 90, 120, 160] {
            for height in MIN_HEIGHT..=24 {
                let rendered = render_text(&fixtures::render_dialog(width, height));
                let rows = reload_rows(&rendered);
                let joined = rows.join(" ");
                // Either the whole sentence is there, or some row says it
                // was cut: a shed continuation marks the row above it, and
                // a word longer than the column marks its own row. Never a
                // prefix check, which passes on the broken output.
                assert!(
                    joined.contains(&whole) || rows.iter().any(|row| row.ends_with('\u{2026}')),
                    "{width}x{height}: the caveat went missing unmarked: {rows:?}"
                );
            }
        }
    }

    /// The reload sentence's own rows in `frame`: the `L` row and the
    /// continuations indented under it, up to the `c` row that ends them.
    ///
    /// Read from inside the border when there is one, since the muted
    /// pane behind the box keeps drawing to the right of it.
    ///
    /// # Panics
    ///
    /// If no `L` row is on screen, which every size draws one of.
    #[track_caller]
    fn reload_rows(frame: &str) -> Vec<String> {
        let mut rows = Vec::new();
        for line in frame.lines() {
            let text = dialog_interior(line);
            if rows.is_empty() {
                if text.starts_with("L   reload") {
                    rows.push(text);
                }
                continue;
            }
            if text.starts_with("c   continue") || text.is_empty() {
                break;
            }
            rows.push(text);
        }
        assert!(!rows.is_empty(), "no reload row in:\n{frame}");
        rows
    }

    /// One frame row's dialog content: what the box holds, or the whole
    /// row when the borderless form is drawn.
    fn dialog_interior(line: &str) -> String {
        let inside = match (line.find(BOX_LEFT), line.rfind(BOX_RIGHT)) {
            (Some(left), Some(right)) if left < right => &line[left + BOX_LEFT.len_utf8()..right],
            _ => line,
        };
        inside.trim().to_owned()
    }

    /// Dimming changes style and leaves every character alone, so a test
    /// that asserts text here is asserting nothing.
    #[test]
    fn the_pane_behind_the_dialog_is_muted() {
        let buffer = fixtures::draw_pane_with_dialog(160, 48);
        let behind = buffer[(2, 4)].style();
        assert_eq!(behind.fg, fixtures::plain_dimmed().fg);
    }

    /// Muting is a colour operation: under `NO_COLOR` there is no ink to
    /// dim with, so only the reset half of the mute pass does anything and
    /// the pane behind goes completely flat, no reverse video and no
    /// background, rather than staying lit. The border and the
    /// reverse-video heading carry the separation on their own then.
    #[test]
    fn no_color_flattens_the_pane_behind_instead_of_leaving_it_lit() {
        let buffer = fixtures::draw_pane_with_dialog_and_palette(160, 48, fixtures::no_color());
        // The title band: `title_band_line` styles its whole row
        // `REVERSED`, the one modifier the mute pass has to clear even
        // when there is no colour to dim with.
        let title_band = buffer[(2, 0)].style();
        assert_eq!(title_band.add_modifier, ratatui::style::Modifier::empty());
    }

    /// The cell at `(x, y)` in `buffer`'s own rendered text where row `y`
    /// contains `needle`, `x` being the column `needle` starts at plus
    /// `offset`.
    ///
    /// # Panics
    ///
    /// Panics if no row contains `needle`, a fixture bug rather than a
    /// failure the test is about.
    #[track_caller]
    fn cell_in_row_containing(buffer: &Buffer, needle: &str, offset: u16) -> ratatui::style::Style {
        let text = render_text(buffer);
        let (y, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
            .unwrap_or_else(|| panic!("no row contains {needle:?}"));
        let x = line.find(needle).unwrap_or(0);
        let x = u16::try_from(x).unwrap_or(0) + offset;
        let y = u16::try_from(y).unwrap_or(0);
        buffer[(x, y)].style()
    }

    /// 12a's own rule: colour is always redundant with the text, so
    /// `NO_COLOR` loses decoration and never information. The heading's
    /// only decoration is the `REVERSED` band every other chip on this
    /// dashboard carries; a heading styled with `attention` alone (a bare
    /// foreground colour) would lose it entirely under `NO_COLOR`, since
    /// `attention` sets no modifier for `NO_COLOR` to leave behind.
    #[test]
    fn the_heading_stays_a_reversed_band_under_no_color() {
        let buffer = fixtures::draw_pane_with_dialog_and_palette(160, 48, fixtures::no_color());
        let heading = cell_in_row_containing(&buffer, "NEEDS A RESPAWN", 0);
        assert!(
            heading
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
    }

    /// The three the check found. `▐` is Neutral and the four corners are
    /// too; `▀`, `▄` and `▌` are East-Asian Ambiguous, and a terminal that
    /// doubles the right edge shifts every interior row. Recorded rather
    /// than fixed, since no Neutral right-half block exists to swap in.
    #[test]
    fn the_border_vocabulary_is_the_one_that_was_checked() {
        for glyph in ['▛', '▜', '▙', '▟', '▐', '▀', '▄', '▌'] {
            assert_eq!(char_columns(glyph), 1, "{glyph}");
        }
    }

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

    /// Checks `args` too, since shep writes it happily: muting must mark
    /// both reasons a row can be locked, not just the structural one.
    #[test]
    fn a_structural_field_renders_muted_and_the_cost_column_says_why() {
        let lines = all_group_lines(fixtures::coloured());
        let instances = lines
            .iter()
            .find(|line| text_of(core::slice::from_ref(line))[0].contains("instances"))
            .expect("every field is drawn at 89 columns");
        assert!(
            text_of(core::slice::from_ref(instances))[0].contains("read-only"),
            "{instances:?}"
        );
        assert_eq!(
            instances.spans[1].style,
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
            probe.spans[1].style,
            fixtures::coloured().muted(),
            "muting says `not from here`, which is true of both kinds"
        );
    }

    /// Every field row over all eight groups, walked with `next_group`,
    /// styled by `palette`. What a test that used to find any field at
    /// one width in one shot now needs, since the active group is the
    /// only one a single render draws.
    ///
    /// 89 columns, one short of [`panel_width`]'s own floor: these tests
    /// are about a field row's own COST cell and lock glyph, which the
    /// panel suppresses once it draws, so the width has to stay below the
    /// ladder rather than at the 120 it used before the ladder existed.
    fn all_group_lines(palette: Palette) -> Vec<Line<'static>> {
        let mut pane = web_pane();
        let mut lines = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            lines.extend(pane_lines(&pane, palette, 89, 0));
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
        // 40, not 41: `env` no longer draws its own field row, folded into
        // the env rows below the field list instead.
        assert_eq!(
            rows_of(&text).len(),
            40,
            "every field but env is drawn at 89"
        );
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
        // 40, not 41: `env` no longer draws its own field row.
        assert_eq!(glyphed(' ').len(), 40 - 2 - 2);
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
                .unwrap_or_else(|| panic!("{key} is drawn at 89 columns: {text:?}"))
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
            let text = text_of(&pane_lines(&pane, fixtures::plain(), width, 0));
            rows.extend(rows_of(&text));
            all_text.extend(text);
            pane.next_group();
        }
        // `parts(line).is_some()` alone is not enough at this width any
        // more: the legend line now spells out `= read-only, set it in
        // the Flockfile` (finding 3), and its own leading two spaces plus
        // an `=` parse the same shape `parts` reads off a field row. Real
        // field keys only, the same filter `rows_of` applies, keeps this
        // test about the cost cell rather than the legend.
        let keys: Vec<String> = web_pane()
            .fields()
            .fields()
            .iter()
            .map(|field| field.key.clone())
            .collect();
        assert!(
            !all_text.iter().any(|line| {
                parts(line).is_some_and(|(_, _, _, key)| keys.contains(&key))
                    && line.contains("read-only")
            }),
            "no field row can afford the cost cell at {width}: {all_text:?}"
        );
        let glyph = |key: &str| rows.iter().find(|(_, _, _, k)| k == key).map(|r| r.1);
        assert_eq!(glyph("instances"), Some('='));
        assert_eq!(glyph("liveness_probe"), Some('~'));
        assert_eq!(glyph("watch"), Some(' '));
    }

    /// The whole frame at `height`, 120 columns wide, through the same
    /// `note_body_rows` and `draw` the event loop runs before each one.
    fn screen_at(app: &mut crate::lookout::app::App, height: u16) -> String {
        screen_of(app, 120, height)
    }

    /// [`screen_at`] at a width of the caller's own choosing.
    fn screen_of(app: &mut crate::lookout::app::App, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        app.note_body_rows(super::super::body_rows(area));
        app.note_body_width(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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
    #[test]
    fn the_body_never_outgrows_the_height_it_was_given() {
        let mut pane = web_pane();
        for height in 1..=60u16 {
            pane.set_rows(usize::from(height.saturating_sub(1)));
            for cursor in [0usize, 7, 20, 38] {
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

    /// `max_memory`'s own blurb, read off the same schema `Field::help`
    /// is built from.
    #[test]
    fn help_open_draws_the_selected_fields_own_text_under_the_title() {
        let mut pane = web_pane();
        pane.move_to_key("max_memory");
        pane.toggle_help();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        assert!(
            text.iter()
                .any(|line| line.contains("Restart the app if it climbs above this much memory")),
            "{text:?}"
        );
    }

    /// Below the panel's own floor: the panel prints the cursor's field's
    /// blurb unconditionally, `h` or no `h`, so this has to run where the
    /// panel does not draw at all to see the top line's own text disappear
    /// on the second toggle.
    #[test]
    fn toggling_help_again_dismisses_it() {
        let mut pane = web_pane();
        pane.move_to_key("max_memory");
        pane.toggle_help();
        pane.toggle_help();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 89, 0));
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
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
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

    /// The folded env rows at a comfortable width: both of the fixture's
    /// keys are listed with `(set)` rather than a value, and `+ add a key`
    /// is last.
    ///
    /// Asserts on the env rows alone via
    /// [`fixtures::config_pane_env_rows_for_tests`], not on the whole
    /// rendered frame: a search over the joined frame can match any row,
    /// field or env, once folding put both in the same screen.
    #[test]
    fn the_env_rows_draw_at_a_comfortable_width() {
        let pane = web_pane();
        let rows = fixtures::config_pane_env_rows_for_tests(&pane);
        assert!(rows.iter().any(|row| row.contains("DB_HOST")), "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("LOG_LEVEL")), "{rows:?}");
        assert!(
            rows.iter()
                .all(|row| row.contains("(set)") || row.contains("+ add a key")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("+ add a key")),
            "{rows:?}"
        );
    }

    /// A filed env write draws nothing at all on this screen, and above
    /// all not the value that was typed into it.
    #[test]
    fn a_filed_env_write_never_reaches_the_screen() {
        let mut pane = web_pane();
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        for typed in "NEW_KEY=hunter2".chars() {
            pane.type_env_char(typed);
        }
        pane.apply_env_typing();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        assert!(!text.join("\n").contains("hunter2"), "{text:?}");
    }

    /// The shepherd sends keys with no values at all, so a rendered value
    /// could only have been invented. Replaces
    /// `the_env_sub_screen_never_renders_a_value`, which pinned the same
    /// fact on the sub-screen the env rows folded into.
    ///
    /// Asserts on the env rows alone via
    /// [`fixtures::config_pane_env_rows_for_tests`], not on the whole
    /// rendered frame: see [`the_env_rows_draw_at_a_comfortable_width`]'s
    /// own doc comment for why a frame-wide search is the wrong tool here.
    #[test]
    fn every_env_value_renders_as_set_and_never_as_itself() {
        let pane = ConfigPane::sheep({
            let mut config = shep_core::config::AppConfig {
                name: "web".to_string(),
                ..Default::default()
            };
            config
                .env
                .insert("DB_PASSWORD".to_string(), "hunter2".to_string());
            shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new())
        });
        let rows = fixtures::config_pane_env_rows_for_tests(&pane);
        assert!(
            rows.iter().any(|row| row.contains("DB_PASSWORD")),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("hunter2")),
            "an env value reached the pane: {rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("(set)")), "{rows:?}");
    }

    #[test]
    fn every_env_line_fits_the_width_it_was_drawn_for() {
        let mut pane = web_pane();
        pane.move_to_last();
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        pane.begin_env_typing();
        for typed in "A_VERY_LONG_ENV_KEY_NAME=and a value longer still".chars() {
            pane.type_env_char(typed);
        }
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

    #[test]
    fn the_title_names_the_target_and_no_longer_calls_it_read_only() {
        let text = text_of(&pane_lines(&web_pane(), fixtures::plain(), 120, 0));
        // Named once, on the title band: `title_line` no longer draws a
        // second row repeating it (see `title_band_line`'s own doc).
        assert!(text[0].contains("web"), "{:?}", text[0]);
        assert!(text[0].contains("(sheep config)"), "{:?}", text[0]);
        assert!(!text[0].contains("read-only"), "{:?}", text[0]);
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
        text_of(&pane_lines(&pane, fixtures::plain(), width, height))
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

    /// A dog whose one string field is `x-shep-secret`, edited once. No
    /// Flockfile field is secret today, but a dog's schema can mark one,
    /// and the pending-edits section has to mask it the same way
    /// [`field_line`] already does for the row it scrolls off of.
    fn secret_dog_pane_with_an_edit() -> ConfigPane {
        let schema = serde_json::json!({
            "properties": {
                "token": {
                    "type": "string",
                    "x-shep-secret": true,
                }
            }
        });
        let mut pane = ConfigPane::dog(
            "watch".into(),
            None,
            schema,
            "token = \"ab12cd34\"\n".into(),
        );
        pane.move_to_key("token");
        pane.begin_typing();
        for c in "ef56gh78".chars() {
            pane.type_char(c);
        }
        pane.apply_typing();
        pane
    }

    /// `pending_edit_line` has its own secret check rather than relying on
    /// nothing ever reaching it: a dog can mark a field secret, and the
    /// pending-edits section draws every filed edit regardless of which
    /// group is active, so a secret's row is on screen the moment its edit
    /// is filed, not just while its own field row scrolls into view.
    #[test]
    fn the_pending_section_masks_a_secret_fields_old_and_new_value() {
        let pane = secret_dog_pane_with_an_edit();
        let (_, entry) = pane.edits().iter().next().expect("one edit was filed");
        let line = pending_edit_line(&pane, entry.edit(), 120, fixtures::plain());
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(!text.contains("ab12cd34"), "the old value leaked: {text}");
        assert!(!text.contains("ef56gh78"), "the new value leaked: {text}");
        assert!(text.contains("<set> -> <set>"), "{text}");
    }

    /// The field list's own `old -> new` cell, which had no test of its
    /// own while it spelled the mask inline. It is the path that draws a
    /// secret an operator has just edited, on the row they edited it on.
    #[test]
    fn the_field_row_masks_both_halves_of_a_secrets_edited_value() {
        let pane = secret_dog_pane_with_an_edit();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        let row = text
            .iter()
            .find(|line| line.contains(" token"))
            .expect("token is drawn at 120 columns");
        assert!(row.contains("<set> -> <set>"), "{row:?}");
        assert!(
            !text.join("\n").contains("ab12cd34"),
            "the old value leaked: {text:?}"
        );
        assert!(
            !text.join("\n").contains("ef56gh78"),
            "the new value leaked: {text:?}"
        );
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

    /// No Flockfile field is secret today, but a dog's schema can mark one,
    /// and the list sub-screen has to mask it the same way the field list
    /// and the confirm sentence already do.
    #[test]
    fn the_list_screen_masks_a_secret_arrays_elements() {
        let text = text_of(&pane_lines(
            &secret_list_dog_pane(),
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
                let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, height));
                assert!(
                    text.len() <= usize::from(height),
                    "height {height}, step {step}: {text:?}"
                );
                pane.list_mut().unwrap().move_by(1);
            }
        }
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
    ///
    /// Bounded to the tab row alone
    /// ([`fixtures::config_pane_tab_row_for_tests`]), not a search over the
    /// whole rendered frame: every group's name is drawn on every render
    /// regardless of which is active, so a frame-wide `contains` would pass
    /// whatever `next_group` did or did not do. [`tab_draws_the_active_group_as_its_own_chip`]
    /// covers the half this test cannot: which one is drawn as the chip.
    #[test]
    fn tab_walks_every_group_and_each_one_shows_its_own_fields() {
        let mut app = fixtures::app_in_sheep_pane();
        let mut seen = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            let group = app.config_pane().unwrap().group().to_owned();
            let tab_row = fixtures::config_pane_tab_row_for_tests(&app, 160);
            for name in GROUP_ORDER {
                assert!(
                    tab_row.contains(name),
                    "the tab row does not name {name}: {tab_row:?}"
                );
            }
            seen.push(group);
            app.update(Msg::Key(KeyPress::NextGroup));
        }
        assert_eq!(seen, GROUP_ORDER.to_vec());
    }

    /// All eight group names are listed every render, so the only thing
    /// that changes as `next_group` walks is which one carries the
    /// paper-2 ground: the chip. Checked in colour, since [`fixtures::plain`]
    /// makes [`super::super::super::theme::Palette::ground`] a no-op and
    /// every span would look alike.
    #[test]
    fn tab_draws_the_active_group_as_its_own_chip() {
        for active in GROUP_ORDER {
            let mut pane = web_pane();
            let digit =
                u8::try_from(GROUP_ORDER.iter().position(|g| g == active).unwrap() + 1).unwrap();
            pane.set_group(digit);
            let line = tab_row_line(&pane, fixtures::coloured(), 160);
            let painted: Vec<&str> = line
                .spans
                .iter()
                .filter(|span| span.style.bg.is_some())
                .map(|span| span.content.as_ref().trim())
                .collect();
            assert_eq!(
                painted,
                vec![*active],
                "exactly one chip should carry a ground, the active group's own"
            );
        }
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

    /// `▲` and `●` join the vocabulary `flock.rs:46` already checks `mark`
    /// against: East-Asian Ambiguous width would shift every column after
    /// the glyph.
    #[test]
    fn the_impact_glyphs_are_one_column_wide() {
        assert_eq!(char_columns('\u{25b2}'), 1);
        assert_eq!(char_columns('\u{25cf}'), 1);
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
    #[test]
    fn a_refused_form_reads_as_refused_without_colour() {
        let app = fixtures::app_with_plain_palette_in_sheep_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        let refusal = panel
            .iter()
            .find(|row| row.contains("cannot enter"))
            .expect("cwd states a refusal");
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

    /// The finding this fix round closes: nothing on the live screen drew
    /// the panel, because nothing called it outside a test fixture. This
    /// pins the wiring itself, through the same [`pane_lines`] the real
    /// draw path calls, not through a fixture built to reach
    /// [`panel_lines`] directly.
    ///
    /// The "not below the design target" half of this test's original name
    /// no longer holds: the fixed 160-column threshold became
    /// [`panel_width`]'s continuous ladder, so the panel now draws down to
    /// 90 columns. What still has to hold, and what this pins instead,
    /// is the panel's own floor: nothing below it.
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

    #[test]
    fn the_design_target_splits_eighty_eight_and_seventy_two() {
        assert_eq!(panel_width(160), Some(72));
    }

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

    #[test]
    fn the_panel_goes_below_ninety_columns() {
        assert!(panel_width(90).is_some());
        assert_eq!(panel_width(89), None);
    }

    #[test]
    fn the_panel_never_drops_below_its_own_floor() {
        for width in 90..=200 {
            let panel = panel_width(width).expect("the panel is drawn above 89");
            assert!((50..=72).contains(&panel), "{panel} at {width}");
        }
    }

    #[test]
    fn the_left_column_never_falls_below_its_own_floor() {
        for width in 90..=200 {
            let panel = panel_width(width).expect("the panel is drawn above 89");
            assert!(width - panel >= 40, "{} left at {width}", width - panel);
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
