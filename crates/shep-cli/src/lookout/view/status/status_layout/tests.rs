use super::super::super::super::app::{App, Control};

use super::super::super::fixtures::{
    acting_app, allowed_app, app_armed_to_delete_a_secret, app_in_settings, app_in_settings_on,
    app_in_settings_with_control, armed_app, armed_app_with_a_filter_and_a_notice, editing_app,
    filtered_app, rendered, with_selection,
};
use super::super::testing::*;
use super::*;
use crate::commands::dogs::Step;
use crate::lookout::app::{ActionVerb, Effect, KeyPress, Msg};
use crate::lookout::theme::Palette;
use shep_core::protocol::BusEvent;
use std::ffi::OsStr;
use std::time::{Duration, Instant};

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
    let mut app = app_in_settings_on("daemon.socket");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    let bar = rendered(&status_line(&app, 120));
    assert!(
        bar.contains("editing daemon.socket  "),
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

#[test]
fn the_panes_edit_keys_are_advertised_only_when_the_gate_is_open() {
    let closed = rendered(&status_line(
        &super::super::super::fixtures::app_in_sheep_pane(),
        200,
    ));
    let open = rendered(&status_line(
        &super::super::super::fixtures::app_in_sheep_pane_with_control(),
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
        open.contains("esc close"),
        "the gate is open, but esc only asks or closes now: {open:?}"
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
/// field list's own hint: `esc close`, not a sub-screen's `esc back`,
/// and `e edit` since `begin_env_typing` is the same door
/// `confirm_field` already opens for a typed field.
#[test]
fn an_env_rows_status_bar_hint_is_the_field_lists_own() {
    let mut app = super::super::super::fixtures::app_in_sheep_pane_with_control();
    app.update(Msg::Key(KeyPress::SelectLast));
    assert!(matches!(
        app.config_pane().unwrap().cursor(),
        Some(crate::lookout::pane::PaneRow::Env(_) | crate::lookout::pane::PaneRow::AddEnv)
    ));
    let bar = rendered(&status_line(&app, 200));
    assert!(bar.contains("esc close"), "got {bar:?}");
    assert!(bar.contains("e edit"), "got {bar:?}");
}

/// Nothing the config pane files is armed or in flight, so the bar
/// keeps its key hint rather than asking a question the operator's
/// next keystroke does not answer.
#[test]
fn a_filed_pane_edit_leaves_the_status_bar_on_its_key_hint() {
    let mut app = super::super::super::fixtures::app_in_sheep_pane_with_control();
    pane_to(&mut app, "autorestart");
    app.update(Msg::Key(KeyPress::Cycle));
    let bar = rendered(&status_line(&app, 200));
    assert!(!bar.contains("enter confirms"), "got {bar:?}");
    assert!(!bar.contains("sent, waiting"), "got {bar:?}");
    assert!(bar.contains("esc close"), "got {bar:?}");
}

#[test]
fn the_panes_editors_get_their_own_status_line_rather_than_the_filters() {
    let mut app = super::super::super::fixtures::app_in_sheep_pane_with_control();
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
    assert!(env.contains("env DB_HOST VALUE"), "got {env:?}");
    assert!(env.contains("\u{2588}\u{258f}"), "got {env:?}");
    assert!(!env.contains("  y"), "the typed value is drawn: {env:?}");
    assert!(!env.contains("filter"), "got {env:?}");
}

/// The bar echoes the open editor, so it masks what its row masks and
/// `ctrl-r` shows both.
#[test]
fn a_secrets_value_is_masked_on_the_bar_while_it_is_typed() {
    use crate::lookout::textbox::TextEdit;

    let mut app = super::super::super::fixtures::app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::SelectLast));
    app.update(Msg::Key(KeyPress::Confirm));
    for c in "DB_NEW".chars() {
        app.update(Msg::Key(KeyPress::TextChar(c)));
    }
    let key = rendered(&status_line(&app, 200));
    assert!(
        key.contains("DB_NEW\u{258f}"),
        "KEY is not a secret: {key:?}"
    );

    app.update(Msg::Key(KeyPress::NextField));
    for c in "hunter2".chars() {
        app.update(Msg::Key(KeyPress::TextChar(c)));
    }
    let masked = rendered(&status_line(&app, 200));
    assert!(!masked.contains("hunter2"), "got {masked:?}");
    assert!(
        masked.contains("\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{258f}"),
        "got {masked:?}"
    );

    app.update(Msg::Key(KeyPress::TextEdit(TextEdit::Reveal)));
    let shown = rendered(&status_line(&app, 200));
    assert!(shown.contains("hunter2\u{258f}"), "got {shown:?}");
}

/// A sheep's env VALUE, through the same slot.
#[test]
fn an_env_value_is_masked_on_the_bar_while_it_is_typed() {
    let mut app = super::super::super::fixtures::app_in_sheep_pane_with_control();
    app.update(Msg::Key(KeyPress::SelectLast));
    app.update(Msg::Key(KeyPress::Confirm));
    app.update(Msg::Key(KeyPress::TextChar('K')));
    app.update(Msg::Key(KeyPress::NextField));
    for c in "hunter2".chars() {
        app.update(Msg::Key(KeyPress::TextChar(c)));
    }
    let bar = rendered(&status_line(&app, 200));
    assert!(!bar.contains("hunter2"), "got {bar:?}");
    assert!(bar.contains("\u{2588}\u{258f}"), "got {bar:?}");
}

/// The whole point of one editor: the keys the bar names for a focused
/// field are the same words on both panes, so an operator who learned one
/// has learned the other.
#[test]
fn the_key_hint_for_each_field_reads_the_same_in_both_panes() {
    let mut env_app = super::super::super::fixtures::app_in_sheep_pane_with_control();
    // `SelectLast` lands on `+ add a key`, the env row whose editor opens
    // on KEY.
    env_app.update(Msg::Key(KeyPress::SelectLast));
    env_app.update(Msg::Key(KeyPress::Confirm));
    let env_key = rendered(&status_line(&env_app, 200));

    let mut secret_app = super::super::super::fixtures::app_with_secrets_and_control();
    secret_app.update(Msg::Key(KeyPress::SelectLast));
    secret_app.update(Msg::Key(KeyPress::Confirm));
    let secret_key = rendered(&status_line(&secret_app, 200));

    let on_key = "tab value   enter next   esc cancels";
    assert!(env_key.contains(on_key), "env KEY focus: {env_key:?}");
    assert!(
        secret_key.contains(on_key),
        "secrets KEY focus: {secret_key:?}"
    );

    env_app.update(Msg::Key(KeyPress::TextChar('A')));
    secret_app.update(Msg::Key(KeyPress::TextChar('A')));
    env_app.update(Msg::Key(KeyPress::NextField));
    secret_app.update(Msg::Key(KeyPress::NextField));
    let env_value = rendered(&status_line(&env_app, 200));
    let secret_value = rendered(&status_line(&secret_app, 200));

    let on_value = "tab key   enter writes   esc cancels";
    assert!(
        env_value.contains(on_value),
        "env VALUE focus: {env_value:?}"
    );
    assert!(
        secret_value.contains(on_value),
        "secrets VALUE focus: {secret_value:?}"
    );
}

/// An armed env delete takes the bar the way an armed secret delete
/// already does, with the same promise.
#[test]
fn an_armed_env_delete_owns_the_status_line() {
    let mut app = super::super::super::fixtures::app_in_sheep_pane_with_control();
    // `SelectLast` lands on `+ add a key`; two steps up is `DB_HOST`, the
    // fixture's first env key. `D` arms its removal.
    app.update(Msg::Key(KeyPress::SelectLast));
    app.update(Msg::Key(KeyPress::SelectUp));
    app.update(Msg::Key(KeyPress::SelectUp));
    app.update(Msg::Key(KeyPress::ArmDelete));
    let bar = rendered(&status_line(&app, 200));
    assert!(
        bar.contains("delete DB_HOST? enter confirms, any other key cancels"),
        "{bar:?}"
    );
    assert!(
        !bar.contains("e edit"),
        "the arm outranks the pane's own hint: {bar:?}"
    );
}

#[test]
fn the_config_pane_gets_its_own_key_hint() {
    let app = super::super::super::fixtures::app_in_sheep_pane();
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
    let mut app = super::super::super::fixtures::app_in_sheep_pane_with_control();
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

/// The bar's dog-job item: the dog, the step and the time spent, and on a
/// bar too narrow for both, the item goes and the key hint stays whole.
///
/// The keys are what every screen needs while an install runs, so they
/// outrank the one item that says what this machine is doing on its own.
#[test]
fn the_dog_job_item_draws_with_name_step_and_time_and_goes_before_the_hint() {
    let t0 = Instant::now();
    let mut app = App::new(
        Palette::detect(None, None, None),
        Control::Allowed,
        "/home/ada/.shep".to_string(),
        t0,
    );
    assert!(matches!(
        app.start_install("otel"),
        Effect::StartDogJob { .. }
    ));
    let _ = app.update(Msg::DogStep {
        name: "otel".to_owned(),
        step: Step::Building,
    });
    let _ = app.update(Msg::Tick {
        now: t0 + Duration::from_secs(7),
    });

    let wide = rendered(&status_line(&app, 200));
    assert!(
        wide.contains("installing otel  building with cargo  7s"),
        "{wide:?}"
    );

    // The narrowest bar that still shows the whole key hint. The item has
    // to be the thing that goes.
    let hint = hint_for(app.control(), app.settings().is_some(), app.grouping());
    let label = u16::try_from(CONTROL_ENABLED_LABEL.chars().count()).unwrap();
    let fits = u16::try_from(hint.chars().count()).expect("the hint has a width") + label + 1;
    let bar = rendered(&status_line(&app, fits));
    assert!(!bar.contains("installing otel"), "{bar:?}");
    assert!(bar.contains(&hint), "the whole hint survives: {bar:?}");
}

const A_HINT: &str = "hint: web has restarted 3 times. Try: shep enable bark";

fn app_with_hint(control: Control) -> App {
    let mut app = App::new(
        Palette::detect(None, None, None),
        control,
        "/home/ada/.shep".to_string(),
        Instant::now(),
    );
    let _ = app.update(Msg::Hint {
        line: Some(A_HINT.to_owned()),
    });
    app
}

fn keys_of(app: &App) -> String {
    hint_for(app.control(), app.settings().is_some(), app.grouping())
}

#[test]
fn a_hint_follows_the_dashboard_keys_when_both_fit() {
    let app = app_with_hint(Control::ReadOnly);
    let keys = keys_of(&app);

    let bar = rendered(&status_line(&app, 200));

    assert!(
        bar.starts_with(&format!("{keys}   {A_HINT}")),
        "keys, three spaces, then the hint: {bar:?}"
    );
    assert!(bar.trim_end().ends_with("read-only"), "{bar:?}");
}

#[test]
fn a_hint_that_does_not_fit_beside_the_keys_is_dropped_and_the_keys_stay_whole() {
    let app = app_with_hint(Control::ReadOnly);
    let keys = keys_of(&app);
    let label = u16::try_from(READ_ONLY_LABEL.chars().count()).unwrap();
    let keys_width = u16::try_from(keys.chars().count()).unwrap();
    let hint_width = u16::try_from(A_HINT.chars().count()).unwrap();
    let both = keys_width + 3 + hint_width + label + 1;

    let fits = rendered(&status_line(&app, both));
    assert!(fits.contains(A_HINT), "{fits:?}");

    for width in [both - 1, keys_width + label + 1] {
        let bar = rendered(&status_line(&app, width));
        assert!(!bar.contains("hint:"), "width {width}: {bar:?}");
        assert!(
            bar.contains(&keys),
            "width {width}: the keys are whole: {bar:?}"
        );
    }
}

#[test]
fn a_prompt_a_notice_and_the_frozen_line_outrank_the_hint() {
    let hint = Msg::Hint {
        line: Some(A_HINT.to_owned()),
    };

    let mut prompt = armed_app(ActionVerb::Stop);
    let _ = prompt.update(hint.clone());
    assert!(!rendered(&status_line(&prompt, 300)).contains("hint:"));

    let mut notice = App::new(
        Palette::detect(None, None, None),
        Control::ReadOnly,
        "/home/ada/.shep".to_string(),
        Instant::now(),
    );
    let _ = notice.update(hint.clone());
    let _ = notice.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
    assert!(notice.notice().is_some());
    assert!(!rendered(&status_line(&notice, 300)).contains("hint:"));

    let mut filtered = filtered_app("web");
    let _ = filtered.update(hint.clone());
    assert!(!rendered(&status_line(&filtered, 300)).contains("hint:"));

    let mut frozen = app_with_hint(Control::Allowed);
    let _ = frozen.update(Msg::Frozen {
        at_local: "2026-08-14 14:32:07".to_string(),
        why: "the shepherd did not answer".to_string(),
    });
    let _ = frozen.update(hint);
    let bar = rendered(&status_line(&frozen, 300));
    assert!(bar.contains(FROZEN_HINT), "{bar:?}");
    assert!(!bar.contains("hint:"), "{bar:?}");
}

#[test]
fn the_settings_screen_keeps_its_keys_and_carries_no_hint() {
    let mut app = app_in_settings();
    let _ = app.update(Msg::Hint {
        line: Some(A_HINT.to_owned()),
    });

    let bar = rendered(&status_line(&app, 300));

    assert!(!bar.contains("hint:"), "{bar:?}");
}
