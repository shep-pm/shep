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
use shep_core::dogs::manifest::Serves;

use super::super::field::{flatten_values, segment};
use super::sheep_dog::SheepDogTable;
use super::{ConfigPane, PaneTarget, StandardSection};

/// The section a sheep's table sits over, and the rows it fills.
#[derive(Clone)]
pub(super) struct Inherits {
    /// The dog's section without its dog-wide keys.
    shared: Map<String, Value>,
    /// Which sheep the dog serves: an opted-in dog reads the section for a
    /// sheep only once the sheep carries a table.
    serves: Serves,
    /// Whether the sheep carries a table for the dog, empty or not.
    pub(super) has_table: bool,
    /// `shared` by row key, what a cleared row goes back to.
    shared_values: Map<String, Value>,
    /// The rows the sheep's own table leaves to the section.
    inherited: BTreeSet<String>,
}

impl SheepDogTable {
    /// What the rows show, and which of them the section fills.
    pub(super) fn displayed(&self) -> (Map<String, Value>, BTreeSet<String>) {
        let own = flatten_values(&self.table, &self.paths);
        let Some(inherits) = self
            .inherits
            .as_ref()
            .filter(|inherits| inherits.serves == Serves::EverySheep || inherits.has_table)
        else {
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
    /// `table` is [`None`] for a sheep with no table. A row the table lacks
    /// and the section has shows the section's value, and
    /// [`Self::is_inherited`] says so. A dog serving opted-in sheep shows
    /// none for a sheep with no table, since the dog reads none there.
    #[must_use]
    pub fn sheep_dog_inheriting(
        sheep: String,
        dog: String,
        schema: &Value,
        table: Option<Map<String, Value>>,
        section: &StandardSection,
    ) -> Self {
        let has_table = table.is_some();
        let mut pane = Self::sheep_dog(sheep, dog, schema, table.unwrap_or_default());
        if let Some(state) = pane.dog_table.as_mut() {
            let shared_values = flatten_values(&section.shared, &state.paths);
            state.inherits = Some(Inherits {
                shared: section.shared.clone(),
                serves: section.serves,
                has_table,
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

    /// Whether `key` is dog-wide on a dog's own pane: the section sets it
    /// for every sheep, and no sheep's table can hold its own.
    #[must_use]
    pub fn is_dog_wide(&self, key: &str) -> bool {
        self.section.as_ref().is_some_and(|section| {
            section.dog_wide.iter().any(|wide| {
                let wide = segment(wide);
                key == wide
                    || key
                        .strip_prefix(wide.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
    }

    /// Whether this is a dog's pane over a dog that follows the standard,
    /// whose values every sheep shares.
    #[must_use]
    pub fn applies_to_every_sheep(&self) -> bool {
        self.section
            .as_ref()
            .is_some_and(|section| !section.dog_wide.is_empty())
    }

    /// The section's value for `key`, which a row cleared in the pane goes
    /// back to. [`None`] when the section has none.
    pub(super) fn inherited_value(&self, key: &str) -> Option<&Value> {
        self.dog_table
            .as_ref()?
            .inherits
            .as_ref()?
            .shared_values
            .get(key)
            .filter(|value| !value.is_null())
    }

    /// Takes the dog's section as it stands now, keeping the cursor and the
    /// filed edits. A pane over no section ignores it.
    pub(in crate::lookout) fn adopt_section(&mut self, section: &StandardSection) {
        let Some(state) = self.dog_table.as_mut() else {
            return;
        };
        let paths = &state.paths;
        let Some(inherits) = state.inherits.as_mut() else {
            return;
        };
        inherits.shared = section.shared.clone();
        inherits.shared_values = flatten_values(&section.shared, paths);
        let table = state.table.clone();
        self.adopt_table(Some(table));
    }

    /// What `key` goes back to when its own value is cleared: the section's
    /// value, rendered like a stored one. [`None`] when the section has none.
    pub(super) fn inherited_text(&self, key: &str) -> Option<String> {
        let shared = self.inherited_value(key)?;
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

    fn section(serves: Serves, shared: Value) -> StandardSection {
        StandardSection {
            serves,
            shared: table(shared),
        }
    }

    fn pane(own: Value) -> ConfigPane {
        ConfigPane::sheep_dog_inheriting(
            "web".into(),
            "jobs".into(),
            &sheep_form(),
            Some(table(own)),
            &section(Serves::EverySheep, json!({ "model": "sonnet", "limit": 3 })),
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
    fn an_opted_in_dog_shows_the_section_only_once_the_sheep_has_a_table() {
        let section = section(Serves::OptedIn, json!({ "model": "sonnet" }));
        let mut pane = ConfigPane::sheep_dog_inheriting(
            "web".into(),
            "jobs".into(),
            &sheep_form(),
            None,
            &section,
        );
        assert!(!pane.is_inherited("model"));
        assert_eq!(pane.value("model"), "(unset)");

        pane.adopt_table(Some(table(json!({ "limit": 9 }))));
        assert!(pane.is_inherited("model"));
        assert_eq!(pane.value("model"), "sonnet");
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

        pane.adopt_table(Some(written));
        assert!(!pane.is_inherited("model"));
        assert_eq!(pane.value("model"), "haiku");

        let cleared = pane.edited_table_with(&set("model", Value::Null));
        assert_eq!(Value::Object(cleared.clone()), json!({ "limit": 9 }));

        pane.adopt_table(Some(cleared));
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
            None,
            &section(Serves::EverySheep, json!({ "auto": true })),
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
    fn a_bool_cleared_in_the_pane_cycles_from_the_sections_value() {
        let schema = json!({ "type": "object", "properties": { "auto": { "type": "boolean" } } });
        let mut pane = ConfigPane::sheep_dog_inheriting(
            "web".into(),
            "jobs".into(),
            &schema,
            Some(table(json!({ "auto": true }))),
            &section(Serves::EverySheep, json!({ "auto": true })),
        );
        pane.move_to_key("auto");
        pane.file_default();
        pane.cycle();
        let filed = pane
            .edits()
            .get(&EditKey::Field("auto".into()))
            .expect("filed");
        assert!(
            matches!(filed.edit(), PaneEdit::Set { value, .. } if value.as_value() == &json!(false)),
            "{filed:?}"
        );
    }

    #[test]
    fn a_new_section_replaces_what_the_inherited_rows_show() {
        let mut pane = pane(json!({ "limit": 9 }));
        pane.adopt_section(&section(Serves::EverySheep, json!({ "model": "haiku" })));
        assert!(pane.is_inherited("model"));
        assert_eq!(pane.value("model"), "haiku");
        assert!(!pane.is_inherited("limit"));
        assert_eq!(pane.value("limit"), "9");
    }

    #[test]
    fn a_dog_wide_key_with_a_dot_in_its_name_is_found_by_its_quoted_row() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a.b": { "type": "string", "x-shep-dog-wide": true },
                "model": { "type": "string" },
            },
        });
        let pane = ConfigPane::dog("jobs".into(), None, schema, String::new(), Vec::new());
        assert!(pane.is_dog_wide("\"a.b\""));
        assert!(!pane.is_dog_wide("a.b"));
        assert!(!pane.is_dog_wide("model"));
    }

    fn jobs_dog_pane() -> ConfigPane {
        let schema = json!({
            "type": "object",
            "properties": {
                "poll": { "type": "string", "x-shep-dog-wide": true },
                "model": { "type": "string" },
            },
        });
        ConfigPane::dog(
            "jobs".into(),
            None,
            schema,
            "poll = \"5m\"\nmodel = \"sonnet\"\n".into(),
            Vec::new(),
        )
    }

    #[test]
    fn the_dog_pane_marks_a_dog_wide_row_and_says_its_values_reach_every_sheep() {
        let pane = jobs_dog_pane();
        assert!(pane.applies_to_every_sheep());
        assert!(pane.is_dog_wide("poll") && !pane.is_dog_wide("model"));

        let text = render_all(&pane_lines(&pane, plain(), 120, 0));
        let row = |key: &str| {
            text.lines()
                .find(|line| line.trim_start_matches(['>', ' ', '@']).starts_with(key))
                .unwrap_or_else(|| panic!("no {key} row in {text}"))
                .to_owned()
        };
        assert!(row("poll").contains("@poll"), "{}", row("poll"));
        assert!(!row("model").contains('@'), "{}", row("model"));
        assert!(text.contains("apply to every sheep it serves"), "{text}");
    }

    #[test]
    fn a_dog_pane_marking_nothing_dog_wide_draws_as_before() {
        let schema = json!({ "type": "object", "properties": { "model": { "type": "string" } } });
        let pane = ConfigPane::dog("jobs".into(), None, schema, String::new(), Vec::new());
        assert!(!pane.applies_to_every_sheep());
        let text = render_all(&pane_lines(&pane, plain(), 120, 0));
        assert!(
            !text.contains("every sheep") && !text.contains('@'),
            "{text}"
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
        assert!(text.contains("^ comes from [jobs]"), "{text}");
    }
}
