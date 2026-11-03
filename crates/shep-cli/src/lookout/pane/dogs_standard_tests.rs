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

/// A row's key and value alone: the mark and gutter columns dropped, the
/// spacing squeezed and the explanation panel beside it cut off, so a row
/// inside the tab and the same row in a pane of its own compare.
fn squeezed(text: &str, key: &str) -> String {
    row(text, key)
        .trim_start_matches(['>', ' ', '^'])
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The dog is probed from a manifest with no `serves` field and nothing
/// marked dog-wide, through the same probe the lookout runs, and its table
/// is then compared with a pane built the way it was before the standard.
#[cfg(unix)]
#[test]
fn a_dog_whose_manifest_lacks_the_field_draws_exactly_as_before() {
    let dir = tempfile::tempdir().unwrap();
    crate::commands::standard_dog_testing::adopt_jobs_without_the_standard(dir.path(), true);
    let paths = shep_core::paths::ShepPaths::at(dir.path().to_path_buf());
    let probed = crate::lookout::probe_sheep_dogs::probe_sheep_dog(
        &paths,
        "jobs",
        Some(dir.path().join("shep-jobs")),
        std::time::Duration::from_secs(30),
    );
    assert_eq!(probed.standard, None);
    let schema = probed.schema.clone().expect("the x-shep-sheep schema");
    let table = json!({ "limit": 9 });

    let mut tab = web(Some(table.clone()));
    tab.list_dogs(vec![ListedDog {
        entry: probed,
        asked: true,
    }]);
    let _ = tab.toggle_dog(0);
    let baseline = ConfigPane::sheep_dog(
        "web".into(),
        "jobs".into(),
        &schema,
        table.as_object().cloned().expect("a table"),
    );

    let (text, before) = (drawn(&tab), drawn(&baseline));
    assert!(!text.contains('^'), "{text}");
    assert!(!text.contains("comes from"), "{text}");
    for key in ["model", "limit"] {
        assert_eq!(
            squeezed(&text, key),
            squeezed(&before, key),
            "{text}\n{before}"
        );
    }
    assert!(row(&text, "model").contains("(unset)"), "{text}");
}

#[test]
fn an_opted_in_dog_shows_the_section_on_a_sheep_with_an_empty_table() {
    let mut pane = web(Some(json!({})));
    pane.list_dogs(vec![standard_jobs(Serves::OptedIn)]);
    let _ = pane.toggle_dog(0);

    let text = drawn(&pane);
    assert!(row(&text, "model").contains("^model"), "{text}");
    assert!(row(&text, "model").contains("sonnet"), "{text}");
}

#[test]
fn a_re_read_of_the_sections_reaches_an_open_table() {
    let mut pane = web(Some(json!({ "limit": 9 })));
    pane.list_dogs(vec![standard_jobs(Serves::EverySheep)]);
    let _ = pane.toggle_dog(0);
    assert!(drawn(&pane).contains("sonnet"));

    let mut fresh = standard_jobs(Serves::EverySheep).entry;
    fresh.standard = Some(StandardSection {
        serves: Serves::EverySheep,
        shared: json!({ "model": "haiku" })
            .as_object()
            .cloned()
            .expect("a table"),
    });
    pane.refresh_sections(vec![fresh]);

    let text = drawn(&pane);
    assert!(row(&text, "model").contains("haiku"), "{text}");
    assert!(row(&text, "model").contains("^model"), "{text}");
    assert!(!text.contains("sonnet"), "{text}");
}

#[test]
fn the_tab_names_the_standard_dogs_to_ask_again() {
    let mut pane = web(None);
    pane.list_dogs(vec![standard_jobs(Serves::EverySheep)]);
    assert_eq!(
        pane.dogs_tab().standard_dogs(),
        [("jobs".to_owned(), Some(PathBuf::from("/opt/jobs")))]
    );
}

#[test]
fn a_standard_section_debug_names_no_value() {
    let section = StandardSection {
        serves: Serves::EverySheep,
        shared: json!({ "token": "hunter2", "model": "sonnet" })
            .as_object()
            .cloned()
            .expect("a table"),
    };
    assert_eq!(
        format!("{section:?}"),
        "StandardSection { serves: EverySheep, keys: 2 }"
    );
}

#[test]
fn the_tab_says_where_a_caret_comes_from_while_the_cursor_is_on_the_dog() {
    let mut pane = web(Some(json!({ "limit": 9 })));
    pane.list_dogs(vec![standard_jobs(Serves::OptedIn)]);
    let _ = pane.toggle_dog(0);

    let text = drawn(&pane);
    assert!(
        text.contains("^ comes from [jobs]; d on a value you set returns it to [jobs]"),
        "{text}"
    );
}

#[test]
fn a_dog_that_is_not_the_standard_draws_no_such_note() {
    let mut entry = standard_jobs(Serves::OptedIn);
    entry.entry.standard = None;
    let mut pane = web(Some(json!({ "limit": 9 })));
    pane.list_dogs(vec![entry]);
    let _ = pane.toggle_dog(0);

    assert!(!drawn(&pane).contains("comes from"));
}
