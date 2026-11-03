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
    ///
    /// The properties are dropped wherever `root` holds them: at the root,
    /// in a variant, or behind a `$ref`.
    pub(crate) fn sheep_form(&self, root: &Value) -> Value {
        let mut form = root.clone();
        let mut pending = Vec::new();
        self.strip(&mut form, &mut pending);
        let mut seen = BTreeSet::new();
        while let Some(pointer) = pending.pop() {
            if !seen.insert(pointer.clone()) {
                continue;
            }
            if let Some(target) = form.pointer_mut(&pointer) {
                self.strip(target, &mut pending);
            }
        }
        form
    }

    /// Drops the dog-wide properties of `node` and of each variant inline in
    /// it, and queues the pointer of each `$ref` found on the way.
    fn strip(&self, node: &mut Value, refs: &mut Vec<String>) {
        let Value::Object(map) = node else {
            return;
        };
        if let Some(Value::Object(properties)) = map.get_mut("properties") {
            properties.retain(|name, _| !self.dog_wide.contains(name));
        }
        if let Some(Value::Array(required)) = map.get_mut("required") {
            required.retain(|name| name.as_str().is_none_or(|n| !self.dog_wide.contains(n)));
        }
        if let Some(pointer) = map
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix('#'))
        {
            refs.push(pointer.to_owned());
        }
        for branch in ["allOf", "anyOf", "oneOf"] {
            if let Some(Value::Array(children)) = map.get_mut(branch) {
                for child in children {
                    self.strip(child, refs);
                }
            }
        }
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
    /// with `own` layered over it, and the paths `own` leaves to the section.
    ///
    /// A table the sheep sets only some keys of is merged key by key, so the
    /// keys it leaves are inherited. A `null` in `own` is a value, as the dog
    /// reads it: it replaces the section's and the key is not inherited.
    pub(crate) fn layer(&self, section: &Map<String, Value>, own: &Map<String, Value>) -> Layered {
        let shared = self.shared(section);
        let mut inherited = BTreeSet::new();
        inherited_paths(&shared, own, &mut Vec::new(), &mut inherited);
        let table = layered(&[&DogTable::from(shared), &DogTable::from(own.clone())]);
        Layered {
            table: table.into_map(),
            inherited,
        }
    }
}

/// Adds to `out` the path of each key `shared` holds and `own` lacks, going
/// into a table both hold, which the dog merges key by key.
fn inherited_paths(
    shared: &Map<String, Value>,
    own: &Map<String, Value>,
    at: &mut Vec<String>,
    out: &mut BTreeSet<Vec<String>>,
) {
    for (key, value) in shared {
        at.push(key.clone());
        match (own.get(key), value) {
            (None, _) => {
                out.insert(at.clone());
            }
            (Some(Value::Object(held)), Value::Object(under)) => {
                inherited_paths(under, held, at, out);
            }
            _ => {}
        }
        at.pop();
    }
}

/// A sheep's table with the dog's section under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layered {
    /// The table the dog reads.
    pub(crate) table: Map<String, Value>,
    /// The path of each value the sheep's own table leaves to the section.
    pub(crate) inherited: BTreeSet<Vec<String>>,
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
    fn the_sheep_form_drops_a_dog_wide_property_behind_a_ref_and_in_a_variant() {
        let root = json!({
            "$ref": "#/$defs/Settings",
            "$defs": {
                "Settings": {
                    "properties": {
                        "poll": { "x-shep-dog-wide": true },
                        "model": {},
                    },
                    "allOf": [{ "properties": {
                        "token": { "x-shep-dog-wide": true },
                        "limit": {},
                    } }],
                },
            },
        });
        let standard = Standard::recognise(&root, Serves::OptedIn).expect("standard");
        let form = standard.sheep_form(&root);
        assert_eq!(
            form["$defs"]["Settings"],
            json!({
                "properties": { "model": {} },
                "allOf": [{ "properties": { "limit": {} } }],
            })
        );
        assert_eq!(form["$ref"], "#/$defs/Settings");
    }

    #[test]
    fn the_sheep_table_wins_and_the_section_fills_the_rest() {
        let standard = Standard::recognise(&jobs(), Serves::OptedIn).expect("standard");
        let section = map(json!({ "poll": "5m", "model": "sonnet", "limit": 3 }));
        let own = map(json!({ "limit": 9 }));
        let layered = standard.layer(&section, &own);
        assert_eq!(layered.table, map(json!({ "model": "sonnet", "limit": 9 })));
        assert_eq!(
            layered.inherited,
            BTreeSet::from([vec!["model".to_owned()]])
        );
    }

    #[test]
    fn a_table_the_sheep_sets_part_of_inherits_the_keys_it_leaves() {
        let standard = Standard::recognise(&jobs(), Serves::OptedIn).expect("standard");
        let section = map(json!({ "db": { "host": "a", "port": 1 }, "model": "sonnet" }));
        let own = map(json!({ "db": { "host": "b" }, "model": "haiku" }));
        let layered = standard.layer(&section, &own);
        assert_eq!(
            layered.table,
            map(json!({ "db": { "host": "b", "port": 1 }, "model": "haiku" }))
        );
        assert_eq!(
            layered.inherited,
            BTreeSet::from([vec!["db".to_owned(), "port".to_owned()]])
        );
    }

    #[test]
    fn a_null_in_the_sheep_table_replaces_the_section_as_the_dog_reads_it() {
        let standard = Standard::recognise(&jobs(), Serves::OptedIn).expect("standard");
        let section = map(json!({ "model": "sonnet" }));
        let own = map(json!({ "model": null }));
        let layered = standard.layer(&section, &own);
        assert!(layered.inherited.is_empty());
        assert_eq!(layered.table, map(json!({ "model": null })));
    }
}
