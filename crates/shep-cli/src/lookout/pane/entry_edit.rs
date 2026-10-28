//! Editing a field inside an entry of an array or map of tables, on the pane
//! alone: the editors, the lock and what a keystroke files.

use serde_json::{Value, json};

use super::fixtures::filed;
use super::{ConfigPane, Lock, PaneRow, SectionWrite};
use crate::lookout::field::FieldKind;

/// Entries with one of each kind of field: text, a secret with a pattern,
/// a bool, a choice, an integer in a nested table, a list and a count.
fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "clients": { "type": "array", "items": { "$ref": "#/$defs/Client" } },
        },
        "$defs": {
            "Client": {
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean" },
                    "key": { "type": "string", "pattern": "^sk-", "x-shep-secret": true },
                    "limits": {
                        "type": "object",
                        "properties": { "rate": { "type": "integer", "minimum": 1 } },
                    },
                    "name": { "type": "string" },
                    "peers": { "type": "array", "items": { "$ref": "#/$defs/Client" } },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "tier": { "enum": ["free", "pro"] },
                },
            },
        },
    })
}

const SECTION: &str = concat!(
    "[[clients]]\nname = \"alpha\"\nkey = \"sk-old\"\nenabled = true\n",
    "tags = [\"a\"]\n[clients.limits]\nrate = 5\n",
    "[[clients]]\nname = \"beta\"\n",
);

fn pane() -> ConfigPane {
    ConfigPane::dog(
        "paddock".into(),
        None,
        schema(),
        SECTION.to_owned(),
        Vec::new(),
    )
}

/// The pane with its cursor on the row `key`, as an operator would reach it.
fn at(key: &str) -> ConfigPane {
    let mut pane = pane();
    pane.move_to_key(key);
    assert_eq!(
        cursor_key(&pane).as_deref(),
        Some(key),
        "the cursor is there"
    );
    pane
}

fn cursor_key(pane: &ConfigPane) -> Option<String> {
    let PaneRow::Entry(index) = pane.cursor()? else {
        return None;
    };
    match &pane.entry_row(index)?.body {
        super::EntryBody::Leaf { field, .. } => Some(field.key.clone()),
        super::EntryBody::Node { .. } | super::EntryBody::New { .. } => None,
    }
}

fn type_out(pane: &mut ConfigPane, text: &str) {
    for typed in text.chars() {
        pane.type_char(typed);
    }
}

#[test]
fn a_text_field_in_an_entry_is_typed_and_filed_under_its_full_path() {
    let mut pane = at("clients[1].name");
    assert_eq!(pane.cursor_lock(), None);
    pane.begin_typing();
    let typing = pane.typing().expect("the editor opens");
    assert_eq!(typing.key, "clients[1].name");
    assert_eq!(typing.buffer.as_str(), "beta", "seeded with what is stored");
    type_out(&mut pane, "x");
    assert_eq!(pane.apply_typing(), None);
    assert_eq!(filed(&pane, "clients[1].name"), Some(json!("betax")));
    assert_eq!(
        pane.edited_value("clients[1].name").as_deref(),
        Some("betax")
    );
    assert_eq!(pane.edits().len(), 1);
}

#[test]
fn a_field_nested_in_a_table_of_the_entry_files_under_the_dotted_path() {
    let mut pane = at("clients[0].limits.rate");
    pane.begin_typing();
    type_out(&mut pane, "0");
    assert_eq!(pane.apply_typing(), None);
    assert_eq!(filed(&pane, "clients[0].limits.rate"), Some(json!(50)));
    assert!(
        pane.apply_typing().is_none() && pane.typing().is_none(),
        "closed"
    );
}

#[test]
fn an_integer_field_in_an_entry_is_validated_against_its_bounds() {
    let mut pane = at("clients[0].limits.rate");
    pane.begin_typing();
    pane.type_backspace();
    type_out(&mut pane, "0");
    let refusal = pane.apply_typing().expect("below the minimum");
    assert!(refusal.text.contains('0'), "{}", refusal.text);
    assert!(pane.typing().is_some(), "the editor stays open");
    assert!(pane.edits().is_empty());
}

#[test]
fn space_cycles_a_bool_and_a_choice_in_an_entry() {
    let mut pane = at("clients[0].enabled");
    pane.cycle();
    assert_eq!(filed(&pane, "clients[0].enabled"), Some(json!(false)));
    pane.cycle();
    assert_eq!(filed(&pane, "clients[0].enabled"), None, "back to stored");
    let mut pane = at("clients[1].tier");
    pane.cycle();
    assert_eq!(filed(&pane, "clients[1].tier"), Some(json!("free")));
    pane.cycle();
    assert_eq!(filed(&pane, "clients[1].tier"), Some(json!("pro")));
}

#[test]
fn d_files_a_null_and_does_nothing_on_a_field_the_entry_lacks() {
    let mut pane = at("clients[0].name");
    pane.file_default();
    assert_eq!(filed(&pane, "clients[0].name"), Some(Value::Null));
    let mut pane = at("clients[1].tier");
    pane.file_default();
    assert!(pane.edits().is_empty(), "already at its default");
}

#[test]
fn an_empty_buffer_unsets_the_field_in_the_entry() {
    let mut pane = at("clients[1].name");
    pane.begin_typing();
    for _ in 0..4 {
        pane.type_backspace();
    }
    assert_eq!(pane.apply_typing(), None);
    assert_eq!(filed(&pane, "clients[1].name"), Some(Value::Null));
}

#[test]
fn a_list_and_a_count_in_an_entry_stay_read_only() {
    for key in ["clients[0].tags", "clients[0].peers"] {
        let mut pane = at(key);
        assert_eq!(
            pane.cursor_lock().map(|(_, lock)| lock),
            Some(Lock::NoWidget)
        );
        pane.begin_typing();
        pane.cycle();
        pane.file_default();
        pane.open_list();
        assert!(pane.typing().is_none() && pane.list().is_none(), "{key}");
        assert!(pane.edits().is_empty(), "{key}");
    }
    let pane = at("clients[0].peers");
    assert!(matches!(pane.cursor_kind(), Some(FieldKind::Entries(_))));
}

#[test]
fn a_secret_in_an_entry_reads_as_set_seeds_empty_and_never_prints_a_refused_value() {
    let mut pane = at("clients[0].key");
    pane.begin_typing();
    assert_eq!(pane.typing().expect("opens").buffer.as_str(), "");
    type_out(&mut pane, "hunter2");
    let refusal = pane.apply_typing().expect("the pattern refuses it");
    assert!(!refusal.text.contains("hunter2"), "{}", refusal.text);
    assert!(pane.edits().is_empty());
    let debug = format!("{pane:?}");
    assert!(
        !debug.contains("hunter2") && !debug.contains("sk-old"),
        "{debug}"
    );
}

#[test]
fn an_edit_in_an_entry_lands_in_the_section_text() {
    let mut pane = at("clients[1].name");
    pane.begin_typing();
    type_out(&mut pane, "!");
    assert_eq!(pane.apply_typing(), None);
    let SectionWrite::Written(out) = pane.edited_section_with(pane.edits()) else {
        panic!("the edit lands");
    };
    assert_eq!(out.as_str(), SECTION.replace("\"beta\"", "\"beta!\""));
}

#[test]
fn the_per_sheep_table_keeps_its_entries_read_only() {
    let table = json!({ "clients": [{ "name": "alpha" }] });
    let mut pane = ConfigPane::sheep_dog(
        "web".into(),
        "paddock".into(),
        &schema(),
        table.as_object().cloned().expect("a table"),
    );
    pane.move_to_key("clients");
    pane.open_table();
    pane.move_by(1);
    pane.open_table();
    pane.move_by(1);
    let Some(PaneRow::Entry(_)) = pane.cursor() else {
        panic!(
            "the cursor is on a row under the entry: {:?}",
            pane.cursor()
        );
    };
    assert_eq!(
        pane.cursor_lock().map(|(_, lock)| lock),
        Some(Lock::NoWidget)
    );
    pane.begin_typing();
    pane.cycle();
    pane.file_default();
    assert!(pane.typing().is_none() && pane.edits().is_empty());
}
