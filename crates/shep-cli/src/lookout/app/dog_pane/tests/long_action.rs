use super::super::*;
use crate::lookout::view::fixtures;
use shep_client::dogs::ActionId;
use shep_core::dogs::pane::{Destructive, Pane};
use shep_core::protocol::{ActionOutcome, ActionReply};

fn bar(app: &App) -> String {
    fixtures::rendered(&crate::lookout::view::status::status_line(app, 200))
}

/// `paddock`'s status tab with a `pause` pane action started, and the id
/// the dog answered with.
fn running() -> (App, ActionId) {
    let pane = Pane::new()
        .fields("host", [("VRAM", "20 / 24 GiB")])
        .action("pause", "pause the board", Destructive::No);
    let mut app =
        fixtures::app_in_pane_dog_with_actions(&["pause"], Some(fixtures::pane_reply(&pane)));
    let effect = app.update(Msg::Key(KeyPress::TextChar('p')));
    let Effect::Send(sent) = effect else {
        panic!("p runs the pause action: {effect:?}");
    };
    let (id, body) = ActionId::accept();
    let effect = app.update(Msg::Replied {
        sent,
        result: Ok(Response::Triggered(vec![ActionReply {
            id: 92,
            name: "paddock".to_owned(),
            outcome: ActionOutcome::Replied { body },
        }])),
    });
    assert_eq!(effect, Effect::None);
    (app, id)
}

fn done(dog: &str, payload: serde_json::Value) -> Msg {
    Msg::Event(BusEvent::Dog {
        dog: dog.to_owned(),
        kind: "action.done".to_owned(),
        payload,
    })
}

fn pull() -> Effect {
    Effect::Send(Sent::DogPane {
        name: "paddock".to_owned(),
        opening: 1,
    })
}

#[test]
fn a_matching_done_shows_the_result_for_a_few_seconds_and_pulls_the_pane() {
    let (mut app, id) = running();
    assert!(bar(&app).contains("paddock: pause the board running"));

    let effect = app.update(done(
        "paddock",
        serde_json::json!({ "id": id.as_str(), "result": { "paused": true } }),
    ));

    assert_eq!(effect, pull());
    assert!(
        bar(&app).starts_with("pause the board: done"),
        "{:?}",
        bar(&app)
    );
    assert!(!bar(&app).contains("running"), "{:?}", bar(&app));

    let start = app.now();
    app.update(Msg::Tick {
        now: start + Duration::from_secs(4),
    });
    assert!(
        bar(&app).starts_with("pause the board: done"),
        "{:?}",
        bar(&app)
    );
    app.update(Msg::Tick {
        now: start + Duration::from_secs(6),
    });
    assert!(!bar(&app).contains("pause the board"), "{:?}", bar(&app));
}

#[test]
fn a_done_nobody_started_and_another_dogs_done_change_nothing() {
    let (mut app, id) = running();
    let before = bar(&app);

    let stranger = ActionId::accept().0;
    let unknown = app.update(done(
        "paddock",
        serde_json::json!({ "id": stranger.as_str(), "result": 1 }),
    ));
    let other_dog = app.update(done(
        "otel",
        serde_json::json!({ "id": id.as_str(), "result": 1 }),
    ));
    let unreadable = app.update(done("paddock", serde_json::json!({ "id": id.as_str() })));

    assert_eq!(
        [unknown, other_dog, unreadable],
        [const { Effect::None }; 3]
    );
    assert_eq!(bar(&app), before);
}

#[test]
fn a_failure_shows_its_error_cleaned_and_cut_to_a_line() {
    let (mut app, id) = running();
    let error = format!("disk \u{1b}[31mfull\u{7}\nsecond line\n{}", "x".repeat(200));

    app.update(done(
        "paddock",
        serde_json::json!({ "id": id.as_str(), "error": error }),
    ));

    let shown = bar(&app);
    assert!(
        shown.starts_with("pause the board: failed, disk [31mfull"),
        "{shown:?}"
    );
    assert!(!shown.contains("second line"), "{shown:?}");
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\u{7}'),
        "{shown:?}"
    );

    let (mut app, id) = running();
    let long = "y".repeat(200);
    app.update(done(
        "paddock",
        serde_json::json!({ "id": id.as_str(), "error": long }),
    ));
    assert!(bar(&app).contains(&format!("failed, {}\u{2026}", "y".repeat(79))));
}

#[test]
fn closing_the_pane_keeps_the_item_until_the_result_arrives() {
    let (mut app, id) = running();

    app.update(Msg::Key(KeyPress::Escape));
    assert!(app.config_pane().is_none());
    assert!(
        bar(&app).contains("paddock: pause the board running"),
        "{:?}",
        bar(&app)
    );

    let effect = app.update(done(
        "paddock",
        serde_json::json!({ "id": id.as_str(), "result": null }),
    ));
    assert_eq!(effect, Effect::None, "no pane is open to pull");
    assert!(
        bar(&app).starts_with("pause the board: done"),
        "{:?}",
        bar(&app)
    );
}

#[test]
fn a_lost_link_or_a_dropped_repair_ends_the_item_as_unknown() {
    let losses = [
        Msg::Event(BusEvent::Dropped { count: 3 }),
        Msg::BusLagged { count: 3 },
        Msg::Retrying { attempt: 1 },
        Msg::Frozen {
            at_local: "12:00".to_owned(),
            why: "gone".to_owned(),
        },
    ];
    for loss in losses {
        let (mut app, id) = running();
        let label = format!("{loss:?}");

        app.update(loss);

        let shown = bar(&app);
        assert!(
            shown.starts_with("pause the board: result unknown"),
            "{label}: {shown:?}"
        );
        assert!(!shown.contains("running"), "{label}: {shown:?}");

        let effect = app.update(done(
            "paddock",
            serde_json::json!({ "id": id.as_str(), "result": 1 }),
        ));
        assert_eq!(
            effect,
            Effect::None,
            "{label}: a late result has nothing to match"
        );
    }
}
