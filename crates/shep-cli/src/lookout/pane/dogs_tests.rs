//! The `dogs` tab's rows, probes, anchors and removals.

use serde_json::json;
use shep_core::config::AppConfig;

use super::*;

/// `web`'s tables: `jobs` with a credential in it, and a `legacy` one
/// for a dog that publishes no sheep schema.
fn tables() -> Map<String, Value> {
    json!({
        "jobs": { "concurrency": 2, "token": "sk-live-51Hx9Qa" },
        "legacy": { "url": "https://ops:hunter2@example.test", "retries": 3 },
    })
    .as_object()
    .cloned()
    .expect("an object")
}

fn jobs_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "concurrency": { "type": "integer" },
            "token": { "type": "string", "x-shep-secret": true },
        },
    })
}

fn listed(name: &str, schema: Option<Value>, asked: bool) -> ListedDog {
    ListedDog {
        entry: SheepDogEntry {
            name: name.to_owned(),
            adopted_path: Some(PathBuf::from(format!("/opt/{name}"))),
            schema,
        },
        asked,
    }
}

fn plain_schema() -> Option<Value> {
    Some(json!({ "type": "object", "properties": {} }))
}

/// `web`, on its `dogs` tab, carrying [`tables`].
fn web_on_dogs() -> ConfigPane {
    let mut config = AppConfig {
        name: "web".into(),
        ..AppConfig::default()
    };
    for (name, table) in tables() {
        let table = table.as_object().cloned().expect("a table");
        config.dogs.insert(name, table.into());
    }
    let mut pane = ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()));
    pane.set_group(9);
    pane
}

/// `web_on_dogs`, listing `jobs` (answered, with a schema) and `legacy`.
fn listed_web() -> ConfigPane {
    let mut pane = web_on_dogs();
    pane.list_dogs(vec![
        listed("jobs", Some(jobs_schema()), true),
        listed("legacy", None, true),
    ]);
    pane
}

fn states(pane: &ConfigPane) -> Vec<(&str, DogTableState)> {
    pane.dogs_tab()
        .rows()
        .iter()
        .map(|row| (row.name(), row.state()))
        .collect()
}

#[test]
fn a_sheep_with_no_tables_shows_none() {
    let view = SheepConfigView::new(
        AppConfig {
            name: "web".into(),
            ..AppConfig::default()
        },
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(ConfigPane::sheep(view).value("dogs"), "none");
}

#[test]
fn the_tab_marks_set_unset_and_read_only_and_sorts_by_name() {
    let mut pane = web_on_dogs();
    pane.list_dogs(vec![
        listed("jobs", Some(jobs_schema()), true),
        listed("deploy", plain_schema(), true),
        listed("legacy", None, true),
        listed("metrics", None, true),
    ]);
    assert_eq!(
        states(&pane),
        [
            ("deploy", DogTableState::Unset),
            ("jobs", DogTableState::Set),
            ("legacy", DogTableState::ReadOnly),
        ],
        "a dog with neither a schema nor a table is not listed"
    );
}

#[test]
fn a_table_whose_dog_was_never_listed_is_a_read_only_row() {
    let mut pane = web_on_dogs();
    pane.list_dogs(vec![listed("jobs", Some(jobs_schema()), true)]);
    assert_eq!(
        states(&pane),
        [
            ("jobs", DogTableState::Set),
            ("legacy", DogTableState::ReadOnly)
        ]
    );
    assert_eq!(pane.dogs_tab().rows()[1].keys(), ["retries", "url"]);
}

#[test]
fn an_adopted_dog_is_listed_closed_and_unasked() {
    let mut pane = web_on_dogs();
    pane.list_dogs(vec![listed("deploy", None, false)]);
    let deploy = &pane.dogs_tab().rows()[0];
    assert_eq!(deploy.name(), "deploy");
    assert!(deploy.needs_probe() && !deploy.is_open());
    assert_eq!(deploy.state(), DogTableState::Unset);
}

#[test]
fn a_closed_row_carries_key_names_and_no_value() {
    let pane = listed_web();
    let debug = format!("{:?}", pane.dogs_tab());
    assert!(debug.contains("retries"), "{debug}");
    assert!(!debug.contains("hunter2"), "{debug}");
    assert!(!debug.contains("sk-live-51Hx9Qa"), "{debug}");
}

#[test]
fn an_empty_tab_moves_nowhere_and_arms_nothing() {
    let mut pane = ConfigPane::sheep(SheepConfigView::new(
        AppConfig {
            name: "web".into(),
            ..AppConfig::default()
        },
        Vec::new(),
        Vec::new(),
    ));
    pane.set_group(9);
    pane.list_dogs(Vec::new());
    pane.dogs_tab_mut().disarm();
    pane.move_by(1);
    pane.move_to_last();
    pane.move_by(-1);
    assert_eq!(pane.view().cursor(), 0);
    assert_eq!(pane.cursor(), None);
    assert!(!pane.dogs_tab_mut().arm_removal(0));
}

#[test]
fn arming_a_removal_on_an_unset_row_does_nothing() {
    let mut pane = web_on_dogs();
    pane.list_dogs(vec![
        listed("deploy", plain_schema(), true),
        listed("jobs", Some(jobs_schema()), true),
    ]);
    assert_eq!(pane.dog_anchor().map(|a| a.name).as_deref(), Some("deploy"));
    assert!(!pane.dogs_tab_mut().arm_removal(0));
    assert_eq!(pane.dogs_tab().armed(), None);
    assert!(pane.dogs_tab_mut().arm_removal(2), "a read-only table too");
    assert_eq!(pane.dogs_tab().armed(), Some("legacy"));
    assert!(pane.dogs_tab_mut().disarm());
    assert_eq!(pane.dogs_tab().armed(), None);
}

#[test]
fn every_dog_lists_closed() {
    let pane = listed_web();
    assert!(pane.dogs_tab().rows().iter().all(|row| !row.is_open()));
    assert_eq!(pane.rows(), [PaneRow::Dog(0), PaneRow::Dog(1)]);
}

#[test]
fn opening_an_answered_dog_draws_its_table_under_its_row() {
    let mut pane = listed_web();
    assert_eq!(pane.toggle_dog(0), DogToggle::Opened);
    assert_eq!(
        pane.rows(),
        [
            PaneRow::Dog(0),
            PaneRow::Under { dog: 0, at: 0 },
            PaneRow::Under { dog: 0, at: 1 },
            PaneRow::Dog(1),
        ],
        "jobs's two fields sit between its row and the next dog's"
    );
    assert_eq!(pane.toggle_dog(0), DogToggle::Closed);
    assert_eq!(pane.rows(), [PaneRow::Dog(0), PaneRow::Dog(1)]);
}

#[test]
fn a_closed_table_keeps_the_edit_filed_in_it() {
    let mut pane = listed_web();
    let _ = pane.toggle_dog(0);
    pane.dogs_tab_mut()
        .take_table(0)
        .map(|mut table| {
            table.begin_typing();
            table.type_char('9');
            let _ = table.apply_typing();
            pane.dogs_tab_mut().seat_table(0, table);
        })
        .expect("an open table");
    assert_eq!(pane.dogs_tab().edited(), [0]);
    let _ = pane.toggle_dog(0);
    let _ = pane.toggle_dog(0);
    assert_eq!(pane.dogs_tab().edited(), [0], "collapse is not undo");
}

#[test]
fn an_unasked_dog_asks_once_and_opens_on_its_answer() {
    let mut pane = web_on_dogs();
    pane.list_dogs(vec![listed("deploy", None, false)]);
    let ask = DogToggle::Ask {
        dog: "deploy".into(),
        adopted_path: Some(PathBuf::from("/opt/deploy")),
    };
    assert_eq!(pane.toggle_dog(0), ask);
    assert!(pane.dogs_tab().rows()[0].is_waiting());
    assert_eq!(
        pane.toggle_dog(0),
        DogToggle::Closed,
        "a second Enter closes"
    );
    assert_eq!(pane.toggle_dog(0), DogToggle::Waiting, "and opens again");

    let answered = pane.answer_dog(SheepDogEntry {
        name: "deploy".into(),
        adopted_path: Some(PathBuf::from("/opt/deploy")),
        schema: plain_schema(),
    });
    assert_eq!(answered, Some(DogToggle::Opened));
    assert!(pane.dogs_tab().rows()[0].is_open());
    assert!(pane.dogs_tab().rows()[0].table().is_some());
}

#[test]
fn a_dog_that_answers_with_no_schema_opens_nothing() {
    let mut pane = web_on_dogs();
    pane.list_dogs(vec![listed("deploy", None, false)]);
    let _ = pane.toggle_dog(0);
    let answered = pane.answer_dog(SheepDogEntry {
        name: "deploy".into(),
        adopted_path: Some(PathBuf::from("/opt/deploy")),
        schema: None,
    });
    assert_eq!(answered, Some(DogToggle::NoSchema("deploy".into())));
    let deploy = &pane.dogs_tab().rows()[0];
    assert!(!deploy.is_open());
    assert_eq!(deploy.state(), DogTableState::NoSchema);
}

#[test]
fn an_answer_for_an_unlisted_dog_is_dropped() {
    let mut pane = listed_web();
    let answered = pane.answer_dog(SheepDogEntry {
        name: "ghost".into(),
        adopted_path: None,
        schema: plain_schema(),
    });
    assert_eq!(answered, None);
}

/// The tab's cursor is the one cursor: walking it into a table moves the
/// table's own, and a key moving the table's moves the tab's.
#[test]
fn the_cursor_walks_into_a_table_and_the_table_follows() {
    let mut pane = listed_web();
    let _ = pane.toggle_dog(0);
    pane.move_by(2);
    assert_eq!(pane.cursor(), Some(PaneRow::Under { dog: 0, at: 1 }));
    assert_eq!(pane.focused_dog(), Some(0));
    assert_eq!(pane.focus().view().cursor(), 1);
    assert_eq!(pane.focus().target().name(), "web");

    pane.move_to_first();
    assert_eq!(pane.focused_dog(), None);
    pane.move_to_last();
    assert_eq!(pane.cursor(), Some(PaneRow::Dog(1)));
}

/// A table added above the cursor moves its dog down a row, so a cursor
/// carried by index would land on a neighbour.
#[test]
fn a_refresh_keeps_the_cursor_on_its_dog() {
    let mut pane = listed_web();
    pane.move_by(1);
    let anchor = pane.dog_anchor();
    assert_eq!(anchor.as_ref().map(|a| a.name.as_str()), Some("legacy"));
    let mut more = tables();
    more.insert("alpha".into(), json!({ "on": true }));
    let mut fresh = web_on_dogs();
    fresh.values = [("dogs".to_owned(), Value::Object(more))]
        .into_iter()
        .collect();
    fresh.adopt_dogs_tab(pane.dogs_tab().clone());
    fresh.restore_dog_anchor(&anchor.expect("on a dog"));
    assert_eq!(
        fresh.dog_anchor().map(|a| a.name).as_deref(),
        Some("legacy")
    );
}

/// A schema answer carries a dog's own defaults (IR-41).
#[test]
fn the_debug_of_an_entry_names_no_schema() {
    let entry = SheepDogEntry {
        name: "jobs".into(),
        adopted_path: None,
        schema: Some(json!({ "default": "sk-live-51Hx9Qa" })),
    };
    assert_eq!(
        format!("{entry:?}"),
        r#"SheepDogEntry { name: "jobs", adopted_path: None, schema: Some(..) }"#
    );
}
