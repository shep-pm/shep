//! A pane over one dog's `[app.dogs.<dog>]` table on one sheep.
//!
//! The rows come from the dog's per-sheep schema, flattened so a nested
//! table's fields are dotted rows of their own. The write carries the whole
//! table: `Request::SetSheepDogSettings` replaces it, so every key no edit
//! touched, the schema's or not, goes back exactly as it came.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::super::edits::{EditKey, Edits};
use super::super::field::{Flattened, flatten_values, flattened};
use super::super::viewport::Viewport;
use super::{ConfigPane, PaneEdit, PaneTarget};

/// The table a [`ConfigPane::sheep_dog`] pane edits, and where each of its
/// dotted rows lives in it.
///
/// `Debug` is manual and redacted (IR-41): the table can hold a credential,
/// so it prints how many rows and keys there are and nothing else.
#[derive(Clone)]
pub(in crate::lookout) struct SheepDogTable {
    /// Dotted row key to the real key path. A row key is never split on its
    /// dots, since a property can be named `a.b`.
    paths: BTreeMap<String, Vec<String>>,
    /// The table as the shepherd holds it, keys the schema does not name
    /// included.
    table: Map<String, Value>,
}

impl core::fmt::Debug for SheepDogTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "SheepDogTable {{ rows: {}, keys: {} }}",
            self.paths.len(),
            self.table.len()
        )
    }
}

impl ConfigPane {
    /// A pane over `dog`'s table on `sheep`.
    ///
    /// `schema` is the dog's `x-shep-sheep` answer with the root's `$defs`
    /// attached; `table` is what the sheep carries for it, empty for none.
    /// Flat, in key order, with no group headers, like [`Self::dog`].
    #[must_use]
    pub fn sheep_dog(
        sheep: String,
        dog: String,
        schema: &Value,
        table: Map<String, Value>,
    ) -> Self {
        let Flattened { fields, paths } = flattened(schema);
        let values = flatten_values(&table, &paths);
        Self {
            target: PaneTarget::SheepDog { sheep, dog },
            fields,
            values,
            env_keys: Vec::new(),
            overridden: Vec::new(),
            pending: Vec::new(),
            group: 0,
            view: Viewport::new(),
            typing: None,
            edits: Edits::default(),
            env_typing: None,
            list: None,
            dogs: None,
            section: None,
            dog_table: Some(Box::new(SheepDogTable { paths, table })),
        }
    }

    /// The whole table with every field edit in `edits` applied at its own
    /// path, ready for `Request::SetSheepDogSettings`.
    ///
    /// A `null` removes that leaf and nothing above it. A set creates any
    /// table on its path the sheep does not carry yet. Every key no edit
    /// names stays exactly as the shepherd sent it. Empty for any other
    /// target.
    #[must_use]
    pub fn edited_table_with(&self, edits: &Edits) -> Map<String, Value> {
        let Some(state) = &self.dog_table else {
            return Map::new();
        };
        let mut table = state.table.clone();
        for (key, edit) in edits.iter() {
            let (EditKey::Field(row), PaneEdit::Set { value, .. }) = (key, edit.edit()) else {
                continue;
            };
            if let Some(path) = state.paths.get(row) {
                write_at(&mut table, path, value.as_value());
            }
        }
        table
    }

    /// Whether `table` is the one this pane last took from the shepherd.
    /// [`None`] is a sheep with no table. False for any other target.
    #[must_use]
    pub(in crate::lookout) fn holds_table(&self, table: Option<&Map<String, Value>>) -> bool {
        self.dog_table.as_ref().is_some_and(|state| {
            table.map_or(state.table.is_empty(), |table| state.table == *table)
        })
    }

    /// Replaces the table under an open pane with the shepherd's current
    /// one, keeping the cursor and the filed edits. An open editor is
    /// dropped, for the reason [`Self::adopt_edits`] gives.
    pub(in crate::lookout) fn adopt_table(&mut self, table: Map<String, Value>) {
        let Some(state) = self.dog_table.as_mut() else {
            return;
        };
        self.values = flatten_values(&table, &state.paths);
        state.table = table;
        self.typing = None;
    }
}

/// Sets `value` at `path` inside `table`, or removes that leaf for `null`.
///
/// A set replaces a step on the path that holds something other than a
/// table with one: the schema says a table lives there, and the edit is the
/// operator's. A removal through a step that is not a table does nothing.
fn write_at(table: &mut Map<String, Value>, path: &[String], value: &Value) {
    let Some((leaf, parents)) = path.split_last() else {
        return;
    };
    let mut here = table;
    for step in parents {
        if value.is_null() && !here.get(step).is_some_and(Value::is_object) {
            return;
        }
        let slot = here
            .entry(step.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        let Value::Object(next) = slot else {
            return;
        };
        here = next;
    }
    if value.is_null() {
        here.remove(leaf);
    } else {
        here.insert(leaf.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::fixtures::field;
    use super::*;
    use crate::lookout::view::fixtures::{plain, render_all};
    use crate::lookout::view::pane::pane_lines;

    /// The `jobs` dog's per-sheep schema: a scalar, a closed choice, a
    /// nested `hours` table, and `models.worker` two tables deep holding a
    /// secret beside a plain field.
    fn jobs_schema() -> Value {
        json!({
            "$ref": "#/$defs/ProjectSettings",
            "$defs": {
                "ProjectSettings": {
                    "type": "object",
                    "properties": {
                        "concurrency": { "type": "integer" },
                        "merge": { "enum": ["ask", "auto"] },
                        "hours": { "$ref": "#/$defs/Hours" },
                        "models": { "$ref": "#/$defs/Models" },
                    },
                },
                "Hours": {
                    "type": "object",
                    "properties": {
                        "start": { "type": "string" },
                        "end": { "type": "string" },
                    },
                },
                "Models": {
                    "type": "object",
                    "properties": { "worker": { "$ref": "#/$defs/Worker" } },
                },
                "Worker": {
                    "type": "object",
                    "properties": {
                        "model": { "type": "string" },
                        "token": { "type": "string", "x-shep-secret": true },
                    },
                },
            },
        })
    }

    /// `web`'s `jobs` table, with a credential in `models.worker.token` and
    /// a `legacy` table the schema does not name.
    fn jobs_table() -> Map<String, Value> {
        json!({
            "concurrency": 2,
            "merge": "ask",
            "hours": { "start": "09:00", "end": "17:00" },
            "models": { "worker": { "model": "small", "token": "sk-live-51Hx9Qa" } },
            "legacy": { "kept": true },
        })
        .as_object()
        .cloned()
        .expect("an object")
    }

    fn jobs_pane() -> ConfigPane {
        ConfigPane::sheep_dog("web".into(), "jobs".into(), &jobs_schema(), jobs_table())
    }

    fn edits_of(pairs: &[(&str, Value)]) -> Edits {
        let mut edits = Edits::default();
        for (key, value) in pairs {
            edits.set(field(key, value.clone()), None);
        }
        edits
    }

    #[test]
    fn a_nested_table_shows_as_dotted_rows_with_the_tables_values() {
        let pane = jobs_pane();
        let keys: Vec<&str> = pane
            .fields()
            .fields()
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "concurrency",
                "hours.end",
                "hours.start",
                "merge",
                "models.worker.model",
                "models.worker.token"
            ]
        );
        assert_eq!(pane.value("hours.start"), "09:00");
        assert_eq!(pane.value("models.worker.model"), "small");
        assert_eq!(pane.value("concurrency"), "2");
        assert_eq!(pane.target().name(), "web", "the sheep owns the table");
        assert_eq!(pane.cost("concurrency"), None, "the dog decides");
    }

    #[test]
    fn a_secret_draws_set_and_its_value_reaches_no_row() {
        let pane = jobs_pane();
        let text = render_all(&pane_lines(&pane, plain(), 160, 0));
        assert!(!text.contains("sk-live-51Hx9Qa"), "{text}");
        let token_row = text
            .lines()
            .find(|line| line.contains("models.worker.token"))
            .expect("the secret has a row");
        assert!(token_row.contains("<set>"), "{token_row}");
    }

    #[test]
    fn the_title_names_the_sheep_and_the_dog() {
        let text = render_all(&pane_lines(&jobs_pane(), plain(), 120, 0));
        assert!(text.contains("web \u{203a} jobs"), "{text}");
    }

    #[test]
    fn an_edit_sets_a_nested_leaf_and_keeps_every_other_key() {
        let pane = jobs_pane();
        let table = pane.edited_table_with(&edits_of(&[
            ("hours.start", json!("10:00")),
            ("merge", json!("auto")),
        ]));
        let mut want = jobs_table();
        want["hours"]["start"] = json!("10:00");
        want["merge"] = json!("auto");
        assert_eq!(Value::Object(table), Value::Object(want));
    }

    #[test]
    fn a_null_removes_only_its_leaf() {
        let pane = jobs_pane();
        let table = pane.edited_table_with(&edits_of(&[
            ("concurrency", Value::Null),
            ("hours.end", Value::Null),
        ]));
        assert!(!table.contains_key("concurrency"));
        assert_eq!(table["hours"], json!({ "start": "09:00" }));
        assert_eq!(table["models"], jobs_table()["models"], "a sibling table");
        assert_eq!(
            table["legacy"],
            json!({ "kept": true }),
            "a key no schema names"
        );
    }

    #[test]
    fn a_set_under_a_table_the_sheep_lacks_creates_it() {
        let pane = ConfigPane::sheep_dog("web".into(), "jobs".into(), &jobs_schema(), Map::new());
        let table = pane.edited_table_with(&edits_of(&[
            ("models.worker.model", json!("large")),
            ("hours.end", Value::Null),
        ]));
        assert_eq!(
            Value::Object(table),
            json!({ "models": { "worker": { "model": "large" } } })
        );
    }

    /// A set through a step the sheep holds as a scalar replaces it with a
    /// table: the schema says a table lives there, and the edit is the
    /// operator's.
    #[test]
    fn a_set_through_a_scalar_replaces_it_with_a_table() {
        let stored = json!({ "concurrency": 2, "models": "gpt" });
        let pane = ConfigPane::sheep_dog(
            "web".into(),
            "jobs".into(),
            &jobs_schema(),
            stored.as_object().cloned().expect("a table"),
        );
        let table = pane.edited_table_with(&edits_of(&[("models.worker.model", json!("large"))]));
        assert_eq!(
            Value::Object(table),
            json!({ "concurrency": 2, "models": { "worker": { "model": "large" } } })
        );
    }

    /// `worker.allowed_domains` as a dog's own schema writes it: a list
    /// whose items are a `$ref`, one table down, beside an array of
    /// tables the pane has no editor for.
    fn worker_schema() -> Value {
        json!({
            "$ref": "#/$defs/Settings",
            "$defs": {
                "Settings": {
                    "type": "object",
                    "properties": {
                        "worker": { "$ref": "#/$defs/Worker" },
                        "hosts": { "type": "array", "items": { "$ref": "#/$defs/Host" } },
                    },
                },
                "Worker": {
                    "type": "object",
                    "properties": {
                        "allowed_domains": {
                            "type": "array",
                            "items": { "$ref": "#/$defs/NonBlank" },
                        },
                    },
                },
                "Host": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                },
                "NonBlank": { "type": "string" },
            },
        })
    }

    #[test]
    fn a_nested_list_opens_the_list_sub_screen_and_writes_at_its_path() {
        let stored = json!({
            "worker": { "allowed_domains": ["a.example"], "kept": 1 },
            "hosts": [{ "name": "h" }],
        });
        let mut pane = ConfigPane::sheep_dog(
            "web".into(),
            "jobs".into(),
            &worker_schema(),
            stored.as_object().cloned().expect("a table"),
        );
        pane.move_to_key("worker.allowed_domains");
        pane.open_list();
        let list = pane.list_mut().expect("the list sub-screen opens");
        assert_eq!(list.key(), "worker.allowed_domains");
        assert_eq!(list.elements(), ["a.example"]);
        list.move_to_last();
        pane.file_list_element("b.example".into());

        let table = pane.edited_table_with(pane.edits());
        assert_eq!(
            table["worker"],
            json!({ "allowed_domains": ["a.example", "b.example"], "kept": 1 })
        );
        assert_eq!(table["hosts"], json!([{ "name": "h" }]));
    }

    #[test]
    fn an_array_of_tables_opens_no_list_sub_screen() {
        let mut pane =
            ConfigPane::sheep_dog("web".into(), "jobs".into(), &worker_schema(), Map::new());
        pane.move_to_key("hosts");
        pane.open_list();
        assert!(pane.list().is_none());
        assert!(pane.lock("hosts").is_some());
    }

    #[test]
    fn no_edit_gives_back_the_table_as_it_came() {
        assert_eq!(
            jobs_pane().edited_table_with(&Edits::default()),
            jobs_table()
        );
    }

    /// The dog decides what an edit costs, so the pane names no cost for
    /// any row, as a dog's own pane does.
    #[test]
    fn no_row_has_a_cost_shep_can_name() {
        let pane = jobs_pane();
        for key in ["concurrency", "merge", "hours.start", "models.worker.token"] {
            assert_eq!(pane.cost(key), None, "{key}");
        }
    }

    /// The table carries a credential in `models.worker.token`, and a
    /// derived `Debug` anywhere on the way would print it (IR-41).
    #[test]
    fn the_panes_debug_names_no_table_value() {
        let pane = jobs_pane();
        assert_eq!(
            format!("{pane:?}"),
            r#"ConfigPane { target: SheepDog { sheep: "web", dog: "jobs" }, fields: 6, env_keys: 0, cursor: 0 }"#
        );
        let state = pane.dog_table.as_ref().expect("a table pane");
        assert_eq!(format!("{state:?}"), "SheepDogTable { rows: 6, keys: 5 }");
    }
}
