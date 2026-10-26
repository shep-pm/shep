//! A tagged union opened under its row in a sheep's dog table, edited by key
//! and written on close as the one table its arm describes.

use serde_json::json;

use super::super::*;
use crate::lookout::app::testing::*;

/// `kelpie`'s settings: one step, `retro`, that is one of three shapes.
fn step_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "retro": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "kind": { "type": "string", "const": "none" } },
                        "required": ["kind"],
                    },
                    {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "const": "path" },
                            "path": { "type": "string" },
                        },
                        "required": ["kind", "path"],
                    },
                ],
            },
        },
    })
}

/// The table `kelpie` holds before the edit.
fn stored() -> serde_json::Value {
    json!({ "retro": { "kind": "path", "path": "skills/mine" } })
}

fn web_with_retro() -> SheepConfigView {
    let mut view = web_view(false);
    view.config.dogs.insert(
        "kelpie".into(),
        stored().as_object().cloned().expect("a table").into(),
    );
    view
}

/// `kelpie`'s table pane open over `web`, the cursor on `retro`.
fn app_on_retro() -> App {
    let mut app = fixtures::with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    let _ = app.update(Msg::Key(KeyPress::Edit));
    let _ = app.update(Msg::Replied {
        sent: Sent::SheepConfig { name: "web".into() },
        result: Ok(Response::SheepConfig(Box::new(web_with_retro()))),
    });
    let ask = ask_dogs(&mut app);
    answer(
        &mut app,
        "web",
        ask,
        vec![ListedDog {
            entry: SheepDogEntry {
                name: "kelpie".into(),
                adopted_path: Some("/opt/kelpie".into()),
                schema: Some(step_schema()),
            },
            asked: true,
        }],
    );
    cursor_on(&mut app, "kelpie");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    pane_to(&mut app, "retro");
    app
}

fn press(app: &mut App, key: KeyPress) {
    let _ = app.update(Msg::Key(key));
}

/// `retro` opened, and the cursor walked `down` rows below it.
fn open_and_walk(app: &mut App, down: usize) {
    press(app, KeyPress::Confirm);
    for _ in 0..down {
        press(app, KeyPress::SelectDown);
    }
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
        result: Ok(Response::SheepConfig(Box::new(web_with_retro()))),
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
fn space_on_the_kind_row_writes_the_table_of_the_next_arm() {
    let mut app = app_on_retro();
    open_and_walk(&mut app, 1);
    press(&mut app, KeyPress::Cycle);
    assert_eq!(
        closed_table(&mut app),
        json!({ "retro": { "kind": "none" } })
    );
}

#[test]
fn d_on_the_union_row_writes_the_table_without_it() {
    let mut app = app_on_retro();
    press(&mut app, KeyPress::Remove);
    assert_eq!(closed_table(&mut app), json!({}));
}

#[test]
fn a_field_of_the_arm_is_typed_like_any_other() {
    let mut app = app_on_retro();
    open_and_walk(&mut app, 2);
    press(&mut app, KeyPress::Confirm);
    for _ in 0.."skills/mine".len() {
        press(&mut app, KeyPress::TextBackspace);
    }
    for character in "skills/yours".chars() {
        press(&mut app, KeyPress::TextChar(character));
    }
    press(&mut app, KeyPress::TextApply);
    assert_eq!(
        closed_table(&mut app),
        json!({ "retro": { "kind": "path", "path": "skills/yours" } })
    );
}

#[test]
fn d_on_a_required_field_says_it_is_required_and_files_nothing() {
    let mut app = app_on_retro();
    open_and_walk(&mut app, 2);
    press(&mut app, KeyPress::Remove);
    let notice = app.notice().expect("a refusal");
    assert!(notice.grave);
    assert!(notice.text.contains("path is required"), "{}", notice.text);
    assert!(app.config_pane().unwrap().edits().is_empty());
}

#[test]
fn esc_holds_the_pane_while_a_picked_tag_lacks_a_field_it_requires() {
    let mut app = app_on_retro();
    open_and_walk(&mut app, 1);
    press(&mut app, KeyPress::Cycle);
    press(&mut app, KeyPress::Cycle);
    assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
    let notice = app.notice().expect("the hold says why");
    assert!(notice.grave);
    assert!(notice.text.contains("path is required"), "{}", notice.text);
    assert!(app.config_pane().is_some(), "the pane stays open");
    assert_eq!(table_edits(&app), 1);

    press(&mut app, KeyPress::SelectDown);
    press(&mut app, KeyPress::Confirm);
    for character in "skills/new".chars() {
        press(&mut app, KeyPress::TextChar(character));
    }
    press(&mut app, KeyPress::TextApply);
    assert_eq!(
        closed_table(&mut app),
        json!({ "retro": { "kind": "path", "path": "skills/new" } })
    );
}

#[test]
fn a_tag_picked_while_the_close_read_is_out_is_held_when_the_read_lands() {
    let mut app = app_on_retro();
    open_and_walk(&mut app, 2);
    press(&mut app, KeyPress::Confirm);
    press(&mut app, KeyPress::TextChar('x'));
    press(&mut app, KeyPress::TextApply);
    assert_eq!(
        app.update(Msg::Key(KeyPress::Escape)),
        Effect::Send(Sent::SheepConfig { name: "web".into() }),
        "the table is whole, so esc asks for the re-read"
    );
    press(&mut app, KeyPress::SelectUp);
    press(&mut app, KeyPress::Cycle);
    press(&mut app, KeyPress::Cycle);
    let effect = app.update(Msg::Replied {
        sent: Sent::SheepConfig { name: "web".into() },
        result: Ok(Response::SheepConfig(Box::new(web_with_retro()))),
    });
    assert_eq!(effect, Effect::None, "nothing is written");
    let notice = app.notice().expect("the hold says why");
    assert!(notice.grave);
    assert!(notice.text.contains("path is required"), "{}", notice.text);
    assert!(app.config_pane().is_some(), "the pane stays open");
}
