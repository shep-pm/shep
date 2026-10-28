//! A tagged union on a dog's table on a sheep: how it reads, opens, and
//! what it files.

use serde_json::{Map, Value, json};

use super::fixtures::filed;
use super::{ConfigPane, Lock, PaneRow};

/// The settings of a dog whose `retro` step is a `#[serde(tag = "kind")]`
/// enum: a unit-like arm, an arm with a required `path`, and an arm with a
/// required `plugin` beside a `skill` that defaults. `lint` is the same
/// enum, optional.
fn schema() -> Value {
    json!({
        "$ref": "#/$defs/Settings",
        "$defs": {
            "Settings": {
                "type": "object",
                "properties": {
                    "retro": { "$ref": "#/$defs/Step" },
                    "lint": { "anyOf": [{ "$ref": "#/$defs/Step" }, { "type": "null" }] },
                },
            },
            "Step": {
                "oneOf": [
                    {
                        "description": "Skip the step.",
                        "type": "object",
                        "properties": { "kind": { "type": "string", "const": "none" } },
                        "required": ["kind"],
                    },
                    {
                        "description": "A skill in the repo.",
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "const": "path" },
                            "path": { "type": "string" },
                        },
                        "required": ["kind", "path"],
                    },
                    {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "const": "plugin" },
                            "plugin": { "type": "string" },
                            "skill": { "type": "string", "default": "retro" },
                        },
                        "required": ["kind", "plugin"],
                    },
                ],
            },
        },
    })
}

fn pane_over(table: Value) -> ConfigPane {
    let Value::Object(table) = table else {
        panic!("a table is an object");
    };
    ConfigPane::sheep_dog("web".into(), "kelpie".into(), &schema(), table)
}

fn written(pane: &ConfigPane) -> Map<String, Value> {
    pane.edited_table_with(pane.edits())
}

/// Opens the union `key` and puts the cursor on its row number `item`.
fn on_arm(pane: &mut ConfigPane, key: &str, item: usize) {
    pane.move_to_key(key);
    pane.open_table();
    let field = pane
        .fields()
        .fields()
        .iter()
        .position(|field| field.key == key)
        .expect("a union field");
    pane.move_to_row(PaneRow::Arm { field, item });
}

/// What the row under the cursor says, as `name  value`.
fn cursor_text(pane: &ConfigPane) -> String {
    let row = pane.cursor().expect("a row");
    let key = pane.arm_field(row).expect("an arm row").key.clone();
    let (_, label) = pane.arm_label(row).expect("a label");
    format!("{label}  {}", pane.value(&key))
}

#[test]
fn a_union_reads_as_a_union_of_three_tags() {
    let pane = pane_over(json!({}));
    let field = pane.fields().by_key("retro").expect("a row");
    let crate::lookout::field::FieldKind::Union(union) = &field.kind else {
        panic!("expected a union, got {:?}", field.kind);
    };
    let tags: Vec<&str> = union.tags().collect();
    assert_eq!(tags, ["none", "path", "plugin"]);
    assert_eq!(pane.lock("retro"), None, "a union is editable");
}

#[test]
fn an_unset_union_reads_unset_and_opens_to_its_tag() {
    let mut pane = pane_over(json!({}));
    assert_eq!(pane.value("lint"), "(unset)");
    on_arm(&mut pane, "lint", 0);
    assert_eq!(cursor_text(&pane), "kind  (unset)");
    assert_eq!(pane.rows().len(), 3, "retro, lint, and lint's kind row");
}

#[test]
fn a_set_union_shows_its_tag_and_opens_to_that_arms_fields() {
    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "skills/mine" } }));
    assert_eq!(pane.value("retro"), "kind = path");
    on_arm(&mut pane, "retro", 0);
    assert_eq!(cursor_text(&pane), "kind  path");
    pane.move_by(1);
    assert_eq!(cursor_text(&pane), "path  skills/mine");
    assert_eq!(pane.rows().len(), 4, "retro, kind, path and lint");
}

#[test]
fn cycling_the_kind_row_writes_exactly_each_arms_table() {
    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "skills/mine" } }));
    on_arm(&mut pane, "retro", 0);

    pane.cycle();
    assert_eq!(
        written(&pane)["retro"],
        json!({ "kind": "plugin", "skill": "retro" })
    );
    assert_eq!(pane.edits().len(), 1, "one union, one edit");

    pane.cycle();
    assert_eq!(written(&pane)["retro"], json!({ "kind": "none" }));

    pane.cycle();
    assert_eq!(written(&pane)["retro"], json!({ "kind": "path" }));
}

#[test]
fn picking_a_tag_swaps_the_rows_for_the_new_arms() {
    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "skills/mine" } }));
    on_arm(&mut pane, "retro", 0);
    pane.cycle();
    assert_eq!(cursor_text(&pane), "kind  plugin");
    pane.move_by(1);
    assert_eq!(cursor_text(&pane), "plugin  (unset)");
    pane.move_by(1);
    assert_eq!(cursor_text(&pane), "skill  retro");
}

#[test]
fn cycling_an_unset_union_picks_the_first_tag() {
    let mut pane = pane_over(json!({}));
    on_arm(&mut pane, "lint", 0);
    pane.cycle();
    assert_eq!(written(&pane)["lint"], json!({ "kind": "none" }));
    assert_eq!(pane.value("lint"), "kind = none");
}

#[test]
fn typing_a_field_of_the_arm_files_the_whole_table() {
    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "skills/mine" } }));
    on_arm(&mut pane, "retro", 1);
    pane.begin_typing();
    for _ in 0.."skills/mine".len() {
        pane.type_backspace();
    }
    "skills/yours".chars().for_each(|c| pane.type_char(c));
    assert_eq!(pane.apply_typing(), None);
    assert_eq!(
        written(&pane)["retro"],
        json!({ "kind": "path", "path": "skills/yours" })
    );
    assert_eq!(pane.edits().len(), 1);
    assert_eq!(
        filed(&pane, "retro"),
        Some(json!({ "kind": "path", "path": "skills/yours" }))
    );
}

#[test]
fn a_required_field_cannot_be_left_empty() {
    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "skills/mine" } }));
    on_arm(&mut pane, "retro", 1);
    pane.begin_typing();
    for _ in 0.."skills/mine".len() {
        pane.type_backspace();
    }
    let refused = pane.apply_typing().expect("an empty path is refused");
    assert!(
        refused.text.contains("path is required"),
        "{}",
        refused.text
    );
    assert!(pane.typing().is_some(), "the editor stays open");
    assert!(pane.edits().is_empty());

    pane.abandon_typing();
    assert!(pane.cursor_refusal().is_some());
    pane.file_default();
    assert!(
        pane.edits().is_empty(),
        "d leaves a required field as it is"
    );
}

#[test]
fn a_field_the_arm_does_not_require_is_removed_by_d() {
    let mut pane = pane_over(json!({ "retro": { "kind": "plugin", "plugin": "p", "skill": "s" } }));
    on_arm(&mut pane, "retro", 2);
    assert_eq!(cursor_text(&pane), "skill  s");
    assert_eq!(pane.cursor_refusal(), None);
    pane.file_default();
    assert_eq!(
        written(&pane)["retro"],
        json!({ "kind": "plugin", "plugin": "p" })
    );
}

#[test]
fn d_on_a_union_clears_it_back_to_unset() {
    let mut pane = pane_over(json!({ "lint": { "kind": "path", "path": "skills/mine" } }));
    pane.move_to_key("lint");
    pane.file_default();
    assert_eq!(pane.value("lint"), "(unset)");
    assert!(!written(&pane).contains_key("lint"));
}

#[test]
fn d_on_the_kind_row_files_nothing() {
    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "skills/mine" } }));
    on_arm(&mut pane, "retro", 0);
    pane.file_default();
    assert!(pane.edits().is_empty());
}

#[test]
fn a_tag_that_names_no_arm_reads_as_stored_and_marked() {
    let mut pane = pane_over(json!({ "retro": { "kind": "gone" } }));
    assert_eq!(pane.value("retro"), "kind = gone (unknown)");
    on_arm(&mut pane, "retro", 0);
    assert_eq!(cursor_text(&pane), "kind  gone (unknown)");
    assert_eq!(
        pane.rows().len(),
        3,
        "retro, kind and lint: no arm's fields"
    );
    pane.close_table();
    assert!(pane.edits().is_empty(), "opening and closing files nothing");
}

#[test]
fn an_unknown_tag_changes_only_when_the_operator_picks_one() {
    let mut pane = pane_over(json!({ "retro": { "kind": "gone", "extra": 1 } }));
    on_arm(&mut pane, "retro", 0);
    assert!(pane.edits().is_empty());
    pane.cycle();
    assert_eq!(written(&pane)["retro"], json!({ "kind": "none" }));
}

#[test]
fn a_value_that_is_not_a_table_reads_unknown() {
    let pane = pane_over(json!({ "retro": "path" }));
    assert_eq!(pane.value("retro"), "(unknown)");
}

#[test]
fn the_arms_description_is_what_the_panel_carries() {
    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "x" } }));
    on_arm(&mut pane, "retro", 1);
    assert_eq!(
        pane.arm_description_of("retro"),
        Some("A skill in the repo.")
    );
    pane.cycle();
    assert_eq!(
        pane.arm_description_of("retro"),
        Some("A skill in the repo.")
    );
}

#[test]
fn a_dogs_own_section_shows_the_union_and_opens_nothing() {
    let schema = json!({ "properties": { "retro": schema()["$defs"]["Step"].clone() } });
    let mut pane = ConfigPane::dog(
        "kelpie".into(),
        None,
        schema,
        "retro = { kind = \"none\" }\n".to_owned(),
        Vec::new(),
    );
    assert_eq!(pane.lock("retro"), Some(Lock::NoWidget));
    pane.move_to_key("retro");
    pane.open_table();
    assert_eq!(pane.rows().len(), 1, "no row under it");
    pane.cycle();
    assert!(pane.edits().is_empty());
}

#[test]
fn a_recorded_kelpie_answer_makes_skills_retro_editable() {
    let table = json!({ "skills": { "implement": { "kind": "path", "path": "skills/mine" } } });
    let Value::Object(table) = table else {
        panic!("a table is an object");
    };
    let mut pane = ConfigPane::sheep_dog(
        "api".into(),
        "kelpie".into(),
        &crate::lookout::testing::kelpie_sheep_schema(),
        table,
    );
    assert_eq!(pane.value("skills.retro"), "(unset)");
    assert_eq!(pane.value("skills.implement"), "kind = path");
    assert_eq!(pane.lock("skills.retro"), None);

    pane.open_down_to("skills.retro");
    on_arm(&mut pane, "skills.retro", 0);
    pane.cycle();
    assert_eq!(
        written(&pane)["skills"],
        json!({ "implement": { "kind": "path", "path": "skills/mine" }, "retro": { "kind": "none" } })
    );
}

#[test]
fn an_open_union_draws_its_rows_and_the_arms_blurb() {
    use crate::lookout::view::fixtures::{plain, render_all};
    use crate::lookout::view::pane::pane_lines;

    let mut pane = pane_over(json!({ "retro": { "kind": "path", "path": "skills/mine" } }));
    on_arm(&mut pane, "retro", 1);
    let text = render_all(&pane_lines(&pane, plain(), 160, 0));
    let row = |name: &str| {
        text.lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("no {name} row in {text}"))
    };
    assert!(row("\u{25be} retro").contains("kind = path"), "{text}");
    assert!(row("   kind ").contains("path"), "{text}");
    assert!(row("> ").contains("skills/mine"), "{text}");
    assert!(
        text.contains("retro.path"),
        "the panel names the row: {text}"
    );
    assert!(text.contains("A skill in the repo."), "{text}");
    assert!(!text.contains('\u{1f}'), "no key leaks onto the screen");
}

#[test]
fn an_unknown_tag_is_marked_on_the_screen() {
    use crate::lookout::view::fixtures::{plain, render_all};
    use crate::lookout::view::pane::pane_lines;

    let mut pane = pane_over(json!({ "retro": { "kind": "gone" } }));
    on_arm(&mut pane, "retro", 0);
    let text = render_all(&pane_lines(&pane, plain(), 160, 0));
    assert!(text.contains("kind = gone (unknown)"), "{text}");
    assert!(text.contains("gone (unknown)"), "{text}");
}

#[test]
fn a_tag_whose_required_field_has_no_default_holds_the_close_until_it_is_given() {
    let mut pane = pane_over(json!({ "retro": { "kind": "none" } }));
    assert_eq!(pane.incomplete_union(), None);
    on_arm(&mut pane, "retro", 0);
    pane.cycle();
    assert_eq!(written(&pane)["retro"], json!({ "kind": "path" }));
    let text = pane.incomplete_union().expect("path is missing");
    assert_eq!(text, "path is required when kind is path; give it a value");

    pane.move_by(1);
    pane.begin_typing();
    "skills/mine".chars().for_each(|c| pane.type_char(c));
    assert_eq!(pane.apply_typing(), None);
    assert_eq!(pane.incomplete_union(), None);
}

#[test]
fn a_required_field_with_no_editor_is_not_told_to_be_given_a_value() {
    let schema = json!({
        "properties": {
            "retro": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "const": "blob" },
                            "blob": {},
                        },
                        "required": ["kind", "blob"],
                    },
                ],
            },
        },
    });
    let table = json!({ "retro": { "kind": "blob", "blob": 1 } });
    let Value::Object(table) = table else {
        panic!("a table is an object");
    };
    let mut pane = ConfigPane::sheep_dog("web".into(), "kelpie".into(), &schema, table);
    on_arm(&mut pane, "retro", 1);
    assert_eq!(pane.lock("retro\u{1f}blob\u{1f}blob"), Some(Lock::NoWidget));
    assert_eq!(pane.cursor_refusal(), None);
    pane.file_default();
    assert!(pane.edits().is_empty());
}

#[test]
fn a_union_naming_the_key_separator_stays_opaque() {
    let schema = json!({
        "properties": {
            "retro": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "kind": { "type": "string", "const": "a\u{1f}b" } },
                        "required": ["kind"],
                    },
                ],
            },
        },
    });
    let pane = ConfigPane::sheep_dog("web".into(), "kelpie".into(), &schema, Map::new());
    assert_eq!(pane.lock("retro"), Some(Lock::NoWidget));
}

#[test]
fn an_ordinary_key_holding_the_separator_is_not_read_as_a_union_row() {
    let mut pane = pane_over(json!({}));
    let key = "x\u{1f}y".to_owned();
    pane.file_field(key.clone(), json!("kept"));
    assert_eq!(filed(&pane, &key), Some(json!("kept")));
}

#[test]
fn a_required_field_with_no_editor_does_not_hold_the_close() {
    let schema = json!({
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
                            "kind": { "type": "string", "const": "blob" },
                            "blob": {},
                        },
                        "required": ["kind", "blob"],
                    },
                ],
            },
        },
    });
    let table = json!({ "retro": { "kind": "none" } });
    let Value::Object(table) = table else {
        panic!("a table is an object");
    };
    let mut pane = ConfigPane::sheep_dog("web".into(), "kelpie".into(), &schema, table);
    on_arm(&mut pane, "retro", 0);
    pane.cycle();
    assert_eq!(written(&pane)["retro"], json!({ "kind": "blob" }));
    assert_eq!(pane.incomplete_union(), None);
}
