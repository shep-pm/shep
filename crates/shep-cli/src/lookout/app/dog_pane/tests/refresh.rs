use super::super::*;
use crate::lookout::app::testing::*;
use shep_core::dogs::pane::Pane;
use shep_core::protocol::ActionOutcome;

/// A pane whose `host` section names a declared kind, and whose `leases`
/// table names one the manifest does not declare.
fn paddock_pane() -> ActionOutcome {
    let pane = Pane::new()
        .fields("host", [("VRAM", "20 / 24 GiB")])
        .refresh_on(["lease.changed"])
        .table("leases", ["holder"], [])
        .refresh_on(["ghost.kind"]);
    ActionOutcome::Replied {
        body: pane.body().unwrap(),
    }
}

/// `paddock`'s status tab with its first pull answered, a second after
/// the app's clock started.
fn status_tab() -> App {
    let mut app = fixtures::app_in_pane_dog(Some(paddock_pane()));
    let at = app.now() + Duration::from_secs(2);
    assert_eq!(app.update(Msg::Tick { now: at }), Effect::None);
    app
}

fn pull() -> Sent {
    Sent::DogPane {
        name: "paddock".to_string(),
        opening: 1,
    }
}

fn published(dog: &str, kind: &str) -> Msg {
    Msg::Event(BusEvent::Dog {
        dog: dog.to_string(),
        kind: kind.to_string(),
        payload: serde_json::Value::Null,
    })
}

fn answered(app: &mut App) {
    app.update(Msg::Replied {
        sent: pull(),
        result: Ok(Response::Triggered(vec![
            shep_core::protocol::ActionReply {
                id: 92,
                name: "paddock".to_string(),
                outcome: paddock_pane(),
            },
        ])),
    });
}

fn tick_at(app: &mut App, after: Duration) -> Effect {
    let now = app.now() + after;
    app.update(Msg::Tick { now })
}

#[test]
fn an_event_a_section_names_pulls_at_once_when_none_went_in_the_last_second() {
    let mut app = status_tab();
    let effect = app.update(published("paddock", "lease.changed"));
    assert_eq!(effect, Effect::Send(pull()));
}

#[test]
fn ten_named_events_in_half_a_second_give_one_pull_at_once_and_one_on_the_tick_after() {
    let mut app = status_tab();
    let start = app.now();
    let tick = |app: &mut App, after: Duration| app.update(Msg::Tick { now: start + after });
    let mut sent = Vec::new();
    for step in 0..10 {
        tick(&mut app, Duration::from_millis(step * 50));
        sent.push(app.update(published("paddock", "lease.changed")));
    }
    assert_eq!(sent[0], Effect::Send(pull()), "the first goes at once");
    assert!(sent[1..].iter().all(|effect| *effect == Effect::None));
    answered(&mut app);

    assert_eq!(tick(&mut app, Duration::from_millis(500)), Effect::None);
    let trailing = tick(&mut app, Duration::from_secs(1));
    assert_eq!(trailing, Effect::Send(pull()), "one for the rest");
    answered(&mut app);
    assert_eq!(tick(&mut app, Duration::from_secs(3)), Effect::None);
}

#[test]
fn an_event_that_changes_nothing_on_screen_sends_nothing() {
    let named = published("paddock", "lease.changed");
    let cases = [
        ("another dog", published("kelpie", "lease.changed")),
        ("an unnamed kind", published("paddock", "model.loaded")),
        ("an undeclared kind", published("paddock", "ghost.kind")),
    ];
    for (why, event) in cases {
        let mut app = status_tab();
        assert_eq!(app.update(event), Effect::None, "{why}");
        assert_eq!(tick_at(&mut app, Duration::from_secs(3)), Effect::None);
    }

    let mut app = status_tab();
    app.update(Msg::Key(KeyPress::NextGroup));
    assert_eq!(app.update(named.clone()), Effect::None, "settings tab");

    let mut app = fixtures::app_in_pane_dog(None);
    let at = app.now() + Duration::from_secs(2);
    app.update(Msg::Tick { now: at });
    assert_eq!(app.update(named.clone()), Effect::None, "a pull waiting");

    let mut app = status_tab();
    app.update(Msg::Key(KeyPress::Escape));
    assert!(app.config_pane().is_none());
    assert_eq!(app.update(named), Effect::None, "a closed pane");
    assert_eq!(tick_at(&mut app, Duration::from_secs(3)), Effect::None);
}

#[test]
fn leaving_the_status_tab_drops_a_pending_pull_and_returning_pulls_once() {
    let mut app = status_tab();
    assert_eq!(
        app.update(published("paddock", "lease.changed")),
        Effect::Send(pull())
    );
    answered(&mut app);
    let held = app.update(published("paddock", "lease.changed"));
    assert_eq!(held, Effect::None, "a second too soon, so it waits");

    assert_eq!(app.update(Msg::Key(KeyPress::NextGroup)), Effect::None);
    assert_eq!(tick_at(&mut app, Duration::from_secs(3)), Effect::None);

    let back = app.update(Msg::Key(KeyPress::NextGroup));
    assert_eq!(back, Effect::Send(pull()));
    answered(&mut app);
    assert_eq!(tick_at(&mut app, Duration::from_secs(3)), Effect::None);
}

#[test]
fn a_lost_event_marks_a_pull_due_for_an_open_status_tab() {
    let lost = [
        Msg::Event(BusEvent::Dropped { count: 3 }),
        Msg::BusLagged { count: 3 },
    ];
    for msg in lost {
        let mut app = status_tab();
        assert_eq!(app.update(msg.clone()), Effect::PollNow);
        assert_eq!(tick_at(&mut app, Duration::ZERO), Effect::Send(pull()));
    }

    let mut app = status_tab();
    app.update(Msg::Key(KeyPress::NextGroup));
    app.update(Msg::Event(BusEvent::Dropped { count: 3 }));
    app.update(Msg::Key(KeyPress::NextGroup));
    answered(&mut app);
    assert_eq!(tick_at(&mut app, Duration::from_secs(3)), Effect::None);
}

#[test]
fn a_relink_marks_a_pull_due_since_events_in_the_gap_were_never_delivered() {
    let mut app = status_tab();
    app.update(Msg::Retrying { attempt: 1 });
    assert_eq!(app.update(Msg::Relinked), Effect::None);
    assert_eq!(tick_at(&mut app, Duration::ZERO), Effect::Send(pull()));
}
