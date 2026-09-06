//! The three chrome lines: the title, the link banner, and the status bar.
//!
//! Every sentence here is literal: nothing about damage gets charming. The
//! frozen banner, the drop notice and the refusal all live here.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::app::{
    ActionState, App, Control, InputMode, Link, RowKey, Settings, SettingsPrompt, retrying_sentence,
};
use super::super::pane::{ConfigPane, PanePending};
use super::flock::fit;
use super::settings::field_label;

/// The banner, when there is one. `None` while the link is live.
///
/// The frozen sentence names what happened and when the values stopped
/// being current, so an operator reading `online` knows how much to trust
/// it.
#[must_use]
pub fn banner_line(app: &App) -> Option<Line<'static>> {
    let palette = app.palette();
    match app.link() {
        Link::Live => None,
        Link::Retrying { attempt } => Some(Line::from(Span::styled(
            retrying_sentence(*attempt),
            palette.attention(),
        ))),
        Link::Lost { at_local } => Some(Line::from(Span::styled(
            format!("the shepherd has died: these values are frozen as of {at_local}"),
            palette.alarm(),
        ))),
    }
}

/// The bottom line: eight slots, highest priority first: the settings
/// screen's armed or in-flight edit, a dashboard confirm, the settings
/// screen's free-text editor, the filter box, a notice, an in-flight
/// action, the applied filter line, then the key hint. Control state
/// always renders on the right.
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
    } else if let Some(prompt) = app.config_pane().and_then(pane_prompt) {
        // Slot 1b. The config pane's own armed edit or in-flight sentence,
        // on the fixed row the layout never cuts, so an operator can never
        // press Enter into a change nothing showed them. The pane and the
        // settings screen cannot both be open, so they never compete for it.
        let text = if prompt.sent {
            format!("{}  sent, waiting for the shepherd", prompt.text)
        } else {
            format!("{}  enter confirms, any other key cancels", prompt.text)
        };
        (text, palette.attention())
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
    } else if app.settings().is_none() && !app.filter().is_empty() {
        // Gated on the screen being closed: the filter survives the swap
        // into settings (`App::on_settings_key` never touches it), but `/`
        // and `esc` mean something else entirely while the screen owns the
        // keyboard, so this line would be false the moment it stayed up.
        (
            format!("filter \"{}\"   / edit   esc clear", app.filter()),
            palette.muted(),
        )
    } else {
        // Butter: the keys, same rule as the pane's own hint above.
        (
            hint_for(app.control(), app.settings().is_some()),
            palette.attention(),
        )
    };
    // Always rendered, in both states. An operator who does not know whether
    // their dashboard can act is one keystroke from finding out the wrong
    // way.
    let right = match app.control() {
        Control::ReadOnly => "read-only",
        Control::Allowed => "control enabled",
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
        RowKey::Section(_) => unreachable!("a header is never an action target"),
    }
}

/// The config pane's armed or in-flight question, or [`None`].
///
/// Reuses [`SettingsPrompt`] rather than declaring a second two-field
/// struct that says the same thing: the bar asks one question of both
/// screens, what is the sentence and has it gone out.
fn pane_prompt(pane: &ConfigPane) -> Option<SettingsPrompt<'_>> {
    match pane.pending_edit()? {
        PanePending::Armed { text, .. } => Some(SettingsPrompt { text, sent: false }),
        PanePending::Sent { text, .. } => Some(SettingsPrompt { text, sent: true }),
        PanePending::Typing { .. } => None,
    }
}

/// What the pane's open editor is labelled, and what is in it.
///
/// Three editors, one slot: a field edit is labelled with the field, an
/// env edit with `env` and the key, a list edit with the field and the
/// element's position, and either sub-screen's `+ new` row with what it
/// wants, since there is nothing yet to name.
fn pane_editor(pane: &ConfigPane) -> Option<(String, &str)> {
    if let Some(list) = pane.list() {
        return match list.typing()? {
            (Some(index), buffer) => Some((format!("{} {index} =", list.key()), buffer)),
            (None, buffer) => Some((format!("new {} element", list.key()), buffer)),
        };
    }
    if let Some(env) = pane.env() {
        return match env.typing()? {
            (Some(key), buffer) => Some((format!("env {key} ="), buffer)),
            (None, buffer) => Some(("new env KEY=value".to_owned(), buffer)),
        };
    }
    match pane.pending_edit()? {
        PanePending::Typing { key, buffer } => Some((format!("editing {key}"), buffer.as_str())),
        PanePending::Armed { .. } | PanePending::Sent { .. } => None,
    }
}

/// The config pane's own key hint.
///
/// Five forms: `space cycle`/`e edit` show only under [`Control::Allowed`]
/// (`Enter` also opens the pane, sharing `e`'s slot). Each sub-screen gets its
/// own, since `esc` backs out rather than closing there, and the list also
/// names `d`/`K`/`J`. `* yours`/`! parked` repeat the field list's glyphs
/// ([`super::pane::field_line`]); the flock table's `CFG` column carries the
/// same two with no legend of its own.
const fn pane_hint(control: Control, screen: PaneScreen) -> &'static str {
    match (control, screen) {
        (Control::ReadOnly, PaneScreen::Fields) => {
            "esc close   j/k select   g/G first/last   r refresh   h help   * yours   ! parked   q quit"
        }
        (Control::Allowed, PaneScreen::Fields) => {
            "esc close   j/k select   g/G first/last   r refresh   space cycle   e edit   h help   * yours   ! parked   q quit"
        }
        (Control::ReadOnly, PaneScreen::Env | PaneScreen::List) => {
            "esc back   j/k select   g/G first/last   r refresh   q quit"
        }
        (Control::Allowed, PaneScreen::Env) => {
            "esc back   j/k select   g/G first/last   r refresh   e set   q quit"
        }
        (Control::Allowed, PaneScreen::List) => {
            "esc back   j/k select   g/G first/last   r refresh   e edit   d remove   K/J move   q quit"
        }
    }
}

/// Which of the pane's three screens is up.
///
/// `Debug` is derived (IR-41): a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneScreen {
    /// The field list.
    Fields,
    /// The env sub-screen.
    Env,
    /// The list sub-screen.
    List,
}

/// Which screen `pane` is showing. A sub-screen wins, and the two are
/// never open at once.
fn pane_screen(pane: &ConfigPane) -> PaneScreen {
    if pane.list().is_some() {
        PaneScreen::List
    } else if pane.env().is_some() {
        PaneScreen::Env
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
fn hint_for(control: Control, settings_open: bool) -> String {
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
    match control {
        Control::ReadOnly => {
            "q quit   j/k select   g/G first/last   r refresh   / filter   s settings   e edit   * yours   ! parked"
        }
        // `g/G` and `r` drop out to make room. They are the two an operator
        // rediscovers by pressing them; an action key is not.
        Control::Allowed => {
            "q quit   j/k select   / filter   x stop   R restart   L reload   s settings   e edit   * yours   ! parked"
        }
    }
    .to_string()
}

/// A run of `─` across the pane, under the header.
///
/// One rule, not a box. `output::table`'s own doc argues that a table a
/// user can `awk` over beats one that looks nice; the same instinct applies
/// to a pane an operator reads at 3am, and a full border costs two columns
/// and two rows of the thing they are trying to read.
#[must_use]
pub fn rule_line(style: Style, width: u16) -> Line<'static> {
    Line::from(Span::styled("─".repeat(usize::from(width)), style))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::Instant;

    use shep_core::protocol::BusEvent;

    use super::super::fixtures::{
        acting_app, allowed_app, app_in_settings, app_in_settings_on, app_in_settings_with_control,
        armed_app, armed_app_with_a_filter_and_a_notice, editing_app, filtered_app, rendered,
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
            hint_for(Control::ReadOnly, false),
            hint_for(Control::Allowed, false),
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
            let hint = hint_for(control, false);
            assert!(hint.contains("* yours"), "{hint}");
            assert!(hint.contains("! parked"), "{hint}");
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
    fn pane_to(app: &mut App, key: &str) {
        let index = app
            .config_pane()
            .expect("the pane is open")
            .fields()
            .fields()
            .iter()
            .position(|field| field.key == key)
            .unwrap_or_else(|| panic!("no field named {key}"));
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..index {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
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
        for both in [&closed, &open] {
            assert!(both.contains("esc close"), "got {both:?}");
            assert!(both.contains("h help"), "got {both:?}");
            assert!(both.contains("* yours"), "got {both:?}");
            assert!(both.contains("! parked"), "got {both:?}");
            assert!(both.contains("q quit"), "got {both:?}");
            assert!(!both.contains("x stop"), "got {both:?}");
        }
    }

    #[test]
    fn the_env_sub_screen_says_esc_backs_out_rather_than_closes() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "env");
        app.update(Msg::Key(KeyPress::Confirm));
        let bar = rendered(&status_line(&app, 200));
        assert!(bar.contains("esc back"), "got {bar:?}");
        assert!(bar.contains("e set"), "got {bar:?}");
        assert!(
            !bar.contains("e close"),
            "e no longer closes anything: {bar:?}"
        );
        assert!(!bar.contains("enter set"), "got {bar:?}");
    }

    /// `g`/`G` and `r` are bound on the field list and both sub-screens,
    /// in both control states. A hint that needs a footnote is an
    /// asterisk in both directions.
    #[test]
    fn every_pane_hint_names_the_movement_and_refresh_keys_it_binds() {
        for screen in [PaneScreen::Fields, PaneScreen::Env, PaneScreen::List] {
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
    /// than something `--allow-control` gates. The env sub-screen has no
    /// rows of its own to flag, so it carries neither.
    #[test]
    fn the_field_lists_hint_carries_a_legend_for_its_own_flag_glyphs() {
        for control in [Control::ReadOnly, Control::Allowed] {
            let hint = pane_hint(control, PaneScreen::Fields);
            assert!(hint.contains("* yours"), "{control:?}: {hint:?}");
            assert!(hint.contains("! parked"), "{control:?}: {hint:?}");
            for screen in [PaneScreen::Env, PaneScreen::List] {
                let sub = pane_hint(control, screen);
                assert!(!sub.contains('*'), "{control:?} {screen:?}: {sub:?}");
                assert!(!sub.contains('!'), "{control:?} {screen:?}: {sub:?}");
            }
        }
    }

    /// Guards against the hint naming a key that does not actually work,
    /// a lie rather than merely a gap.
    #[test]
    fn the_env_sub_screens_movement_and_refresh_keys_do_what_the_hint_says() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "env");
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().env().unwrap().view().cursor(), 2);
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(app.config_pane().unwrap().env().unwrap().view().cursor(), 0);
        assert!(matches!(
            app.update(Msg::Key(KeyPress::Refresh)),
            crate::lookout::app::Effect::Send(_)
        ));
    }

    #[test]
    fn an_armed_pane_edit_reaches_the_status_bar_and_says_so_once_sent() {
        let mut app = super::super::fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        let armed = rendered(&status_line(&app, 200));
        assert!(armed.contains("set autorestart = false"), "got {armed:?}");
        assert!(armed.contains("enter confirms"), "got {armed:?}");

        app.update(Msg::Key(KeyPress::Confirm));
        let sent = rendered(&status_line(&app, 200));
        assert!(sent.contains("set autorestart = false"), "got {sent:?}");
        assert!(
            sent.contains("sent, waiting for the shepherd"),
            "got {sent:?}"
        );
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

        pane_to(&mut app, "env");
        app.update(Msg::Key(KeyPress::Confirm));
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
        app.update(Msg::Key(KeyPress::ListRemove));
        assert!(app.config_pane().unwrap().is_armed(), "d arms a removal");
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::ListMoveDown));
        assert!(app.config_pane().unwrap().is_armed(), "J arms a move");
    }
}
