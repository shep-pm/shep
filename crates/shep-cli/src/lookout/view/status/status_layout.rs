use super::super::super::app::{App, Body, Control, InputMode, Link, Settings, retrying_sentence};
use super::super::super::pane_bleats::BleatsPane;
use super::super::super::textbox::TextBox;
use super::super::cell;
use super::super::flock::fit;
use super::action_gate::{CONTROL_ENABLED_LABEL, READ_ONLY_LABEL, confirm_prompt, in_flight_text};
use super::key_hint::{
    BLEATS_HINT, CLOSE_DIALOG_HINT, EntryActions, FROZEN_HINT, catalogue_hint, env_armed, hint_for,
    modal_keys, pane_editor, pane_hint, pane_screen, secrets_armed, secrets_hint, secrets_typing,
    sheep_pane_hint,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// What a text-entry bar names beside its buffer: the one-field editors that
/// apply on enter and cancel on escape. Each bar adds its own gap before it.
pub(in crate::lookout::view) const TEXT_ENTRY_HINT: &str = "enter applies   esc cancels";

/// A text-entry bar: `label`, two columns of gap, the buffer, then `keys`
/// three columns after it. `secret` masks the buffer the way its own row does.
fn entry_line(buffer: &TextBox, secret: bool, label: &str, keys: &str, width: u16) -> String {
    buffer.line(secret, &format!("{label}  "), &format!("   {keys}"), width)
}

/// The banner, when there is one. `None` while the link is live.
///
/// What happened and when the values stopped being current, so an operator
/// reading `online` knows how much to trust it. Once the link is lost,
/// `view::title_band` carries the death sentence above, and this row picks
/// up the `$SHEP_HOME` the title band has no room for.
#[must_use]
pub fn banner_line(app: &App, width: u16) -> Option<Line<'static>> {
    let palette = app.palette();
    match app.link() {
        Link::Live => None,
        Link::Retrying { attempt } => Some(Line::from(Span::styled(
            retrying_sentence(*attempt),
            palette.attention(),
        ))),
        // Through `fit`, unlike the retrying sentence above, which is short
        // enough that no terminal cuts it. `Buffer::set_line` cuts in
        // silence: a mid-word cut beside lines that mark their own
        // truncation reads as a rendering fault.
        Link::Lost { .. } => Some(Line::from(Span::styled(
            fit(
                &format!(
                    " shep lookout   {}  ·  the dashboard stays up so you can read what it had  ·  it will not exit on its own",
                    app.place()
                ),
                width,
            ),
            palette.muted(),
        ))),
    }
}

/// The bottom line: eight slots, highest priority first.
///
/// The settings screen's armed or in-flight edit, a dashboard confirm, its
/// free-text editor, the filter box. A notice, an in-flight action, the
/// applied filter line, then the key hint. Control state always renders on
/// the right, and the row is never cut.
#[must_use]
pub fn status_line(app: &App, width: u16) -> Line<'static> {
    let palette = app.palette();
    // Always rendered, in both states. An operator who cannot tell whether
    // the dashboard can act finds out the wrong way. The bleats pane borrows
    // this slot while following the tail: it has no action keys of its own,
    // and pinning is what its reader needs.
    let right = if matches!(app.link(), Link::Lost { .. }) {
        // Ahead of both: whether this dashboard is reading a live shepherd
        // outranks whether a pane is pinned to the newest line, and outranks
        // a control state the lost link already settles.
        "\u{2588} frozen"
    } else if app.bleats_pane().is_some_and(BleatsPane::following) {
        "\u{2588} following"
    } else {
        match app.control() {
            Control::ReadOnly => READ_ONLY_LABEL,
            Control::Allowed => CONTROL_ENABLED_LABEL,
        }
    };
    let right_len = u16::try_from(right.chars().count()).unwrap_or(0);
    // `+ 1` reserves one column of gap so a truncated left side's `…` never
    // butts against the label. The gap rides inside the right span, styled
    // like the label, keeping the line two spans rather than three.
    let left_width = width.saturating_sub(right_len).saturating_sub(1);
    let (left, left_style) = if let Some(modal) = app.modal() {
        // First: a modal takes every key ahead of whatever is under it, so
        // any other hint would name keys that do nothing.
        (
            modal_keys(modal, app.modal_scrolls()).to_string(),
            palette.attention(),
        )
    } else if let Some(prompt) = app.settings().and_then(Settings::pending) {
        // The settings screen's armed scalar or dog edit, or its in-flight
        // sentence once sent. Opening clears `self.action`. `on_key` routes
        // to the settings keymap, so no dashboard confirm can arm while it
        // stays open.
        let text = if prompt.sent {
            format!("{}  sent, waiting for the shepherd", prompt.text)
        } else {
            format!("{}  enter confirms, any other key cancels", prompt.text)
        };
        (text, palette.attention())
    } else if let Some(buffer) = app.bleats_pane().and_then(BleatsPane::match_editing) {
        // Ahead of the filter branch for the reason the comment below gives:
        // the bleats pane's match box shares `InputMode::Text` with the
        // dashboard's name filter, and falling through would label the
        // dashboard's own untouched query as this pane's match.
        (
            entry_line(buffer, false, "match", TEXT_ENTRY_HINT, left_width),
            palette.attention(),
        )
    } else if let Some(buffer) = app
        .sheep_pane()
        .and_then(|pane| pane.feed().match_editing())
    {
        // The same box, embedded: the sheep pane's feed shares
        // `InputMode::Text` with the dashboard's name filter. A fall-through
        // here would label the dashboard's untouched query as this feed's
        // match.
        (
            entry_line(buffer, false, "match", TEXT_ENTRY_HINT, left_width),
            palette.attention(),
        )
    } else if let Some(editor) = app.config_pane().and_then(pane_editor) {
        // The pane's own free-text editor, and the key/value editor the env
        // rows open, ahead of the filter branch: all three share
        // `InputMode::Text`, and a bar that fell through would label this the
        // dashboard's own query. The keys come from the slot, not from here.
        (
            entry_line(
                editor.buffer,
                editor.secret,
                &editor.label,
                editor.keys,
                left_width,
            ),
            palette.attention(),
        )
    } else if let Some(action) = app.action().filter(|a| !a.sent) {
        // A question awaiting an answer outranks everything, including the
        // filter box: `/` cancels a confirm before it opens the box.
        (confirm_prompt(&action), palette.attention())
    } else if let Some((settings, label, buffer)) = app.settings().and_then(|settings| {
        settings
            .typing()
            .map(|(label, buffer)| (settings, label, buffer))
    }) {
        // Checked ahead of the filter-box branch below: both share
        // `InputMode::Text`, but this types into a `shep.toml` key, not
        // `App::filter`.
        (
            entry_line(
                buffer,
                settings.typing_is_secret(),
                &format!("editing {label}"),
                TEXT_ENTRY_HINT,
                left_width,
            ),
            palette.attention(),
        )
    } else if let Some(key) = app.config_pane().and_then(env_armed) {
        // The config pane's one armed thing, ranked with the secrets pane's
        // own below for the same reason: it is a question awaiting an answer,
        // and it must outrank `pane_hint`, which still reads `e edit` while an
        // arm is live.
        (
            format!("delete {key}? enter confirms, any other key cancels"),
            palette.attention(),
        )
    } else if let Some(editor) = secrets_typing(app) {
        // Ahead of the filter-box branch below, for the reason the config
        // pane's own free-text branch above gives: this shares
        // `InputMode::Text` with `App::filter` too.
        (
            entry_line(
                editor.buffer,
                editor.secret,
                &editor.label,
                editor.keys,
                left_width,
            ),
            palette.attention(),
        )
    } else if let Some(key) = secrets_armed(app) {
        // Ranked with the dashboard's own confirm above, for the same
        // reason: an armed delete is a question awaiting an answer, and it
        // must outrank `key_hint::secrets_hint`, which still reads `enter sets a
        // value` while an arm is live.
        (
            format!("delete {key}? enter confirms, any other key cancels"),
            palette.attention(),
        )
    } else if app.mode() == InputMode::Text {
        // Above the notice: bus events arrive with no keypress and
        // `on_text_key` never clears them, so ranking the notice higher
        // would erase a half-typed query. The cursor is a character, not
        // a style: the ANSI gallery renders foregrounds only.
        (
            entry_line(
                app.filter_box(),
                false,
                "filter",
                &format!("{TEXT_ENTRY_HINT}   ctrl-c quits"),
                left_width,
            ),
            palette.attention(),
        )
    } else if let Some(notice) = app.notice() {
        (
            notice.to_string(),
            if notice.is_grave() {
                palette.refusal()
            } else {
                palette.attention()
            },
        )
    } else if let Some(action) = app.action() {
        // Below the notice: `arm`'s "one action is already in flight"
        // refusal is itself a notice, so ranking this above notices would
        // hide it. A keypress cannot wipe this line; the reducer clears
        // the notice, not this ordering.
        let text = in_flight_text(&action);
        // `attention`, the same butter the non-grave notice uses. No modal,
        // no box, no `ratatui::widgets::Clear`. One rule under the header
        // beats a full border for a pane read at 3am. This module draws no
        // overlay.
        (text, palette.attention())
    } else {
        let (hint, style) = body_hint(app);
        (with_dog_job(app, &hint, left_width), style)
    };
    // `patch`, not a fresh `Style`. `ground` sets only the background, so
    // patching it keeps each span's own foreground. `fit` pads `left` to
    // `left_width`, so the ground reaches every column the label does not.
    let ground = palette.ground();
    Line::from(vec![
        Span::styled(fit(&left, left_width), left_style.patch(ground)),
        Span::styled(format!(" {right}"), palette.muted().patch(ground)),
    ])
}

/// The key hint for whatever body is up, and the style it is drawn in.
///
/// Reached only when [`status_line`] has no question, editor, notice or
/// in-flight line to show: while one of those is up the bar says that, and a
/// hint naming keys the screen is not taking is the asterisk this file's
/// standing rule forbids.
#[must_use]
fn body_hint(app: &App) -> (String, Style) {
    let palette = app.palette();
    if app.close_dialog().is_some() {
        // Ahead of the config pane's own hint below: the dialog owns the
        // keyboard while it is up. A bar naming `space cycle` under that
        // screen would be the asterisk this rule forbids.
        // Muted: the dialog's own rows name what each key does.
        return (CLOSE_DIALOG_HINT.to_string(), palette.muted());
    }
    if let Some(pane) = app.config_pane() {
        // The pane owns the keyboard, so neither dashboard hint is true
        // while it is up. Naming `x stop` where `x` does nothing is the
        // asterisk this rule forbids. Butter: this is a key hint, same as
        // the dashboard's own below.
        return (
            pane_hint(app.control(), pane_screen(pane)).to_string(),
            palette.attention(),
        );
    }
    if app.bleats_pane().is_some() {
        // Checked below the match box's branch, which owns this slot while
        // open. The design's status-bar line
        // (docs/lookout/design-files/README.md:274) names no key for `m`, so
        // it is appended rather than inserted.
        return (BLEATS_HINT.to_string(), palette.attention());
    }
    if matches!(app.body(), Body::Secrets(_)) {
        // The pane owns the keyboard here too, same reasoning as the config
        // pane's own branch above: `x stop`/`R restart`/`L reload`/`F folds`
        // belong to the dashboard underneath and do nothing on this screen.
        return (secrets_hint(app.control()), palette.attention());
    }
    if let Body::Catalogue(pane) = app.body() {
        // The open row is named by `adopt_as`, which is also what each
        // action addresses it by.
        let actions = pane.entry.as_ref().map(|name| {
            let row = app
                .catalogue_rows()
                .iter()
                .find(|row| row.dog.adopt_as == *name);
            EntryActions {
                install: row.is_some_and(|row| app.can_install(row)),
                upgrade: row.is_some_and(|row| app.can_upgrade(row)),
                delete: row.is_some_and(|row| app.can_delete(row)),
            }
        });
        return (catalogue_hint(actions), palette.attention());
    }
    if app.sheep_pane().is_some() {
        // Checked below the bleats pane's own branch, the same as the
        // config pane's above it: the full-screen panes cannot be open at
        // once, so their order here is documentation, not correctness.
        return (
            sheep_pane_hint(app.control()).to_string(),
            palette.attention(),
        );
    }
    if app.settings().is_none() && !app.filter().is_empty() {
        // Gated on the screen being closed: the filter survives the swap
        // into settings (`App::on_settings_key` never touches it), but `/`
        // and `esc` mean something else entirely while the screen owns the
        // keyboard, so this line would be false the moment it stayed up.
        return (
            format!("filter \"{}\"   / edit   esc clear", app.filter()),
            palette.muted(),
        );
    }
    if matches!(app.link(), Link::Lost { .. }) {
        // Only this branch, not the pane hints above. A pane opened before
        // the freeze keeps naming its own keys. This line is the flock
        // table's. Every action key `key_hint::hint_for` would name is
        // refused once the link is gone.
        return (FROZEN_HINT.to_string(), palette.attention());
    }
    // Butter: the keys, same rule as the pane's own hint above.
    (
        hint_for(app.control(), app.settings().is_some(), app.grouping()),
        palette.attention(),
    )
}

/// Puts the running dog job's item ahead of `hint`, or drops it where both
/// would not fit.
///
/// The job goes first: the keys are what every screen needs to keep working
/// while an install runs, and the item is the one thing on the bar that says
/// what this machine is doing on its own.
fn with_dog_job(app: &App, hint: &str, width: u16) -> String {
    let Some(job) = app.dog_job_line() else {
        return hint.to_owned();
    };
    let joined = format!("{job}   {hint}");
    let Ok(needed) = u16::try_from(joined.chars().count()) else {
        return hint.to_owned();
    };
    if needed <= width {
        joined
    } else {
        hint.to_owned()
    }
}

/// A run of `─` across the pane, under the header.
///
/// One rule, not a box. `output::table`'s own doc argues that a table a
/// user can `awk` over beats one that looks nice; the same instinct applies
/// to a pane an operator reads at 3am, and a full border costs two columns
/// and two rows of the thing they are trying to read.
#[must_use]
pub fn rule_line(style: Style, width: u16) -> Line<'static> {
    Line::from(Span::styled(cell::rule(usize::from(width)), style))
}

#[cfg(test)]
mod tests;
