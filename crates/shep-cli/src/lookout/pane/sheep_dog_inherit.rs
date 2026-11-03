//! A sheep's table for a dog that follows the settings standard, with the
//! dog's section shown under it.
//!
//! The rows show what the dog reads: the sheep's own value where it has one,
//! else the section's. A row the sheep does not set is inherited. It is not
//! stored, so typing the same value files an own one, and `d` has nothing to
//! restore.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Map, Value};
use shep_core::config::DogTable;
use shep_core::dog_defaults::layered;

use super::super::field::flatten_values;
use super::sheep_dog::SheepDogTable;
use super::{ConfigPane, PaneTarget};

/// The section a sheep's table sits over, and the rows it fills.
#[derive(Clone)]
pub(super) struct Inherits {
    /// The dog's section without its dog-wide keys.
    shared: Map<String, Value>,
    /// `shared` by row key, what a cleared row goes back to.
    shared_values: Map<String, Value>,
    /// The rows the sheep's own table leaves to the section.
    inherited: BTreeSet<String>,
}

impl SheepDogTable {
    /// What the rows show, and which of them the section fills.
    pub(super) fn displayed(&self) -> (Map<String, Value>, BTreeSet<String>) {
        let own = flatten_values(&self.table, &self.paths);
        let Some(inherits) = &self.inherits else {
            return (own, BTreeSet::new());
        };
        let table = layered(&[
            &DogTable::from(inherits.shared.clone()),
            &DogTable::from(self.table.clone()),
        ])
        .into_map();
        let shown = flatten_values(&table, &self.paths);
        let inherited = shown
            .iter()
            .filter(|(key, value)| !value.is_null() && own.get(*key).is_none_or(Value::is_null))
            .map(|(key, _)| key.clone())
            .collect();
        (shown, inherited)
    }

    pub(super) fn set_inherited(&mut self, inherited: BTreeSet<String>) {
        if let Some(inherits) = &mut self.inherits {
            inherits.inherited = inherited;
        }
    }
}

impl ConfigPane {
    /// A pane over `dog`'s table on `sheep` with `shared` under it.
    ///
    /// `schema` is the dog's one schema without its dog-wide properties.
    /// `shared` is the dog's section without its dog-wide keys. A row the
    /// table lacks and the section has shows the section's value, and
    /// [`Self::is_inherited`] says so.
    #[must_use]
    pub fn sheep_dog_inheriting(
        sheep: String,
        dog: String,
        schema: &Value,
        table: Map<String, Value>,
        shared: Map<String, Value>,
    ) -> Self {
        let mut pane = Self::sheep_dog(sheep, dog, schema, table);
        if let Some(state) = pane.dog_table.as_mut() {
            let shared_values = flatten_values(&shared, &state.paths);
            state.inherits = Some(Inherits {
                shared,
                shared_values,
                inherited: BTreeSet::new(),
            });
            let (values, inherited) = state.displayed();
            state.set_inherited(inherited);
            pane.entries = Arc::new(pane.entries.rebuilt(&pane.fields, &state.paths, &values));
            pane.values = values;
        }
        pane
    }

    /// Whether the dog's section, not this sheep's table, gives `key` its
    /// value. False for any pane that does not sit over a section.
    #[must_use]
    pub fn is_inherited(&self, key: &str) -> bool {
        self.dog_table
            .as_ref()
            .and_then(|state| state.inherits.as_ref())
            .is_some_and(|inherits| inherits.inherited.contains(key))
    }

    /// Whether this pane sits over the dog's section.
    #[must_use]
    pub fn inherits_section(&self) -> bool {
        matches!(self.target, PaneTarget::SheepDog { .. })
            && self
                .dog_table
                .as_ref()
                .is_some_and(|state| state.inherits.is_some())
    }

    /// What `key` goes back to when its own value is cleared: the section's
    /// value, rendered like a stored one. [`None`] when the section has none.
    pub(super) fn inherited_text(&self, key: &str) -> Option<String> {
        let shared = self
            .dog_table
            .as_ref()?
            .inherits
            .as_ref()?
            .shared_values
            .get(key)
            .filter(|value| !value.is_null())?;
        Some(self.resolved(key, &super::fields::render_json(shared)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::edits::{EditKey, Edits};
    use super::super::PaneEdit;
    use super::super::fixtures::field;
    use super::*;
    use crate::lookout::view::fixtures::{plain, render_all};
    use crate::lookout::view::pane::pane_lines;

    /// The `jobs` dog's one schema as a sheep sees it: `poll` is dog-wide
    /// and already gone.
    fn sheep_form() -> Value {
        json!({
            "type": "object",
            "properties": {
                "model": { "type": "string" },
                "limit": { "type": "integer" },
            },
        })
    }

    fn table(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("a table")
    }

    fn pane(own: Value) -> ConfigPane {
        ConfigPane::sheep_dog_inheriting(
            "web".into(),
            "jobs".into(),
            &sheep_form(),
            table(own),
            table(json!({ "model": "sonnet", "limit": 3 })),
        )
    }

    fn set(key: &str, value: Value) -> Edits {
        let mut edits = Edits::default();
        edits.set(field(key, value), None);
        edits
    }

    #[test]
    fn a_key_the_sheep_lacks_shows_the_sections_value_as_inherited() {
        let pane = pane(json!({ "limit": 9 }));
        assert!(pane.is_inherited("model"));
        assert_eq!(pane.value("model"), "sonnet");
        assert!(!pane.is_inherited("limit"));
        assert_eq!(pane.value("limit"), "9");
    }

    #[test]
    fn a_sheep_with_no_table_inherits_every_key_the_section_sets() {
        let pane = pane(json!({}));
        assert!(pane.is_inherited("model") && pane.is_inherited("limit"));
        assert_eq!(pane.value("limit"), "3");
    }

    #[test]
    fn a_pane_over_no_section_inherits_nothing() {
        let pane = ConfigPane::sheep_dog(
            "web".into(),
            "jobs".into(),
            &sheep_form(),
            table(json!({ "limit": 9 })),
        );
        assert!(!pane.is_inherited("model") && !pane.inherits_section());
        assert_eq!(pane.value("model"), "(unset)");
    }

    #[test]
    fn setting_then_clearing_a_key_writes_it_to_the_sheeps_table_then_removes_it() {
        let mut pane = pane(json!({ "limit": 9 }));

        let written = pane.edited_table_with(&set("model", json!("haiku")));
        assert_eq!(
            Value::Object(written.clone()),
            json!({ "limit": 9, "model": "haiku" })
        );

        pane.adopt_table(written);
        assert!(!pane.is_inherited("model"));
        assert_eq!(pane.value("model"), "haiku");

        let cleared = pane.edited_table_with(&set("model", Value::Null));
        assert_eq!(Value::Object(cleared.clone()), json!({ "limit": 9 }));

        pane.adopt_table(cleared);
        assert!(pane.is_inherited("model"));
        assert_eq!(pane.value("model"), "sonnet");
    }

    #[test]
    fn typing_the_inherited_value_pins_it_as_an_own_value() {
        let mut pane = pane(json!({ "limit": 9 }));
        pane.file_field("model".into(), json!("sonnet"));
        assert!(pane.edits().get(&EditKey::Field("model".into())).is_some());
        let written = pane.edited_table_with(pane.edits());
        assert_eq!(written["model"], "sonnet");
    }

    #[test]
    fn restoring_an_inherited_row_files_nothing() {
        let mut pane = pane(json!({ "limit": 9 }));
        pane.move_to_key("model");
        pane.file_default();
        assert!(pane.edits().is_empty());
    }

    #[test]
    fn restoring_an_own_row_files_its_removal_and_names_what_it_goes_back_to() {
        let mut pane = pane(json!({ "model": "haiku" }));
        pane.move_to_key("model");
        pane.file_default();
        let filed = pane
            .edits()
            .get(&EditKey::Field("model".into()))
            .expect("filed");
        assert!(matches!(filed.edit(), PaneEdit::Set { value, .. } if value.as_value().is_null()));
        assert_eq!(
            pane.edited_value("model").as_deref(),
            Some("sonnet (inherited)")
        );
    }

    #[test]
    fn a_bool_cycles_from_the_inherited_value() {
        let schema = json!({ "type": "object", "properties": { "auto": { "type": "boolean" } } });
        let mut pane = ConfigPane::sheep_dog_inheriting(
            "web".into(),
            "jobs".into(),
            &schema,
            Map::new(),
            table(json!({ "auto": true })),
        );
        pane.move_to_key("auto");
        pane.cycle();
        let filed = pane
            .edits()
            .get(&EditKey::Field("auto".into()))
            .expect("filed");
        assert!(
            matches!(filed.edit(), PaneEdit::Set { value, .. } if value.as_value() == &json!(false))
        );
    }

    #[test]
    fn the_pane_marks_an_inherited_row_and_draws_no_dog_wide_row() {
        let text = render_all(&pane_lines(&pane(json!({ "limit": 9 })), plain(), 120, 0));
        let row = |key: &str| {
            text.lines()
                .find(|line| line.contains(key))
                .unwrap_or_else(|| panic!("no {key} row in {text}"))
                .to_owned()
        };
        assert!(row("model").contains("^model"), "{}", row("model"));
        assert!(row("model").contains("sonnet"));
        assert!(row("limit").contains(" limit") && !row("limit").contains("^limit"));
        assert!(!text.contains("poll"), "{text}");
    }
}
