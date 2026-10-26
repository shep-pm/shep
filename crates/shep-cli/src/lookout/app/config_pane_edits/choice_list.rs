//! A list of choices opened under its row in a sheep's dog table, toggled
//! and written on close.

use serde_json::json;

use super::super::*;
use crate::lookout::app::testing::*;

/// `jobs` with a `kinds` list whose items are one of three names.
fn kinds_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "concurrency": { "type": "integer" },
            "kinds": {
                "type": "array",
                "items": { "enum": ["a", "b", "c"] },
            },
        },
    })
}

/// The table `jobs` holds before the edit.
fn stored() -> serde_json::Value {
    json!({ "concurrency": 2, "kinds": ["b", "a"] })
}

/// `web`'s config with `jobs` holding [`stored`].
fn web_with_kinds() -> SheepConfigView {
    let mut view = web_view(false);
    view.config.dogs.insert(
        "jobs".into(),
        stored().as_object().cloned().expect("a table").into(),
    );
    view
}

/// `jobs`'s table pane open over `web`, with `kinds` as its choice list.
fn app_in_kinds_table() -> App {
    let mut app = fixtures::with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    let _ = app.update(Msg::Key(KeyPress::Edit));
    let _ = app.update(Msg::Replied {
        sent: Sent::SheepConfig { name: "web".into() },
        result: Ok(Response::SheepConfig(Box::new(web_with_kinds()))),
    });
    let ask = ask_dogs(&mut app);
    answer(
        &mut app,
        "web",
        ask,
        vec![ListedDog {
            entry: SheepDogEntry {
                name: "jobs".into(),
                adopted_path: Some("/opt/jobs".into()),
                schema: Some(kinds_schema()),
            },
            asked: true,
        }],
    );
    cursor_on(&mut app, "jobs");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    assert!(
        app.config_pane()
            .is_some_and(|pane| pane.dogs_tab().rows().iter().any(DogRow::is_open)),
        "the dog's table is open inline"
    );
    app
}

fn press(app: &mut App, key: KeyPress) {
    let _ = app.update(Msg::Key(key));
}

/// The table `esc` writes, once the re-read it sends is answered with the
/// table the pane opened on.
fn closed_table(app: &mut App) -> serde_json::Value {
    assert_eq!(
        app.update(Msg::Key(KeyPress::Escape)),
        Effect::Send(Sent::SheepConfig { name: "web".into() }),
        "esc reads the sheep again before writing"
    );
    let effect = app.update(Msg::Replied {
        sent: Sent::SheepConfig { name: "web".into() },
        result: Ok(Response::SheepConfig(Box::new(web_with_kinds()))),
    });
    let batch = wire_batch(effect);
    let [
        Sent::SetSheepDogTable {
            table: Some(table), ..
        },
    ] = batch.as_slice()
    else {
        panic!("one table write: {batch:?}");
    };
    serde_json::Value::Object(table.as_map().clone())
}

#[test]
fn a_name_toggled_on_in_a_choice_list_is_the_only_difference_in_the_filed_table() {
    let mut app = app_in_kinds_table();
    pane_to(&mut app, "kinds");
    press(&mut app, KeyPress::Confirm);
    // The names are `a`, `b`, `c` in the schema's order, under the row.
    press(&mut app, KeyPress::SelectDown);
    press(&mut app, KeyPress::SelectDown);
    press(&mut app, KeyPress::SelectDown);
    press(&mut app, KeyPress::Cycle);
    press(&mut app, KeyPress::SelectUp);
    press(&mut app, KeyPress::SelectUp);
    press(&mut app, KeyPress::SelectUp);
    press(&mut app, KeyPress::Confirm);

    let written = closed_table(&mut app);

    let mut want = stored();
    want["kinds"] = json!(["b", "a", "c"]);
    assert_eq!(written, want);
}

#[test]
fn enter_on_a_name_toggles_it_as_space_does() {
    let mut app = app_in_kinds_table();
    pane_to(&mut app, "kinds");
    press(&mut app, KeyPress::Confirm);
    press(&mut app, KeyPress::SelectDown);
    press(&mut app, KeyPress::SelectDown);
    press(&mut app, KeyPress::Confirm);

    let written = closed_table(&mut app);

    let mut want = stored();
    want["kinds"] = json!(["a"]);
    assert_eq!(written, want, "`b` toggled off, in place");
}
