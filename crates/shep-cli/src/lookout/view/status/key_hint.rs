use super::super::super::app::{App, Body, Control, Grouping, TypingWhat};
use super::super::super::pane::ConfigPane;

/// The key hint once the link is [`Link::Lost`](crate::lookout::app::Link::Lost).
///
/// `q` leaves, movement still walks a cursor over values that are already
/// history, and `h` opens the keymap overlay, which is read only and wanted
/// most on the screen where nothing else works. `r` is refused
/// (`App::on_key` tests [`Link::Lost`](crate::lookout::app::Link::Lost)), even though 1l's own design copy
/// says it redials: `super::super::super::link::run_link` has already returned by
/// the time a freeze lands, so no task survives to answer one.
///
/// The hint does not claim every other key is refused, because that is not
/// true: `esc`, `j`, `g`/`G`, `h`, `/`, `Enter`, `e`, `s` and `S` all still
/// act. Only `Refresh` and the three action verbs test the link.
///
/// What the hint states instead is the consequence: the shepherd is
/// unreachable, so nothing pressed here changes anything out there.
/// Movement, the keymap and the filter stay local, and every key that would
/// reach the shepherd fails to.
pub(super) const FROZEN_HINT: &str = "q quit   h keymap   j/k g/G move   \
     nothing you press can reach the shepherd";

/// What the pane's open editor is labelled, and what is in it.
///
/// Three editors, one slot: a field edit is labelled with the field, an
/// env edit with `env` and the key, a list edit with the field and the
/// element's position, and either `+ new`/`+ add a key` row with what it
/// wants, since there is nothing yet to name.
pub(super) fn pane_editor(pane: &ConfigPane) -> Option<(String, &str)> {
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
pub(super) fn secrets_typing(app: &App) -> Option<(String, &str)> {
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
/// [`Control::Allowed`], mirroring [`hint_for`]'s own split: a hint naming
/// a key that always refuses teaches the operator the key is broken. `D`
/// sits beside `\u{21b5}` because both write; `y` sits outside that split
/// because copying an already-revealed value writes nothing. `v` and `y`
/// name no gate of their own, which the pane's own gates row two lines
/// above the table already states.
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
/// Five forms: `space cycle`/`e edit`/`d back to default` show only under
/// [`Control::Allowed`] (`Enter` also opens the pane, sharing `e`'s slot).
/// Each sub-screen gets its own, since `esc` backs out rather than closing
/// there, and the list also names `d`/`K`/`J`, worded `d remove` there since
/// it drops an element rather than restoring a default. `* yours`/`! parked`
/// repeat the field list's glyphs
/// (`view::pane::field_row::field_line`); the flock table's `CFG` column
/// carries the
/// same two with no legend of its own.
///
/// `Control::Allowed` at [`PaneScreen::Fields`] said `esc write & close`,
/// not `esc close`, until frame 1g landed: `esc` used to send every filed
/// edit before it closed the pane, with no question asked first. The close
/// dialog is that question now, so `esc` alone only asks or, with nothing
/// to ask about, closes; either way it does not write on its own anymore,
/// and the hint reverted to what the design doc always said.
///
/// Said unconditionally, whether or not anything is filed, rather than
/// keyed on the edit count: `esc` behaves the same way over zero edits as
/// over three, so the sentence is true either way and this stays a
/// `const fn`.
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
pub(super) const fn sheep_pane_hint(control: Control) -> &'static str {
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
pub(super) enum PaneScreen {
    /// The field list, which env rows now walk too.
    Fields,
    /// The list sub-screen.
    List,
}

/// Which screen `pane` is showing.
pub(super) fn pane_screen(pane: &ConfigPane) -> PaneScreen {
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

#[cfg(test)]
mod tests {
    use super::super::status_layout::status_line;

    use super::super::super::super::app::{Control, Grouping};

    use super::super::super::fixtures::{rendered, with_selection};
    use super::*;

    use crate::lookout::app::{KeyPress, Msg};

    /// The hint's clause separators are three spaces, every one of them.
    ///
    /// `FROZEN_HINT` is written with a `\` line continuation: Rust strips
    /// the next line's leading whitespace after that continuation, so the
    /// source's own indent never becomes extra spaces in the string, and
    /// the gallery's own rendered frame confirms three.
    ///
    /// The sibling tests around this constant call `contains` on each
    /// clause separately, so none of them can see the spacing between
    /// clauses; this is the one that does. A future continuation, or a
    /// hand-typed run of spaces, would otherwise go unnoticed in text an
    /// operator reads at 3am.
    ///
    /// Two assertions with different guarantees: "no run of four or more"
    /// covers a clause added later automatically, but the exact-three
    /// count does not. A fourth clause fails it deliberately, and the
    /// count needs a manual bump when that happens on purpose.
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

    /// Three claims in two macros, and the second macro is the load-bearing
    /// one. `h keymap` alone would pass on a rewrite that appended the new
    /// key and dropped the two it landed between, which is how a hint loses
    /// the keys it always had. The rest of the sentence, the clause about
    /// every key it does not list, is its own test below: naming `h` and
    /// keeping that clause true have to happen together, and each half needs
    /// a test that fails without the other.
    #[test]
    fn the_frozen_hint_names_the_keymap_it_no_longer_refuses() {
        assert!(FROZEN_HINT.contains("h keymap"), "{FROZEN_HINT}");
        assert!(
            FROZEN_HINT.contains("q quit") && FROZEN_HINT.contains("j/k g/G move"),
            "{FROZEN_HINT}"
        );
    }

    /// The hint must not claim every other key is refused, because seven of
    /// them are not.
    ///
    /// Enumerated against a frozen app rather than reasoned about: `esc`,
    /// `g`/`G`, `/`, `Enter`, `e`, `s` and `S` all still act, and `/` opens
    /// the filter box outright. Only `Refresh` and the three action verbs
    /// test [`Link::Lost`](crate::lookout::app::Link::Lost). The hint states the consequence instead, which
    /// stays true however many local keys keep working.
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
