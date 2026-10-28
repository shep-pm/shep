//! The dog pane's write for a field inside an entry of an array or map of
//! tables: exact text, for each way `dogs.toml` can spell one.

use serde_json::{Value, json};

use super::super::edits::Edits;
use super::fixtures::{entries_schema, field};
use super::{ConfigPane, SectionWrite};

fn pane(section: &str) -> ConfigPane {
    ConfigPane::dog(
        "paddock".into(),
        None,
        entries_schema(),
        section.to_owned(),
        Vec::new(),
    )
}

fn edits_of(pairs: &[(&str, Value)]) -> Edits {
    let mut edits = Edits::default();
    for (key, value) in pairs {
        edits.set(field(key, value.clone()), None);
    }
    edits
}

fn written(pane: &ConfigPane, pairs: &[(&str, Value)]) -> String {
    match pane.edited_section_with(&edits_of(pairs)) {
        SectionWrite::Written(toml) => toml.as_str().to_owned(),
        other => panic!("wanted a section to send, got {other:?}"),
    }
}

/// Three `[[clients]]`, each with a comment above it, and one inside the
/// second.
const THREE: &str = concat!(
    "retries = 3\n",
    "\n",
    "# first\n",
    "[[clients]]\n",
    "name = \"alpha\"\n",
    "\n",
    "# second\n",
    "[[clients]]\n",
    "id = \"b-2\" # the id\n",
    "name = \"beta\"\n",
    "\n",
    "# third\n",
    "[[clients]]\n",
    "name = \"gamma\"\n",
);

#[test]
fn a_set_in_entry_one_of_three_keeps_the_comments_around_the_others() {
    let out = written(&pane(THREE), &[("clients[1].name", json!("bravo"))]);
    assert_eq!(out, THREE.replace("\"beta\"", "\"bravo\""));
}

#[test]
fn a_set_of_a_field_the_entry_lacks_joins_that_entry_alone() {
    let out = written(&pane(THREE), &[("clients[2].id", json!("c-3"))]);
    assert_eq!(out, format!("{THREE}id = \"c-3\"\n"));
    let out = written(&pane(THREE), &[("clients[0].id", json!("a-1"))]);
    assert_eq!(
        out,
        THREE.replace("name = \"alpha\"\n", "name = \"alpha\"\nid = \"a-1\"\n")
    );
}

#[test]
fn a_null_removes_that_key_of_that_entry_and_no_other() {
    let out = written(&pane(THREE), &[("clients[1].name", Value::Null)]);
    assert_eq!(out, THREE.replace("name = \"beta\"\n", ""));
}

#[test]
fn a_set_in_a_table_of_a_map_leaves_the_other_models() {
    let section = concat!(
        "# the small one\n",
        "[models.small]\n",
        "provider = \"local\"\n",
        "\n",
        "[models.gpt]\n",
        "provider = \"remote\"\n",
        "context = 4096\n",
    );
    let out = written(
        &pane(section),
        &[("models[\"gpt\"].provider", json!("edge"))],
    );
    assert_eq!(out, section.replace("\"remote\"", "\"edge\""));
}

#[test]
fn a_set_in_an_array_of_inline_tables_lands_in_that_inline_table() {
    let section = "clients = [{ name = \"alpha\" }, { name = \"beta\" }]\nretries = 3\n";
    let out = written(&pane(section), &[("clients[1].name", json!("bravo"))]);
    assert_eq!(
        out,
        "clients = [{ name = \"alpha\" }, { name = \"bravo\" }]\nretries = 3\n"
    );
}

#[test]
fn a_set_below_an_entry_creates_the_table_it_needs_inside_that_entry() {
    let out = written(&pane(THREE), &[("clients[1].limits.rate", json!(9))]);
    assert_eq!(
        out,
        THREE.replace(
            "name = \"beta\"\n",
            "name = \"beta\"\n\n[clients.limits]\nrate = 9\n"
        )
    );
}

#[test]
fn a_map_key_with_a_dot_is_one_key() {
    let section = "[models.\"a.b\"]\nprovider = \"local\"\n\n[models.a]\nprovider = \"other\"\n";
    let out = written(
        &pane(section),
        &[("models[\"a.b\"].provider", json!("edge"))],
    );
    assert_eq!(
        out,
        "[models.\"a.b\"]\nprovider = \"edge\"\n\n[models.a]\nprovider = \"other\"\n"
    );
}

#[test]
fn edits_to_two_entries_land_in_one_write() {
    let out = written(
        &pane(THREE),
        &[
            ("clients[0].name", json!("zero")),
            ("clients[2].name", json!("two")),
        ],
    );
    assert_eq!(
        out,
        THREE
            .replace("\"alpha\"", "\"zero\"")
            .replace("\"gamma\"", "\"two\"")
    );
}

/// The pane held three entries when the edit was filed; the section it
/// writes into holds two.
#[test]
fn an_index_past_the_end_of_the_array_writes_nothing() {
    let mut stale = pane(THREE);
    stale.section.as_mut().expect("a dog pane").toml =
        THREE.replace("# third\n[[clients]]\nname = \"gamma\"\n", "");
    assert_eq!(
        stale.edited_section_with(&edits_of(&[("clients[2].name", json!("two"))])),
        SectionWrite::Refused
    );
    assert_eq!(
        stale.edited_section_with(&edits_of(&[("clients[2].name", Value::Null)])),
        SectionWrite::Refused
    );
}

/// A refresh rebuilds the rows, and a key whose entry is gone names no row.
/// It is not a top-level key spelled with brackets.
#[test]
fn a_filed_key_whose_entry_is_gone_writes_nothing() {
    let two = THREE.replace("# third\n[[clients]]\nname = \"gamma\"\n", "");
    assert_eq!(
        pane(&two).edited_section_with(&edits_of(&[("clients[2].name", json!("two"))])),
        SectionWrite::Refused
    );
}

#[test]
fn a_missing_map_entry_is_not_created() {
    let mut stale = pane("[models.small]\nprovider = \"local\"\n");
    stale.section.as_mut().expect("a dog pane").toml =
        "[models.large]\nprovider = \"remote\"\n".to_owned();
    assert_eq!(
        stale.edited_section_with(&edits_of(&[("models[\"small\"].provider", json!("edge"))])),
        SectionWrite::Refused
    );
}

#[test]
fn removing_the_middle_of_three_clients_leaves_each_other_its_own_comment() {
    let out = written(
        &pane(THREE),
        &[("clients", json!([{ "name": "alpha" }, { "name": "gamma" }]))],
    );
    assert_eq!(
        out,
        concat!(
            "retries = 3\n",
            "\n",
            "# first\n",
            "[[clients]]\n",
            "name = \"alpha\"\n",
            "\n",
            "# third\n",
            "[[clients]]\n",
            "name = \"gamma\"\n",
        )
    );
}

#[test]
fn removing_the_first_of_three_clients_takes_its_comment_with_it() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([{ "id": "b-2", "name": "beta" }, { "name": "gamma" }]),
        )],
    );
    assert_eq!(
        out,
        concat!(
            "retries = 3\n",
            "\n",
            "# second\n",
            "[[clients]]\n",
            "id = \"b-2\" # the id\n",
            "name = \"beta\"\n",
            "\n",
            "# third\n",
            "[[clients]]\n",
            "name = \"gamma\"\n",
        )
    );
}

#[test]
fn removing_the_last_of_three_clients_leaves_the_other_two_as_they_were() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([{ "name": "alpha" }, { "id": "b-2", "name": "beta" }]),
        )],
    );
    assert_eq!(
        out,
        concat!(
            "retries = 3\n",
            "\n",
            "# first\n",
            "[[clients]]\n",
            "name = \"alpha\"\n",
            "\n",
            "# second\n",
            "[[clients]]\n",
            "id = \"b-2\" # the id\n",
            "name = \"beta\"\n",
        )
    );
}

#[test]
fn removing_the_only_client_writes_no_clients_table_at_all() {
    let section = "retries = 3\n\n# only\n[[clients]]\nname = \"alpha\"\n";
    let out = written(&pane(section), &[("clients", json!([]))]);
    assert_eq!(out, "retries = 3\n");
}

#[test]
fn removing_the_only_client_of_an_inline_array_leaves_an_empty_array() {
    let section = "clients = [{ name = \"alpha\" }]\nretries = 3\n";
    let out = written(&pane(section), &[("clients", json!([]))]);
    assert_eq!(out, "clients = []\nretries = 3\n");
}

#[test]
fn removing_one_of_two_inline_clients_keeps_the_other() {
    let section = "clients = [{ name = \"alpha\" }, { name = \"beta\" }]\n";
    let out = written(&pane(section), &[("clients", json!([{ "name": "beta" }]))]);
    assert_eq!(out, "clients = [{ name = \"beta\" }]\n");
}

#[test]
fn removing_the_last_of_two_inline_clients_keeps_the_first() {
    let section = "clients = [{ name = \"alpha\" }, { name = \"beta\" }]\n";
    let out = written(&pane(section), &[("clients", json!([{ "name": "alpha" }]))]);
    assert_eq!(out, "clients = [{ name = \"alpha\" }]\n");
}

#[test]
fn removing_one_of_two_models_leaves_the_other_with_its_comment() {
    let section = concat!(
        "# the small one\n",
        "[models.small]\n",
        "provider = \"local\"\n",
        "\n",
        "# the big one\n",
        "[models.gpt]\n",
        "provider = \"remote\"\n",
        "context = 4096\n",
    );
    let out = written(
        &pane(section),
        &[(
            "models",
            json!({ "gpt": { "provider": "remote", "context": 4096 } }),
        )],
    );
    assert_eq!(
        out,
        concat!(
            "\n",
            "# the big one\n",
            "[models.gpt]\n",
            "provider = \"remote\"\n",
            "context = 4096\n",
        )
    );
}

#[test]
fn removing_the_only_model_leaves_no_models_table() {
    let section = "retries = 3\n\n[models.small]\nprovider = \"local\"\n";
    let out = written(&pane(section), &[("models", json!({}))]);
    assert_eq!(out, "retries = 3\n");
}

/// The pane folds an edit to entry 2 into the array it files when entry 0 is
/// removed, so the writer sees entry 2 changed and entry 0 gone at once.
#[test]
fn a_removal_that_carries_an_edit_lands_the_edit_on_the_table_that_stays() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([{ "id": "b-2", "name": "beta" }, { "name": "gamma", "admin": true }]),
        )],
    );
    assert_eq!(
        out,
        concat!(
            "retries = 3\n",
            "\n",
            "# second\n",
            "[[clients]]\n",
            "id = \"b-2\" # the id\n",
            "name = \"beta\"\n",
            "\n",
            "# third\n",
            "[[clients]]\n",
            "name = \"gamma\"\n",
            "admin = true\n",
        )
    );
}

/// With nothing identical to anchor on, the edited table pairs with the one
/// at its place among those left.
#[test]
fn an_edited_table_next_to_a_removed_one_is_matched_by_position() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([{ "name": "alpha" }, { "id": "b-2", "name": "bravo" }]),
        )],
    );
    assert_eq!(
        out,
        THREE
            .replace("\"beta\"", "\"bravo\"")
            .replace("\n# third\n[[clients]]\nname = \"gamma\"\n", "",)
    );
}

#[test]
fn a_removal_that_also_unsets_a_key_of_a_table_that_stays_removes_that_key() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([{ "name": "alpha" }, { "id": "b-2", "name": "beta" }, {}]),
        )],
    );
    assert_eq!(out, THREE.replace("name = \"gamma\"\n", ""));
}

/// Two clients, a comment on each and one inside the second, and a table
/// that follows them.
const BEFORE_SERVER: &str = concat!(
    "# first\n",
    "[[clients]]\n",
    "name = \"alpha\"\n",
    "\n",
    "# second\n",
    "[[clients]]\n",
    "id = \"b-2\" # the id\n",
    "\n",
    "[server]\n",
    "port = 80\n",
);

#[test]
fn an_added_client_lands_after_the_last_one_and_before_the_next_section() {
    let out = written(
        &pane(BEFORE_SERVER),
        &[(
            "clients",
            json!([{ "name": "alpha" }, { "id": "b-2" }, { "name": "new" }]),
        )],
    );
    assert_eq!(
        out,
        concat!(
            "# first\n",
            "[[clients]]\n",
            "name = \"alpha\"\n",
            "\n",
            "# second\n",
            "[[clients]]\n",
            "id = \"b-2\" # the id\n",
            "\n",
            "[[clients]]\n",
            "name = \"new\"\n",
            "\n",
            "[server]\n",
            "port = 80\n",
        )
    );
}

#[test]
fn an_added_client_with_nothing_in_it_is_an_empty_table() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([{ "name": "alpha" }, { "id": "b-2", "name": "beta" }, { "name": "gamma" }, {}]),
        )],
    );
    assert_eq!(out, format!("{THREE}\n[[clients]]\n"));
}

#[test]
fn a_client_added_to_a_section_without_any_creates_the_array() {
    let out = written(
        &pane("retries = 3\n"),
        &[("clients", json!([{ "name": "new" }]))],
    );
    assert_eq!(out, "retries = 3\n\n[[clients]]\nname = \"new\"\n");
    let out = written(&pane(""), &[("clients", json!([{ "name": "new" }]))]);
    assert_eq!(out, "[[clients]]\nname = \"new\"\n");
}

#[test]
fn an_added_client_keeps_its_nested_default_table() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([
                { "name": "alpha" },
                { "id": "b-2", "name": "beta" },
                { "name": "gamma" },
                { "name": "new", "limits": { "rate": 9 } },
            ]),
        )],
    );
    assert_eq!(
        out,
        format!("{THREE}\n[[clients]]\nname = \"new\"\n\n[clients.limits]\nrate = 9\n")
    );
}

#[test]
fn a_client_added_after_a_removal_leaves_the_others_their_comments() {
    let out = written(
        &pane(THREE),
        &[(
            "clients",
            json!([{ "id": "b-2", "name": "beta" }, { "name": "gamma" }, { "name": "new" }]),
        )],
    );
    assert_eq!(
        out,
        THREE
            .replace("# first\n[[clients]]\nname = \"alpha\"\n\n", "")
            .to_owned()
            + "\n[[clients]]\nname = \"new\"\n"
    );
}

#[test]
fn a_client_added_next_to_an_edit_of_a_single_key_table_keeps_the_order() {
    let section = "[[clients]]\nname = \"alpha\"\n\n[[clients]]\nname = \"beta\"\n";
    let out = written(
        &pane(section),
        &[(
            "clients",
            json!([{ "name": "ALPHA" }, { "name": "beta" }, { "name": "new" }]),
        )],
    );
    assert_eq!(
        out,
        "[[clients]]\nname = \"ALPHA\"\n\n[[clients]]\nname = \"beta\"\n\n[[clients]]\nname = \"new\"\n"
    );
}

#[test]
fn a_client_added_to_an_array_of_inline_tables_joins_it() {
    let section = "clients = [{ name = \"alpha\" }, { name = \"beta\" }]\nretries = 3\n";
    let out = written(
        &pane(section),
        &[(
            "clients",
            json!([{ "name": "alpha" }, { "name": "beta" }, { "name": "new", "id": "n-1" }]),
        )],
    );
    assert_eq!(
        out,
        "clients = [{ name = \"alpha\" }, { name = \"beta\" }, { id = \"n-1\", name = \"new\" }]\nretries = 3\n"
    );
}

/// Two models with a comment above each, and a table after them.
const MODELS: &str = concat!(
    "# the small one\n",
    "[models.gpt-3]\n",
    "provider = \"local\"\n",
    "\n",
    "# the big one\n",
    "[models.gpt-4]\n",
    "provider = \"remote\" # hosted\n",
    "\n",
    "[server]\n",
    "port = 80\n",
);

#[test]
fn an_added_model_lands_after_the_last_model_and_before_the_next_section() {
    let out = written(
        &pane(MODELS),
        &[(
            "models",
            json!({
                "gpt-3": { "provider": "local" },
                "gpt-4": { "provider": "remote" },
                "gpt-5": { "provider": "edge" },
            }),
        )],
    );
    assert_eq!(
        out,
        MODELS.replace(
            "\n[server]",
            "\n[models.gpt-5]\nprovider = \"edge\"\n\n[server]"
        )
    );
}

#[test]
fn an_added_model_with_a_dot_or_a_space_in_its_key_is_written_quoted() {
    for (key, header) in [("a.b", "[models.\"a.b\"]"), ("a b", "[models.\"a b\"]")] {
        let out = written(
            &pane(MODELS),
            &[(
                "models",
                json!({
                    "gpt-3": { "provider": "local" },
                    "gpt-4": { "provider": "remote" },
                    key: {},
                }),
            )],
        );
        assert_eq!(
            out,
            MODELS.replace("\n[server]", &format!("\n{header}\n\n[server]")),
            "{key}"
        );
    }
}

#[test]
fn a_model_added_to_a_section_without_any_creates_its_table() {
    let out = written(
        &pane("retries = 3\n"),
        &[("models", json!({ "gpt-5": { "provider": "edge" } }))],
    );
    assert_eq!(out, "retries = 3\n\n[models.gpt-5]\nprovider = \"edge\"\n");
}

#[test]
fn a_model_added_to_an_inline_map_joins_it() {
    let section = "models = { gpt-3 = { provider = \"local\" } }\nretries = 3\n";
    let out = written(
        &pane(section),
        &[(
            "models",
            json!({ "gpt-3": { "provider": "local" }, "gpt-5": { "provider": "edge" } }),
        )],
    );
    assert_eq!(
        out,
        "models = { gpt-3 = { provider = \"local\" }, gpt-5 = { provider = \"edge\" } }\nretries = 3\n"
    );
}

#[test]
fn a_model_added_next_to_a_removal_lands_after_the_one_that_stays() {
    let out = written(
        &pane(MODELS),
        &[(
            "models",
            json!({
                "gpt-3": { "provider": "local" },
                "gpt-5": { "provider": "new" },
            }),
        )],
    );
    assert_eq!(
        out,
        concat!(
            "# the small one\n",
            "[models.gpt-3]\n",
            "provider = \"local\"\n",
            "\n",
            "[models.gpt-5]\n",
            "provider = \"new\"\n",
            "\n",
            "[server]\n",
            "port = 80\n",
        )
    );
}

#[test]
fn a_secret_in_an_added_table_is_written_like_any_other_value() {
    let out = written(
        &pane("retries = 3\n"),
        &[("clients", json!([{ "key": "hunter9" }]))],
    );
    assert_eq!(out, "retries = 3\n\n[[clients]]\nkey = \"hunter9\"\n");
}

#[test]
fn a_whole_value_whose_added_table_has_no_toml_type_is_refused() {
    assert_eq!(
        pane(THREE).edited_section_with(&edits_of(&[(
            "clients",
            json!([
                { "name": "alpha" },
                { "id": "b-2", "name": "beta" },
                { "name": "gamma" },
                { "peers": [{ "name": "x" }] },
            ]),
        )])),
        SectionWrite::Refused
    );
}

#[test]
fn a_filed_whole_value_that_is_already_the_section_changes_nothing() {
    let section = "[[clients]]\nname = \"alpha\"\n";
    assert_eq!(
        pane(section).edited_section_with(&edits_of(&[("clients", json!([{ "name": "alpha" }]))])),
        SectionWrite::Nothing
    );
}

#[test]
fn leaf_edits_after_a_removal_use_the_indexes_the_array_has_then() {
    let out = written(
        &pane(THREE),
        &[
            ("clients", json!([{ "name": "alpha" }, { "name": "gamma" }])),
            ("clients[1].name", json!("delta")),
        ],
    );
    assert!(out.contains("name = \"delta\""), "{out}");
    assert!(!out.contains("gamma") && !out.contains("beta"), "{out}");
}
