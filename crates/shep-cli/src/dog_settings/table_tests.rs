//! [`super::table::table_refusal`] and [`super::touched_refusal`]: what a
//! proposed dog table has to be, and what it is refused for.
//!
//! Every case is driven through a schema spelled the way a dog's `--schema`
//! answer is spelled, through a `$ref` and an `anyOf`, so a test proves the
//! gate reads the schema and not a fixture shaped like one.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use shep_core::config::DogTable;

use super::table::{default_refusals, table_refusal, touched_refusal};

/// A dog's own section schema: a bounded integer, a list whose item schema
/// publishes `minLength`, a closed set, a nested table with a `pattern`, an
/// array of tables, a tagged union, a secret with a grammar of the dog's own,
/// and a duration spelled `anyOf: [{$ref}, {type: null}]` the way
/// `shep-log-rotate` spells one.
fn schema() -> Value {
    json!({
        "$ref": "#/$defs/Settings",
        "$defs": {
            "Settings": {
                "type": "object",
                "properties": {
                    "keep": { "type": "integer", "minimum": 1, "maximum": 64 },
                    "rate": { "type": "number", "exclusiveMinimum": 0 },
                    "tags": { "type": "array", "items": { "type": "string", "minLength": 3 } },
                    "levels": { "type": "array", "items": { "type": "integer", "minimum": 1 } },
                    "modes": {
                        "type": "array",
                        "items": { "oneOf": [{ "const": "ask" }, { "const": "auto" }] },
                    },
                    "merge": { "enum": ["ask", "auto"] },
                    "hours": {
                        "type": "object",
                        "properties": {
                            "start": { "type": "string", "pattern": "^\\d{2}:\\d{2}$" },
                        },
                    },
                    "hosts": { "type": "array", "items": { "$ref": "#/$defs/Host" } },
                    "retro": { "$ref": "#/$defs/Step" },
                    "token": { "type": "string", "x-shep-secret": true, "pattern": "^sk-" },
                    "region": { "type": "string", "pattern": "^eu-" },
                    "max_age": {
                        "anyOf": [{ "$ref": "#/$defs/UpDuration" }, { "type": "null" }],
                    },
                },
            },
            "Host": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "weight": { "type": "integer", "minimum": 1 },
                },
            },
            "Step": {
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
                            "path": { "type": "string", "pattern": "^skills/" },
                        },
                        "required": ["kind", "path"],
                    },
                    {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "const": "plugin" },
                            "plugin": { "type": "string" },
                            "depth": { "type": "integer", "minimum": 1, "maximum": 3 },
                        },
                        "required": ["kind", "plugin"],
                    },
                ],
            },
            "UpDuration": { "type": "string", "pattern": "^\\d+(ms|h|m|s)?$" },
        },
    })
}

/// `table` as the map [`table_refusal`] takes.
fn table(value: Value) -> Map<String, Value> {
    value.as_object().expect("a table is an object").clone()
}

/// The whole table the schema takes, which is the case a gate that refuses
/// everything would also pass.
fn accepted() -> Value {
    json!({
        "keep": 8,
        "rate": 1.5,
        "tags": ["ask", "auto"],
        "levels": [1, 2],
        "modes": ["ask", "auto"],
        "merge": "ask",
        "hours": { "start": "09:30" },
        "hosts": [{ "name": "build", "weight": 2 }],
        "retro": { "kind": "plugin", "plugin": "retro", "depth": 2 },
        "token": "sk-live-51Hx9Qa",
        "max_age": "2h",
    })
}

#[test]
fn a_table_the_schema_takes_is_refused_nowhere() {
    assert_eq!(table_refusal(&schema(), &table(accepted())), None);
}

#[test]
fn a_scalar_out_of_its_floor_is_refused_with_its_path_and_sentence() {
    let refused = table_refusal(&schema(), &table(json!({ "keep": 0 }))).expect("0 is under 1");
    assert_eq!(refused.path, ["keep"]);
    assert_eq!(refused.text, "keep is \"0\"; keep starts at 1");
}

/// The same arm [`crate::dog_settings::refusal`] runs for a typed buffer,
/// answering a value that is already a number: the refusal is the sentence
/// the pane would have printed, not a second opinion about the type.
#[test]
fn a_scalar_that_is_not_a_number_is_refused_in_the_panes_own_words() {
    let refused = table_refusal(&schema(), &table(json!({ "keep": "lots" })))
        .expect("\"lots\" is not a whole number");
    assert_eq!(refused.path, ["keep"]);
    assert_eq!(
        refused.text,
        "keep is \"lots\", which is not a whole number shep accepts"
    );
}

#[test]
fn a_closed_set_refuses_a_value_it_does_not_name() {
    let refused = table_refusal(&schema(), &table(json!({ "merge": "never" })))
        .expect("never is not in the set");
    assert_eq!(refused.path, ["merge"]);
    assert_eq!(
        refused.text,
        "merge is \"never\"; merge takes one of ask, auto"
    );
}

/// The element, not the array, is what the item schema bounds, so the path
/// names the element and the sentence quotes it.
#[test]
fn a_list_element_the_item_schema_refuses_is_named_with_its_index() {
    let refused = table_refusal(&schema(), &table(json!({ "tags": ["ask", "ab"] })))
        .expect("ab is under the item schema's minLength");
    assert_eq!(refused.path, ["tags", "1"]);
    assert_eq!(
        refused.text,
        "tags.1 is \"ab\"; tags.1 needs at least 3 characters"
    );
}

#[test]
fn a_value_where_a_list_belongs_is_refused_as_a_list() {
    let refused =
        table_refusal(&schema(), &table(json!({ "tags": "ask" }))).expect("a string is not a list");
    assert_eq!(refused.path, ["tags"]);
    assert_eq!(
        refused.text,
        "tags is \"ask\", which is not a list shep accepts"
    );
}

/// A union is one table, so the refusal for a tag no arm holds a `const` on
/// names the union and lists the tags it does take.
#[test]
fn a_union_that_names_no_kind_is_refused_with_the_kinds_it_takes() {
    let refused = table_refusal(&schema(), &table(json!({ "retro": { "kind": "robot" } })))
        .expect("no arm holds a const on robot");
    assert_eq!(refused.path, ["retro"]);
    assert_eq!(
        refused.text,
        "retro is \"robot\", which is not a kind retro takes; try one of none, path, plugin"
    );
}

#[test]
fn a_union_that_names_no_tag_at_all_says_so() {
    let refused = table_refusal(&schema(), &table(json!({ "retro": { "plugin": "retro" } })))
        .expect("the arm's fields are not the tag");
    assert_eq!(refused.path, ["retro"]);
    assert_eq!(
        refused.text,
        "retro names no kind of its own; try one of none, path, plugin"
    );
}

/// The arm the tag names is the one whose fields are checked: an out-of-range
/// `depth` belongs to `plugin`, and a `path` held by the `path` arm is not
/// asked about here.
#[test]
fn a_field_of_the_chosen_arm_is_held_to_its_own_bounds() {
    let refused = table_refusal(
        &schema(),
        &table(json!({ "retro": { "kind": "plugin", "plugin": "retro", "depth": 9 } })),
    )
    .expect("9 is over the arm's ceiling");
    assert_eq!(refused.path, ["retro", "depth"]);
    assert_eq!(refused.text, "retro.depth is \"9\"; retro.depth stops at 3");

    let refused = table_refusal(
        &schema(),
        &table(json!({ "retro": { "kind": "path", "path": "mine" } })),
    )
    .expect("mine does not match the arm's pattern");
    assert_eq!(refused.path, ["retro", "path"]);
    assert_eq!(
        refused.text,
        "retro.path is \"mine\", which it does not take; try it matches ^skills/"
    );
}

/// A nested table is not one leaf: the row is `hours.start`, and the path
/// names every table the value sits in.
#[test]
fn a_nested_tables_leaf_is_refused_at_its_whole_path() {
    let refused = table_refusal(&schema(), &table(json!({ "hours": { "start": "9am" } })))
        .expect("9am does not match the pattern");
    assert_eq!(refused.path, ["hours", "start"]);
    assert_eq!(
        refused.text,
        "hours.start is \"9am\", which it does not take; try it matches ^\\d{2}:\\d{2}$"
    );
}

/// The other half of a nested table: a scalar sitting where the schema has a
/// table leaves every leaf below it unreadable, and the key to fix is the one
/// the operator wrote.
#[test]
fn a_scalar_where_the_schema_has_a_table_is_refused_at_the_table() {
    let refused = table_refusal(&schema(), &table(json!({ "hours": "09:30" })))
        .expect("a string is not a table");
    assert_eq!(refused.path, ["hours"]);
    assert_eq!(
        refused.text,
        "hours is \"09:30\", which is not a table shep accepts"
    );
}

/// An array of tables is one row and many entries: the item schema flattened
/// beside it is what bounds each entry's own fields.
#[test]
fn an_entry_of_an_array_of_tables_is_refused_at_its_index() {
    let refused = table_refusal(
        &schema(),
        &table(json!({ "hosts": [{ "name": "build" }, { "name": "web", "weight": 0 }] })),
    )
    .expect("weight 0 is under the item schema's floor");
    assert_eq!(refused.path, ["hosts", "1", "weight"]);
    assert_eq!(
        refused.text,
        "hosts.1.weight is \"0\"; hosts.1.weight starts at 1"
    );
}

/// shep's own grammar, reached through a `$ref` behind an `anyOf`, which is
/// how a dog spells `Option<UpDuration>`. `1w` is a spelling of a retention
/// window that shep's grammar, which stops at days, still refuses.
#[test]
fn a_nullable_grammar_refuses_a_spelling_shep_does_not_take() {
    let refused = table_refusal(&schema(), &table(json!({ "max_age": "1w" })))
        .expect("1w is outside the grammar");
    assert_eq!(refused.path, ["max_age"]);
    assert_eq!(
        refused.text,
        "max_age is \"1w\", which is not a duration shep accepts; \
         try 500ms, 2s, 5m, 1h, 7d, a bare number is milliseconds"
    );
}

/// A secret's value must not come back at the caller: the schema decides that
/// a field is a credential, and every sentence this gate builds resolves what
/// it may print before it builds one.
#[test]
fn a_secrets_value_never_reaches_the_sentence() {
    const CREDENTIAL: &str = "sk_live_HUNTER2";
    let refused = table_refusal(&schema(), &table(json!({ "token": CREDENTIAL })))
        .expect("the pattern refuses it");
    assert_eq!(
        refused.text,
        "token is \"<set>\", which it does not take; try it matches ^sk-"
    );
    assert!(!refused.text.contains(CREDENTIAL), "{}", refused.text);
    assert!(!format!("{refused:?}").contains(CREDENTIAL), "{refused:?}");
}

/// The control on the test above: a value that is not a credential is quoted
/// back, since redacting everything would pass that test while making every
/// refusal useless.
#[test]
fn an_ordinary_value_is_still_quoted_back() {
    let refused = table_refusal(&schema(), &table(json!({ "region": "Europe" })))
        .expect("Europe does not match ^eu-");
    assert_eq!(refused.path, ["region"]);
    assert!(refused.text.contains("Europe"), "{}", refused.text);
}

/// A write carries the whole table, keys the schema has never heard of
/// included, and shep is not the authority on a dog's config beyond what that
/// dog published.
#[test]
fn a_key_the_schema_does_not_name_is_left_alone() {
    assert_eq!(
        table_refusal(
            &schema(),
            &table(
                json!({ "keep": 8, "operator_wrote_this": "anything", "hours": { "start": "09:30", "extra": 7 } })
            )
        ),
        None
    );
}

/// An absent key and a `null` are both unset, which for a dog is the way a
/// value goes back to the dog's own default.
#[test]
fn a_null_is_unset_and_not_a_value_to_check() {
    assert_eq!(
        table_refusal(
            &schema(),
            &table(
                json!({ "keep": null, "tags": null, "retro": null, "max_age": null, "hours": null })
            )
        ),
        None
    );
}

/// A read-only row is a row this pane has no editor for: a list whose items
/// may be `null`, and a map of arbitrary keys. Neither is checked, exactly as
/// neither is edited.
#[test]
fn a_read_only_row_refuses_nothing() {
    let schema = json!({
        "type": "object",
        "properties": {
            "maybe": { "type": "array", "items": { "type": ["string", "null"] } },
            "labels": { "type": "object", "additionalProperties": { "type": "string" } },
        },
    });
    assert_eq!(
        table_refusal(
            &schema,
            &table(json!({ "maybe": [1, 2], "labels": { "any": 7 } }))
        ),
        None
    );
}

/// The first refusal, in row order, and nothing after it.
#[test]
fn the_first_refusal_in_row_order_is_the_only_answer() {
    let refused = table_refusal(&schema(), &table(json!({ "keep": 0, "merge": "never" })))
        .expect("two refusals are available");
    assert_eq!(refused.path, ["keep"]);
}

/// IR-41: the `Debug` decision is derived, and the exact string is what pins
/// it.
#[test]
fn the_debug_of_a_refusal_names_the_path_and_the_sentence() {
    let refused = table_refusal(&schema(), &table(json!({ "keep": 0 }))).expect("0 is under 1");
    assert_eq!(
        format!("{refused:?}"),
        "TableRefusal { path: [\"keep\"], text: \"keep is \\\"0\\\"; keep starts at 1\" }"
    );
}

/// The keys a caller chose, as property paths.
fn touched(paths: &[&[&str]]) -> BTreeSet<Vec<String>> {
    paths
        .iter()
        .map(|path| path.iter().map(|name| (*name).to_owned()).collect())
        .collect()
}

/// [`touched_refusal`] is [`table_refusal`] with the walk unchanged and the
/// answers narrowed to the keys its caller chose. Every pair below is the same
/// table asked twice: once about a key it edited, once about one it did not.
#[test]
fn a_touched_scalar_is_refused_and_an_untouched_one_is_not() {
    let proposed = table(json!({ "keep": 0, "merge": "never" }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["keep"]]))
        .expect("keep is a key this caller chose");
    assert_eq!(refused.path, ["keep"]);
    assert_eq!(refused.text, "keep is \"0\"; keep starts at 1");
    assert_eq!(
        touched_refusal(&schema(), &proposed, &touched(&[&["region"]])),
        None,
        "the two refusals in this table sit at keys nobody here chose"
    );
}

#[test]
fn a_touched_list_element_is_refused_and_an_untouched_one_is_not() {
    let proposed = table(json!({ "tags": ["ask", "ab"] }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["tags", "1"]]))
        .expect("the second element is the one that was edited");
    assert_eq!(refused.path, ["tags", "1"]);
    assert_eq!(
        refused.text,
        "tags.1 is \"ab\"; tags.1 needs at least 3 characters"
    );
    assert_eq!(
        touched_refusal(&schema(), &proposed, &touched(&[&["tags", "0"]])),
        None,
        "the element this caller edited is the one the item schema takes"
    );
}

#[test]
fn a_touched_union_is_refused_for_the_tag_it_names() {
    let proposed = table(json!({ "retro": { "kind": "robot" } }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["retro"]]))
        .expect("the union row is the one that was edited");
    assert_eq!(refused.path, ["retro"]);
    assert_eq!(
        refused.text,
        "retro is \"robot\", which is not a kind retro takes; try one of none, path, plugin"
    );
    assert_eq!(
        touched_refusal(&schema(), &proposed, &touched(&[&["keep"]])),
        None,
        "an untouched key does not get to report the union"
    );
}

#[test]
fn a_touched_nested_leaf_is_refused_at_its_whole_path() {
    let proposed = table(json!({ "hours": { "start": "9am" } }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["hours", "start"]]))
        .expect("the leaf is the row an editor writes");
    assert_eq!(refused.path, ["hours", "start"]);
    assert_eq!(
        refused.text,
        "hours.start is \"9am\", which it does not take; try it matches ^\\d{2}:\\d{2}$"
    );
    assert_eq!(
        touched_refusal(&schema(), &proposed, &touched(&[&["hours"]])),
        None,
        "the table row itself names no value to check"
    );
}

/// An entry of an array of tables is a path with an index in it, which is how
/// a pane files an edit under `hosts[0].weight`.
#[test]
fn a_touched_entry_leaf_is_refused_and_another_entry_is_not() {
    let proposed = table(json!({
        "hosts": [{ "name": "build", "weight": 0 }, { "name": "web", "weight": 0 }]
    }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["hosts", "0", "weight"]]))
        .expect("the first entry's weight is the one that was edited");
    assert_eq!(refused.path, ["hosts", "0", "weight"]);
    assert_eq!(
        refused.text,
        "hosts.0.weight is \"0\"; hosts.0.weight starts at 1"
    );
    assert_eq!(
        touched_refusal(&schema(), &proposed, &touched(&[&["hosts", "1", "name"]])),
        None,
        "the other entry's bad weight rode in from the file and is not this caller's"
    );
}

/// The two doors agree wherever both answer, which is the reason there is one
/// module: a value cannot pass the keystroke and fail the write.
#[test]
fn the_scoped_door_answers_what_the_whole_door_answers_for_the_same_key() {
    let proposed = table(json!({ "keep": 0, "merge": "never" }));
    let whole = table_refusal(&schema(), &proposed).expect("keep is the first refusal");
    let scoped = touched_refusal(&schema(), &proposed, &touched(&[&["keep"]]))
        .expect("asked about keep, answered about keep");
    assert_eq!(scoped, whole);
}

/// A table is JSON and a buffer is text, and this is the one rule that
/// belongs to the table door alone: a value has to be the JSON type the schema
/// names before any rule on it is read. `keep = "8"` is what an operator writes
/// into `dogs.toml` when the number is a string in their head, and the pane
/// files an integer as `Value::from(i64)`, never as a string.
#[test]
fn a_string_that_reads_like_a_number_is_refused_for_an_integer_field() {
    let refused = table_refusal(&schema(), &table(json!({ "keep": "8" })))
        .expect("\"8\" is a string, not a number");
    assert_eq!(refused.path, ["keep"]);
    assert_eq!(
        refused.text,
        "keep is \"8\", which is not a whole number shep accepts"
    );
}

#[test]
fn a_string_that_reads_like_a_decimal_is_refused_for_a_number_field() {
    let refused = table_refusal(&schema(), &table(json!({ "rate": "1.5" })))
        .expect("\"1.5\" is a string, not a number");
    assert_eq!(refused.path, ["rate"]);
    assert_eq!(
        refused.text,
        "rate is \"1.5\", which is not a number shep accepts"
    );
}

/// A JSON number is not a whole number either, and the parse still says so:
/// the type gate opens the door, it does not answer who walks through it.
#[test]
fn a_decimal_is_refused_for_an_integer_field() {
    let refused = table_refusal(&schema(), &table(json!({ "keep": 8.5 })))
        .expect("8.5 is not a whole number");
    assert_eq!(
        refused.text,
        "keep is \"8.5\", which is not a whole number shep accepts"
    );
}

/// The control on the three above: the numbers the pane files are taken, and a
/// number out of its bounds is refused by the bound.
#[test]
fn a_json_number_is_taken_and_a_number_out_of_bounds_is_refused_by_its_bound() {
    assert_eq!(
        table_refusal(&schema(), &table(json!({ "keep": 8, "rate": 1.5 }))),
        None,
        "the shape the pane files"
    );
    let refused = table_refusal(&schema(), &table(json!({ "rate": 0 })))
        .expect("0 is not above a floor of 0");
    assert_eq!(refused.text, "rate is \"0\"; rate must be above 0");
}

/// The same rule one level down: an element is converted to text for the item
/// schema's rules, so a number or a boolean in a string list reads as "8" and
/// "true".
#[test]
fn a_number_element_is_refused_for_a_string_list() {
    let refused = table_refusal(&schema(), &table(json!({ "tags": ["ask", 8] })))
        .expect("8 is a number, not a string");
    assert_eq!(refused.path, ["tags", "1"]);
    assert_eq!(
        refused.text,
        "tags.1 is \"8\", which is not a string shep accepts"
    );
}

#[test]
fn a_boolean_element_is_refused_for_a_string_list() {
    let refused = table_refusal(&schema(), &table(json!({ "tags": [true] })))
        .expect("true is a boolean, not a string");
    assert_eq!(refused.path, ["tags", "0"]);
    assert_eq!(
        refused.text,
        "tags.0 is \"true\", which is not a string shep accepts"
    );
}

#[test]
fn a_string_that_reads_like_a_number_is_refused_for_an_integer_list() {
    let refused = table_refusal(&schema(), &table(json!({ "levels": ["2"] })))
        .expect("\"2\" is a string, not a number");
    assert_eq!(refused.path, ["levels", "0"]);
    assert_eq!(
        refused.text,
        "levels.0 is \"2\", which is not a whole number shep accepts"
    );
}

/// The control on the three above, and the bound below it: an integer list
/// takes the numbers `ListPane::apply_typing` files, and refuses one under its
/// item schema's floor.
#[test]
fn an_integer_element_is_taken_and_one_under_the_floor_is_refused_by_the_floor() {
    assert_eq!(
        table_refusal(&schema(), &table(json!({ "levels": [2, 3] }))),
        None,
        "the shape the list editor files"
    );
    let refused = table_refusal(&schema(), &table(json!({ "levels": [0] })))
        .expect("0 is under the item schema's floor");
    assert_eq!(refused.path, ["levels", "0"]);
    assert_eq!(refused.text, "levels.0 is \"0\"; levels.0 starts at 1");
}

/// A type refusal resolves what it may print through the same door every
/// other sentence here does.
#[test]
fn a_type_refusal_for_a_secret_names_the_marker_and_not_the_value() {
    let refused = table_refusal(&schema(), &table(json!({ "token": 90210 })))
        .expect("a number is not a string");
    assert_eq!(
        refused.text,
        "token is \"<set>\", which is not a string shep accepts"
    );
    assert!(!refused.text.contains("90210"), "{}", refused.text);
}

/// The closed set spelled as a `oneOf` of `const`s, which publishes no `enum`
/// and so leaves `bounds.choices` empty: the names reach the walk through
/// [`ListItem::Choice`], and an element outside them is refused at its index.
#[test]
fn a_list_item_closed_set_spelled_as_a_one_of_refuses_a_name_it_does_not_carry() {
    let refused = table_refusal(&schema(), &table(json!({ "modes": ["ask", "never"] })))
        .expect("never is not one of the two names the item carries");
    assert_eq!(refused.path, ["modes", "1"]);
    assert_eq!(
        refused.text,
        "modes.1 is \"never\"; modes.1 takes one of ask, auto"
    );
}

/// The scoped door keeps walking past a value it may not report. The bad value
/// rides along in the write either way, and the key the caller chose is the one
/// it can act on.
#[test]
fn an_untouched_bad_element_does_not_hide_a_touched_one() {
    let proposed = table(json!({ "tags": ["a", "b"] }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["tags", "1"]]))
        .expect("tags.1 is under the item schema's minLength");
    assert_eq!(refused.path, ["tags", "1"]);
    assert_eq!(
        refused.text,
        "tags.1 is \"b\"; tags.1 needs at least 3 characters"
    );
}

#[test]
fn an_untouched_element_of_the_wrong_type_does_not_hide_a_touched_one() {
    let proposed = table(json!({ "tags": [8, "ab"] }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["tags", "1"]]))
        .expect("tags.1 is under the item schema's minLength");
    assert_eq!(refused.path, ["tags", "1"]);
    assert_eq!(
        refused.text,
        "tags.1 is \"ab\"; tags.1 needs at least 3 characters"
    );
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["tags", "0"]]))
        .expect("asked about tags.0, which is a number");
    assert_eq!(
        refused.text,
        "tags.0 is \"8\", which is not a string shep accepts"
    );
}

#[test]
fn an_untouched_entry_that_is_not_a_table_does_not_hide_a_touched_one() {
    let proposed = table(json!({ "hosts": ["not a table", { "name": "web", "weight": 0 }] }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["hosts", "1", "weight"]]))
        .expect("hosts.1.weight is under the item schema's floor");
    assert_eq!(refused.path, ["hosts", "1", "weight"]);
    assert_eq!(
        refused.text,
        "hosts.1.weight is \"0\"; hosts.1.weight starts at 1"
    );
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["hosts", "0"]]))
        .expect("asked about hosts.0, which is a string");
    assert_eq!(
        refused.text,
        "hosts.0 is \"not a table\", which is not a table shep accepts"
    );
}

/// The map half of an entry row, which walks in key order and holds the same
/// promise.
#[test]
fn an_untouched_map_entry_that_is_not_a_table_does_not_hide_a_touched_one() {
    let schema = json!({
        "type": "object",
        "properties": {
            "hosts": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "properties": { "weight": { "type": "integer", "minimum": 1 } },
                },
            },
        },
    });
    let proposed = table(json!({ "hosts": { "a": "not a table", "b": { "weight": 0 } } }));
    let refused = touched_refusal(&schema, &proposed, &touched(&[&["hosts", "b", "weight"]]))
        .expect("hosts.b.weight is under the item schema's floor");
    assert_eq!(refused.path, ["hosts", "b", "weight"]);
    assert_eq!(
        refused.text,
        "hosts.b.weight is \"0\"; hosts.b.weight starts at 1"
    );
}

/// The arm's own `required` list is a rule the schema published, and this is
/// the one key the walk refuses for being absent. The refusal lands on the
/// union, because the key to fix is the table that holds no value for the
/// field, and it names both keys the way the table names them.
#[test]
fn a_union_arm_that_requires_a_field_the_table_does_not_hold_is_refused() {
    let refused = table_refusal(&schema(), &table(json!({ "retro": { "kind": "path" } })))
        .expect("the path arm requires path");
    assert_eq!(refused.path, ["retro"]);
    assert_eq!(
        refused.text,
        "retro.path is required when retro.kind is path; give it a value"
    );
}

/// The control: an arm that requires nothing but its tag, and an arm holding
/// what it requires, are refused nowhere.
#[test]
fn a_union_arm_that_holds_what_it_requires_is_refused_nowhere() {
    assert_eq!(
        table_refusal(&schema(), &table(json!({ "retro": { "kind": "none" } }))),
        None,
        "the none arm requires the tag and nothing else"
    );
    assert_eq!(
        table_refusal(
            &schema(),
            &table(json!({ "retro": { "kind": "path", "path": "skills/a" } }))
        ),
        None,
        "the path arm holding its own field"
    );
}

/// A `null` is the pane's unset spelling, and `ConfigPane::incomplete_union`
/// reads a key held as `null` as present. The table door reads it the same way
/// rather than refusing a table the pane let through.
#[test]
fn a_required_field_held_as_null_is_left_as_the_pane_leaves_it() {
    assert_eq!(
        table_refusal(
            &schema(),
            &table(json!({ "retro": { "kind": "path", "path": null } }))
        ),
        None,
        "the pane files a null for a field it clears, and does not refuse it"
    );
}

/// A field this pane has no editor for is not asked for, which is the rule
/// `ConfigPane::incomplete_union` applies and the one this door mirrors: an
/// arm's nested table would need a sub-screen the arm's rows do not have.
#[test]
fn a_required_field_with_no_editor_is_not_asked_for() {
    let schema = json!({
        "type": "object",
        "properties": {
            "retro": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "kind": { "const": "blob" },
                            "blob": {
                                "type": "object",
                                "properties": { "keep": { "type": "integer" } },
                            },
                        },
                        "required": ["kind", "blob"],
                    },
                ],
            },
        },
    });
    assert_eq!(
        table_refusal(&schema, &table(json!({ "retro": { "kind": "blob" } }))),
        None,
        "the arm's nested table is a read-only row, and a read-only row is not asked for"
    );
}

/// The refusal sits on the union's own path, which is the path a pane's edit
/// of any row under the union files, so the scoped door answers it too.
#[test]
fn a_touched_union_missing_a_required_field_is_refused_and_an_untouched_one_is_not() {
    let proposed = table(json!({ "retro": { "kind": "path" } }));
    let refused = touched_refusal(&schema(), &proposed, &touched(&[&["retro"]]))
        .expect("the union row is the one that was edited");
    assert_eq!(refused.path, ["retro"]);
    assert_eq!(
        refused.text,
        "retro.path is required when retro.kind is path; give it a value"
    );
    assert_eq!(
        touched_refusal(&schema(), &proposed, &touched(&[&["keep"]])),
        None,
        "an untouched union keeps its own business"
    );
}

/// A default's value is judged by the sheep that read it: `a` and `c` read
/// the bad `keep` and are named in name order, `b` holds its own good one and
/// is not, and the default's own value is reported once, against the default,
/// only when no sheep already names it.
#[test]
fn a_default_is_checked_against_the_layered_table_of_every_sheep_it_reaches() {
    let default = DogTable::from(table(json!({ "keep": 100, "merge": "ask" })));
    let sheep = std::collections::BTreeMap::from([
        ("a".to_string(), DogTable::from(table(json!({})))),
        ("b".to_string(), DogTable::from(table(json!({ "keep": 4 })))),
        (
            "c".to_string(),
            DogTable::from(table(json!({ "merge": "auto" }))),
        ),
    ]);

    let refused = default_refusals(&schema(), &default, &sheep, &touched(&[&["keep"]]));
    assert_eq!(
        refused
            .sheep
            .iter()
            .map(|(name, refusal)| (name.as_str(), refusal.path.clone()))
            .collect::<Vec<_>>(),
        [
            ("a", vec!["keep".to_string()]),
            ("c", vec!["keep".to_string()])
        ]
    );
    assert!(
        refused.sheep[0].1.text.contains("keep"),
        "{}",
        refused.sheep[0].1.text
    );
    assert_eq!(refused.default, None, "a sheep already names that value");

    assert!(
        default_refusals(&schema(), &default, &sheep, &BTreeSet::new()).is_empty(),
        "a default that puts no value anywhere is checked against nothing"
    );

    let reaches_nobody = std::collections::BTreeMap::new();
    assert!(
        default_refusals(&schema(), &default, &reaches_nobody, &touched(&[&["keep"]])).is_empty()
    );
}

/// A sheep that holds its own `keep` hides the default's from the dog today,
/// but resetting that `keep` exposes it again, so the default is judged on
/// its own value and reported once, against the default.
#[test]
fn a_default_a_sheep_shadows_is_still_judged_by_its_own_value() {
    let default = DogTable::from(table(json!({ "keep": 100 })));
    let sheep = std::collections::BTreeMap::from([
        ("a".to_string(), DogTable::from(table(json!({ "keep": 4 })))),
        ("b".to_string(), DogTable::from(table(json!({ "keep": 5 })))),
    ]);

    let refused = default_refusals(&schema(), &default, &sheep, &touched(&[&["keep"]]));
    assert_eq!(refused.sheep, []);
    let by_default = refused.default.expect("the default's own keep is over 64");
    assert_eq!(by_default.path, ["keep"]);
    assert!(by_default.text.contains("keep"), "{}", by_default.text);

    let reaches_nobody = std::collections::BTreeMap::new();
    assert!(
        default_refusals(&schema(), &default, &reaches_nobody, &touched(&[&["keep"]])).is_empty(),
        "a default that reaches no sheep is checked against nothing"
    );
    assert!(
        default_refusals(&schema(), &default, &sheep, &touched(&[&["merge"]])).is_empty(),
        "a path the write did not touch is not asked about"
    );
}
