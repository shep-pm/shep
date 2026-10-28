use super::super::super::app::{App, Body, Control, Grouping};
use super::super::super::keyvalue::{Field, KeyValues};
use super::super::super::pane::ConfigPane;
use super::super::super::textbox::TextBox;

/// The key hint once the link is [`Link::Lost`](crate::lookout::app::Link::Lost).
///
/// `q` leaves, and movement still walks a cursor over values that are
/// history. `h` opens the keymap overlay. `r` is refused: `run_link` has
/// returned by the time a freeze lands, so no task survives to answer one.
/// The hint states the consequence rather than a list of refused keys.
pub(super) const FROZEN_HINT: &str = "q quit   h keymap   j/k g/G move   \
     nothing you press can reach the shepherd";

/// What the status bar's editor slot shows while an editor owns the keyboard:
/// what it is labelled, the field to draw, and the keys to name beside it.
///
/// A struct rather than a tuple because the third thing is not the label's
/// other half. The two key/value panes read one [`editor_hint`] for one focused
/// field, and a caller that read the slot as two things would put the config
/// pane's fixed sentence where the editor's own belongs.
pub(super) struct Editor<'a> {
    /// What the editor is called.
    pub label: String,
    /// The field under the cursor, drawn with its cursor in it.
    pub buffer: &'a TextBox,
    /// The keys to name beside it.
    pub keys: &'static str,
}

/// The keys a key/value editor names, for whichever field holds the cursor.
///
/// One function, read by both panes, so the env rows and the secrets pane
/// cannot drift apart in what they tell an operator to press.
#[must_use]
pub(super) const fn editor_hint(field: Field) -> &'static str {
    match field {
        Field::Key => "tab value   enter next   esc cancels",
        Field::Value => "tab key   enter writes   esc cancels",
    }
}

/// What a key/value editor is called: the pane's own noun, the key, and which
/// of its two fields the cursor is in.
///
/// `noun` is what the pane calls its list — `env` for a sheep's environment
/// rows, `secret` for the secrets pane's own table — so the two read the same
/// shape over their own words. A key nothing has been typed into yet is named
/// `new`, the way both panes already name the row it will land on.
#[must_use]
pub(super) fn editor_label(editor: &KeyValues, noun: &str) -> String {
    let field = match editor.focus() {
        Field::Key => "KEY",
        Field::Value => "VALUE",
    };
    let key = editor.key().as_str();
    if key.is_empty() {
        return format!("new {noun} {field}");
    }
    format!("{noun} {key} {field}")
}

/// The pane's own editor slot: the list sub-screen's, the key/value editor's,
/// or the field editor's.
///
/// Three editors, one slot: a list edit is labelled with the field and the
/// element's position, a key/value edit with [`editor_label`], a field edit
/// with the field, and a map's `+ new` entry with the map. Only the key/value
/// editor names keys of its own; the other two are one-field boxes and keep
/// the bar's standing sentence.
pub(super) fn pane_editor(pane: &ConfigPane) -> Option<Editor<'_>> {
    const ONE_FIELD: &str = "enter applies   esc cancels";
    if let Some(list) = pane.list() {
        return match list.typing()? {
            (Some(index), buffer) => Some(Editor {
                label: format!("{} {index} =", list.key()),
                buffer,
                keys: ONE_FIELD,
            }),
            (None, buffer) => Some(Editor {
                label: format!("new {} element", list.key()),
                buffer,
                keys: ONE_FIELD,
            }),
        };
    }
    if let Some(editor) = pane.env_typing() {
        return Some(Editor {
            label: editor_label(editor, "env"),
            buffer: focused_field(editor),
            keys: editor_hint(editor.focus()),
        });
    }
    let typing = pane.typing()?;
    if let Some(entries) = pane.new_entry_editor() {
        return Some(Editor {
            label: format!("new {entries} key"),
            buffer: &typing.buffer,
            keys: ONE_FIELD,
        });
    }
    Some(Editor {
        label: format!("editing {}", typing.key),
        buffer: &typing.buffer,
        keys: ONE_FIELD,
    })
}

/// The secrets pane's open editor, labelled the same way the env rows label
/// theirs.
pub(super) fn secrets_typing(app: &App) -> Option<Editor<'_>> {
    let Body::Secrets(pane) = app.body() else {
        return None;
    };
    let editor = pane.typing.as_ref()?;
    Some(Editor {
        label: editor_label(editor, "secret"),
        buffer: focused_field(editor),
        keys: editor_hint(editor.focus()),
    })
}

/// The field of `editor` that holds the cursor, which is the one the bar draws.
fn focused_field(editor: &KeyValues) -> &TextBox {
    match editor.focus() {
        Field::Key => editor.key(),
        Field::Value => editor.value(),
    }
}

/// The key an armed `D` would remove from a sheep's env rows, or `None`.
pub(super) fn env_armed(pane: &ConfigPane) -> Option<&str> {
    pane.armed_env().map(|armed| armed.key.as_str())
}

/// The key an armed `D` would delete, or `None`.
pub(super) fn secrets_armed(app: &App) -> Option<&str> {
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
pub(super) const BLEATS_HINT: &str = "esc back   j/k line   ctrl-d/u page   G end   \
    / search   n/N match   f follow   w wrap   o out/err/both   m level";

/// While the close dialog is up, the whole bar reduces to this: the
/// dialog's own rows already say what `R`, `L`, `c` and `esc` do, so
/// repeating them here would only be a second copy to keep in step with
/// the first.
pub(super) const CLOSE_DIALOG_HINT: &str =
    "the dialog owns the keyboard until it is answered or it expires";

/// The secrets pane's own key hint.
///
/// `\u{21b5} set a value` and `D delete` name keys gated on
/// [`Control::Allowed`], as in [`hint_for`]. A hint naming a key that
/// always refuses teaches the operator the key is broken. `y` sits outside
/// that split: copying a revealed value writes nothing.
pub(super) fn secrets_hint(control: Control) -> String {
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
/// `space cycle`, `e edit` and `d back to default` show only under
/// [`Control::Allowed`]. Each sub-screen gets its own form, since `esc`
/// backs out there rather than closing. The list words `d` as `d remove`:
/// it drops an element. `esc` asks before it writes, so the sentence is
/// true with or without filed edits.
pub(super) const fn pane_hint(control: Control, screen: PaneScreen) -> &'static str {
    match (control, screen) {
        (Control::ReadOnly, PaneScreen::Fields) => {
            "esc close   j/k select   g/G first/last   r refresh   h help   * yours   ! parked   q quit"
        }
        (Control::Allowed, PaneScreen::Fields) => {
            "esc close   j/k select   g/G first/last   r refresh   space cycle   e edit   d back to default   u undo   h help   * yours   ! parked   q quit"
        }
        (Control::ReadOnly, PaneScreen::List) => {
            "esc back   j/k select   g/G first/last   r refresh   q quit"
        }
        (Control::Allowed, PaneScreen::List) => {
            "esc back   j/k select   g/G first/last   r refresh   e edit   d remove   K/J move   u undo   q quit"
        }
        (Control::ReadOnly, PaneScreen::Dogs) => {
            "esc close   j/k select   g/G first/last   r refresh   enter open   q quit"
        }
        (Control::Allowed, PaneScreen::Dogs) => {
            "esc close   j/k select   g/G first/last   r refresh   enter open   d remove   q quit"
        }
    }
}

/// The sheep pane's own key hint.
///
/// `x stop`, `R restart` and `L reload` are appended only under
/// [`Control::Allowed`]. This is the rule [`hint_for`] gives for the
/// dashboard's write keys. A hint naming an inert key teaches the operator
/// that the key is broken.
pub(super) const fn sheep_pane_hint(control: Control) -> &'static str {
    match control {
        Control::ReadOnly => "esc flock   e edit   J/K next sheep   b full log   / filter",
        Control::Allowed => {
            "esc flock   e edit   J/K next sheep   x stop   R restart   L reload   b full log   / filter"
        }
    }
}

/// Which of the pane's three screens is up.
///
/// `Debug` is derived (IR-41): a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneScreen {
    /// The field list, which env rows now walk too.
    Fields,
    /// The list sub-screen.
    List,
    /// A sheep's `dogs` tab with the cursor on a dog's own row.
    Dogs,
}

/// Which screen `pane` is showing.
pub(super) fn pane_screen(pane: &ConfigPane) -> PaneScreen {
    if pane.focus().list().is_some() {
        PaneScreen::List
    } else if pane.shows_dogs() && pane.focused_dog().is_none() {
        PaneScreen::Dogs
    } else {
        PaneScreen::Fields
    }
}

/// The key hint.
///
/// Three forms: the settings screen's own, the dashboard's two, and the
/// config pane's own [`pane_hint`] above. `settings_open` wins outright,
/// since the dashboard's keys mean nothing while the screen owns the
/// keyboard. Clauses are appended, never inserted: read-only's first 40
/// characters are pinned by the truncation and gallery tests.
pub(super) fn hint_for(control: Control, settings_open: bool, grouping: Grouping) -> String {
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
    // The fold keys splice before the appended tail, not into the movement
    // keys: the doc above pins read-only's first 40 characters. `z` appears
    // only in fold view, the only place it does anything.
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

#[cfg(test)]
mod tests {
    use super::super::status_layout::status_line;

    use super::super::super::super::app::{Control, Grouping};

    use super::super::super::fixtures::{rendered, with_selection};
    use super::*;

    use crate::lookout::app::{KeyPress, Msg};

    #[test]
    fn the_frozen_hint_clauses_are_separated_by_exactly_three_spaces() {
        assert!(
            !FROZEN_HINT.contains("    "),
            "a separator wider than three spaces: {FROZEN_HINT:?}"
        );
        assert_eq!(
            FROZEN_HINT.matches("   ").count(),
            3,
            "three clause gaps, so three separators: {FROZEN_HINT:?}"
        );
    }

    #[test]
    fn the_frozen_hint_names_the_keymap_it_no_longer_refuses() {
        assert!(FROZEN_HINT.contains("h keymap"), "{FROZEN_HINT}");
        assert!(
            FROZEN_HINT.contains("q quit") && FROZEN_HINT.contains("j/k g/G move"),
            "{FROZEN_HINT}"
        );
    }

    #[test]
    fn the_frozen_hint_claims_no_blanket_refusal() {
        assert!(
            !FROZEN_HINT.contains("every other key"),
            "the blanket-refusal claim is back, and it is false: {FROZEN_HINT}"
        );
        assert!(
            FROZEN_HINT.contains("nothing you press can reach the shepherd"),
            "{FROZEN_HINT}"
        );
    }

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
        let cfg_tier = usize::from(super::super::super::flock::cfg_tier_width());
        assert!(
            widest <= cfg_tier,
            "the hint is {widest} wide and CFG draws from {cfg_tier}, so the \
                 legend truncates where the glyph still shows"
        );
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
        let mut app = super::super::super::fixtures::app_in_sheep_pane_with_control();
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
