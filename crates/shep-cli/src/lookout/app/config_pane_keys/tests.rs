use super::*;

use shep_core::dogs::pane::{Destructive, Pane, Row};
use shep_core::protocol::{ActionOutcome, ActionReply, RpcError, RpcErrorCode};

use crate::lookout::pane::Tab;
use crate::lookout::view::fixtures;

fn models() -> Pane {
    let rows = ["a", "b", "c"].map(|key| Row::new(key, [key]));
    Pane::new().table("models", ["MODEL"], rows)
}

fn tab_of(app: &App) -> Tab {
    app.config_pane()
        .and_then(ConfigPane::status)
        .map(crate::lookout::pane::DogStatus::tab)
        .expect("the dog declares a pane")
}

fn cursor_of(app: &App) -> Option<usize> {
    app.config_pane()
        .and_then(ConfigPane::status)
        .and_then(crate::lookout::pane::DogStatus::cursor)
}

/// Whether the config pane holds an armed env delete.
fn armed(app: &App) -> Option<String> {
    app.config_pane()
        .expect("the pane is open")
        .armed_env()
        .map(|armed| armed.key.clone())
}

/// `D` on a row that names no key to remove is inert, and inert here means
/// silent: the gate's "read-only" sentence answers a write the operator
/// actually asked for, and no `D` on a field row ever did.
#[test]
fn d_on_a_field_row_arms_nothing_and_says_nothing() {
    let mut app = fixtures::app_in_sheep_pane_read_only();
    assert!(
        app.config_pane()
            .expect("the pane is open")
            .cursor_env_key_name()
            .is_none(),
        "sanity: the cursor is on a field"
    );

    let effect = app.update(Msg::Key(KeyPress::ArmDelete));

    assert!(matches!(effect, Effect::None));
    assert!(armed(&app).is_none(), "nothing armed");
    assert!(app.notice().is_none(), "and nothing said");
}

/// The same key on a row that does name a key still refuses through the
/// gate, with the reason the gate gives.
#[test]
fn d_on_an_env_row_refuses_through_the_gate_with_its_reason() {
    let mut app = fixtures::app_in_sheep_pane_read_only();
    fixtures::select_env_key(&mut app, "DB_HOST");

    let _ = app.update(Msg::Key(KeyPress::ArmDelete));

    assert!(armed(&app).is_none(), "the gate closed the arm");
    let notice = app.notice().map(ToString::to_string);
    assert!(
        notice
            .as_deref()
            .is_some_and(|notice| notice.contains("read-only")),
        "and named the gate: {notice:?}"
    );
}

#[test]
fn tab_switches_a_pane_dog_between_status_and_settings() {
    let mut app = fixtures::app_in_pane_dog(Some(fixtures::pane_reply(&models())));
    assert_eq!(tab_of(&app), Tab::Status, "a pane dog opens on status");

    let effect = app.update(Msg::Key(KeyPress::NextGroup));
    assert!(matches!(effect, Effect::None));
    assert_eq!(tab_of(&app), Tab::Settings);

    app.update(Msg::Key(KeyPress::PrevGroup));
    assert_eq!(tab_of(&app), Tab::Status, "shift-tab goes back");
    app.update(Msg::Key(KeyPress::PrevGroup));
    assert_eq!(tab_of(&app), Tab::Settings, "and either key switches");
}

#[test]
fn tab_does_nothing_for_a_dog_without_a_pane() {
    let mut app = fixtures::app_in_dog_pane();
    let before = format!("{:?}", app.config_pane());

    for key in [KeyPress::NextGroup, KeyPress::PrevGroup] {
        let effect = app.update(Msg::Key(key));
        assert!(matches!(effect, Effect::None));
    }

    assert!(
        app.config_pane()
            .is_some_and(|pane| pane.status().is_none())
    );
    assert_eq!(format!("{:?}", app.config_pane()), before);
}

#[test]
fn the_select_keys_move_the_status_cursor_and_not_the_settings_form() {
    let mut app = fixtures::app_in_pane_dog(Some(fixtures::pane_reply(&models())));
    let form_cursor = app.config_pane().and_then(ConfigPane::cursor);

    app.update(Msg::Key(KeyPress::SelectDown));
    assert_eq!(cursor_of(&app), Some(1));
    app.update(Msg::Key(KeyPress::SelectLast));
    assert_eq!(cursor_of(&app), Some(2));
    app.update(Msg::Key(KeyPress::SelectUp));
    assert_eq!(cursor_of(&app), Some(1));
    app.update(Msg::Key(KeyPress::SelectFirst));
    assert_eq!(cursor_of(&app), Some(0));

    assert_eq!(app.config_pane().and_then(ConfigPane::cursor), form_cursor);
}

#[test]
fn the_settings_tab_keeps_its_own_keys() {
    let mut app = fixtures::app_in_pane_dog(Some(fixtures::pane_reply(&models())));
    app.update(Msg::Key(KeyPress::NextGroup));
    app.update(Msg::Key(KeyPress::SelectDown));
    assert_eq!(
        cursor_of(&app),
        Some(0),
        "j on settings leaves the status cursor"
    );
}

/// Two leases, a destructive and a plain row action, a destructive and a
/// plain pane action.
fn leases() -> Pane {
    let rows = ["qwen3-32b", "strata-7b"].map(|key| Row::new(key, [key]));
    Pane::new()
        .table("leases", ["LEASE"], rows)
        .row_action("revoke", "revoke lease", Destructive::Yes)
        .row_action("renew", "renew lease", Destructive::No)
        .action("pause", "pause the board", Destructive::No)
        .action("drain", "drain the board", Destructive::Yes)
}

const ACTIONS: [&str; 4] = ["revoke", "renew", "pause", "drain"];

fn app_over(pane: &Pane, actions: &[&str]) -> App {
    fixtures::app_in_pane_dog_declaring(actions, Some(fixtures::pane_reply(pane)))
}

fn press(app: &mut App, letter: char) -> Effect {
    app.update(Msg::Key(KeyPress::TextChar(letter)))
}

fn sent_action(action: &str, row: Option<&str>) -> Effect {
    Effect::Send(Sent::DogPaneAction {
        name: "paddock".to_owned(),
        action: action.to_owned(),
        row: row.map(str::to_owned),
        opening: 1,
    })
}

fn said(app: &App) -> Option<String> {
    app.config_pane()
        .and_then(ConfigPane::status)
        .and_then(crate::lookout::pane::DogStatus::said)
        .map(str::to_owned)
}

/// The question of the modal that is open, drawn: a modal answers nothing
/// until it has been on screen.
fn asked(app: &mut App) -> Option<String> {
    app.note_modal_reach(Some(0));
    match app.modal() {
        Some(crate::lookout::app::Modal::Confirm { question, .. }) => Some(question.clone()),
        _other => None,
    }
}

#[test]
fn a_row_action_runs_on_the_row_under_the_cursor_and_a_pane_action_on_the_pane() {
    let mut app = app_over(&leases(), &ACTIONS);
    app.update(Msg::Key(KeyPress::SelectDown));

    assert_eq!(
        press(&mut app, 'e'),
        sent_action("renew", Some("strata-7b"))
    );
    replied(&mut app, plain("renewed"));
    assert_eq!(press(&mut app, 'p'), sent_action("pause", None));
}

#[test]
fn a_destructive_row_action_asks_first_and_enter_runs_it() {
    let mut app = app_over(&leases(), &ACTIONS);

    let effect = press(&mut app, 'r');

    assert_eq!(effect, Effect::None, "nothing is sent before the answer");
    assert_eq!(
        asked(&mut app).as_deref(),
        Some("Run revoke lease on qwen3-32b?")
    );
    let answer = app.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(answer, sent_action("revoke", Some("qwen3-32b")));
}

#[test]
fn a_destructive_pane_action_asks_without_a_row() {
    let mut app = app_over(&leases(), &ACTIONS);

    press(&mut app, 'd');

    assert_eq!(asked(&mut app).as_deref(), Some("Run drain the board?"));
    assert_eq!(
        app.update(Msg::Key(KeyPress::Confirm)),
        sent_action("drain", None)
    );
}

#[test]
fn any_key_but_enter_cancels_the_question_and_sends_nothing() {
    for cancel in [
        KeyPress::Escape,
        KeyPress::TextChar('y'),
        KeyPress::SelectDown,
        KeyPress::Quit,
    ] {
        let mut app = app_over(&leases(), &ACTIONS);
        press(&mut app, 'r');
        asked(&mut app);

        let effect = app.update(Msg::Key(cancel));

        assert!(
            !matches!(effect, Effect::Send(_)),
            "{cancel:?} sent {effect:?}"
        );
        assert!(app.modal().is_none(), "{cancel:?} left the question up");
        // And the answer is spent: a later Enter runs nothing.
        assert!(!matches!(
            app.update(Msg::Key(KeyPress::Confirm)),
            Effect::Send(_)
        ));
    }
}

#[test]
fn a_row_action_with_no_row_does_nothing_and_says_so() {
    let empty = Pane::new()
        .table("leases", ["LEASE"], Vec::<Row>::new())
        .row_action("revoke", "revoke lease", Destructive::No);
    let mut app = app_over(&empty, &["revoke"]);

    assert_eq!(press(&mut app, 'r'), Effect::None);

    assert_eq!(said(&app).as_deref(), Some("no row selected"));
    assert!(app.modal().is_none());
}

#[test]
fn an_action_the_manifest_lacks_sends_nothing_and_the_footer_says_so() {
    let mut app = app_over(&leases(), &["pause", "drain"]);

    // `r` is no one's letter, so it falls back to the refresh key.
    assert!(!matches!(
        press(&mut app, 'r'),
        Effect::Send(Sent::DogPaneAction { .. })
    ));
    assert_eq!(press(&mut app, 'e'), Effect::None, "renew has no letter");
    assert!(app.modal().is_none());

    let status = app.config_pane().and_then(ConfigPane::status).unwrap();
    assert_eq!(
        status.undeclared().collect::<Vec<_>>(),
        ["revoke", "renew"],
        "and the footer is told which"
    );
}

fn triggered(outcome: ActionOutcome) -> Result<Response, RequestError> {
    Ok(Response::Triggered(vec![ActionReply {
        id: 92,
        name: "paddock".to_owned(),
        outcome,
    }]))
}

fn replied(app: &mut App, result: Result<Response, RequestError>) -> Effect {
    replied_to_opening(app, 1, result)
}

fn replied_to_opening(
    app: &mut App,
    opening: u64,
    result: Result<Response, RequestError>,
) -> Effect {
    app.update(Msg::Replied {
        sent: Sent::DogPaneAction {
            name: "paddock".to_owned(),
            action: "renew".to_owned(),
            row: Some("qwen3-32b".to_owned()),
            opening,
        },
        result,
    })
}

fn plain(body: &str) -> Result<Response, RequestError> {
    triggered(ActionOutcome::Replied {
        body: body.to_owned(),
    })
}

fn pull() -> Effect {
    Effect::Send(Sent::DogPane {
        name: "paddock".to_owned(),
        opening: 1,
    })
}

#[test]
fn a_plain_reply_pulls_the_pane_again_and_shows_its_text() {
    let mut app = app_over(&leases(), &ACTIONS);

    let effect = replied(
        &mut app,
        triggered(ActionOutcome::Replied {
            body: "lease renewed".to_owned(),
        }),
    );

    assert_eq!(
        effect,
        Effect::Send(Sent::DogPane {
            name: "paddock".to_owned(),
            opening: 1,
        })
    );
    assert_eq!(said(&app).as_deref(), Some("lease renewed"));
}

#[test]
fn a_refusal_and_a_timeout_show_their_reasons_and_pull_nothing() {
    let refused = Err(RequestError::Rpc(RpcError {
        code: RpcErrorCode::NotFound,
        message: "no sheep named paddock".into(),
        daemon_version: None,
    }));
    let cases = [
        (refused, "no sheep named paddock"),
        (
            triggered(ActionOutcome::TimedOut),
            "paddock did not answer in time",
        ),
        (
            Err(RequestError::Timeout {
                after: std::time::Duration::from_secs(60),
            }),
            "no reply within 60s",
        ),
    ];
    for (result, reason) in cases {
        let mut app = app_over(&leases(), &ACTIONS);
        assert_eq!(replied(&mut app, result), Effect::None);
        assert_eq!(said(&app).as_deref(), Some(reason));
    }
}

#[test]
fn a_reply_for_a_pane_the_operator_left_is_dropped() {
    let mut app = app_over(&leases(), &ACTIONS);
    app.update(Msg::Key(KeyPress::Escape));
    assert!(app.config_pane().is_none());

    let effect = replied(
        &mut app,
        triggered(ActionOutcome::Replied {
            body: "lease renewed".to_owned(),
        }),
    );

    assert_eq!(effect, Effect::None);
}

#[test]
fn a_read_only_lookout_runs_no_action() {
    let mut app = app_over(&leases(), &ACTIONS);
    app.set_control_for_tests(Control::ReadOnly);

    assert_eq!(press(&mut app, 'e'), Effect::None);
    assert_eq!(press(&mut app, 'r'), Effect::None);

    assert!(app.modal().is_none());
    assert!(app.notice().is_some_and(|notice| notice.is_grave()));
}

#[test]
fn the_status_tab_reads_keys_in_pane_mode_and_the_settings_tab_in_normal() {
    let mut app = app_over(&leases(), &ACTIONS);
    assert_eq!(app.mode(), InputMode::Pane);

    app.update(Msg::Key(KeyPress::NextGroup));
    assert_eq!(app.mode(), InputMode::Normal);
    app.update(Msg::Key(KeyPress::NextGroup));
    assert_eq!(app.mode(), InputMode::Pane);
}

#[test]
fn r_and_h_keep_refresh_and_help_when_no_action_takes_them() {
    let mut app = app_over(&leases(), &ACTIONS);

    assert!(matches!(press(&mut app, 'r'), Effect::None), "revoke has r");
    assert!(app.modal().is_some(), "and asked first");

    let mut app = app_over(&leases(), &["pause"]);
    assert_eq!(
        press(&mut app, 'r'),
        Effect::Send(Sent::DogSection {
            name: "paddock".to_owned()
        }),
        "r re-reads the dog's section"
    );
    press(&mut app, 'h');
    assert!(app.keymap_open());
}

#[test]
fn closing_the_pane_retires_a_question_it_asked() {
    let mut app = app_over(&leases(), &ACTIONS);
    press(&mut app, 'r');
    asked(&mut app);

    app.close_pane();

    assert!(app.modal().is_none(), "the question went with the pane");
    assert!(!matches!(
        app.update(Msg::Key(KeyPress::Confirm)),
        Effect::Send(_)
    ));
}

#[test]
fn a_reply_from_an_earlier_opening_of_the_pane_is_dropped() {
    let mut app = app_over(&leases(), &ACTIONS);

    let effect = replied_to_opening(&mut app, 0, plain("lease renewed"));

    assert_eq!(effect, Effect::None);
    assert_eq!(said(&app), None, "the open pane never heard of it");
}

#[test]
fn an_action_answered_while_a_pull_is_out_pulls_again_when_that_pull_lands() {
    let mut app = app_over(&leases(), &ACTIONS);
    assert_eq!(
        replied(&mut app, plain("one")),
        pull(),
        "the first pull is out"
    );

    let queued = replied(&mut app, plain("two"));
    assert_eq!(queued, Effect::None, "one pull at a time");
    assert_eq!(said(&app).as_deref(), Some("two"));

    let landed = app.update(Msg::Replied {
        sent: Sent::DogPane {
            name: "paddock".to_owned(),
            opening: 1,
        },
        result: plain(&leases().body().unwrap()),
    });
    assert_eq!(
        landed,
        pull(),
        "the pull that may predate the action is redone"
    );

    let again = app.update(Msg::Replied {
        sent: Sent::DogPane {
            name: "paddock".to_owned(),
            opening: 1,
        },
        result: plain(&leases().body().unwrap()),
    });
    assert_eq!(again, Effect::None, "and only once");
}

#[test]
fn one_action_runs_at_a_time_on_a_pane() {
    let mut app = app_over(&leases(), &ACTIONS);
    assert_eq!(press(&mut app, 'p'), sent_action("pause", None));

    assert_eq!(press(&mut app, 'p'), Effect::None, "the first is still out");
    assert_eq!(said(&app).as_deref(), Some("an action is still running"));
    assert_eq!(press(&mut app, 'd'), Effect::None);
    assert!(app.modal().is_none(), "and no question is asked either");

    replied(&mut app, plain("paused"));
    assert_eq!(press(&mut app, 'p'), sent_action("pause", None));
}

#[test]
fn an_action_that_never_left_frees_the_pane() {
    let mut app = app_over(&leases(), &ACTIONS);
    press(&mut app, 'p');

    app.update(Msg::Unsent {
        sent: Sent::DogPaneAction {
            name: "paddock".to_owned(),
            action: "pause".to_owned(),
            row: None,
            opening: 1,
        },
    });

    assert_eq!(said(&app).as_deref(), Some("pause was not sent"));
    assert_eq!(press(&mut app, 'p'), sent_action("pause", None));
}
