//! The three chrome lines: the title, the link banner, and the status bar.
//!
//! Every sentence here is literal: nothing about damage gets charming. The
//! frozen banner, the drop notice and the refusal all live here.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::app::{
    ActionState, App, Body, Control, Grouping, InputMode, Link, RowKey, Settings, TypingWhat,
    retrying_sentence,
};
use super::super::pane::ConfigPane;
use super::super::pane_bleats::BleatsPane;
use super::cell;
use super::flock::fit;
use super::settings::field_label;

/// The banner, when there is one. `None` while the link is live.
///
///
/// What happened and when the values stopped being current, so an operator
/// reading `online` knows how much to trust it. The frozen row says neither:
/// once the link is lost, `view::title_band` carries the death sentence in
/// bark across the whole row above, and this row picks up the `$SHEP_HOME`
/// the title band no longer has space for, plus the two things an operator
/// staring at a dead dashboard actually needs told.
#[must_use]
pub fn banner_line(app: &App, width: u16) -> Option<Line<'static>> {
    let palette = app.palette();
    match app.link() {
        Link::Live => None,
        Link::Retrying { attempt } => Some(Line::from(Span::styled(
            retrying_sentence(*attempt),
            palette.attention(),
        ))),
        // Through `fit`, unlike the retrying sentence above, which is
        // short enough that no terminal cuts it. Three clauses and a
        // `$SHEP_HOME` do not fit 90 columns, and `Buffer::set_line` cuts
        // what does not fit in silence: a sentence ending mid-word beside
        // five other lines that all mark their own truncation reads as a
        // rendering fault rather than as a narrow terminal.
        Link::Lost { .. } => Some(Line::from(Span::styled(
            fit(
                &format!(
                    " shep lookout   {}  ·  the dashboard stays up so you can read what it had  ·  it will not exit on its own",
                    app.home()
                ),
                width,
            ),
            palette.muted(),
        ))),
    }
}

/// The key hint once the link is [`Link::Lost`].
///
/// Two keys, because two keys still do something: `q` leaves, and `j`/`k`
/// move a cursor over values that are already history. `r` is not among
/// them, whatever the design's own copy says: it is refused like the rest
/// (`App::on_key`), and `super::super::link::run_link` has already returned
/// by the time a freeze lands, so no task survives to answer a redial. The
/// last clause is the whole rest of the keymap, said once rather than
/// discovered a keypress at a time.
const FROZEN_HINT: &str =
    "q quit   j/k still moves   every other key is refused while the link is down";

/// The bottom line: eight slots, highest priority first: the settings
/// screen's armed or in-flight edit, a dashboard confirm, the settings
/// screen's free-text editor, the filter box, a notice, an in-flight
/// action, the applied filter line, then the key hint. Control state
/// always renders on the right.
///
/// The config pane has no prompt slot of its own: nothing it edits is
/// armed and nothing is in flight, so there is no question waiting for an
/// answer. Its free-text editor still takes the editor slot.
///
/// The editor slot outranks the filter box: `Settings::typing` is `Some`
/// only while the settings screen owns `InputMode::Text`, while
/// `App::filter` stays untouched. A fixed row the layout never cuts; the
/// body echoes the same line beneath the table when there is room, both
/// reading `Settings::pending`/`Settings::typing` directly.
#[must_use]
pub fn status_line(app: &App, width: u16) -> Line<'static> {
    let palette = app.palette();
    let (left, left_style) = if let Some(prompt) = app.settings().and_then(Settings::pending) {
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
            format!("match  {buffer}\u{258f}   enter applies   esc cancels"),
            palette.attention(),
        )
    } else if let Some(buffer) = app
        .sheep_pane()
        .and_then(|pane| pane.feed().match_editing())
    {
        // The same box, embedded: the sheep pane's own feed shares
        // `InputMode::Text` with the dashboard's name filter too, and a
        // fall-through here would label the dashboard's untouched query as
        // this feed's match, the same mislabel the branch above already
        // guards against for the full-screen pane.
        (
            format!("match  {buffer}\u{258f}   enter applies   esc cancels"),
            palette.attention(),
        )
    } else if let Some((label, buffer)) = app.config_pane().and_then(pane_editor) {
        // The pane's own free-text editor, and the env sub-screen's, ahead
        // of the filter branch: all three share `InputMode::Text`, and a
        // bar that fell through would render the dashboard's untouched
        // query under the label `filter` instead.
        (
            format!("{label}  {buffer}\u{258f}   enter applies   esc cancels"),
            palette.attention(),
        )
    } else if let Some(action) = app.action().filter(|a| !a.sent) {
        // A question awaiting an answer outranks everything, including the
        // filter box: `/` cancels a confirm before it opens the box.
        (confirm_prompt(&action), palette.attention())
    } else if let Some((field, buffer)) = app.settings().and_then(Settings::typing) {
        // Checked ahead of the filter-box branch below: both share
        // `InputMode::Text`, but this types into `socket` or
        // `max_cron_sleep`, not `App::filter`. `field_label` is shared with
        // `view::settings` so the two panes agree on the field's name.
        (
            format!(
                "editing {}  {buffer}\u{258f}   enter applies   esc cancels",
                field_label(*field)
            ),
            palette.attention(),
        )
    } else if let Some((label, buffer)) = secrets_typing(app) {
        // Ahead of the filter-box branch below, for the reason the config
        // pane's own free-text branch above gives: this shares
        // `InputMode::Text` with `App::filter` too.
        (
            format!("{label}  {buffer}\u{258f}   enter applies   esc cancels"),
            palette.attention(),
        )
    } else if let Some(key) = secrets_armed(app) {
        // Ranked with the dashboard's own confirm above, for the same
        // reason: an armed delete is a question awaiting an answer, and it
        // must outrank `secrets_hint`, which still reads `enter sets a
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
            format!(
                "filter  {}\u{258f}   enter applies   esc cancels   ctrl-c quits",
                app.filter()
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
        // `attention`, the same butter the non-grave notice uses. Not a
        // modal, not a box, not a `ratatui::widgets::Clear`: there is no
        // overlay anywhere in this module, and one rule under the header
        // beats a full border for a pane somebody reads at 3am.
        (text, palette.attention())
    } else if let Some(pane) = app.config_pane() {
        // The pane owns the keyboard, so neither the filter line nor
        // either dashboard hint is true while it is up. Its own form: a
        // hint naming `x stop` beside a pane where `x` does nothing is
        // the asterisk this file's standing rule forbids.
        //
        // Butter, not muted: this is a key hint, same as the dashboard's own
        // below, and the redesign paints the keys butter over the bar's
        // ground.
        (
            pane_hint(app.control(), pane_screen(pane)).to_string(),
            palette.attention(),
        )
    } else if app.bleats_pane().is_some() {
        // Checked below the match box's own branch above, which owns this
        // slot instead while it is open. The design's own status-bar line
        // (docs/lookout/design-files/README.md:274) names every key here
        // but the minimum-level axis's own `m`: that line lists no key for
        // it at all, so it is appended rather than inserted, the same rule
        // `hint_for`'s own doc gives for its dashboard forms.
        (BLEATS_HINT.to_string(), palette.attention())
    } else if matches!(app.body(), Body::Secrets(_)) {
        // The pane owns the keyboard here too, same reasoning as the config
        // pane's own branch above: `x stop`/`R restart`/`L reload`/`F folds`
        // belong to the dashboard underneath and do nothing on this screen.
        (secrets_hint(app.control()), palette.attention())
    } else if app.sheep_pane().is_some() {
        // Checked below the bleats pane's own branch, the same as the
        // config pane's above it: the full-screen panes cannot be open at
        // once, so their order here is documentation, not correctness.
        (
            sheep_pane_hint(app.control()).to_string(),
            palette.attention(),
        )
    } else if app.settings().is_none() && !app.filter().is_empty() {
        // Gated on the screen being closed: the filter survives the swap
        // into settings (`App::on_settings_key` never touches it), but `/`
        // and `esc` mean something else entirely while the screen owns the
        // keyboard, so this line would be false the moment it stayed up.
        (
            format!("filter \"{}\"   / edit   esc clear", app.filter()),
            palette.muted(),
        )
    } else if matches!(app.link(), Link::Lost { .. }) {
        // Only this branch, not the pane hints above: a pane opened before
        // the freeze keeps naming its own keys, and this line is the flock
        // table's. Every action key `hint_for` would name is refused once
        // the link is gone, so naming them would teach the operator three
        // keys that do nothing.
        (FROZEN_HINT.to_string(), palette.attention())
    } else {
        // Butter: the keys, same rule as the pane's own hint above.
        (
            hint_for(app.control(), app.settings().is_some(), app.grouping()),
            palette.attention(),
        )
    };
    // Always rendered, in both states. An operator who does not know whether
    // their dashboard can act is one keystroke from finding out the wrong
    // way. The bleats pane borrows this slot while it is following the
    // tail: control state means nothing on a screen with no action keys of
    // its own, and whether the view is pinned to the newest line is the
    // fact this screen's own operator needs a keystroke away from.
    let right = if matches!(app.link(), Link::Lost { .. }) {
        // Ahead of both: whether this dashboard is reading a live shepherd
        // outranks whether a pane is pinned to the newest line, and it
        // outranks a control state that no longer decides anything.
        "\u{2588} frozen"
    } else if app.bleats_pane().is_some_and(BleatsPane::following) {
        "\u{2588} following"
    } else {
        match app.control() {
            Control::ReadOnly => "read-only",
            Control::Allowed => "control enabled",
        }
    };
    let right_len = u16::try_from(right.chars().count()).unwrap_or(0);
    // `+ 1` reserves one column of gap so a truncated left side's `…` never
    // butts against the label. The gap rides inside the right span, styled
    // like the label, keeping the line two spans rather than three.
    let left_width = width.saturating_sub(right_len).saturating_sub(1);
    // `patch`, not a fresh `Style`: `ground` sets only the background, so
    // patching it onto each span's own foreground paints the bar's ground
    // ([`Palette::ground`]) without disturbing what the span already means.
    // `fit` already pads `left` to `left_width`, so the background reaches
    // every column the label does not, the same reasoning `flock::pad_ground`
    // uses for the selected row.
    let ground = palette.ground();
    Line::from(vec![
        Span::styled(fit(&left, left_width), left_style.patch(ground)),
        Span::styled(format!(" {right}"), palette.muted().patch(ground)),
    ])
}

/// The confirm prompt's own sentence: which verb, which target, and how to
/// answer.
///
/// A group row is the one place a keypress reaches several processes, so
/// the prompt says how many before the operator commits. A single sheep
/// keeps the `(id N)` form.
fn confirm_prompt(action: &ActionState<'_>) -> String {
    match action.target {
        RowKey::Sheep(id) => format!(
            "{} {} (id {id})? enter confirms, any other key cancels",
            action.verb.label(),
            action.name
        ),
        RowKey::Group(name) => {
            let count = action.count;
            format!(
                "{} all {count} instances of {name}? enter confirms, any other key cancels",
                action.verb.label()
            )
        }
        RowKey::Fold(name) => format!(
            "{} all {} sheep in fold {name}? enter confirms, any other key cancels",
            action.verb.label(),
            action.count
        ),
        RowKey::Section(_) => unreachable!("a header is never an action target"),
    }
}

/// The in-flight line: the same verb-and-target naming [`confirm_prompt`]
/// uses, once the request has already gone out.
fn in_flight_text(action: &ActionState<'_>) -> String {
    match action.target {
        RowKey::Sheep(id) => format!(
            "{} {} (id {id}): sent, waiting for the shepherd",
            action.verb.label(),
            action.name
        ),
        RowKey::Group(name) => format!(
            "{} all {} instances of {name}: sent, waiting for the shepherd",
            action.verb.label(),
            action.count
        ),
        RowKey::Fold(name) => format!(
            "{} all {} sheep in fold {name}: sent, waiting for the shepherd",
            action.verb.label(),
            action.count
        ),
        RowKey::Section(_) => unreachable!("a header is never an action target"),
    }
}

/// What the pane's open editor is labelled, and what is in it.
///
/// Three editors, one slot: a field edit is labelled with the field, an
/// env edit with `env` and the key, a list edit with the field and the
/// element's position, and either `+ new`/`+ add a key` row with what it
/// wants, since there is nothing yet to name.
fn pane_editor(pane: &ConfigPane) -> Option<(String, &str)> {
    if let Some(list) = pane.list() {
        return match list.typing()? {
            (Some(index), buffer) => Some((format!("{} {index} =", list.key()), buffer)),
            (None, buffer) => Some((format!("new {} element", list.key()), buffer)),
        };
    }
    if let Some(env) = pane.env_typing() {
        return match env.key() {
            Some(key) => Some((format!("env {key} ="), env.buffer())),
            None => Some(("new env KEY=value".to_owned(), env.buffer())),
        };
    }
    let typing = pane.typing()?;
    Some((format!("editing {}", typing.key), typing.buffer.as_str()))
}

/// The secrets pane's open input, labelled by which step it is: the
/// `+ new key` row's name, or a key's value.
fn secrets_typing(app: &App) -> Option<(String, &str)> {
    let Body::Secrets(pane) = app.body() else {
        return None;
    };
    let typing = pane.typing.as_ref()?;
    let label = match &typing.what {
        TypingWhat::NewKey => "new key".to_string(),
        TypingWhat::ValueFor(key) => format!("value for {key}"),
    };
    Some((label, typing.buffer.as_str()))
}

/// The key an armed `D` would delete, or `None`.
fn secrets_armed(app: &App) -> Option<&str> {
    let Body::Secrets(pane) = app.body() else {
        return None;
    };
    pane.armed.as_ref().map(|a| a.key.as_str())
}

/// The bleats pane's key hint: the design's own status-bar line, plus `m`
/// for the minimum-level axis. The design names a key for every other axis
/// (`o` for the stream) but none for this one, so `m` is this crate's own
/// addition, appended after the design's own list rather than sorted into
/// it.
const BLEATS_HINT: &str = "esc back   j/k line   ctrl-d/u page   G end   \
    / search   n/N match   f follow   w wrap   o out/err/both   m level";

/// The secrets pane's own key hint.
///
/// `\u{21b5} set a value` and `D delete` name keys gated on
/// [`Control::Allowed`], mirroring [`hint_for`]'s own split: a hint naming
/// a key that always refuses teaches the operator the key is broken. `D`
/// sits beside `\u{21b5}` because both write; `y` sits outside that split
/// because copying an already-revealed value writes nothing. `v` and `y`
/// name no gate of their own, which the pane's own gates row two lines
/// above the table already states.
fn secrets_hint(control: Control) -> String {
    match control {
        Control::ReadOnly => {
            "esc/S close   \u{2190}/\u{2192} tab   z collapse   v reveal for 10s   \
             y copy   q quit"
                .to_string()
        }
        Control::Allowed => {
            "esc/S close   \u{2190}/\u{2192} tab   z collapse   v reveal for 10s   \
             \u{21b5} set a value   D delete   y copy   q quit"
                .to_string()
        }
    }
}

/// The config pane's own key hint.
///
/// Five forms: `space cycle`/`e edit`/`d back to default` show only under
/// [`Control::Allowed`] (`Enter` also opens the pane, sharing `e`'s slot).
/// Each sub-screen gets its own, since `esc` backs out rather than closing
/// there, and the list also names `d`/`K`/`J`, worded `d remove` there since
/// it drops an element rather than restoring a default. `* yours`/`! parked`
/// repeat the field list's glyphs
/// ([`super::pane::field_line`]); the flock table's `CFG` column carries the
/// same two with no legend of its own.
///
/// `Control::Allowed` at [`PaneScreen::Fields`] says `esc write & close`, not
/// `esc close`: `esc` there sends every filed edit before it closes the
/// pane, which is the whole of how a pane's edits reach the shepherd. The design doc for this pane (see the crate's own
/// `docs/brainstorming/specs/`) does spell it `esc close`, because a later
/// frame adds a confirmation dialog that intercepts the close and asks
/// first; once that dialog exists, `esc` stops writing on its own and this
/// line should revert. Until then, do not "simplify" this back to match the
/// design doc: the design doc describes a frame that is not built yet.
///
/// Said unconditionally, whether or not anything is filed, rather than
/// keyed on the edit count: `esc` writes zero edits the same way it writes
/// three, so the sentence is true either way and this stays a `const fn`.
const fn pane_hint(control: Control, screen: PaneScreen) -> &'static str {
    match (control, screen) {
        (Control::ReadOnly, PaneScreen::Fields) => {
            "esc close   j/k select   g/G first/last   r refresh   h help   * yours   ! parked   q quit"
        }
        (Control::Allowed, PaneScreen::Fields) => {
            "esc write & close   j/k select   g/G first/last   r refresh   space cycle   e edit   d back to default   u undo   h help   * yours   ! parked   q quit"
        }
        (Control::ReadOnly, PaneScreen::List) => {
            "esc back   j/k select   g/G first/last   r refresh   q quit"
        }
        (Control::Allowed, PaneScreen::List) => {
            "esc back   j/k select   g/G first/last   r refresh   e edit   d remove   K/J move   u undo   q quit"
        }
    }
}

/// The sheep pane's own key hint.
///
/// `x stop`, `R restart` and `L reload` are appended only under
/// [`Control::Allowed`], the same rule [`hint_for`]'s own doc gives for the
/// dashboard's write keys: a hint naming a key that is inert where the
/// operator is reading it teaches them the key is broken. `b full log` and
/// `/ filter` are named for every control level, the same as `esc`/`e`/`J`/`K`:
/// both now route to the embedded feed Task 10 wired in.
const fn sheep_pane_hint(control: Control) -> &'static str {
    match control {
        Control::ReadOnly => "esc flock   e edit   J/K next sheep   b full log   / filter",
        Control::Allowed => {
            "esc flock   e edit   J/K next sheep   x stop   R restart   L reload   b full log   / filter"
        }
    }
}

/// Which of the pane's two screens is up.
///
/// `Debug` is derived (IR-41): a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneScreen {
    /// The field list, which env rows now walk too.
    Fields,
    /// The list sub-screen.
    List,
}

/// Which screen `pane` is showing.
fn pane_screen(pane: &ConfigPane) -> PaneScreen {
    if pane.list().is_some() {
        PaneScreen::List
    } else {
        PaneScreen::Fields
    }
}

/// The key hint.
///
/// Three forms: the settings screen's own, the dashboard's two, and the
/// config pane's own [`pane_hint`] above. `settings_open` wins outright,
/// since the dashboard's keys mean nothing while the screen owns the
/// keyboard. A hint needing a footnote is not a hint: `Control::Allowed`'s
/// dashboard form is handed out only where those keys really do arm a
/// confirm.
///
/// `s settings`, `e edit` and the `* yours   ! parked` legend are all
/// appended, never inserted: read-only's first 40 characters must stay
/// byte-identical for the truncation and gallery tests. Settings forms
/// follow suit: read-only is a prefix of control.
fn hint_for(control: Control, settings_open: bool, grouping: Grouping) -> String {
    if settings_open {
        // `esc/s close` names both keys that close the screen: on this
        // screen `s` is the close key, not the open one.
        return match control {
            Control::ReadOnly => "esc/s close   j/k select   g/G first/last   r refresh   q quit",
            Control::Allowed => {
                "esc/s close   j/k select   g/G first/last   r refresh   space cycle   enter apply   q quit"
            }
        }
        .to_string();
    }
    // The fold keys, spliced before the appended tail rather than into the
    // movement keys: the doc above pins read-only's first 40 characters, and
    // the truncation and gallery tests read them.
    //
    // `z` appears only in fold view, because that is the only place it does
    // anything. A hint naming a key that is inert where the operator is
    // reading it teaches them the key is broken.
    let folds = match grouping {
        Grouping::Flat => "F folds   ",
        Grouping::ByFold => "F flat   z collapse   ",
    };
    match control {
        Control::ReadOnly => {
            format!(
                "q quit   j/k select   g/G first/last   r refresh   / filter   {folds}s settings   e edit   * yours   ! parked"
            )
        }
        // `g/G` and `r` drop out to make room. They are the two an operator
        // rediscovers by pressing them; an action key is not.
        Control::Allowed => {
            format!(
                "q quit   j/k select   / filter   {folds}x stop   R restart   L reload   s settings   e edit   * yours   ! parked"
            )
        }
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
mod tests {
    use std::ffi::OsStr;
    use std::time::Instant;

    use shep_core::protocol::BusEvent;

    use super::super::fixtures::{
        acting_app, allowed_app, app_armed_to_delete_a_secret, app_in_settings, app_in_settings_on,
        app_in_settings_with_control, armed_app, armed_app_with_a_filter_and_a_notice, editing_app,
        filtered_app, rendered, with_selection,
    };
    use super::*;
    use crate::commands::settings::SettingField;
    use crate::lookout::app::{ActionVerb, App, KeyPress, Msg};
    use crate::lookout::theme::Palette;

    /// The legend sits at the tail, and the hint truncates from the tail, so
    /// it survives only while the hint fits. The lowest tier that still draws
    /// `CFG` is what it has to fit inside.
    #[test]
    fn the_legend_fits_wherever_the_cfg_column_is_drawn() {
        let widest = [
            hint_for(Control::ReadOnly, false, Grouping::Flat),
            hint_for(Control::Allowed, false, Grouping::Flat),
        ]
        .into_iter()
        .map(|hint| hint.chars().count())
        .max()
        .expect("two hints");
        let cfg_tier = usize::from(super::super::flock::cfg_tier_width());
        assert!(
            widest <= cfg_tier,
            "the hint is {widest} wide and CFG draws from {cfg_tier}, so the \
             legend truncates where the glyph still shows"
        );
    }

    /// Pinned at 49 columns: the default hint is 59 characters and the
    /// label 9, the width where the hint truncates but the label still
    /// fits.
    #[test]
    fn a_truncated_hint_still_leaves_a_gap_before_the_control_label() {
        let palette = Palette::detect(None, Some(OsStr::new("xterm-256color")), None);
        let app = App::new(
            palette,
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        let line = status_line(&app, 49);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(rendered.chars().count(), 49, "must fill the full width");
        assert!(
            rendered.ends_with(" read-only"),
            "expected a space before the label, got: {rendered:?}"
        );
        assert!(
            !rendered.contains("…read-only"),
            "the ellipsis must not butt straight against the label: {rendered:?}"
        );
    }

    /// The first 40 characters of the replacement are unchanged, so
    /// `a_truncated_hint_still_leaves_a_gap_before_the_control_label` still
    /// measures the same thing.
    #[test]
    fn the_key_hint_says_what_the_keys_now_do() {
        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        let hint: String = status_line(&app, 200)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(hint.contains("j/k select"), "got {hint:?}");
        assert!(hint.contains("g/G first/last"), "got {hint:?}");
        assert!(
            !hint.contains("scroll"),
            "the pane no longer scrolls: {hint:?}"
        );
    }

    #[test]
    fn a_wide_status_line_still_pads_out_to_the_full_width() {
        let palette = Palette::detect(None, Some(OsStr::new("xterm-256color")), None);
        let app = App::new(
            palette,
            Control::Allowed,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        let line = status_line(&app, 120);
        let rendered: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert_eq!(rendered.chars().count(), 120);
        assert!(rendered.ends_with(" control enabled"));
    }

    #[test]
    fn the_bar_names_the_filter_keys_while_a_filter_is_applied() {
        let app = filtered_app("web");
        let bar = rendered(&status_line(&app, 120));
        assert!(bar.contains("filter \"web\""), "the query, quoted: {bar:?}");
        assert!(bar.contains("/ edit"), "got {bar:?}");
        assert!(bar.contains("esc clear"), "got {bar:?}");
    }

    #[test]
    fn the_bar_carries_the_query_and_a_cursor_while_editing() {
        let app = editing_app("we");
        let bar = rendered(&status_line(&app, 120));
        assert!(
            bar.contains("filter  we\u{258f}"),
            "query then cursor: {bar:?}"
        );
        assert!(bar.contains("enter applies"), "got {bar:?}");
        assert!(bar.contains("esc cancels"), "got {bar:?}");
        assert!(bar.contains("ctrl-c quits"), "got {bar:?}");
    }

    /// Both share `InputMode::Text`, so this pins that the bar reads the
    /// settings editor's own state, not the dashboard's untouched filter.
    #[test]
    fn the_bar_shows_the_settings_editor_rather_than_the_filter_box() {
        let mut app = app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let bar = rendered(&status_line(&app, 120));
        assert!(
            bar.contains("editing socket  "),
            "names the field being typed: {bar:?}"
        );
        assert!(
            bar.contains("/home/ada/.shep/run/shep.sock\u{258f}"),
            "shows the buffer and the cursor, not the dashboard's own filter: {bar:?}"
        );
        assert!(
            !bar.contains("filter "),
            "must not read as the filter box: {bar:?}"
        );
    }

    /// A `Dropped` event arrives mid-edit and must not cover the box.
    #[test]
    fn a_notice_raised_while_typing_does_not_cover_the_box() {
        let mut app = editing_app("we");
        app.update(Msg::Event(BusEvent::Dropped { count: 3 }));
        let bar = rendered(&status_line(&app, 120));
        assert!(
            bar.contains("filter  we\u{258f}"),
            "the box is still there: {bar:?}"
        );
        assert!(!bar.contains("dropped 3 events"), "got {bar:?}");
    }

    #[test]
    fn closing_the_box_shows_the_notice_that_was_waiting() {
        let mut app = editing_app("we");
        app.update(Msg::Event(BusEvent::Dropped { count: 3 }));
        app.update(Msg::Key(KeyPress::TextApply));
        let bar = rendered(&status_line(&app, 120));
        assert!(bar.contains("dropped 3 events"), "got {bar:?}");
    }

    #[test]
    fn the_read_only_hint_advertises_the_filter_key() {
        let app = filtered_app("");
        let hint = rendered(&status_line(&app, 200));
        assert!(hint.contains("/ filter"), "got {hint:?}");
    }

    #[test]
    fn the_dashboard_hint_says_what_the_cfg_glyphs_mean() {
        for control in [Control::ReadOnly, Control::Allowed] {
            let hint = hint_for(control, false, Grouping::Flat);
            assert!(hint.contains("* yours"), "{hint}");
            assert!(hint.contains("! parked"), "{hint}");
        }
    }

    /// The two fold keys are discoverable, and `z` only where it does
    /// something.
    ///
    /// `docs/lookout/design-files/README.md:282` asks the status bar for both.
    /// They shipped bound and unnamed, so an operator had no way to find
    /// either. `z` is inert outside fold view, and a hint naming an inert key
    /// teaches the operator the key is broken.
    #[test]
    fn the_hint_names_the_fold_keys_and_only_names_z_in_fold_view() {
        for control in [Control::ReadOnly, Control::Allowed] {
            let flat = hint_for(control, false, Grouping::Flat);
            assert!(flat.contains("F folds"), "{flat}");
            assert!(!flat.contains("z collapse"), "z does nothing here: {flat}");

            let folded = hint_for(control, false, Grouping::ByFold);
            assert!(folded.contains("F flat"), "{folded}");
            assert!(folded.contains("z collapse"), "{folded}");
        }
    }

    #[test]
    fn an_armed_confirm_names_the_verb_the_sheep_and_the_answer() {
        let app = armed_app(ActionVerb::Restart);
        let bar = rendered(&status_line(&app, 120));
        assert!(bar.contains("restart api (id 2)?"), "got {bar:?}");
        assert!(
            bar.contains("enter confirms, any other key cancels"),
            "got {bar:?}"
        );
    }

    /// The secrets pane's own destructive arm gets the same sentence the
    /// dashboard's does: which key, and how to answer.
    #[test]
    fn an_armed_secret_delete_names_the_key_and_the_answer() {
        let app = app_armed_to_delete_a_secret();
        let bar = rendered(&status_line(&app, 120));
        assert!(bar.contains("delete DB_PASSWORD"), "got {bar:?}");
        assert!(
            bar.contains("enter confirms, any other key cancels"),
            "got {bar:?}"
        );
    }

    /// Armed, `Enter` deletes rather than opening the value input, so the
    /// bar must stop claiming the older job.
    #[test]
    fn an_armed_secret_delete_stops_advertising_set_a_value() {
        let app = app_armed_to_delete_a_secret();
        let bar = rendered(&status_line(&app, 120));
        assert!(
            !bar.contains("set a value"),
            "the arm changes what enter does: {bar:?}"
        );
    }

    #[test]
    fn an_in_flight_action_says_it_is_waiting() {
        let app = acting_app(ActionVerb::Stop);
        let bar = rendered(&status_line(&app, 120));
        assert!(
            bar.contains("stop api (id 2): sent, waiting for the shepherd"),
            "got {bar:?}"
        );
    }

    #[test]
    fn the_confirm_outranks_a_notice_and_the_filter_line() {
        let app = armed_app_with_a_filter_and_a_notice();
        let bar = rendered(&status_line(&app, 120));
        assert!(bar.contains("stop api (id 2)?"), "got {bar:?}");
        assert!(
            !bar.contains("filter \""),
            "the filter line is below it: {bar:?}"
        );
    }

    /// Also covers the bus-raised case: `DaemonShutdown` must reach the bar
    /// the same way a keypress refusal does.
    #[test]
    fn a_refusal_while_an_action_is_in_flight_reaches_the_bar() {
        let mut app = acting_app(ActionVerb::Stop);
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let bar = rendered(&status_line(&app, 120));
        assert!(
            bar.contains("one action is already in flight"),
            "the refusal is on the bar, not only in the reducer: {bar:?}"
        );

        let mut app = acting_app(ActionVerb::Stop);
        app.update(Msg::Event(BusEvent::DaemonShutdown));
        let bar = rendered(&status_line(&app, 120));
        assert!(bar.contains("the shepherd is shutting down"), "got {bar:?}");
    }

    #[test]
    fn the_in_flight_line_comes_back_when_the_notice_clears() {
        let mut app = acting_app(ActionVerb::Stop);
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        app.update(Msg::Key(KeyPress::SelectDown));
        let bar = rendered(&status_line(&app, 120));
        assert!(
            bar.contains("stop api (id 2): sent, waiting for the shepherd"),
            "got {bar:?}"
        );
    }

    #[test]
    fn the_action_keys_are_advertised_only_when_the_gate_is_open() {
        let closed = rendered(&status_line(&filtered_app(""), 200));
        for key in ["x stop", "R restart", "L reload"] {
            assert!(
                !closed.contains(key),
                "{key} advertised read-only: {closed:?}"
            );
        }
        let open = rendered(&status_line(&allowed_app(), 200));
        for key in ["x stop", "R restart", "L reload"] {
            assert!(
                open.contains(key),
                "{key} missing when the gate is open: {open:?}"
            );
        }
        assert!(
            open.contains("/ filter"),
            "and the filter key survives both forms"
        );
    }

    /// `s` is named as `esc/s close`: on this screen `s` closes rather than
    /// opens.
    #[test]
    fn the_settings_edit_keys_are_advertised_only_when_the_gate_is_open() {
        let closed = rendered(&status_line(&app_in_settings(), 200));
        for key in ["space cycle", "enter apply"] {
            assert!(
                !closed.contains(key),
                "{key} advertised read-only: {closed:?}"
            );
        }
        let open = rendered(&status_line(&app_in_settings_with_control(), 200));
        for key in ["space cycle", "enter apply"] {
            assert!(
                open.contains(key),
                "{key} missing when the gate is open: {open:?}"
            );
        }
        for both in [&closed, &open] {
            assert!(both.contains("esc/s close"), "got {both:?}");
            assert!(both.contains("r refresh"), "got {both:?}");
        }
    }

    /// `App` handles `q` in its settings key dispatch, same as the
    /// dashboard.
    #[test]
    fn q_quit_is_named_on_the_settings_screen_in_both_control_states() {
        let closed = rendered(&status_line(&app_in_settings(), 200));
        let open = rendered(&status_line(&app_in_settings_with_control(), 200));
        for hint in [&closed, &open] {
            assert!(hint.contains("q quit"), "got {hint:?}");
        }
    }

    /// The pane's cursor, walked onto `key` the way an operator walks it.
    /// A thin wrapper: [`super::super::fixtures::select_field`] is this
    /// exact walk, and this module had its own copy before the tab row
    /// gave a field's group somewhere to switch to first.
    fn pane_to(app: &mut App, key: &str) {
        super::super::fixtures::select_field(app, key);
    }

    #[test]
    fn the_panes_edit_keys_are_advertised_only_when_the_gate_is_open() {
        let closed = rendered(&status_line(
            &super::super::fixtures::app_in_sheep_pane(),
            200,
        ));
        let open = rendered(&status_line(
            &super::super::fixtures::app_in_sheep_pane_with_control(),
            200,
        ));
        for key in ["space cycle", "e edit"] {
            assert!(
                !closed.contains(key),
                "{key} advertised read-only: {closed:?}"
            );
            assert!(
                open.contains(key),
                "{key} missing with the gate open: {open:?}"
            );
        }
        assert!(closed.contains("esc close"), "got {closed:?}");
        assert!(
            open.contains("esc write & close"),
            "the gate is open, so esc can write: {open:?}"
        );
        for both in [&closed, &open] {
            assert!(both.contains("h help"), "got {both:?}");
            assert!(both.contains("* yours"), "got {both:?}");
            assert!(both.contains("! parked"), "got {both:?}");
            assert!(both.contains("q quit"), "got {both:?}");
            assert!(!both.contains("x stop"), "got {both:?}");
        }
    }

    /// An env row is on the same screen as every field, so it carries the
    /// field list's own hint: `esc write & close`, not a sub-screen's `esc
    /// back`, and `e edit` since `begin_env_typing` is the same door
    /// `confirm_field` already opens for a typed field.
    #[test]
    fn an_env_rows_status_bar_hint_is_the_field_lists_own() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        app.update(Msg::Key(KeyPress::SelectLast));
        assert!(matches!(
            app.config_pane().unwrap().cursor(),
            Some(crate::lookout::pane::PaneRow::Env(_) | crate::lookout::pane::PaneRow::AddEnv)
        ));
        let bar = rendered(&status_line(&app, 200));
        assert!(bar.contains("esc write & close"), "got {bar:?}");
        assert!(bar.contains("e edit"), "got {bar:?}");
    }

    /// `g`/`G` and `r` are bound on the field list and the list
    /// sub-screen, in both control states. A hint that needs a footnote is
    /// an asterisk in both directions.
    #[test]
    fn every_pane_hint_names_the_movement_and_refresh_keys_it_binds() {
        for screen in [PaneScreen::Fields, PaneScreen::List] {
            for control in [Control::ReadOnly, Control::Allowed] {
                let hint = pane_hint(control, screen);
                for key in ["j/k select", "g/G first/last", "r refresh", "q quit"] {
                    assert!(hint.contains(key), "{control:?} {screen:?}: {hint:?}");
                }
            }
        }
    }

    /// The legend for the flag glyphs `field_line` draws: `*` an
    /// operator's own override, `!` parked until the next respawn. Named
    /// in both control states, since the flags are informational rather
    /// than something `--allow-control` gates. The list sub-screen has no
    /// rows of its own to flag, so it carries neither.
    #[test]
    fn the_field_lists_hint_carries_a_legend_for_its_own_flag_glyphs() {
        for control in [Control::ReadOnly, Control::Allowed] {
            let hint = pane_hint(control, PaneScreen::Fields);
            assert!(hint.contains("* yours"), "{control:?}: {hint:?}");
            assert!(hint.contains("! parked"), "{control:?}: {hint:?}");
            let sub = pane_hint(control, PaneScreen::List);
            assert!(!sub.contains('*'), "{control:?}: {sub:?}");
            assert!(!sub.contains('!'), "{control:?}: {sub:?}");
        }
    }

    /// Guards against the hint naming a key that does not actually work,
    /// a lie rather than merely a gap: `g`/`G` and `r` reach an env row
    /// exactly as they reach a field.
    #[test]
    fn the_env_rows_movement_and_refresh_keys_do_what_the_hint_says() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(
            app.config_pane().unwrap().cursor(),
            Some(crate::lookout::pane::PaneRow::AddEnv)
        );
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(
            app.config_pane().unwrap().cursor(),
            Some(crate::lookout::pane::PaneRow::Field(0))
        );
        assert!(matches!(
            app.update(Msg::Key(KeyPress::Refresh)),
            crate::lookout::app::Effect::Send(_)
        ));
    }

    /// Nothing the config pane files is armed or in flight, so the bar
    /// keeps its key hint rather than asking a question the operator's
    /// next keystroke does not answer.
    #[test]
    fn a_filed_pane_edit_leaves_the_status_bar_on_its_key_hint() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        let bar = rendered(&status_line(&app, 200));
        assert!(!bar.contains("enter confirms"), "got {bar:?}");
        assert!(!bar.contains("sent, waiting"), "got {bar:?}");
        assert!(bar.contains("esc write & close"), "got {bar:?}");
    }

    #[test]
    fn the_panes_editors_get_their_own_status_line_rather_than_the_filters() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::TextChar('x')));
        let field = rendered(&status_line(&app, 200));
        assert!(field.contains("editing cwd"), "got {field:?}");
        assert!(!field.contains("filter"), "got {field:?}");
        app.update(Msg::Key(KeyPress::TextAbandon));

        // `SelectLast` lands on `+ add a key`; two steps up is `DB_HOST`,
        // the fixture's first env key.
        app.update(Msg::Key(KeyPress::SelectLast));
        app.update(Msg::Key(KeyPress::SelectUp));
        app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(
            app.config_pane().unwrap().cursor(),
            Some(crate::lookout::pane::PaneRow::Env(0))
        );
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::TextChar('y')));
        let env = rendered(&status_line(&app, 200));
        assert!(env.contains("env DB_HOST ="), "got {env:?}");
        assert!(env.contains('y'), "got {env:?}");
        assert!(!env.contains("filter"), "got {env:?}");
    }

    #[test]
    fn the_config_pane_gets_its_own_key_hint() {
        let app = super::super::fixtures::app_in_sheep_pane();
        let bar = status_line(&app, 120).to_string();
        assert!(bar.contains("esc close"), "got {bar:?}");
        assert!(bar.contains("r refresh"), "got {bar:?}");
        assert!(!bar.contains("x stop"), "got {bar:?}");
        assert!(!bar.contains("s settings"), "got {bar:?}");
    }

    /// What the pane has filed for `args`, or [`None`].
    fn filed_args(app: &App) -> Option<serde_json::Value> {
        use super::super::super::edits::EditKey;
        use super::super::super::pane::PaneEdit;
        match app
            .config_pane()?
            .edits()
            .get(&EditKey::Field("args".to_owned()))?
            .edit()
        {
            PaneEdit::Set { value, .. } => Some(value.as_value().clone()),
            PaneEdit::SetEnv { .. } => None,
        }
    }

    /// The three keys the list sub-screen binds that no other screen
    /// does, each pressed rather than called.
    #[test]
    fn the_list_sub_screens_own_keys_do_what_its_hint_says() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        app.update(Msg::Key(KeyPress::Confirm));
        let bar = rendered(&status_line(&app, 200));
        assert!(bar.contains("esc back"), "got {bar:?}");
        assert!(bar.contains("d remove"), "got {bar:?}");
        assert!(bar.contains("K/J move"), "got {bar:?}");
        app.update(Msg::Key(KeyPress::StepDown));
        assert_eq!(
            filed_args(&app),
            Some(serde_json::json!(["8080", "--port"])),
            "J files the array with the element moved down"
        );
        // Ordered J before d on purpose: the two compose, so a removal
        // first would leave one element and nothing for J to move.
        app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(
            filed_args(&app),
            Some(serde_json::json!(["--port"])),
            "d files the array without the element under the cursor"
        );
    }

    /// The bleats pane's match box gets the status bar, not the dashboard's
    /// name filter.
    ///
    /// Both own `InputMode::Text`, and the filter branch is a catch-all, so
    /// without a branch of its own the bar labels the dashboard's untouched
    /// query as this pane's match. The same trap the config pane's editor
    /// sits ahead of the filter branch to avoid.
    #[test]
    fn the_bleats_match_box_owns_the_status_bar_while_it_is_open() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        for typed in "pool".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let bar = rendered(&status_line(&app, 160));
        assert!(bar.contains("match  pool"), "got {bar}");
        assert!(!bar.contains("filter"), "not the dashboard's box: {bar}");
    }

    /// The bleats pane's own hint carries the design's full line, plus `m`
    /// for the minimum-level axis the design names no key for at all. Every
    /// other key hint test in this module checks for its own screen's
    /// keys the same way; a bare "the bar is non-empty" would pass for the
    /// dashboard's hint too, since the title alone already renders.
    #[test]
    fn the_bleats_pane_gets_its_own_status_line() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let bar = rendered(&status_line(&app, 200));
        for key in [
            "esc back",
            "j/k line",
            "ctrl-d/u page",
            "G end",
            "/ search",
            "n/N match",
            "f follow",
            "w wrap",
            "o out/err/both",
            "m level",
        ] {
            assert!(bar.contains(key), "missing {key:?}: got {bar}");
        }
    }

    /// The right-aligned `█ following` indicator replaces the control-state
    /// label while the bleats pane is pinned to the tail, and only then:
    /// the design names it for this screen alone.
    #[test]
    fn the_following_indicator_replaces_control_state_while_pinned_to_the_tail() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let bar = rendered(&status_line(&app, 160));
        assert!(bar.contains("\u{2588} following"), "got {bar}");
        assert!(!bar.contains("read-only"), "got {bar}");

        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let scrolled = rendered(&status_line(&app, 160));
        assert!(
            !scrolled.contains("\u{2588} following"),
            "scrolled back, no longer following: {scrolled}"
        );
        assert!(scrolled.contains("read-only"), "got {scrolled}");
    }

    /// `x`/`R`/`L` are wired, so the hint keeps naming them; `b`/`/` are
    /// wired too now, so the hint names them alongside the write keys
    /// rather than dropping them, the same rule that gates the write keys
    /// behind `Control::Allowed` just below.
    #[test]
    fn the_sheep_panes_hint_names_the_write_keys_and_the_feeds_own() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let bar = rendered(&status_line(&app, 200));
        for key in [
            "esc flock",
            "e edit",
            "J/K next sheep",
            "x stop",
            "R restart",
            "L reload",
            "b full log",
            "/ filter",
        ] {
            assert!(bar.contains(key), "missing {key:?}: got {bar}");
        }
    }

    /// `x`/`R`/`L` are hidden under `Control::ReadOnly`, the same rule
    /// `hint_for`'s own dashboard forms follow: a hint naming a key that is
    /// inert where the operator is reading it teaches them the key is
    /// broken.
    #[test]
    fn the_sheep_panes_hint_drops_the_write_keys_under_read_only() {
        use shep_core::protocol::ProcessInfo;
        use shep_core::status::ProcStatus;

        let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let bar = rendered(&status_line(&app, 200));
        assert!(bar.contains("esc flock"), "got {bar}");
        assert!(!bar.contains("x stop"), "got {bar}");
        assert!(!bar.contains("R restart"), "got {bar}");
        assert!(!bar.contains("L reload"), "got {bar}");
        assert!(bar.contains("b full log"), "got {bar}");
        assert!(bar.contains("/ filter"), "got {bar}");
    }
}
