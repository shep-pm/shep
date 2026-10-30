//! The plan half of `shep set`, with no daemon in front of it: what one value
//! parses as, what one write changes, what a section looks like after the edit,
//! and what the flock's schema turns down.
//!
//! The live cases are in `tests/cli_e2e/settings_set.rs`. Everything here is
//! the part that decides, because that is the part that can write something an
//! operator did not ask for.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, json};
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::protocol::{Request, Response};

use super::flow::{ask, holder_moved, report_for, run, section_moved, table_moved};
use super::plan::schema_refusal;
use super::plan::{name_the_path, plan_dog_defaults, touched_paths};
use super::*;
use crate::cli::Format;
use crate::commands::copy::confirm::Answer;
use crate::commands::copy::plan::ChangeKind;
use crate::commands::settings_read::Holder;
use crate::output::Streams;

/// A dog's own section schema, spelled the way a dog's `--schema` answer is
/// spelled, so a refusal test proves the gate reads a schema and not a fixture
/// shaped like one.
fn schema() -> Json {
    json!({
        "$ref": "#/$defs/Settings",
        "$defs": {
            "Settings": {
                "type": "object",
                "properties": {
                    "keep": { "type": "integer", "minimum": 1, "maximum": 4 },
                    "merge": { "enum": ["ask", "auto"] },
                    "hours": {
                        "type": "object",
                        "properties": {
                            "start": { "type": "string", "pattern": "^\\d{2}:\\d{2}$" },
                        },
                    },
                },
            }
        }
    })
}

/// A sheep's config with one dog table, spelled as a Flockfile spells it.
fn config() -> Json {
    json!({
        "name": "api",
        "max_memory": "1G",
        "dogs": {
            "kelpie": {
                "git": { "checkout": "/srv/api", "merging": "ff" },
                "options": {}
            }
        }
    })
}

/// A `dogs.toml` section with a comment above a header, which is the text a
/// write has to survive.
const SECTION: &str = "\
# kelpie reads the repo it was adopted into
repo = \"/srv/project\"

# what it may fetch
[worker]
allowed_domains = [\"github.com\"]
timeout = 30
";

/// The plan for one key inside [`SECTION`], aimed at the dog `kelpie`.
fn section_plan(keys: &[&str], value: Json) -> Result<Plan, WriteRefusal> {
    let doc = SECTION.parse::<toml_edit::DocumentMut>().unwrap();
    plan_section(
        doc,
        "kelpie",
        &keys.iter().map(|key| key.to_string()).collect::<Vec<_>>(),
        &value,
    )
}

/// A sheep's own field is one change, and the same value twice is none.
#[test]
fn a_field_write_is_one_change_or_none() {
    let changes = field_changes("max_memory", &config(), &json!("512M"));
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, "max_memory");
    assert_eq!(changes[0].kind, ChangeKind::Changed);
    assert_eq!(changes[0].old, Some(json!("1G")));
    assert_eq!(changes[0].new, Some(json!("512M")));

    assert!(field_changes("max_memory", &config(), &json!("1G")).is_empty());

    // A field the sheep holds nothing for is an added key, with no old value
    // to print beside the new one.
    let added = field_changes("restart_delay", &config(), &json!(5));
    assert_eq!(added[0].kind, ChangeKind::Added);
    assert_eq!(added[0].old, None);
}

/// A key inside a sheep's dog table is written through the whole table the
/// request carries, and the change list names the one key.
#[test]
fn a_dog_table_key_changes_one_row_of_the_old_and_new_list() {
    let plan = plan_dog_table(
        "api",
        "kelpie",
        &["git".to_string(), "merging".to_string()],
        &config(),
        &json!("rebase"),
    )
    .unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].key, "merging");
    assert_eq!(plan.changes[0].old, Some(json!("ff")));
    assert_eq!(plan.changes[0].new, Some(json!("rebase")));
    // The write is the merged table: the key the operator named, and every key
    // the write left alone.
    let Write::SheepDogTable { dog, table, .. } = &plan.write else {
        panic!("a dog table key writes a dog table");
    };
    assert_eq!(dog, "kelpie");
    assert_eq!(
        table.as_map().get("git").unwrap()["merging"],
        json!("rebase")
    );
    assert_eq!(
        table.as_map().get("git").unwrap()["checkout"],
        json!("/srv/api")
    );
    assert_eq!(
        plan.touched,
        BTreeSet::from([vec!["git".to_string(), "merging".to_string()]]),
        "the schema is asked about the key this run chose, and nothing else"
    );
}

/// A key the sheep's dog table does not hold yet is an added row, named under
/// the table it lands in.
#[test]
fn a_new_key_under_a_dog_table_is_an_added_row() {
    let plan = plan_dog_table(
        "api",
        "kelpie",
        &["git".to_string(), "remote".to_string()],
        &config(),
        &json!("upstream"),
    )
    .unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].key, "remote");
    assert_eq!(plan.changes[0].kind, ChangeKind::Added);
    assert_eq!(plan.changes[0].old, None);
    assert_eq!(plan.changes[0].new, Some(json!("upstream")));
}

/// Writing a whole table whole is the one write that removes keys, and the
/// change list says which.
#[test]
fn a_whole_table_write_reports_the_keys_it_removes() {
    let plan = plan_dog_table(
        "api",
        "kelpie",
        &["git".to_string()],
        &config(),
        &json!({ "merging": "ff", "remote": "upstream" }),
    )
    .unwrap();
    let keys: Vec<(&str, &str)> = plan
        .changes
        .iter()
        .map(|change| (change.key.as_str(), change.kind.as_str()))
        .collect();
    assert_eq!(keys, [("checkout", "removed"), ("remote", "added")]);
    // Every key the replacement put down is in play, and the schema is asked
    // about each of them.
    assert_eq!(
        plan.touched,
        BTreeSet::from([
            vec!["git".to_string(), "merging".to_string()],
            vec!["git".to_string(), "remote".to_string()],
        ])
    );
}

/// A write that would have to go through a value that is not a table is
/// refused, in `copy`'s reason.
#[test]
fn a_write_through_a_value_that_is_not_a_table_is_refused() {
    let mut held = config();
    held["dogs"]["kelpie"]["git"] = json!("not a table");
    let error = plan_dog_table(
        "api",
        "kelpie",
        &["git".to_string(), "merging".to_string()],
        &held,
        &json!("ff"),
    )
    .expect_err("a scalar holds no key to write");
    assert_eq!(error.code(), ExitCode::InvalidConfig);
    assert!(
        error
            .to_string()
            .contains("`api.dogs.kelpie.git` holds a value that is not a table"),
        "{error}"
    );

    // The whole table named, and a value that is not one.
    let error = plan_dog_table(
        "api",
        "kelpie",
        &["git".to_string()],
        &config(),
        &json!("ff"),
    )
    .expect_err("a whole table takes a whole table");
    assert_eq!(error.code(), ExitCode::InvalidConfig);
    assert!(
        error
            .to_string()
            .contains("`api.dogs.kelpie.git` holds a table"),
        "{error}"
    );

    // The whole `dogs.kelpie` map named, and a value that is not one. The
    // sheep holding no table for that dog at all is the same refusal: a whole
    // dog table takes a value that is one, and nothing here says a table that
    // is not there holds one.
    let error = plan_dog_table("api", "kelpie", &[], &config(), &json!("ff"))
        .expect_err("a whole table takes a whole table");
    assert_eq!(error.code(), ExitCode::InvalidConfig);
    assert!(
        error
            .to_string()
            .contains("`api.dogs.kelpie` names a whole dog table"),
        "{error}"
    );
    assert!(
        !error.to_string().contains("holds a table"),
        "{error}: a table the sheep does not hold is not a table it holds"
    );

    let missing = json!({"dogs": {}});
    let error = plan_dog_table("api", "kelpie", &[], &missing, &json!("ff"))
        .expect_err("no table for the dog is still a whole table");
    assert!(
        error
            .to_string()
            .contains("`api.dogs.kelpie` names a whole dog table"),
        "{error}"
    );
}

/// A section is edited in place: the comment above the header survives, the
/// header stays a header, and the key the operator named is the one that moved.
#[test]
fn a_section_write_keeps_the_text_the_operator_wrote() {
    let plan = section_plan(&["worker", "timeout"], json!(60)).unwrap();
    let Write::DogSection { toml, .. } = &plan.write else {
        panic!("a dog's own key writes its section");
    };
    assert!(
        toml.contains("# kelpie reads the repo it was adopted into"),
        "{toml}"
    );
    assert!(toml.contains("# what it may fetch"), "{toml}");
    assert!(toml.contains("[worker]\n"), "{toml}");
    assert!(toml.contains("timeout = 60"), "{toml}");
    assert!(
        toml.contains("allowed_domains = [\"github.com\"]"),
        "{toml}"
    );
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].key, "timeout");
    assert_eq!(plan.changes[0].old, Some(json!(30)));
    assert_eq!(plan.changes[0].new, Some(json!(60)));
}

/// A key the section does not hold yet lands inside the table the address
/// names, without an empty `[worker]` header written above it.
#[test]
fn a_new_key_in_a_section_lands_where_the_address_says() {
    let plan = section_plan(&["worker", "retries"], json!(3)).unwrap();
    let Write::DogSection { toml, .. } = &plan.write else {
        panic!("a dog's own key writes its section");
    };
    assert!(toml.contains("retries = 3"), "{toml}");
    assert!(
        !toml.contains("[worker]\n[worker"),
        "an empty header the operator never wrote: {toml}"
    );
    assert_eq!(plan.changes[0].key, "retries");
    assert_eq!(plan.changes[0].kind, ChangeKind::Added);

    // A table written whole into a section that has none for it is a header,
    // not an inline table, because that is the form a section spells tables in.
    let plan = section_plan(&["limits"], json!({ "keep": 2 })).unwrap();
    let Write::DogSection { toml, .. } = &plan.write else {
        unreachable!()
    };
    assert!(toml.contains("[limits]\nkeep = 2"), "{toml}");
}

/// A section written whole is the one write that replaces a document, and the
/// only thing that loses the comments in it.
#[test]
fn a_whole_section_takes_a_whole_table() {
    let plan = section_plan(&[], json!({ "repo": "/srv/other" })).unwrap();
    let Write::DogSection { toml, .. } = &plan.write else {
        unreachable!()
    };
    assert_eq!(toml, "repo = \"/srv/other\"\n");
    let kinds: Vec<(&str, &str)> = plan
        .changes
        .iter()
        .map(|change| (change.key.as_str(), change.kind.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("repo", "changed"),
            ("worker.allowed_domains", "removed"),
            ("worker.timeout", "removed"),
        ]
    );
    assert!(plan.touched.is_empty(), "a whole section is asked whole");

    let error = section_plan(&[], json!(30)).expect_err("a section is a table");
    assert_eq!(error.code(), ExitCode::Usage);
    assert!(
        error
            .to_string()
            .contains("a whole section takes a whole table"),
        "{error}"
    );
}

/// A write that would have to turn a value the operator wrote into a table is
/// refused rather than made on the way to the key it was asked to write.
#[test]
fn a_write_through_a_sections_scalar_is_refused() {
    let error = section_plan(&["repo", "path"], json!("/srv")).expect_err("a string holds no key");
    assert_eq!(error.code(), ExitCode::InvalidConfig);
    assert!(
        error
            .to_string()
            .contains("`kelpie.repo` holds a value that is not a table"),
        "{error}"
    );
}

/// Every leaf a value puts down is a key the schema gets asked about, and a
/// whole-table write is asked whole instead.
#[test]
fn the_touched_paths_are_the_leaves_the_value_puts_down() {
    let one = touched_paths(&["git".to_string(), "merging".to_string()], &json!("ff"));
    assert_eq!(
        one,
        BTreeSet::from([vec!["git".to_string(), "merging".to_string()]])
    );

    let nested = touched_paths(
        &["git".to_string()],
        &json!({ "merging": "ff", "remote": { "name": "up" } }),
    );
    assert_eq!(
        nested,
        BTreeSet::from([
            vec!["git".to_string(), "merging".to_string()],
            vec!["git".to_string(), "remote".to_string(), "name".to_string()],
        ])
    );

    assert!(touched_paths(&[], &json!({ "keep": 1 })).is_empty());
}

/// A schema refusal names the key path it sits at, and writes nothing.
#[test]
fn a_schema_refusal_names_the_key_path() {
    let mut plan = plan_dog_table(
        "api",
        "kelpie",
        &["keep".to_string()],
        &Json::Object(Map::new()),
        &json!(99),
    )
    .unwrap();
    plan.schema = Some(schema());
    let refusal = schema_refusal(&plan, "api", "dogs.kelpie.keep").expect("99 is over the maximum");
    assert_eq!(refusal.code(), ExitCode::InvalidConfig);
    let text = refusal.to_string();
    assert!(text.starts_with("nothing was written:"), "{text}");
    assert!(text.contains("`api.dogs.kelpie.keep`"), "{text}");

    // The same table with the value the schema takes is no refusal.
    let mut plan = plan_dog_table(
        "api",
        "kelpie",
        &["keep".to_string()],
        &Json::Object(Map::new()),
        &json!(3),
    )
    .unwrap();
    plan.schema = Some(schema());
    assert!(schema_refusal(&plan, "api", "dogs.kelpie.keep").is_none());

    // A nested leaf is named by the whole path to it.
    let mut plan = plan_dog_table(
        "api",
        "kelpie",
        &["hours".to_string(), "start".to_string()],
        &Json::Object(Map::new()),
        &json!("noon"),
    )
    .unwrap();
    plan.schema = Some(schema());
    let refusal =
        schema_refusal(&plan, "api", "dogs.kelpie.hours.start").expect("noon is not HH:MM");
    assert!(refusal.to_string().contains("hours.start"), "{refusal}");
}

/// A key the operator wrote into the file by hand rides along untouched, and
/// the schema is not asked about it, exactly as it is not through a lookout
/// pane close.
#[test]
fn a_schema_check_asks_only_about_the_keys_this_run_chose() {
    let table = json!({ "keep": 99, "merge": "ask" })
        .as_object()
        .unwrap()
        .clone();
    let plan = Plan {
        changes: Vec::new(),
        write: Write::DogSection {
            toml: "merge = \"ask\"\n".to_string(),
            before: "merge = \"ask\"\n".to_string(),
        },
        table,
        schema: Some(schema()),
        touched: BTreeSet::from([vec!["merge".to_string()]]),
    };
    assert!(
        schema_refusal(&plan, "kelpie", "merge").is_none(),
        "`keep = 99` is not this run's key"
    );

    // The same plan asked whole is the refusal it would be through a
    // whole-table write.
    let whole = Plan {
        touched: BTreeSet::new(),
        ..plan
    };
    let refusal = schema_refusal(&whole, "kelpie", "").expect("ask is not a value keep breaks");
    assert!(refusal.to_string().contains("keep"), "{refusal}");
}

/// A dog that publishes nothing is not refused anything: shep is no authority
/// on a config its author wrote elsewhere.
#[test]
fn a_dog_with_no_schema_is_asked_nothing() {
    let plan = plan_dog_table(
        "api",
        "kelpie",
        &["keep".to_string()],
        &Json::Object(Map::new()),
        &json!(99),
    )
    .unwrap();
    assert!(plan.schema.is_none());
    assert!(schema_refusal(&plan, "api", "dogs.kelpie.keep").is_none());
}

/// The answer to a name nothing in the flock is names where the store went,
/// which is the acceptance criterion for the hidden alias this verb replaces.
#[test]
fn an_unknown_name_names_the_key_value_store_it_used_to_write() {
    let text = no_such_name(&["bark".to_string(), "cooldown".to_string()]);
    assert!(
        text.contains("no sheep or dog named `bark.cooldown`"),
        "{text}"
    );
    assert!(text.contains("shep kv set"), "{text}");
    assert!(text.contains("shep flock"), "{text}");
    assert!(text.contains("shep dogs"), "{text}");
}

/// A question read is a yes, anything else is a no, and no question at all is
/// a no rather than a failure.
#[test]
fn the_question_is_answered_the_way_copy_answers_it() {
    use std::io::Cursor;

    let ask_once = |name: &str, path: &str, changes: usize, text: &str| {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let answer = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let mut reader = Cursor::new(text.as_bytes());
            ask(&mut streams, Verb::Set, name, path, changes, &mut reader)
        };
        (answer, String::from_utf8_lossy(&err).to_string())
    };

    let (answer, question) = ask_once("api", "max_memory", 1, "y\n");
    assert!(matches!(answer, Answer::Yes));
    assert!(
        question.contains("set `api.max_memory`? [y/N]"),
        "{question}"
    );
    assert!(matches!(
        ask_once("api", "max_memory", 1, "YES\n").0,
        Answer::Yes
    ));
    assert!(matches!(
        ask_once("api", "max_memory", 1, "n\n").0,
        Answer::No
    ));
    // No answer at all is a no: an operator who never typed is not one who
    // agreed.
    assert!(matches!(ask_once("api", "max_memory", 1, "").0, Answer::No));

    // More than one key is asked about as a count, not as one key.
    let question = ask_once("api", "dogs.kelpie.git", 3, "n\n").1;
    assert!(
        question.contains("set 3 keys under `api.dogs.kelpie.git`? [y/N]"),
        "{question}"
    );
}

/// The report carries the holder the address named, the path it named, and one
/// row per key the write touches.
#[test]
fn a_report_names_the_holder_the_path_and_every_key() {
    let plan = plan_dog_table(
        "api",
        "kelpie",
        &["git".to_string(), "merging".to_string()],
        &config(),
        &json!("rebase"),
    )
    .unwrap();
    let report = report_for(Holder::Sheep, "api", "dogs.kelpie.git", &plan, true, false);
    assert_eq!(report.kind, "sheep");
    assert_eq!(report.name, "api");
    assert_eq!(report.path, "dogs.kelpie.git");
    assert!(report.written);
    assert!(!report.dry_run);
    assert!(report.pending.is_empty());
    assert_eq!(report.changes.len(), 1);
    assert_eq!(report.changes[0].change, "changed");

    let plan = section_plan(&["worker", "timeout"], json!(60)).unwrap();
    let report = report_for(Holder::Dog, "kelpie", "worker.timeout", &plan, false, true);
    assert_eq!(report.kind, "dog");
    assert_eq!(report.name, "kelpie");
    assert_eq!(report.path, "worker.timeout");
    assert!(!report.written);
    assert!(report.dry_run);
}

/// A change list whose only row is a blank names nothing the operator can act
/// on, which is why a write to one key names that key.
#[test]
fn a_change_under_one_key_takes_that_keys_name() {
    let mut changes = vec![
        Change {
            key: String::new(),
            kind: ChangeKind::Added,
            old: None,
            new: Some(json!(1)),
        },
        Change {
            key: "nested.deep".to_string(),
            kind: ChangeKind::Added,
            old: None,
            new: Some(json!(2)),
        },
    ];
    name_the_path(&mut changes, &["git".to_string()]);
    assert_eq!(changes[0].key, "git");
    assert_eq!(
        changes[1].key, "nested.deep",
        "only the bare row is renamed"
    );

    // A whole-table write has no last key to name, and its rows are already
    // named by the keys themselves.
    let mut whole = vec![Change {
        key: String::new(),
        kind: ChangeKind::Changed,
        old: Some(json!(1)),
        new: Some(json!(2)),
    }];
    name_the_path(&mut whole, &[]);
    assert_eq!(whole[0].key, "");
}

/// A section the shepherd answered with text that is not TOML is refused as
/// the file it is, not as a settings problem.
#[test]
fn a_dog_section_that_does_not_parse_is_named_as_the_file() {
    let text = unparsable("kelpie");
    assert!(
        text.starts_with("`kelpie`'s section of `dogs.toml`"),
        "{text}"
    );
    assert!(text.contains("nothing was written"), "{text}");
    assert!(text.contains("shep get kelpie"), "{text}");
}

/// A leaf that holds a table is refused as a table, which is the sentence the
/// operator can act on, and not as a value that is not one.
#[test]
fn a_leaf_table_is_refused_as_the_table_it_is() {
    let error = section_plan(&["worker"], json!(30)).expect_err("a table holds a table");
    assert_eq!(error.code(), ExitCode::InvalidConfig);
    let text = error.to_string();
    assert!(
        text.contains("`kelpie.worker` holds a table"),
        "{text}: the refusal names the address, not a fragment of the section"
    );
    assert!(
        text.contains("does not replace a table with a value that is not one"),
        "{text}"
    );
    assert!(
        !text.contains("holds a value that is not a table"),
        "{text}: `[worker]` is a table"
    );
}

/// An array of tables is its own shape, and `shep set` writes one table.
#[test]
fn an_array_of_tables_is_not_replaced_by_one_table() {
    let section = "[[sinks]]\nurl = \"https://a\"\n[[sinks]]\nurl = \"https://b\"\n";
    let error = plan_section(
        section.parse::<toml_edit::DocumentMut>().unwrap(),
        "kelpie",
        &["sinks".to_string()],
        &json!({ "url": "https://c" }),
    )
    .expect_err("an array of tables is not one table");
    assert_eq!(error.code(), ExitCode::InvalidConfig);
    assert!(
        error
            .to_string()
            .contains("`kelpie.sinks` is an array of tables"),
        "{error}"
    );
}

/// TOML has no null, so a null in a value is refused rather than dropped,
/// which would be a smaller write than the one the operator asked for.
#[test]
fn a_null_is_refused_rather_than_dropped() {
    let error = section_plan(&["limits", "keep"], json!(null)).expect_err("TOML has no null");
    assert_eq!(error.code(), ExitCode::InvalidConfig);
    assert!(
        error
            .to_string()
            .contains("`kelpie.limits.keep` has no TOML to write"),
        "{error}"
    );

    let error = section_plan(&[], json!({ "repo": null })).expect_err("nor in a whole section");
    assert!(
        error
            .to_string()
            .contains("`kelpie.repo` has no TOML to write"),
        "{error}"
    );

    // A null inside a table value is named from the address, not from the
    // value: `{ keep = null }` written at `worker` sits at `worker.keep`.
    let error = section_plan(&["worker"], json!({ "keep": null }))
        .expect_err("a null inside a table is still a null");
    assert!(
        error
            .to_string()
            .contains("`kelpie.worker.keep` has no TOML to write"),
        "{error}"
    );
}

/// The sentence for a section whose name this shepherd knows no dog by, which
/// is the daemon's own remedy in the CLI's own words.
#[test]
fn a_section_of_a_dog_nothing_knows_names_the_dog() {
    let text = no_known_dog("ghost");
    assert!(text.starts_with("nothing was written:"), "{text}");
    assert!(text.contains("`[ghost]` section"), "{text}");
    assert!(text.contains("`shep adopt` or `shep enable`"), "{text}");
    assert!(
        !text.contains("shep kv set"),
        "{text}: this name is not a key/value key"
    );
}

/// A whole-holder write carries the holder it replaces, which is what the write
/// checks the flock still holds before it sends.
#[test]
fn a_whole_holder_write_carries_the_holder_it_replaces() {
    let plan = section_plan(&["worker", "timeout"], json!(45)).unwrap();
    let Write::DogSection { toml, before } = &plan.write else {
        unreachable!()
    };
    assert_eq!(before, SECTION, "the text the shepherd answered");
    assert_ne!(toml, before);

    // A section this run created from nothing answered no text at all, and the
    // check has to read that as nothing having moved.
    let plan = plan_section(
        DocumentMut::new(),
        "ghost",
        &["timeout".to_string()],
        &json!(5),
    )
    .unwrap();
    let Write::DogSection { before, .. } = &plan.write else {
        unreachable!()
    };
    assert_eq!(before, "");

    let plan = plan_dog_table(
        "api",
        "kelpie",
        &["git".to_string()],
        &config(),
        &json!({ "merging": "rebase" }),
    )
    .unwrap();
    let Write::SheepDogTable { before, .. } = &plan.write else {
        unreachable!()
    };
    assert_eq!(
        *before,
        json!({ "git": { "checkout": "/srv/api", "merging": "ff" }, "options": {} }),
        "the whole table the request replaces, not the key the operator named"
    );
}

/// The check itself: a holder that moved anywhere is one the plan no longer
/// describes, and a holder that only gained a blank line is not a holder that
/// moved.
#[test]
fn a_holder_that_moved_anywhere_is_a_holder_the_plan_no_longer_describes() {
    assert!(!section_moved(SECTION, SECTION));
    assert!(
        !section_moved(SECTION, &format!("{SECTION}\n")),
        "a trailing blank line is not a change an operator made"
    );
    assert!(section_moved(
        SECTION,
        &SECTION.replace("timeout = 30", "timeout = 60")
    ));
    assert!(
        section_moved(SECTION, &SECTION.replace("# what it may fetch\n", "")),
        "a comment is part of the text a whole-section write sends back"
    );
    assert!(
        section_moved(SECTION, ""),
        "a section deleted is a section moved"
    );
    assert!(!section_moved("", ""));

    assert!(!table_moved(&json!({"a": 1}), &json!({"a": 1})));
    assert!(!table_moved(&json!({}), &json!({})));
    assert!(table_moved(&json!({"a": 1}), &json!({"a": 2})));
    assert!(
        table_moved(&json!({"a": 1}), &json!({"a": 1, "b": 2})),
        "a key added elsewhere is a key this plan would overwrite with nothing"
    );
    assert!(table_moved(&json!({"a": 1}), &Json::Null));
}

/// The sentence for a holder that moved, naming the holder and the reader that
/// shows the operator what it holds now.
#[test]
fn a_holder_that_moved_names_the_holder_and_the_reader() {
    let section = Write::DogSection {
        toml: String::new(),
        before: String::new(),
    };
    let text = holder_moved(Verb::Set, "kelpie", "", &section);
    assert!(text.starts_with("nothing was written:"), "{text}");
    assert!(text.contains("`kelpie`'s section"), "{text}");
    assert!(text.contains("changed while `shep set` waited"), "{text}");
    assert!(text.contains("`shep get kelpie`"), "{text}");

    let table = Write::SheepDogTable {
        dog: "kelpie".to_string(),
        table: DogTable::from(Map::new()),
        before: Json::Null,
    };
    let text = holder_moved(Verb::Set, "api", "dogs.kelpie.git", &table);
    assert!(text.contains("`api`'s table for `kelpie`"), "{text}");
    assert!(text.contains("`shep get api.dogs.kelpie.git`"), "{text}");

    // A moved holder is not a config that failed validation and not bad
    // arguments, so it is the code with nothing more specific.
    assert_eq!(WriteRefusal::Stale(text).code(), ExitCode::Failure);
}

/// `keep` is bounded at 4, so a default of 9 breaks it on every sheep that
/// does not hold its own `keep`: `a` and `c` are named, `b` is not.
fn default_plan(keep: i64) -> Plan {
    let sheep = BTreeMap::from([
        ("a".to_string(), DogTable::from(Map::new())),
        (
            "b".to_string(),
            DogTable::from(json!({ "keep": 2 }).as_object().unwrap().clone()),
        ),
        (
            "c".to_string(),
            DogTable::from(json!({ "merge": "ask" }).as_object().unwrap().clone()),
        ),
    ]);
    let plan = plan_dog_defaults(
        "kelpie",
        &["keep".to_string()],
        Json::Object(Map::new()),
        sheep,
        &json!(keep),
    )
    .unwrap();
    Plan {
        schema: Some(schema()),
        ..plan
    }
}

/// A default some sheep's layered table would break is refused naming each of
/// them and the rule, and the shepherd is sent nothing.
#[tokio::test]
async fn a_refused_default_names_every_failing_sheep_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |_request| Ok(Response::Pong))
            .await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        run(
            &client,
            &mut streams,
            &paths,
            "kelpie",
            "defaults.keep",
            &default_plan(9),
            Flags {
                verb: Verb::Set,
                yes: true,
                dry_run: false,
            },
        )
        .await
    };

    let said = String::from_utf8_lossy(&err).to_string();
    assert_eq!(code, ExitCode::InvalidConfig, "{said}");
    assert!(said.contains("`kelpie.defaults.keep`"), "{said}");
    assert!(said.contains("2 of the sheep it reaches"), "{said}");
    assert!(said.contains("`a` would hold"), "{said}");
    assert!(said.contains("`c` would hold"), "{said}");
    assert!(!said.contains("`b`"), "b holds its own `keep`: {said}");
    assert!(
        envelopes.try_recv().is_err(),
        "a refused default sends nothing"
    );
}

/// The control: the default every sheep it reaches takes goes out whole, as
/// `SetDogDefaults`, after the holder is read once more.
#[tokio::test]
async fn a_default_every_sheep_takes_is_sent_whole() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |request| match request {
            Request::DogDefaults { .. } => Ok(Response::DogDefaults { table: None }),
            Request::SetDogDefaults { dog, .. } => {
                Ok(Response::DogDefaultsSet { dog: dog.clone() })
            }
            _other => Ok(Response::Pong),
        })
        .await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        run(
            &client,
            &mut streams,
            &paths,
            "kelpie",
            "defaults.keep",
            &default_plan(3),
            Flags {
                verb: Verb::Set,
                yes: true,
                dry_run: false,
            },
        )
        .await
    };

    let said = String::from_utf8_lossy(&err).to_string();
    assert_eq!(code, ExitCode::Success, "{said}");
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        sent.push(envelope.body);
    }
    let Some(Request::SetDogDefaults { dog, table }) = sent.last() else {
        panic!("the write went out last: {sent:?}");
    };
    assert_eq!(dog, "kelpie");
    assert_eq!(
        table.as_ref().map(|table| table.as_map().clone()),
        json!({ "keep": 3 }).as_object().cloned()
    );
}

/// A dog that published no per-sheep schema is checked against nothing, so a
/// default no schema can judge is not refused.
#[test]
fn a_default_for_a_dog_with_no_sheep_schema_is_checked_against_nothing() {
    let plan = Plan {
        schema: None,
        ..default_plan(9)
    };
    assert!(schema_refusal(&plan, "kelpie", "defaults.keep").is_none());
}
