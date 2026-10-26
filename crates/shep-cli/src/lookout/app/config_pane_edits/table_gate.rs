//! What the write gate does not do: refuse a value the close changed nothing
//! of.
//!
//! Every edit in a pane was already held to the schema on the keystroke that
//! filed it. The gate [`crate::dog_settings::touched_refusal`] adds is the
//! check that the write goes through the same door the keystroke did, asked
//! about the keys the operator chose. A value the operator wrote into
//! `dogs.toml` or the Flockfile by hand, and which the dog's own schema turns
//! down, is the shepherd's to take or refuse: the pane never read it, so it
//! goes back out as it came. These two tests are that promise, one per table
//! pane.

use serde_json::{Value, json};
use shep_core::protocol::Response;

use super::super::*;
use crate::lookout::app::testing::*;
use crate::lookout::pane::SheepDogEntry;
use crate::lookout::view::fixtures;

/// A dog whose `keep` publishes a floor, the way `shep-log-rotate` publishes
/// one to say `keep = 0` deletes every rotation.
fn gate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "keep": { "type": "integer", "minimum": 1 },
        },
    })
}

/// The `[jobs]` section as an operator who knows what they mean writes it,
/// with the one value the dog's own schema refuses.
fn section_with_a_refused_keep() -> String {
    "# how many to keep\nkeep = 0\nname = \"small\"\n".to_string()
}

/// A dashboard with a dog pane open over `schema` and `section`, opened the
/// way the event loop opens one: the schema probe, then the shepherd's
/// section. The control gate is open, so the pane can write.
fn app_in_dog_pane_over(schema: Value, section: &str) -> App {
    let mut app = fixtures::app_with(fixtures::flock_of(3, 0), fixtures::plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::DogPane {
        name: "jobs".to_string(),
        adopted_path: None,
        result: Ok(schema),
    });
    app.update(Msg::Replied {
        sent: Sent::DogSection {
            name: "jobs".to_string(),
        },
        result: Ok(Response::DogSection {
            toml: section.to_owned().into(),
        }),
    });
    app
}

/// Types `text` into `key` and files it.
fn type_field(app: &mut App, key: &str, text: &str) {
    pane_to(app, key);
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    for _ in 0..app
        .config_pane()
        .expect("a pane")
        .value(key)
        .chars()
        .count()
    {
        let _ = app.update(Msg::Key(KeyPress::TextBackspace));
    }
    for c in text.chars() {
        let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
    }
    let _ = app.update(Msg::Key(KeyPress::TextApply));
}

/// `esc` on the `jobs` pane above, with the re-read it asks for answered by
/// the same section: the effect carrying the write.
fn close_over(app: &mut App, section: &str) -> Effect {
    assert_eq!(
        app.update(Msg::Key(KeyPress::Escape)),
        Effect::Send(Sent::DogSection {
            name: "jobs".into()
        }),
        "esc reads the section again before writing"
    );
    app.update(Msg::Replied {
        sent: Sent::DogSection {
            name: "jobs".into(),
        },
        result: Ok(Response::DogSection {
            toml: section.to_owned().into(),
        }),
    })
}

/// The edit is one the schema takes. The `keep = 0` beside it is not, and no
/// edit touched it, so the section goes out with both, exactly as it did
/// before the write went through the shared validator.
#[test]
fn a_dog_section_holds_a_value_the_schema_refuses_and_still_writes_it() {
    let section = section_with_a_refused_keep();
    let mut app = app_in_dog_pane_over(gate_schema(), &section);
    type_field(&mut app, "name", "large");
    assert_eq!(
        app.config_pane().expect("the pane is open").edits().len(),
        1
    );
    let written = section.replace("name = \"small\"", "name = \"large\"");
    let effect = close_over(&mut app, &section);
    let Effect::SendAll(sent) = effect else {
        panic!("the close writes: {effect:?}");
    };
    assert_eq!(sent.len(), 1);
    let Sent::SetDogSection { name, toml, .. } = &sent[0] else {
        panic!("a dog section write: {sent:?}");
    };
    assert_eq!(name, "jobs");
    assert_eq!(toml.as_str(), written);
    assert!(toml.as_str().contains("keep = 0"), "{}", toml.as_str());
    assert!(app.notice().is_none(), "{:?}", app.notice());
}

/// `web`'s `jobs` table carries `concurrency = 2`, and this schema asks for a
/// floor of 10. The value came from the Flockfile, and the edit is to a key
/// of its own, so the table goes back out whole.
#[test]
fn a_sheep_dog_table_holds_a_value_the_schema_refuses_and_still_writes_it() {
    let mut app = app_in_web(Control::Allowed);
    let ask = ask_dogs(&mut app);
    answer(
        &mut app,
        "web",
        ask,
        vec![ListedDog {
            asked: true,
            entry: SheepDogEntry {
                name: "jobs".into(),
                adopted_path: Some("/opt/jobs".into()),
                schema: Some(json!({
                    "type": "object",
                    "properties": {
                        "concurrency": { "type": "integer", "minimum": 10 },
                        "region": { "type": "string" },
                        "token": { "type": "string", "x-shep-secret": true },
                    },
                })),
            },
        }],
    );
    cursor_on(&mut app, "jobs");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_field(&mut app, "region", "eu-west-1");
    let effect = close_jobs_table(&mut app);
    let Effect::SendAll(sent) = effect else {
        panic!("the close writes: {effect:?}");
    };
    let [
        Sent::SetSheepDogTable {
            table: Some(table), ..
        },
    ] = sent.as_slice()
    else {
        panic!("one table write: {sent:?}");
    };
    let table = table.as_map();
    assert_eq!(
        table.get("concurrency"),
        Some(&json!(2)),
        "the Flockfile's own value rides out"
    );
    assert_eq!(table.get("region"), Some(&json!("eu-west-1")));
    assert!(app.notice().is_none(), "{:?}", app.notice());
}

/// The gate is on the path even when it has nothing to say: a pane with
/// nothing filed writes nothing, and a pane with an edit the schema takes
/// writes one request.
#[test]
fn a_dog_section_the_schema_takes_writes_in_one_request() {
    let section = "# how many to keep\nkeep = 8\nname = \"small\"\n".to_string();
    let mut app = app_in_dog_pane_over(gate_schema(), &section);
    type_field(&mut app, "name", "large");
    let written = section.replace("name = \"small\"", "name = \"large\"");
    let effect = close_over(&mut app, &section);
    let Effect::SendAll(sent) = effect else {
        panic!("the close writes: {effect:?}");
    };
    assert_eq!(sent.len(), 1);
    let Sent::SetDogSection { name, toml, .. } = &sent[0] else {
        panic!("a dog section write: {sent:?}");
    };
    assert_eq!(name, "jobs");
    assert_eq!(toml.as_str(), written);
}
