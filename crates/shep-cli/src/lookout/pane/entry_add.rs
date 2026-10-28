//! Adding an entry to an array or map of tables, on the pane alone.
//!
//! `Enter` on the `+ new` row at the foot of an open entries row adds one. An
//! array appends at once; a map opens a text editor for the new entry's key,
//! which [`ConfigPane::apply_typing`] hands to [`ConfigPane::apply_new_key`].
//! The entry holds what its schema declares a `default` for and nothing else:
//! the dog is the authority on its own config.
//!
//! Like a removal, an addition files the entries row's whole new value, see
//! [`ConfigPane::file_whole`], and `u` gives it back through the same record.

use serde_json::Value;

use super::super::field::{Entries, FieldKind};
use super::super::textbox::TextBox;
use super::edit::PaneTyping;
use super::entries::EntryBody;
use super::{ConfigPane, PaneRow, PaneTarget, PathStep};
use crate::dog_settings::Refusal;

/// What the key editor's [`PaneTyping::key`] ends with after the entries
/// field's own key, so it names no field the pane holds.
const NEW_KEY_SUFFIX: &str = "[]";

impl ConfigPane {
    /// Whether the cursor is on the `+ new` row of a dog's own entries row.
    #[must_use]
    pub(in crate::lookout) fn cursor_on_new(&self) -> bool {
        matches!(self.target, PaneTarget::Dog { .. })
            && matches!(
                self.cursor(),
                Some(PaneRow::Entry(row)) if self.entries.new_field(row).is_some()
            )
    }

    /// `Enter` on `+ new`: appends an entry to an array, and opens the key
    /// editor for a map. Does nothing on any other row.
    pub(in crate::lookout) fn begin_new_entry(&mut self) {
        let Some(PaneRow::Entry(row)) = self.cursor().filter(|_| self.cursor_on_new()) else {
            return;
        };
        let Some(field) = self.entries.new_field(row) else {
            return;
        };
        let Some(FieldKind::Entries(kind)) =
            self.fields.fields().get(field).map(|f| f.kind.clone())
        else {
            return;
        };
        match kind {
            Entries::Array => self.add_entry(field, None),
            Entries::Map => {
                let key = format!("{}{NEW_KEY_SUFFIX}", self.fields.fields()[field].key);
                self.typing = Some(PaneTyping {
                    key,
                    buffer: TextBox::new(),
                });
            }
        }
    }

    /// The key of the map the open key editor adds an entry to, or [`None`]
    /// when no editor is open or it is a field's.
    #[must_use]
    pub(in crate::lookout) fn new_entry_editor(&self) -> Option<&str> {
        let field = self.new_entry_field(&self.typing.as_ref()?.key)?;
        self.fields
            .fields()
            .get(field)
            .map(|field| field.key.as_str())
    }

    /// The entries field the key editor `key` is for, or [`None`] when `key`
    /// is a field's own.
    pub(super) fn new_entry_field(&self, key: &str) -> Option<usize> {
        let base = key.strip_suffix(NEW_KEY_SUFFIX)?;
        if self.fields.by_key(key).is_some() {
            return None;
        }
        self.entries_field(base)?;
        self.fields
            .fields()
            .iter()
            .position(|field| field.key == base)
    }

    /// `Enter` in the key editor of a map's `+ new` row: adds the entry the
    /// buffer names. An empty buffer abandons. A key the map already holds is
    /// refused with a sentence naming it, and the editor stays open.
    ///
    /// The key is any text: one with a dot or a space is written quoted.
    pub(super) fn apply_new_key(
        &mut self,
        field: usize,
        key: String,
        buffer: TextBox,
    ) -> Option<Refusal> {
        let name = buffer.as_str();
        if name.is_empty() {
            return None;
        }
        let held = self.fields.fields().get(field).map(|f| f.key.clone())?;
        let taken = matches!(
            self.entries_value(&held),
            Some(Value::Object(map)) if map.contains_key(name)
        );
        if taken {
            let refusal = Refusal {
                text: format!(
                    "{held} already has an entry {name:?}: type another key, or none to give up"
                ),
            };
            self.typing = Some(PaneTyping { key, buffer });
            return Some(refusal);
        }
        self.add_entry(field, Some(name.to_owned()));
        None
    }

    /// Files the entries field `field`'s whole value with one more entry,
    /// holding the schema's defaults: the next index of an array, or `key` in
    /// a map. Opens the entry and puts the cursor on its first field.
    fn add_entry(&mut self, field: usize, key: Option<String>) {
        let Some(held) = self.fields.fields().get(field).map(|f| f.key.clone()) else {
            return;
        };
        let (Some(mut whole), Some(item)) =
            (self.entries_value(&held), self.entries.defaults(&held))
        else {
            return;
        };
        let step = match (&mut whole, key) {
            (Value::Array(items), None) => {
                items.push(item);
                PathStep::Index(items.len() - 1)
            }
            (Value::Object(map), Some(key)) => {
                map.insert(key.clone(), item);
                PathStep::Key(key)
            }
            _ => return,
        };
        self.file_whole(&held, whole, None);
        let Some(entry) = self.entries.entry_at(field, &step) else {
            return;
        };
        let Some(EntryBody::Node { path, children, .. }) =
            self.entries.row(entry).map(|row| row.body.clone())
        else {
            return;
        };
        self.open.insert(path);
        let first = children.iter().copied().find(|&child| {
            matches!(
                self.entries.row(child).map(|row| &row.body),
                Some(EntryBody::Leaf { .. })
            )
        });
        self.move_to_row(PaneRow::Entry(first.unwrap_or(entry)));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::fixtures::{defaulted_pane, defaulted_schema, entries_schema, filed};
    use super::super::{ConfigPane, EntryBody, PaneRow, SectionWrite};

    /// The pane with `key`'s entries open and the cursor on its `+ new`.
    fn on_new(mut pane: ConfigPane, key: &str) -> ConfigPane {
        pane.move_to_key(key);
        pane.open_table();
        let at = pane
            .rows()
            .iter()
            .position(|row| {
                matches!(*row, PaneRow::Entry(index) if pane.entries.new_field(index).is_some()
                    && pane.entries.row(index).is_some_and(|r| r.parent == pane_field(&pane, key)))
            })
            .expect("the entries row has a + new");
        let len = pane.rows().len();
        pane.view.move_to(at, len);
        pane
    }

    fn pane_field(pane: &ConfigPane, key: &str) -> PaneRow {
        PaneRow::Field(
            pane.fields()
                .fields()
                .iter()
                .position(|field| field.key == key)
                .expect("the field exists"),
        )
    }

    /// The key of the field row under the cursor.
    fn cursor_key(pane: &ConfigPane) -> Option<String> {
        let PaneRow::Entry(index) = pane.cursor()? else {
            return None;
        };
        match &pane.entry_row(index)?.body {
            EntryBody::Leaf { field, .. } => Some(field.key.clone()),
            EntryBody::Node { .. } | EntryBody::New { .. } => None,
        }
    }

    fn type_out(pane: &mut ConfigPane, text: &str) {
        for typed in text.chars() {
            pane.type_char(typed);
        }
    }

    /// The text the section is written as, or the reason it was not.
    fn written(pane: &ConfigPane) -> String {
        match pane.edited_section_with(pane.edits()) {
            SectionWrite::Written(text) => text.as_str().to_owned(),
            other => panic!("expected a write, got {other:?}"),
        }
    }

    #[test]
    fn plus_new_is_the_last_child_of_an_open_entries_row_and_of_nothing_closed() {
        let mut pane = defaulted_pane();
        pane.move_to_key("clients");
        let closed = pane.rows().len();
        pane.open_table();
        let rows = pane.rows();
        assert_eq!(rows.len(), closed + 3 + 1, "three clients and + new");
        let Some(PaneRow::Entry(last)) = rows
            .iter()
            .copied()
            .rfind(|row| matches!(row, PaneRow::Entry(_)))
        else {
            panic!("an entry row: {rows:?}");
        };
        assert_eq!(pane.entry_row(last).expect("a row").label, "+ new");
        assert_eq!(
            pane.tree_row(PaneRow::Entry(last)).expect("drawn").open,
            None
        );
    }

    #[test]
    fn a_dogs_table_on_a_sheep_has_no_plus_new() {
        let table = json!({ "clients": [{ "name": "alpha" }] });
        let mut pane = ConfigPane::sheep_dog(
            "web".into(),
            "paddock".into(),
            &entries_schema(),
            table.as_object().cloned().expect("a table"),
        );
        pane.move_to_key("clients");
        pane.open_table();
        assert!(!pane.cursor_on_new());
        assert!(
            pane.rows().iter().all(|row| !matches!(
                *row,
                PaneRow::Entry(index) if pane.entries.new_field(index).is_some()
            )),
            "{:?}",
            pane.rows()
        );
    }

    #[test]
    fn enter_on_plus_new_in_an_array_appends_an_entry_of_only_its_defaults() {
        let mut pane = on_new(defaulted_pane(), "clients");
        assert!(pane.cursor_on_new());
        pane.begin_new_entry();
        assert!(pane.typing().is_none(), "an array asks for nothing");
        let whole = filed(&pane, "clients").expect("the array is filed");
        let items = whole.as_array().expect("an array");
        assert_eq!(items.len(), 4);
        assert_eq!(
            items[3],
            json!({ "enabled": true, "key": "change-me", "limits": { "rate": 5 } }),
            "defaults only: no name, and no burst in limits"
        );
        assert_eq!(
            items[0]["name"],
            json!("alpha"),
            "the others are as they were"
        );
        assert_eq!(pane.edits().len(), 1);
    }

    #[test]
    fn an_entry_whose_schema_declares_no_default_is_an_empty_table() {
        let mut pane = on_new(super::super::fixtures::entries_pane(), "clients");
        pane.begin_new_entry();
        let whole = filed(&pane, "clients").expect("the array is filed");
        assert_eq!(whole[3], json!({}));
    }

    #[test]
    fn a_null_default_is_no_default_so_the_new_entry_still_reads_back() {
        let mut schema = defaulted_schema();
        schema["$defs"]["Client"]["properties"]["name"]["default"] = Value::Null;
        schema["$defs"]["Client"]["properties"]["limits"]["properties"]["burst"]["default"] =
            Value::Null;
        let pane = ConfigPane::dog(
            "paddock".into(),
            None,
            schema,
            super::super::fixtures::entries_section(),
            Vec::new(),
        );
        let mut pane = on_new(pane, "clients");
        pane.begin_new_entry();
        let whole = filed(&pane, "clients").expect("the array is filed");
        assert_eq!(
            whole[3],
            json!({ "enabled": true, "key": "change-me", "limits": { "rate": 5 } }),
            "no null leaf for name or burst"
        );
        assert!(written(&pane).contains("[[clients]]\nenabled = true"));
    }

    #[test]
    fn the_new_entry_opens_and_the_cursor_lands_on_its_first_field() {
        let mut pane = on_new(defaulted_pane(), "clients");
        pane.begin_new_entry();
        assert!(pane.open_tables().contains("clients[3]"));
        assert_eq!(cursor_key(&pane).as_deref(), Some("clients[3].enabled"));
    }

    #[test]
    fn a_secret_default_never_reaches_the_panes_debug_output() {
        let mut pane = on_new(defaulted_pane(), "clients");
        pane.begin_new_entry();
        let debug = format!("{pane:?}");
        assert!(!debug.contains("change-me"), "{debug}");
    }

    #[test]
    fn the_entry_lands_after_the_last_client_and_before_the_next_section() {
        let mut pane = on_new(defaulted_pane(), "clients");
        pane.begin_new_entry();
        let out = written(&pane);
        let tail =
            "[[clients]]\nenabled = true\nkey = \"change-me\"\n\n[clients.limits]\nrate = 5\n";
        assert!(out.contains(tail), "{out}");
        let at = out.find(tail).expect("present");
        assert!(
            out.find("hunter4").expect("the last client") < at
                && at < out.find("[models.small]").expect("the next section"),
            "{out}"
        );
    }

    #[test]
    fn an_array_the_section_lacks_is_created() {
        let mut pane = ConfigPane::dog(
            "paddock".into(),
            None,
            defaulted_schema(),
            "retries = 3\n".to_owned(),
            Vec::new(),
        );
        pane = on_new(pane, "clients");
        pane.begin_new_entry();
        assert_eq!(
            written(&pane),
            concat!(
                "retries = 3\n",
                "\n",
                "[[clients]]\nenabled = true\nkey = \"change-me\"\n",
                "\n",
                "[clients.limits]\nrate = 5\n",
            )
        );
    }

    #[test]
    fn enter_on_plus_new_in_a_map_opens_the_key_editor_and_files_nothing_yet() {
        let mut pane = on_new(defaulted_pane(), "models");
        pane.begin_new_entry();
        let typing = pane.typing().expect("the key editor opens");
        assert_eq!(typing.buffer.as_str(), "");
        assert_eq!(pane.new_entry_editor(), Some("models"));
        assert!(pane.edits().is_empty());
        assert_eq!(pane.cursor_kind(), None, "the cursor is on no field");
    }

    #[test]
    fn a_key_typed_into_the_editor_adds_that_model_with_its_defaults() {
        let mut pane = on_new(defaulted_pane(), "models");
        pane.begin_new_entry();
        type_out(&mut pane, "gpt-5");
        assert_eq!(pane.apply_typing(), None);
        assert!(pane.typing().is_none(), "the editor closes");
        let whole = filed(&pane, "models").expect("the map is filed");
        assert_eq!(whole["gpt-5"], json!({ "context": 4096 }));
        assert_eq!(whole["large"], json!({ "provider": "remote" }));
        assert!(pane.open_tables().contains("models[\"gpt-5\"]"));
        assert_eq!(
            cursor_key(&pane).as_deref(),
            Some("models[\"gpt-5\"].context")
        );
    }

    #[test]
    fn a_key_the_map_already_holds_is_refused_by_name_and_the_editor_stays_open() {
        let mut pane = on_new(defaulted_pane(), "models");
        pane.begin_new_entry();
        type_out(&mut pane, "large");
        let refused = pane.apply_typing().expect("a duplicate is refused");
        assert!(refused.text.contains("\"large\""), "{}", refused.text);
        let typing = pane.typing().expect("the editor stays open");
        assert_eq!(typing.buffer.as_str(), "large");
        assert!(pane.edits().is_empty());
    }

    #[test]
    fn a_key_added_earlier_in_the_session_is_a_duplicate_too() {
        let mut pane = on_new(defaulted_pane(), "models");
        pane.begin_new_entry();
        type_out(&mut pane, "gpt-5");
        assert_eq!(pane.apply_typing(), None);
        let mut pane = on_new(pane, "models");
        pane.begin_new_entry();
        type_out(&mut pane, "gpt-5");
        let refused = pane.apply_typing().expect("refused");
        assert!(refused.text.contains("gpt-5"), "{}", refused.text);
    }

    #[test]
    fn an_empty_key_abandons_the_add() {
        let mut pane = on_new(defaulted_pane(), "models");
        pane.begin_new_entry();
        assert_eq!(pane.apply_typing(), None);
        assert!(pane.typing().is_none());
        assert!(pane.edits().is_empty());
        assert!(pane.cursor_on_new(), "the cursor stays on + new");
    }

    #[test]
    fn a_key_with_a_dot_or_a_space_is_written_quoted() {
        for (key, header) in [("a.b", "[models.\"a.b\"]"), ("a b", "[models.\"a b\"]")] {
            let mut pane = on_new(defaulted_pane(), "models");
            pane.begin_new_entry();
            type_out(&mut pane, key);
            assert_eq!(pane.apply_typing(), None, "{key}");
            let out = written(&pane);
            assert!(
                out.contains(&format!("{header}\ncontext = 4096\n")),
                "{out}"
            );
        }
    }

    #[test]
    fn a_model_lands_after_the_last_model() {
        let mut pane = on_new(defaulted_pane(), "models");
        pane.begin_new_entry();
        type_out(&mut pane, "gpt-5");
        assert_eq!(pane.apply_typing(), None);
        assert_eq!(
            written(&pane),
            super::super::fixtures::entries_section() + "\n[models.gpt-5]\ncontext = 4096\n"
        );
    }

    #[test]
    fn adding_then_editing_a_field_of_the_new_entry_is_one_write_with_both() {
        let mut pane = on_new(defaulted_pane(), "clients");
        pane.begin_new_entry();
        pane.move_to_key("clients[3].name");
        pane.begin_typing();
        type_out(&mut pane, "delta");
        assert_eq!(pane.apply_typing(), None);
        assert_eq!(pane.edits().len(), 2);
        let out = written(&pane);
        let want = concat!(
            "[[clients]]\nkey = \"hunter4\"\n",
            "\n[[clients]]\nenabled = true\nkey = \"change-me\"\nname = \"delta\"\n",
            "\n[clients.limits]\nrate = 5\n",
        );
        assert!(out.contains(want), "{out}");
        assert_eq!(out.matches("[[clients]]").count(), 4, "{out}");
    }

    #[test]
    fn adding_then_removing_the_same_entry_files_nothing() {
        for (key, section) in [
            ("clients", "retries = 3\n"),
            ("models", "retries = 3\n"),
            ("clients", "clients = []\n"),
        ] {
            let pane = ConfigPane::dog(
                "paddock".into(),
                None,
                defaulted_schema(),
                section.to_owned(),
                Vec::new(),
            );
            let mut pane = on_new(pane, key);
            pane.begin_new_entry();
            if pane.typing().is_some() {
                type_out(&mut pane, "x");
                assert_eq!(pane.apply_typing(), None);
            }
            assert_eq!(pane.edits().len(), 1, "{key} added");
            let Some(PaneRow::Entry(_)) = pane.cursor() else {
                panic!("on the new entry's field");
            };
            // Up to the entry's own row, then `d`.
            while cursor_key(&pane).is_some() {
                pane.move_by(-1);
            }
            pane.file_entry_removal();
            assert!(pane.edits().is_empty(), "{key}: {:?}", pane.edits().len());
            assert_eq!(
                pane.edited_section_with(pane.edits()),
                SectionWrite::Nothing,
                "{key}"
            );
        }
    }

    #[test]
    fn an_add_on_an_array_with_filed_edits_keeps_them() {
        let mut pane = defaulted_pane();
        pane.move_to_key("clients[0].name");
        pane.begin_typing();
        for _ in 0..5 {
            pane.type_backspace();
        }
        type_out(&mut pane, "zed");
        assert_eq!(pane.apply_typing(), None);
        pane.move_to_key("retries");
        pane.begin_typing();
        type_out(&mut pane, "0");
        assert_eq!(pane.apply_typing(), None);
        let mut pane = on_new(pane, "clients");
        pane.begin_new_entry();
        let whole = filed(&pane, "clients").expect("filed");
        assert_eq!(whole[0]["name"], json!("zed"), "the edit is folded in");
        assert_eq!(whole.as_array().map(Vec::len), Some(4));
        assert_eq!(
            filed(&pane, "retries"),
            Some(json!(30)),
            "another field's edit stays"
        );
        assert_eq!(pane.edits().len(), 2);
        let out = written(&pane);
        assert!(out.contains("name = \"zed\""), "{out}");
        assert!(out.contains("retries = 30"), "{out}");
    }

    #[test]
    fn an_edit_inside_the_new_entry_is_written_with_it() {
        let mut pane = on_new(defaulted_pane(), "clients");
        pane.begin_new_entry();
        pane.move_to_key("clients[3].name");
        pane.begin_typing();
        type_out(&mut pane, "delta");
        assert_eq!(pane.apply_typing(), None);
        assert_eq!(pane.edits().len(), 2);
        let edits = pane.close();
        assert_eq!(
            edits.len(),
            1,
            "closing folds the leaf into the whole value"
        );
        let out = match pane.edited_section_with(&edits) {
            SectionWrite::Written(text) => text.as_str().to_owned(),
            other => panic!("expected a write, got {other:?}"),
        };
        assert!(
            out.contains("[[clients]]\nenabled = true\nkey = \"change-me\"\nname = \"delta\"\n"),
            "{out}"
        );
    }

    #[test]
    fn undo_gives_the_array_back_and_the_write_is_nothing() {
        let mut pane = on_new(defaulted_pane(), "clients");
        pane.begin_new_entry();
        assert!(pane.undo_edit().is_some());
        assert!(pane.edits().is_empty());
        assert_eq!(
            pane.edited_section_with(pane.edits()),
            SectionWrite::Nothing
        );
        assert!(!pane.open_tables().contains("clients[3]"));
    }

    #[test]
    fn plus_new_does_nothing_on_any_other_row() {
        let mut pane = defaulted_pane();
        pane.move_to_key("retries");
        pane.begin_new_entry();
        assert!(pane.typing().is_none() && pane.edits().is_empty());
        let value: Option<Value> = filed(&pane, "clients");
        assert_eq!(value, None);
    }
}
