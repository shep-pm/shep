//! The `dogs` tab for a dog that follows the settings standard.

use serde_json::json;
use shep_core::config::AppConfig;
use shep_core::dogs::manifest::Serves;

use super::super::super::view::fixtures::{plain, render_all};
use super::super::super::view::pane::pane_lines;
use super::*;

/// `jobs`' one schema as a sheep sees it, `poll` already left out.
fn sheep_form() -> Value {
    json!({
        "type": "object",
        "properties": { "model": { "type": "string" }, "limit": { "type": "integer" } },
    })
}

fn standard_jobs(serves: Serves) -> ListedDog {
    let shared = json!({ "model": "sonnet", "limit": 3 });
    ListedDog {
        entry: SheepDogEntry {
            name: "jobs".into(),
            adopted_path: Some(PathBuf::from("/opt/jobs")),
            schema: Some(sheep_form()),
            standard: Some(StandardSection {
                serves,
                shared: shared.as_object().cloned().expect("a table"),
            }),
        },
        asked: true,
    }
}

fn web(own: Option<Value>) -> ConfigPane {
    let mut config = AppConfig {
        name: "web".into(),
        ..AppConfig::default()
    };
    if let Some(own) = own {
        config.dogs.insert(
            "jobs".into(),
            own.as_object().cloned().expect("a table").into(),
        );
    }
    let mut pane = ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()));
    pane.set_group(9);
    pane
}

fn drawn(pane: &ConfigPane) -> String {
    render_all(&pane_lines(pane, plain(), 120, 0))
}

fn row<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find(|line| line.trim_start_matches(['>', ' ', '^']).starts_with(key))
        .unwrap_or_else(|| panic!("no {key} row in {text}"))
}

#[test]
fn a_dog_serving_every_sheep_opens_its_form_on_a_sheep_with_no_table() {
    let mut pane = web(None);
    pane.list_dogs(vec![standard_jobs(Serves::EverySheep)]);
    assert_eq!(pane.toggle_dog(0), DogToggle::Opened);

    let text = drawn(&pane);
    assert!(row(&text, "model").contains("^model"), "{text}");
    assert!(row(&text, "model").contains("sonnet"), "{text}");
    assert!(!text.contains("poll"), "{text}");
}

#[test]
fn the_form_marks_an_inherited_value_and_leaves_an_own_value_plain() {
    let mut pane = web(Some(json!({ "limit": 9 })));
    pane.list_dogs(vec![standard_jobs(Serves::EverySheep)]);
    let _ = pane.toggle_dog(0);

    let text = drawn(&pane);
    assert!(row(&text, "model").contains("^model"), "{text}");
    assert!(!row(&text, "limit").contains('^'), "{text}");
    assert!(row(&text, "limit").contains('9'), "{text}");
}

#[test]
fn an_opted_in_dog_shows_nothing_inherited_on_a_sheep_with_no_table() {
    let mut pane = web(None);
    pane.list_dogs(vec![standard_jobs(Serves::OptedIn)]);
    assert_eq!(
        pane.dogs_tab().rows()[0].state(),
        DogTableState::Unset,
        "the row is how a sheep takes a table"
    );
    let _ = pane.toggle_dog(0);

    let text = drawn(&pane);
    assert!(!text.contains('^'), "{text}");
    assert!(row(&text, "model").contains("(unset)"), "{text}");
}

#[test]
fn a_dog_that_does_not_follow_the_standard_draws_exactly_as_before() {
    let table = json!({ "limit": 9 });
    let mut entry = standard_jobs(Serves::EverySheep);
    entry.entry.standard = None;

    let mut tab = web(Some(table.clone()));
    tab.list_dogs(vec![entry]);
    let _ = tab.toggle_dog(0);
    let mut plain_tab = web(Some(table));
    plain_tab.list_dogs(vec![ListedDog {
        entry: SheepDogEntry {
            standard: None,
            ..standard_jobs(Serves::OptedIn).entry
        },
        asked: true,
    }]);
    let _ = plain_tab.toggle_dog(0);

    let text = drawn(&tab);
    assert!(!text.contains('^'), "{text}");
    assert_eq!(text, drawn(&plain_tab));
    assert!(row(&text, "model").contains("(unset)"), "{text}");
}
