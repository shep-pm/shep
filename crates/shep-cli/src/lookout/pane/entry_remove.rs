//! Removing an entry from an array or map of tables, on the pane alone, and
//! the whole-value filing that adding one shares.
//!
//! A removal files the entries row's whole new value, the way the list
//! sub-screen files its whole array. The edits filed under the row's entries
//! are folded into that value first and dropped from the set, so none is
//! left aimed at an entry that has since moved. The rows are then rebuilt
//! over the filed value, which is what the cursor lands in.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value};

use shep_core::config::ApplyGroup;

use super::super::edits::EditKey;
use super::super::field::{Entries, FieldKind};
use super::{ConfigPane, PaneEdit, PaneRow, PaneTarget, PathStep};

/// What filing one whole value for an entries field took out of the pane,
/// kept so `u` can give it back: the edits it folded into the whole value,
/// the whole value it replaced, and which entries were open. An entry's
/// removal and an entry's addition both file one.
///
/// `Debug` is derived, safe because [`PaneEdit`]'s own `Debug` withholds its
/// values (IR-41).
#[derive(Debug, Clone)]
pub(super) struct Reshape {
    /// The entries field the whole value was filed for.
    key: String,
    /// The whole value that was filed for it before, if there was one.
    prior: Option<(PaneEdit, Option<ApplyGroup>)>,
    /// The edits filed under its entries, folded into the new whole value.
    folded: Vec<(PaneEdit, Option<ApplyGroup>)>,
    /// The open paths under it.
    open: BTreeSet<String>,
}

impl ConfigPane {
    /// Whether the cursor is on the row of an entry of an array or map of
    /// tables that `d` removes: on a dog's own pane, and not a table inside
    /// the entry or a field of it.
    #[must_use]
    pub(in crate::lookout) fn cursor_on_entry(&self) -> bool {
        matches!(self.target, PaneTarget::Dog { .. })
            && matches!(
                self.cursor(),
                Some(PaneRow::Entry(row)) if self.entries.entry_place(row).is_some()
            )
    }

    /// `d` on an entry's row: files the entries row's whole value without
    /// that entry, and puts the cursor on the entry that follows it, the one
    /// before when it was the last, and the entries row when none are left.
    ///
    /// Does nothing on any other row, and on a dog's table on a sheep, whose
    /// entries are read-only.
    pub(in crate::lookout) fn file_entry_removal(&mut self) {
        if !matches!(self.target, PaneTarget::Dog { .. }) {
            return;
        }
        let Some(PaneRow::Entry(row)) = self.cursor() else {
            return;
        };
        let Some((field, step)) = self.entries.entry_place(row) else {
            return;
        };
        let (step, Some(key)) = (
            step.clone(),
            self.fields
                .fields()
                .get(field)
                .map(|field| field.key.clone()),
        ) else {
            return;
        };
        let Some(place) = self
            .entries
            .roots(field)
            .and_then(|roots| roots.iter().position(|&root| root == row))
        else {
            return;
        };
        let Some(mut whole) = self.entries_value(&key) else {
            return;
        };
        if !take_entry(&mut whole, &step) {
            return;
        }
        self.file_whole(&key, whole, Some(&step));
        let left = self.entries.roots(field).map_or(0, <[usize]>::len);
        let land = match left {
            0 => PaneRow::Field(field),
            left => self
                .entries
                .roots(field)
                .and_then(|roots| roots.get(place.min(left - 1)))
                .map_or(PaneRow::Field(field), |&root| PaneRow::Entry(root)),
        };
        self.move_to_row(land);
    }

    /// Files `whole` as the new value of the entries field `key`, and
    /// rebuilds the rows over it. `gone` is the array entry `whole` lacks,
    /// whose open state goes with it. Edits filed under the field's entries
    /// are folded into `whole` by the caller, and what that dropped is kept
    /// for [`Self::restore_reshape`].
    ///
    /// A `whole` equal to what the target holds leaves `u` nothing to undo.
    pub(super) fn file_whole(&mut self, key: &str, whole: Value, gone: Option<&PathStep>) {
        let head = format!("{key}[");
        let reshape = Reshape {
            prior: self
                .edits
                .get(&EditKey::Field(key.to_owned()))
                .map(|edit| (edit.edit().clone(), edit.impact())),
            folded: self.drop_edits_under(key),
            open: self
                .open
                .iter()
                .filter(|path| path.starts_with(&head))
                .cloned()
                .collect(),
            key: key.to_owned(),
        };
        if let Some(gone) = gone {
            self.shift_open_entries(key, gone);
        }
        self.file_field(key.to_owned(), whole);
        if self.edits.get(&EditKey::Field(key.to_owned())).is_some() {
            Arc::make_mut(&mut self.reshapes).push(reshape);
        } else {
            Arc::make_mut(&mut self.reshapes).retain(|held| held.key != key);
        }
        self.resync_entries();
        self.retain_known_open();
    }

    /// [`Self::adopt_edits`] from `previous`, less every edit filed under an
    /// array entry this pane no longer holds at the same position, and every
    /// whole value filed for an array or map of tables the new section no
    /// longer matches. That is how many it dropped.
    ///
    /// An array entry is named by its place in the array, so after another
    /// writer inserts or removes one, the same key names a different entry.
    pub(in crate::lookout) fn adopt_edits_from(&mut self, previous: &Self) -> usize {
        let mut kept = previous.edits.clone();
        let mut dropped = 0;
        for (key, _) in previous.edits.iter() {
            let EditKey::Field(name) = key else {
                continue;
            };
            if previous.entries_field(name).is_some()
                && previous.values.get(name) != self.values.get(name)
            {
                kept.remove(key);
                dropped += 1;
            }
        }
        self.edits = kept;
        self.resync_entries();
        for (key, _) in previous.edits.iter() {
            let EditKey::Field(name) = key else {
                continue;
            };
            if self.edits.get(key).is_some()
                && !previous.entries.names_same_entry(name, &self.entries)
            {
                self.edits.remove(key);
                dropped += 1;
            }
        }
        dropped
    }

    /// Folds every edit filed under an entry of an entries field that also
    /// has a whole value filed into that value, so a close writes the field
    /// once. An entry the whole value adds has no place in the stored rows, so
    /// its leaf edits could not be written on their own after the close
    /// rebuilds them.
    pub(super) fn fold_entry_edits(&mut self) {
        let keys: Vec<String> = self
            .fields
            .fields()
            .iter()
            .filter(|field| matches!(field.kind, FieldKind::Entries(_)))
            .map(|field| field.key.clone())
            .filter(|key| self.edits.get(&EditKey::Field(key.clone())).is_some())
            .collect();
        for key in keys {
            let Some(whole) = self.entries_value(&key) else {
                continue;
            };
            if self.drop_edits_under(&key).is_empty() {
                continue;
            }
            self.file_field(key, whole);
        }
    }

    /// Rebuilds the rows under every entries field over what the pane holds
    /// for it: the filed whole value where there is one, else the target's.
    pub(super) fn resync_entries(&mut self) {
        if !matches!(self.target, PaneTarget::Dog { .. }) {
            return;
        }
        let mut values = self.values.clone();
        for field in self.fields.fields() {
            if !matches!(field.kind, FieldKind::Entries(_)) {
                continue;
            }
            if let Some(PaneEdit::Set { value, .. }) = self
                .edits
                .get(&EditKey::Field(field.key.clone()))
                .map(super::super::edits::Edit::edit)
            {
                values.insert(field.key.clone(), value.as_value().clone());
            }
        }
        self.entries = Arc::new(
            self.entries
                .rebuilt(&self.fields, &self.section_paths, &values),
        );
    }

    /// The value of the entries field `key` as the operator has it: what is
    /// filed for it, else what the target holds, with every edit filed under
    /// one of its entries applied. [`None`] for a key that is not an entries
    /// field.
    pub(super) fn entries_value(&self, key: &str) -> Option<Value> {
        let (kind, path) = self.entries_field(key)?;
        let mut whole = match self
            .edits
            .get(&EditKey::Field(key.to_owned()))
            .map(super::super::edits::Edit::edit)
        {
            Some(PaneEdit::Set { value, .. }) => value.as_value().clone(),
            _ => self.values.get(key).cloned().unwrap_or(Value::Null),
        };
        if whole.is_null() {
            whole = match kind {
                Entries::Array => Value::Array(Vec::new()),
                Entries::Map => Value::Object(Map::new()),
            };
        }
        let under = format!("{key}[");
        for (edit_key, edit) in self.edits.iter() {
            let (EditKey::Field(name), PaneEdit::Set { value, .. }) = (edit_key, edit.edit())
            else {
                continue;
            };
            let Some(leaf) = self.entries.leaf(name).filter(|_| name.starts_with(&under)) else {
                continue;
            };
            let value = Some(value.as_value()).filter(|value| !value.is_null());
            put(
                &mut whole,
                leaf.at.get(path.len()..).unwrap_or_default(),
                value.cloned(),
            );
        }
        Some(whole)
    }

    /// Drops every edit filed under an entry of the field `key` and hands
    /// them back.
    fn drop_edits_under(&mut self, key: &str) -> Vec<(PaneEdit, Option<ApplyGroup>)> {
        let under = format!("{key}[");
        let filed: Vec<EditKey> = self
            .edits
            .iter()
            .filter(|(edit_key, _)| matches!(edit_key, EditKey::Field(name) if name.starts_with(&under)))
            .map(|(edit_key, _)| edit_key.clone())
            .collect();
        let mut folded = Vec::new();
        for edit_key in filed {
            if let Some(edit) = self.edits.get(&edit_key) {
                folded.push((edit.edit().clone(), edit.impact()));
            }
            self.edits.remove(&edit_key);
        }
        folded
    }

    /// Gives back what the newest whole value filed for the entries field
    /// `key` took out, after `u` has dropped the whole value it filed: the
    /// whole value it replaced, the edits it folded in, and the entries that
    /// were open. Does nothing when none is held for `key`.
    pub(super) fn restore_reshape(&mut self, key: &str) {
        let Some(at) = self.reshapes.iter().rposition(|held| held.key == key) else {
            return;
        };
        let reshape = Arc::make_mut(&mut self.reshapes).remove(at);
        for (edit, impact) in reshape.prior.into_iter().chain(reshape.folded) {
            self.edits.set(edit, impact);
        }
        let head = format!("{key}[");
        self.open.retain(|path| !path.starts_with(&head));
        self.open.extend(reshape.open);
    }

    /// Renames the open entries of the array `key` the way removing the entry
    /// at `gone` renames the entries themselves: the removed one's open
    /// state goes with it and every later one moves up an index.
    fn shift_open_entries(&mut self, key: &str, gone: &PathStep) {
        let PathStep::Index(gone) = gone else {
            return;
        };
        let head = format!("{key}[");
        let mut shifted = BTreeSet::new();
        for path in &self.open {
            let Some(rest) = path.strip_prefix(&head) else {
                shifted.insert(path.clone());
                continue;
            };
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            let (Ok(index), Some(tail)) = (digits.parse::<usize>(), rest.get(digits.len()..))
            else {
                shifted.insert(path.clone());
                continue;
            };
            match index.cmp(gone) {
                core::cmp::Ordering::Less => {
                    shifted.insert(path.clone());
                }
                core::cmp::Ordering::Equal => {}
                core::cmp::Ordering::Greater => {
                    shifted.insert(format!("{head}{}{tail}", index - 1));
                }
            }
        }
        self.open = shifted;
    }
}

/// Removes the entry `step` names from `whole`, and reports whether there
/// was one.
fn take_entry(whole: &mut Value, step: &PathStep) -> bool {
    match (whole, step) {
        (Value::Array(items), PathStep::Index(at)) if *at < items.len() => {
            items.remove(*at);
            true
        }
        (Value::Object(map), PathStep::Key(key)) => map.remove(key).is_some(),
        _ => false,
    }
}

/// Sets the leaf `at` names inside `whole` to `value`, or removes it for
/// [`None`], creating the tables on the way for a set. An entry or table
/// `whole` lacks is left lacking.
fn put(whole: &mut Value, at: &[PathStep], value: Option<Value>) {
    match (at, whole) {
        ([PathStep::Index(index), rest @ ..], Value::Array(items)) => {
            if let Some(item) = items.get_mut(*index) {
                put(item, rest, value);
            }
        }
        ([PathStep::Key(key)], Value::Object(map)) => match value {
            Some(value) => {
                map.insert(key.clone(), value);
            }
            None => {
                map.remove(key);
            }
        },
        ([PathStep::Key(key), rest @ ..], Value::Object(map)) => {
            if value.is_none() && !map.contains_key(key) {
                return;
            }
            put(
                map.entry(key.clone())
                    .or_insert_with(|| Value::Object(Map::new())),
                rest,
                value,
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::fixtures::{entries_pane, entries_schema, filed};
    use super::super::{ConfigPane, EntryBody, PaneRow, SectionWrite};

    /// The labels of the entries directly under `key`, in the order drawn.
    fn labels(pane: &ConfigPane, key: &str) -> Vec<String> {
        let field = pane
            .fields()
            .fields()
            .iter()
            .position(|field| field.key == key)
            .expect("the field exists");
        pane.entries
            .roots(field)
            .expect("an entries field")
            .iter()
            .filter_map(|&row| pane.entry_row(row))
            .map(|entry| entry.label.clone())
            .collect()
    }

    /// Walks the cursor onto the entry or table whose open-set key is `path`.
    fn select(pane: &mut ConfigPane, path: &str) {
        let at = pane
            .rows()
            .iter()
            .position(|row| match *row {
                PaneRow::Entry(index) => pane.entry_row(index).is_some_and(
                    |entry| matches!(&entry.body, EntryBody::Node { path: at, .. } if at == path),
                ),
                _ => false,
            })
            .expect("the row is drawn");
        let len = pane.rows().len();
        pane.view.move_to(at, len);
    }

    /// The paddock pane with its entries open, its cursor on `entry`.
    fn on(entry: &str) -> ConfigPane {
        let mut pane = entries_pane();
        pane.move_to_key(entry.split_once('[').expect("an entry key").0);
        pane.open_table();
        select(&mut pane, entry);
        pane
    }

    #[test]
    fn removing_an_entry_files_the_whole_array_without_it() {
        let mut pane = on("clients[0]");
        pane.file_entry_removal();
        let filed = filed(&pane, "clients").expect("the array is filed");
        let names: Vec<&Value> = filed
            .as_array()
            .expect("an array")
            .iter()
            .map(|client| &client["key"])
            .collect();
        assert_eq!(names, [&json!("hunter3"), &json!("hunter4")]);
        assert_eq!(pane.edits().len(), 1);
        assert_eq!(labels(&pane, "clients"), ["b-2", "#1"]);
        assert_eq!(pane.value("clients"), "3 entries");
        assert_eq!(pane.edited_value("clients").as_deref(), Some("2 entries"));
    }

    #[test]
    fn the_cursor_lands_on_the_next_entry_after_a_removal() {
        let mut pane = on("clients[1]");
        pane.file_entry_removal();
        let Some(PaneRow::Entry(row)) = pane.cursor() else {
            panic!("the cursor is on an entry: {:?}", pane.cursor());
        };
        assert_eq!(pane.entry_row(row).expect("a row").label, "#1");
    }

    #[test]
    fn the_cursor_lands_on_the_previous_entry_after_removing_the_last() {
        let mut pane = on("clients[2]");
        pane.file_entry_removal();
        let Some(PaneRow::Entry(row)) = pane.cursor() else {
            panic!("the cursor is on an entry: {:?}", pane.cursor());
        };
        assert_eq!(pane.entry_row(row).expect("a row").label, "b-2");
    }

    #[test]
    fn the_cursor_lands_on_the_entries_row_after_removing_the_only_entry() {
        let section = "[[clients]]\nname = \"alpha\"\n";
        let mut pane = ConfigPane::dog(
            "paddock".into(),
            None,
            entries_schema(),
            section.into(),
            Vec::new(),
        );
        pane.move_to_key("clients");
        pane.open_table();
        pane.move_by(1);
        pane.file_entry_removal();
        let Some(PaneRow::Field(field)) = pane.cursor() else {
            panic!("the cursor is on a field: {:?}", pane.cursor());
        };
        assert_eq!(pane.fields().fields()[field].key, "clients");
        assert_eq!(labels(&pane, "clients"), Vec::<String>::new());
    }

    #[test]
    fn removing_a_map_entry_files_the_map_without_that_key() {
        let mut pane = on("models[\"large\"]");
        pane.file_entry_removal();
        assert_eq!(
            filed(&pane, "models"),
            Some(json!({ "small": { "provider": "local", "context": 4096 } }))
        );
        assert_eq!(labels(&pane, "models"), ["small"]);
    }

    #[test]
    fn an_edit_in_a_later_entry_is_folded_into_the_removal_and_dropped_from_the_set() {
        let mut pane = entries_pane();
        pane.move_to_key("clients[2].name");
        pane.begin_typing();
        pane.type_char('z');
        assert_eq!(pane.apply_typing(), None);
        assert_eq!(pane.edits().len(), 1);
        select(&mut pane, "clients[0]");
        pane.file_entry_removal();
        assert_eq!(pane.edits().len(), 1, "only the array is filed now");
        let filed = filed(&pane, "clients").expect("the array is filed");
        assert_eq!(filed[1]["name"], json!("z"));
        let SectionWrite::Written(out) = pane.edited_section_with(pane.edits()) else {
            panic!("the removal lands");
        };
        assert!(out.as_str().contains("name = \"z\""), "{}", out.as_str());
        assert!(!out.as_str().contains("alpha"), "{}", out.as_str());
    }

    #[test]
    fn an_edit_after_a_removal_is_filed_under_the_place_the_entry_has_now() {
        let mut pane = on("clients[0]");
        pane.file_entry_removal();
        pane.move_to_key("clients[1].name");
        pane.begin_typing();
        pane.type_char('z');
        assert_eq!(pane.apply_typing(), None);
        assert_eq!(filed(&pane, "clients[1].name"), Some(json!("z")));
        assert_eq!(pane.edits().len(), 2);
        let SectionWrite::Written(out) = pane.edited_section_with(pane.edits()) else {
            panic!("both land");
        };
        assert_eq!(
            out.as_str().matches("[[clients]]").count(),
            2,
            "{}",
            out.as_str()
        );
        assert!(out.as_str().contains("name = \"z\""), "{}", out.as_str());
    }

    #[test]
    fn undo_gives_the_shepherds_own_array_back_and_the_write_is_nothing() {
        let mut pane = on("clients[0]");
        pane.file_entry_removal();
        assert!(pane.undo_edit().is_some());
        assert!(pane.edits().is_empty());
        assert_eq!(labels(&pane, "clients"), ["alpha", "b-2", "#2"]);
        assert_eq!(
            pane.edited_section_with(pane.edits()),
            SectionWrite::Nothing
        );
    }

    #[test]
    fn undo_gives_back_an_edit_the_removal_folded_in() {
        let mut pane = on("clients[0]");
        pane.move_to_key("clients[2].name");
        pane.begin_typing();
        pane.type_char('z');
        assert_eq!(pane.apply_typing(), None);
        select(&mut pane, "clients[0]");
        pane.file_entry_removal();
        assert_eq!(pane.edits().len(), 1, "the edit is folded in");
        assert!(pane.undo_edit().is_some());
        assert_eq!(pane.edits().len(), 1, "the edit is back");
        assert_eq!(filed(&pane, "clients[2].name"), Some(json!("z")));
        assert_eq!(labels(&pane, "clients"), ["alpha", "b-2", "#2"]);
        let SectionWrite::Written(out) = pane.edited_section_with(pane.edits()) else {
            panic!("the edit lands");
        };
        assert_eq!(out.as_str().matches("[[clients]]").count(), 3);
        assert!(out.as_str().contains("name = \"z\""), "{}", out.as_str());
    }

    #[test]
    fn undo_gives_back_the_whole_value_an_earlier_removal_filed() {
        let mut pane = on("clients[0]");
        pane.file_entry_removal();
        select(&mut pane, "clients[0]");
        pane.file_entry_removal();
        assert_eq!(labels(&pane, "clients"), ["#0"]);
        assert!(pane.undo_edit().is_some());
        assert_eq!(labels(&pane, "clients"), ["b-2", "#1"]);
        assert!(pane.undo_edit().is_some());
        assert_eq!(labels(&pane, "clients"), ["alpha", "b-2", "#2"]);
        assert!(pane.edits().is_empty());
    }

    #[test]
    fn undo_opens_the_entries_that_were_open_before_the_removal() {
        let mut pane = on("clients[0]");
        select(&mut pane, "clients[0]");
        pane.open_table();
        select(&mut pane, "clients[2]");
        pane.open_table();
        select(&mut pane, "clients[0]");
        pane.open_table();
        let before = pane.open.clone();
        pane.file_entry_removal();
        assert_ne!(pane.open, before, "the open set shifted with the entries");
        assert!(pane.undo_edit().is_some());
        assert_eq!(pane.open, before);
    }

    #[test]
    fn d_on_the_entries_row_and_on_a_table_in_an_entry_removes_nothing() {
        let mut pane = entries_pane();
        pane.move_to_key("clients");
        pane.file_entry_removal();
        assert!(pane.edits().is_empty());
        let mut pane = on("clients[0]");
        pane.open_table();
        select(&mut pane, "clients[0].limits");
        pane.file_entry_removal();
        assert!(pane.edits().is_empty());
    }

    #[test]
    fn a_dog_table_on_a_sheep_keeps_its_entries() {
        let table = json!({ "clients": [{ "name": "alpha" }] });
        let mut pane = ConfigPane::sheep_dog(
            "web".into(),
            "paddock".into(),
            &entries_schema(),
            table.as_object().cloned().expect("a table"),
        );
        pane.move_to_key("clients");
        pane.open_table();
        pane.move_by(1);
        assert!(matches!(
            pane.cursor().and_then(|row| match row {
                PaneRow::Entry(index) => pane.entry_row(index),
                _ => None,
            }),
            Some(entry) if matches!(entry.body, EntryBody::Node { entry: true, .. })
        ));
        pane.file_entry_removal();
        assert!(pane.edits().is_empty());
    }
}
