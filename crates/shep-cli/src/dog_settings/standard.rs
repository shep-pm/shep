//! The dog settings standard, read off a dog's one schema: which keys are
//! dog-wide, the form a sheep's own table is edited with, and the table a dog
//! reads once the section sits under it.
//!
//! The same rules `shep_client::dogs::sheep_settings` applies inside a dog,
//! over JSON, so a pane and a verb show what the dog will read.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use shep_core::config::DogTable;
use shep_core::dog_defaults::layered;
use shep_core::dogs::manifest::Serves;
use shep_core::dogs::{DOG_WIDE_KEY, SHEEP_SCHEMA_KEY};

/// How many `allOf`, `anyOf`, `oneOf` and `$ref` steps are followed below the
/// root, the bound `shep-client` reads the same schema with.
const MAX_DEPTH: usize = 8;

/// A dog that follows the standard: one schema, `[<dog>]` as the value for
/// every sheep, and a sheep's table overriding any key not dog-wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Standard {
    /// Which sheep the dog serves.
    pub(crate) serves: Serves,
    /// The root-level keys only the dog's own section may set.
    pub(crate) dog_wide: BTreeSet<String>,
}

impl Standard {
    /// `root` as a standard dog's schema, or [`None`] for a dog that does not
    /// follow the standard.
    ///
    /// A dog publishing a second schema for its sheep tables is the older
    /// form. Otherwise a dog follows the standard when it serves every sheep
    /// or marks a field dog-wide: an opted-in dog that marks none cannot be
    /// told from one that never heard of the standard.
    pub(crate) fn recognise(root: &Value, serves: Serves) -> Option<Self> {
        if root.get(SHEEP_SCHEMA_KEY).is_some() {
            return None;
        }
        let dog_wide = dog_wide_keys(root);
        (serves == Serves::EverySheep || !dog_wide.is_empty()).then_some(Self { serves, dog_wide })
    }

    /// The schema a sheep's own table is edited against: `root` without its
    /// dog-wide properties, `$defs` kept.
    pub(crate) fn sheep_form(&self, root: &Value) -> Value {
        let mut form = root.clone();
        if let Value::Object(map) = &mut form {
            if let Some(Value::Object(properties)) = map.get_mut("properties") {
                properties.retain(|name, _| !self.dog_wide.contains(name));
            }
            if let Some(Value::Array(required)) = map.get_mut("required") {
                required.retain(|name| name.as_str().is_none_or(|n| !self.dog_wide.contains(n)));
            }
        }
        form
    }

    /// Whether `key`, the first one under a sheep's `dogs.<dog>`, is dog-wide.
    pub(crate) fn is_dog_wide(&self, key: &str) -> bool {
        self.dog_wide.contains(key)
    }

    /// What every sheep shares: `section` without its dog-wide keys.
    pub(crate) fn shared(&self, section: &Map<String, Value>) -> Map<String, Value> {
        section
            .iter()
            .filter(|(key, _)| !self.dog_wide.contains(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// What the dog reads for a sheep: `section` without its dog-wide keys,
    /// with `own` layered over it, and the top-level keys `own` leaves to the
    /// section.
    pub(crate) fn layer(&self, section: &Map<String, Value>, own: &Map<String, Value>) -> Layered {
        let shared = self.shared(section);
        let inherited = shared
            .keys()
            .filter(|key| own.get(*key).is_none_or(Value::is_null))
            .cloned()
            .collect();
        let table = layered(&[&DogTable::from(shared), &DogTable::from(own.clone())]);
        Layered {
            table: table.into_map(),
            inherited,
        }
    }
}

/// A sheep's table with the dog's section under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layered {
    /// The table the dog reads.
    pub(crate) table: Map<String, Value>,
    /// The top-level keys the sheep's own table does not set.
    pub(crate) inherited: BTreeSet<String>,
}

/// The root-level properties `root` marks `x-shep-dog-wide`, in each variant
/// or flattened type it is made of.
pub(crate) fn dog_wide_keys(root: &Value) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    collect(root, root, MAX_DEPTH, &mut keys);
    keys
}

fn collect(root: &Value, node: &Value, depth: usize, keys: &mut BTreeSet<String>) {
    let Some(depth) = depth.checked_sub(1) else {
        return;
    };
    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        keys.extend(
            properties
                .iter()
                .filter(|(_, property)| property.get(DOG_WIDE_KEY) == Some(&Value::Bool(true)))
                .map(|(name, _)| name.clone()),
        );
    }
    for branch in ["allOf", "anyOf", "oneOf"] {
        for child in node
            .get(branch)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect(root, child, depth, keys);
        }
    }
    let target = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| root.pointer(pointer));
    if let Some(target) = target {
        collect(root, target, depth, keys);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn jobs() -> Value {
        json!({
            "type": "object",
            "properties": {
                "poll": { "type": "string", "x-shep-dog-wide": true },
                "model": { "type": "string" },
                "limit": { "type": "integer" },
            },
            "required": ["poll"],
            "$defs": { "Hours": { "type": "string" } },
        })
    }

    fn map(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not a table: {other}"),
        }
    }

    #[test]
    fn a_dog_wide_property_is_found_at_the_root_and_behind_a_ref() {
        let root = json!({
            "$ref": "#/$defs/Settings",
            "$defs": { "Settings": { "properties": {
                "poll": { "x-shep-dog-wide": true },
                "model": {},
            } } },
        });
        assert_eq!(
            dog_wide_keys(&root).into_iter().collect::<Vec<_>>(),
            ["poll"]
        );
    }

    #[test]
    fn a_dog_marking_a_field_follows_the_standard() {
        let standard = Standard::recognise(&jobs(), Serves::OptedIn).expect("standard");
        assert_eq!(standard.dog_wide.iter().collect::<Vec<_>>(), ["poll"]);
    }

    #[test]
    fn a_dog_serving_every_sheep_follows_the_standard_with_no_mark() {
        let root = json!({ "properties": { "model": {} } });
        assert!(Standard::recognise(&root, Serves::EverySheep).is_some());
    }

    #[test]
    fn an_opted_in_dog_marking_nothing_does_not() {
        let root = json!({ "properties": { "model": {} } });
        assert_eq!(Standard::recognise(&root, Serves::OptedIn), None);
    }

    #[test]
    fn a_dog_with_a_second_sheep_schema_keeps_the_older_form() {
        let mut root = jobs();
        root["x-shep-sheep"] = json!({ "properties": {} });
        assert_eq!(Standard::recognise(&root, Serves::EverySheep), None);
    }

    #[test]
    fn the_sheep_form_drops_dog_wide_properties_and_keeps_the_defs() {
        let standard = Standard::recognise(&jobs(), Serves::OptedIn).expect("standard");
        let form = standard.sheep_form(&jobs());
        assert_eq!(
            form,
            json!({
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "limit": { "type": "integer" },
                },
                "required": [],
                "$defs": { "Hours": { "type": "string" } },
            })
        );
    }

    #[test]
    fn the_sheep_table_wins_and_the_section_fills_the_rest() {
        let standard = Standard::recognise(&jobs(), Serves::OptedIn).expect("standard");
        let section = map(json!({ "poll": "5m", "model": "sonnet", "limit": 3 }));
        let own = map(json!({ "limit": 9 }));
        let layered = standard.layer(&section, &own);
        assert_eq!(layered.table, map(json!({ "model": "sonnet", "limit": 9 })));
        assert_eq!(layered.inherited.iter().collect::<Vec<_>>(), ["model"]);
    }

    #[test]
    fn a_null_in_the_sheep_table_inherits_again() {
        let standard = Standard::recognise(&jobs(), Serves::OptedIn).expect("standard");
        let section = map(json!({ "model": "sonnet" }));
        let own = map(json!({ "model": null }));
        assert!(standard.layer(&section, &own).inherited.contains("model"));
    }
}
