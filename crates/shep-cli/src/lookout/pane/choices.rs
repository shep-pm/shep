//! A list of choices, drawn as a checklist under its row.
//!
//! An array whose items are a closed set ([`ListItem::Choice`]) opens like a
//! table, to one row per name in the set. The row for a name toggles it, so
//! the list can never hold a duplicate or a name the schema does not know,
//! and no text is typed. An element the config already carried that is not
//! in the set is drawn after the names and stays unless toggled off.
//!
//! A toggle files the whole array through [`ConfigPane::file_field`], keeping
//! the stored order.

use serde_json::Value;

use super::super::edits::EditKey;
use super::super::field::{Field, FieldKind, ListItem};
use super::fields::render_json;
use super::{ConfigPane, PaneEdit, PaneRow};

/// What a list of choices says in its row when it holds nothing.
const NONE: &str = "(none)";

/// One row of a checklist, as it is drawn.
///
/// No `Debug` (IR-41): the label of a row outside the set is a stored
/// element, which the pane withholds.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::lookout) struct ChoiceRow {
    /// How many rows it is indented under, the same unit as a field's.
    pub(in crate::lookout) depth: usize,
    /// The name, or the element as it reads.
    pub(in crate::lookout) label: String,
    /// Whether the list holds it.
    pub(in crate::lookout) held: bool,
    /// Whether the schema's set has it. A row for an element the config
    /// carried that the set does not have is not.
    pub(in crate::lookout) known: bool,
}

impl ChoiceRow {
    /// The row as the checklist draws it: `[x] name` when held, `[ ] name`
    /// when not, and an element outside the set marked as such.
    #[must_use]
    pub(in crate::lookout) fn text(&self) -> String {
        let mark = if self.held { 'x' } else { ' ' };
        if self.known {
            format!("[{mark}] {}", self.label)
        } else {
            format!("[{mark}] {} (not in the set)", self.label)
        }
    }
}

/// One thing a checklist toggles.
struct Choice {
    value: Value,
    held: bool,
    known: bool,
}

impl ConfigPane {
    /// The names of field `field`'s set, when it is a list of choices.
    fn choice_names(&self, field: usize) -> Option<&[String]> {
        match &self.fields.fields().get(field)?.kind {
            FieldKind::List(ListItem::Choice(names)) => Some(names),
            _ => None,
        }
    }

    /// Whether field `field` is a list of choices drawn as a checklist: on
    /// a pane that nests, since a flat one has no rows to hang names from,
    /// and not a secret one, whose held names the rows would show.
    pub(super) fn is_checklist(&self, field: usize) -> bool {
        self.fields
            .fields()
            .get(field)
            .is_some_and(|field| self.drawn_as_checklist(field))
    }

    /// [`Self::is_checklist`] for the field itself.
    pub(super) fn drawn_as_checklist(&self, field: &Field) -> bool {
        self.is_tree()
            && !field.secret
            && matches!(field.kind, FieldKind::List(ListItem::Choice(_)))
    }

    /// `key`'s array as the operator has it: what is filed for the field,
    /// else what the target holds, as the elements themselves. A value that
    /// is not an array holds none.
    fn choice_elements(&self, key: &str) -> Vec<Value> {
        let filed = match self.edits.get(&EditKey::Field(key.to_owned())) {
            Some(entry) => match entry.edit() {
                PaneEdit::Set { value, .. } => Some(value.as_value()),
                PaneEdit::SetEnv { .. } => None,
            },
            None => None,
        };
        match filed.or_else(|| self.values.get(key)) {
            Some(Value::Array(elements)) => elements.clone(),
            _ => Vec::new(),
        }
    }

    /// Every name in field `field`'s set, then each distinct element the
    /// array holds that the set does not. Empty for any other field.
    fn choices(&self, field: usize) -> Vec<Choice> {
        let (Some(names), Some(key)) = (
            self.choice_names(field),
            self.fields.fields().get(field).map(|f| f.key.as_str()),
        ) else {
            return Vec::new();
        };
        let elements = self.choice_elements(key);
        let mut choices: Vec<Choice> = names
            .iter()
            .map(|name| Choice {
                value: Value::String(name.clone()),
                held: elements.iter().any(|held| held.as_str() == Some(name)),
                known: true,
            })
            .collect();
        for element in &elements {
            if !choices.iter().any(|choice| choice.value == *element) {
                choices.push(Choice {
                    value: element.clone(),
                    held: true,
                    known: false,
                });
            }
        }
        choices
    }

    /// The rows under field `field`, pushed onto `rows`, when it is a
    /// checklist drawn open.
    pub(super) fn push_choice_rows(&self, field: usize, rows: &mut Vec<PaneRow>) {
        if !self.is_checklist(field) || !self.is_open(PaneRow::Field(field)) {
            return;
        }
        rows.extend((0..self.choices(field).len()).map(|item| PaneRow::Choice { field, item }));
    }

    /// How `row` is drawn, or [`None`] for a row that is not one of a
    /// checklist.
    #[must_use]
    pub(in crate::lookout) fn choice_row(&self, row: PaneRow) -> Option<ChoiceRow> {
        let PaneRow::Choice { field, item } = row else {
            return None;
        };
        let choice = self.choices(field).into_iter().nth(item)?;
        let path = self.row_path(&self.fields.fields().get(field)?.key)?;
        Some(ChoiceRow {
            depth: path.len(),
            label: match &choice.value {
                Value::String(name) => name.clone(),
                other => render_json(other),
            },
            held: choice.held,
            known: choice.known,
        })
    }

    /// What a list of choices says in its row: the held elements, or
    /// `(none)`. [`None`] for any other field.
    pub(super) fn checklist_text(&self, key: &str, value: Option<&Value>) -> Option<String> {
        let index = self.fields.fields().iter().position(|f| f.key == key)?;
        self.choice_names(index)?;
        let held: Vec<String> = match value {
            Some(Value::Array(elements)) => elements
                .iter()
                .map(|element| match element {
                    Value::String(name) => name.clone(),
                    other => render_json(other),
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(if held.is_empty() {
            NONE.to_owned()
        } else {
            held.join(", ")
        })
    }

    /// Files the array with the name `item` of field `field` toggled: held,
    /// it leaves wherever it sat, and not held, it is appended. Does
    /// nothing for a locked field.
    pub(super) fn toggle_choice(&mut self, field: usize, item: usize) {
        let Some(key) = self.fields.fields().get(field).map(|f| f.key.clone()) else {
            return;
        };
        if self.lock(&key).is_some() {
            return;
        }
        let Some(choice) = self.choices(field).into_iter().nth(item) else {
            return;
        };
        let mut elements = self.choice_elements(&key);
        if choice.held {
            elements.retain(|element| *element != choice.value);
        } else {
            elements.push(choice.value);
        }
        // An empty list where the target holds none is no change, and
        // `file_field` only reads an absent key as equal to `null`.
        if elements.is_empty() && self.values.get(&key).is_none_or(Value::is_null) {
            self.edits.remove(&EditKey::Field(key));
        } else {
            self.file_field(key, Value::Array(elements));
        }
        // An element outside the set leaves its row when toggled off. The
        // cursor goes to the list's own row, not the name that slides under
        // it, which the next key would toggle.
        let row = if item < self.choices(field).len() {
            PaneRow::Choice { field, item }
        } else {
            PaneRow::Field(field)
        };
        let rows = self.rows();
        match rows.iter().position(|candidate| *candidate == row) {
            Some(at) => self.view.move_to(at, rows.len()),
            None => self.view.clamp(rows.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::super::Lock;
    use super::super::fixtures::filed;
    use super::*;

    /// A dog's per-sheep schema with one list of choices through a `$ref`,
    /// one inline, and a scalar after them.
    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "kinds": { "type": "array", "items": { "$ref": "#/$defs/Kind" } },
                "rulings": {
                    "type": "object",
                    "properties": {
                        "pm_alone": { "type": "array", "items": { "enum": ["x", "y"] } },
                    },
                },
                "name": { "type": "string" },
            },
            "$defs": { "Kind": { "enum": ["a", "b", "c"] } },
        })
    }

    fn pane_over(table: Value) -> ConfigPane {
        let Value::Object(table): Value = table else {
            panic!("a table is an object");
        };
        ConfigPane::sheep_dog("web".into(), "ruler".into(), &schema(), table)
    }

    fn go_to(pane: &mut ConfigPane, row: PaneRow) {
        let rows = pane.rows();
        let position = rows
            .iter()
            .position(|candidate| *candidate == row)
            .expect("the row is drawn");
        pane.view.move_to(position, rows.len());
    }

    fn go_to_kinds(pane: &mut ConfigPane) {
        let row = kinds_row(pane);
        go_to(pane, row);
    }

    fn kinds_row(pane: &ConfigPane) -> PaneRow {
        let index = pane
            .fields()
            .fields()
            .iter()
            .position(|f| f.key == "kinds")
            .expect("kinds");
        PaneRow::Field(index)
    }

    /// The checklist under `kinds`, as drawn.
    fn checklist(pane: &ConfigPane) -> Vec<String> {
        pane.rows()
            .into_iter()
            .filter_map(|row| pane.choice_row(row))
            .map(|row| row.text())
            .collect()
    }

    /// Opens `kinds` and walks onto the row for `name`.
    fn on(pane: &mut ConfigPane, name: &str) {
        go_to_kinds(pane);
        pane.open_table();
        let row = pane
            .rows()
            .into_iter()
            .find(|row| {
                pane.choice_row(*row)
                    .is_some_and(|choice| choice.label == name)
            })
            .expect(name);
        go_to(pane, row);
    }

    #[test]
    fn a_closed_list_draws_no_names_and_an_open_one_draws_every_name_in_the_set() {
        let mut pane = pane_over(json!({ "kinds": ["b"] }));
        assert_eq!(checklist(&pane), Vec::<String>::new());
        go_to_kinds(&mut pane);
        pane.open_table();
        assert_eq!(checklist(&pane), ["[ ] a", "[x] b", "[ ] c"]);
    }

    #[test]
    fn a_held_name_reads_x_and_with_every_name_held_every_row_does() {
        let mut pane = pane_over(json!({ "kinds": ["c", "a", "b"] }));
        go_to_kinds(&mut pane);
        pane.open_table();
        assert_eq!(checklist(&pane), ["[x] a", "[x] b", "[x] c"]);
    }

    #[test]
    fn the_row_lists_the_held_names_or_none() {
        let held = pane_over(json!({ "kinds": ["b", "a"] }));
        assert_eq!(held.value("kinds"), "b, a");
        let empty = pane_over(json!({ "kinds": [] }));
        assert_eq!(empty.value("kinds"), "(none)");
        let absent = pane_over(json!({}));
        assert_eq!(absent.value("kinds"), "(none)");
    }

    #[test]
    fn toggling_a_name_not_held_appends_it_after_the_stored_order() {
        let mut pane = pane_over(json!({ "kinds": ["b", "a"] }));
        on(&mut pane, "c");
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!(["b", "a", "c"])));
        assert_eq!(checklist(&pane), ["[x] a", "[x] b", "[x] c"]);
        assert_eq!(pane.value("kinds"), "b, a");
        assert_eq!(pane.edited_value("kinds").as_deref(), Some("b, a, c"));
    }

    #[test]
    fn toggling_a_held_name_removes_it_from_where_it_was() {
        let mut pane = pane_over(json!({ "kinds": ["c", "a", "b"] }));
        on(&mut pane, "a");
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!(["c", "b"])));
        assert_eq!(checklist(&pane), ["[ ] a", "[x] b", "[x] c"]);
    }

    #[test]
    fn toggling_a_name_back_to_the_stored_list_files_nothing() {
        let mut pane = pane_over(json!({ "kinds": ["b", "a"] }));
        on(&mut pane, "c");
        pane.cycle();
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), None);
        assert!(pane.edits().is_empty());
    }

    #[test]
    fn toggling_the_last_name_off_files_an_empty_list() {
        let mut pane = pane_over(json!({ "kinds": ["b"] }));
        on(&mut pane, "b");
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!([])));
    }

    #[test]
    fn toggling_a_name_on_a_list_the_table_does_not_carry_files_a_one_name_list() {
        let mut pane = pane_over(json!({}));
        on(&mut pane, "b");
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!(["b"])));
    }

    #[test]
    fn a_stored_element_outside_the_set_is_drawn_below_the_names_and_survives_a_toggle() {
        let mut pane = pane_over(json!({ "kinds": ["legacy", "a"] }));
        go_to_kinds(&mut pane);
        pane.open_table();
        assert_eq!(
            checklist(&pane),
            ["[x] a", "[ ] b", "[ ] c", "[x] legacy (not in the set)"]
        );
        pane.close_table();
        pane.open_table();
        assert!(pane.edits().is_empty(), "opening and closing files nothing");

        on(&mut pane, "b");
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!(["legacy", "a", "b"])));
    }

    #[test]
    fn toggling_an_element_outside_the_set_off_removes_it() {
        let mut pane = pane_over(json!({ "kinds": ["legacy", "a"] }));
        on(&mut pane, "legacy");
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!(["a"])));
        assert_eq!(checklist(&pane), ["[x] a", "[ ] b", "[ ] c"]);
        assert_eq!(
            pane.cursor(),
            Some(kinds_row(&pane)),
            "the cursor does not slide onto a name the next key would toggle"
        );
    }

    #[test]
    fn toggling_a_name_on_and_off_in_a_list_the_table_does_not_carry_files_nothing() {
        for table in [json!({}), json!({ "kinds": null })] {
            let mut pane = pane_over(table);
            on(&mut pane, "b");
            pane.cycle();
            assert_eq!(filed(&pane, "kinds"), Some(json!(["b"])));
            pane.cycle();
            assert!(
                pane.edits().is_empty(),
                "an empty list where the key was absent is not an edit"
            );
        }
    }

    #[test]
    fn a_secret_list_of_choices_stays_locked_and_draws_no_names() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tokens": {
                    "type": "array",
                    "items": { "enum": ["a", "b"] },
                    "x-shep-secret": true,
                },
            },
        });
        let table = json!({ "tokens": ["a", "sk-live-51Hx9Qa"] });
        let Value::Object(table) = table else {
            panic!("a table is an object");
        };
        let mut pane = ConfigPane::sheep_dog("web".into(), "ruler".into(), &schema, table);
        pane.open_table();
        assert!(
            pane.rows()
                .iter()
                .all(|row| pane.choice_row(*row).is_none()),
            "no name is drawn"
        );
        assert_eq!(pane.lock("tokens"), Some(Lock::NoWidget));
        pane.open_list();
        assert!(pane.list().is_none(), "no sub-screen over a secret list");
    }

    #[test]
    fn an_element_that_is_not_a_string_is_kept_as_it_was_stored() {
        let mut pane = pane_over(json!({ "kinds": [7, "a"] }));
        on(&mut pane, "b");
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!([7, "a", "b"])));
    }

    #[test]
    fn a_name_stored_twice_reads_held_once_and_toggles_off_whole() {
        let mut pane = pane_over(json!({ "kinds": ["a", "b", "a"] }));
        on(&mut pane, "a");
        assert_eq!(checklist(&pane), ["[x] a", "[x] b", "[ ] c"]);
        pane.cycle();
        assert_eq!(filed(&pane, "kinds"), Some(json!(["b"])));
    }

    #[test]
    fn the_checklist_sits_under_its_row_at_the_depth_below_it() {
        let mut pane = pane_over(json!({ "rulings": { "pm_alone": ["y"] } }));
        let row = PaneRow::Field(
            pane.fields()
                .fields()
                .iter()
                .position(|f| f.key == "rulings.pm_alone")
                .expect("nested"),
        );
        go_to(&mut pane, row);
        pane.open_table();
        let rows = pane.rows();
        let at = rows.iter().position(|candidate| *candidate == row).unwrap();
        let tree = |index: usize| pane.tree_row(rows[index]).expect("a tree row");
        assert_eq!(tree(at).depth, 1);
        assert_eq!(tree(at).open, Some(true));
        assert_eq!(tree(at + 1).depth, 2);
        assert_eq!(tree(at + 1).open, None);
        assert_eq!(
            pane.choice_row(rows[at + 1])
                .map(|row| row.text())
                .as_deref(),
            Some("[ ] x")
        );
        assert_eq!(
            pane.choice_row(rows[at + 2])
                .map(|row| row.text())
                .as_deref(),
            Some("[x] y")
        );
    }

    #[test]
    fn left_on_a_name_moves_to_the_row_that_holds_it() {
        let mut pane = pane_over(json!({ "kinds": ["b"] }));
        on(&mut pane, "c");
        pane.close_table();
        assert_eq!(pane.cursor(), Some(kinds_row(&pane)));
        assert_eq!(checklist(&pane).len(), 3, "the list stays open");
        pane.close_table();
        assert_eq!(checklist(&pane).len(), 0);
    }

    #[test]
    fn a_refresh_keeps_the_list_open() {
        let mut pane = pane_over(json!({ "kinds": ["b"] }));
        go_to_kinds(&mut pane);
        pane.open_table();
        let table: Map<String, Value> =
            json!({ "kinds": ["b", "c"] }).as_object().cloned().unwrap();
        pane.adopt_table(table);
        assert_eq!(checklist(&pane), ["[ ] a", "[x] b", "[x] c"]);
    }

    #[test]
    fn the_filed_list_is_what_a_reopened_checklist_draws() {
        let mut pane = pane_over(json!({ "kinds": ["b"] }));
        on(&mut pane, "a");
        pane.cycle();
        go_to_kinds(&mut pane);
        pane.close_table();
        pane.open_table();
        assert_eq!(checklist(&pane), ["[x] a", "[x] b", "[ ] c"]);
    }

    #[test]
    fn the_edited_table_differs_from_the_stored_one_in_that_list_only() {
        let table =
            json!({ "kinds": ["b", "a"], "name": "keep", "rulings": { "pm_alone": ["x"] } });
        let mut pane = pane_over(table.clone());
        on(&mut pane, "c");
        pane.cycle();
        let written = pane.edited_table_with(pane.edits());
        let mut want = table.as_object().cloned().unwrap();
        want.insert("kinds".into(), json!(["b", "a", "c"]));
        assert_eq!(written, want);
    }
}
