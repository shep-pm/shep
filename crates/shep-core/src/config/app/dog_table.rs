//! [`DogTable`]: one dog's opaque per-sheep settings table.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::config::key_path::{push_index, push_key};
use crate::config::table::{TOML_PRIVATE_DATETIME_KEY, deserialize_table};
use crate::secrets::references_in;

/// One dog's `[app.dogs.<name>]` table on a sheep, held opaque.
///
/// shep stores this and hands it to the dog it names; nothing here reads a
/// key inside it. `Debug` prints only how many keys the table holds, never
/// their values: a table can carry a credential the way a dog's own
/// `dogs.toml` section can.
// wire format: changing this is a breaking change
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DogTable(Map<String, Value>);

impl DogTable {
    /// Borrows the underlying table.
    #[must_use]
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Unwraps the underlying table.
    #[must_use]
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }

    /// `layers` folded into one table, the lowest first: a later layer's
    /// value wins.
    ///
    /// Nested tables merge key by key. An array or a plain value replaces
    /// what is under it whole, so a sheep's list never adds to a section's.
    #[must_use]
    pub fn layered(layers: &[&Self]) -> Self {
        let mut out = Map::new();
        for layer in layers {
            merge(&mut out, &layer.0);
        }
        Self(out)
    }

    /// Every `{{secret:...}}` reference a string in this table names, as
    /// `KEY` or `namespace/KEY`.
    ///
    /// Walks the same strings [`Self::try_map_strings`] visits. A table this
    /// reports nothing for has no reference that resolves, but a resolve can
    /// still refuse malformed `{{secret:` text, which this does not count.
    /// Keys are not among them.
    #[must_use]
    pub fn references(&self) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let _ = self.try_map_strings::<core::convert::Infallible>("", |_, text| {
            found.extend(references_in(text));
            Ok(None)
        });
        found
    }

    /// The key path of the first string that names a `{{secret:...}}`
    /// reference, in table order, under `root`.
    #[must_use]
    pub fn first_reference_path(&self, root: &str) -> Option<String> {
        let mut first = None;
        let _ = self.try_map_strings::<core::convert::Infallible>(root, |path, text| {
            if first.is_none() && !references_in(text).is_empty() {
                first = Some(path.to_string());
            }
            Ok(None)
        });
        first
    }

    /// This table with every string value `visit` replaces swapped for its
    /// replacement.
    ///
    /// `visit` gets the key path of each string, nested objects and arrays
    /// included, and its text; it answers the replacement, or `None` to leave
    /// the string as it is. Keys and every non-string value are carried over
    /// untouched. A path is spelled by [`push_key`] and [`push_index`] and
    /// starts at `root`, so a caller can name the table's place in a larger
    /// document; pass `""` for the table's own keys.
    ///
    /// # Errors
    /// The first error `visit` returns, which stops the walk.
    pub fn try_map_strings<E>(
        &self,
        root: &str,
        mut visit: impl FnMut(&str, &str) -> Result<Option<String>, E>,
    ) -> Result<Self, E> {
        map_object(&self.0, &mut root.to_string(), &mut visit).map(Self)
    }
}

fn merge(under: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (key, value) in over {
        match (under.get_mut(key), value) {
            (Some(Value::Object(held)), Value::Object(incoming)) => merge(held, incoming),
            _ => {
                under.insert(key.clone(), value.clone());
            }
        }
    }
}

/// What a visitor does with one string value: the key path it sits at and
/// its text in, the replacement out, or `None` to leave it.
type Visit<'a, E> = dyn FnMut(&str, &str) -> Result<Option<String>, E> + 'a;

/// One object in [`DogTable::try_map_strings`]'s walk. `path` is the key
/// path of `map`, restored on return.
fn map_object<E>(
    map: &Map<String, Value>,
    path: &mut String,
    visit: &mut Visit<'_, E>,
) -> Result<Map<String, Value>, E> {
    let mut out = Map::new();
    for (key, value) in map {
        let mark = path.len();
        push_key(path, key);
        let mapped = map_value(value, path, visit);
        path.truncate(mark);
        out.insert(key.clone(), mapped?);
    }
    Ok(out)
}

/// One value in [`DogTable::try_map_strings`]'s walk.
fn map_value<E>(value: &Value, path: &mut String, visit: &mut Visit<'_, E>) -> Result<Value, E> {
    match value {
        Value::String(text) => Ok(visit(path, text)?.map_or_else(|| value.clone(), Value::String)),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let mark = path.len();
                push_index(path, index);
                let mapped = map_value(item, path, visit);
                path.truncate(mark);
                out.push(mapped?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => map_object(map, path, visit).map(Value::Object),
        other => Ok(other.clone()),
    }
}

impl From<Map<String, Value>> for DogTable {
    fn from(map: Map<String, Value>) -> Self {
        Self(map)
    }
}

impl fmt::Debug for DogTable {
    /// Prints only the key count, never a value: a table can carry a
    /// credential the way a dog's own `dogs.toml` section can.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0.len();
        write!(f, "DogTable(<{n} key{}>)", if n == 1 { "" } else { "s" })
    }
}

/// Reads the `dogs` table itself, one [`DogTable`] per dog. The
/// `deserialize_with` on [`AppConfig::dogs`](crate::config::AppConfig::dogs).
///
/// The shared reader is what refuses `dogs: null` in YAML, where a null
/// scalar asked for as a map would otherwise become an empty one, and a bare
/// TOML datetime under `[[app]]`, which arrives as a one-key table. Each
/// table inside `dogs` keeps its own rules, below.
pub(super) fn deserialize_dogs<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, DogTable>, D::Error> {
    deserialize_table(de, "dogs")
}

impl<'de> Deserialize<'de> for DogTable {
    /// Reads a table, refusing anything else, and rewrites a TOML datetime,
    /// date or time to its plain string at any depth.
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(de)?;
        let Value::Object(map) = value else {
            return Err(serde::de::Error::custom(
                "a dog's table must be a table, not a bare value",
            ));
        };
        Ok(Self(convert_datetimes(map)))
    }
}

/// Walks `map`, turning a toml_datetime private one-key object into its
/// plain string, at any depth and inside an array.
fn convert_datetimes(map: Map<String, Value>) -> Map<String, Value> {
    map.into_iter().map(|(k, v)| (k, convert(v))).collect()
}

/// One value in [`convert_datetimes`]'s walk.
fn convert(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(Value::String(s)) = map.get(TOML_PRIVATE_DATETIME_KEY)
            {
                Value::String(s.clone())
            } else {
                Value::Object(convert_datetimes(map))
            }
        }
        Value::Array(items) => Value::Array(items.into_iter().map(convert).collect()),
        other => other,
    }
}

/// A table's keys and their shapes are the dog's own business, never
/// shep's: the schema can say no more than "a table".
#[cfg(feature = "schema")]
impl schemars::JsonSchema for DogTable {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DogTable".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "additionalProperties": true,
            "description": "A dog's own settings for this sheep. shep stores it and never reads a key inside.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    struct Holder {
        table: DogTable,
    }

    fn table_of(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> DogTable {
        DogTable::from(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<Map<String, Value>>(),
        )
    }

    fn table(value: Value) -> DogTable {
        match value {
            Value::Object(map) => DogTable::from(map),
            other => panic!("not a table: {other}"),
        }
    }

    #[test]
    fn a_later_layer_wins_and_a_nested_table_merges_key_by_key() {
        let section = table(json!({
            "model": "opus",
            "review": { "lines": 8, "tone": "terse" },
            "labels": ["a", "b"],
        }));
        let sheep = table(json!({
            "model": "sonnet",
            "review": { "lines": 3 },
        }));
        assert_eq!(
            DogTable::layered(&[&section, &sheep]),
            table(json!({
                "model": "sonnet",
                "review": { "lines": 3, "tone": "terse" },
                "labels": ["a", "b"],
            }))
        );
    }

    #[test]
    fn an_array_or_a_plain_value_in_a_later_layer_replaces_the_lower_whole() {
        let section = table(json!({
            "labels": ["a", "b"],
            "review": { "lines": 8 },
            "budget": 5,
        }));
        let sheep = table(json!({
            "labels": ["c"],
            "review": "off",
            "budget": { "daily": 1 },
        }));
        assert_eq!(DogTable::layered(&[&section, &sheep]), sheep);
    }

    #[test]
    fn no_layers_fold_to_an_empty_table() {
        assert_eq!(DogTable::layered(&[]), table(json!({})));
    }

    #[test]
    fn a_bare_toml_time_arrives_as_its_plain_string() {
        let holder: Holder = toml::from_str("[table]\nstart = 09:00:00\n").unwrap();
        assert_eq!(
            holder.table.as_map().get("start"),
            Some(&Value::String("09:00:00".to_string()))
        );
    }

    #[test]
    fn a_bare_toml_date_arrives_as_its_plain_string() {
        let holder: Holder = toml::from_str("[table]\nday = 2026-09-27\n").unwrap();
        assert_eq!(
            holder.table.as_map().get("day"),
            Some(&Value::String("2026-09-27".to_string()))
        );
    }

    #[test]
    fn a_bare_toml_datetime_arrives_as_its_plain_string() {
        let holder: Holder = toml::from_str("[table]\nat = 2026-09-27T09:00:00Z\n").unwrap();
        assert_eq!(
            holder.table.as_map().get("at"),
            Some(&Value::String("2026-09-27T09:00:00Z".to_string()))
        );
    }

    #[test]
    fn a_toml_datetime_nested_in_a_table_still_converts() {
        let toml_src = "[table.hours]\nstart = 09:00:00\nend = 17:00:00\n";
        let holder: Holder = toml::from_str(toml_src).unwrap();
        let hours = holder
            .table
            .as_map()
            .get("hours")
            .and_then(Value::as_object)
            .expect("hours must still be a table");
        assert_eq!(
            hours.get("start"),
            Some(&Value::String("09:00:00".to_string()))
        );
        assert_eq!(
            hours.get("end"),
            Some(&Value::String("17:00:00".to_string()))
        );
    }

    #[test]
    fn a_toml_datetime_inside_an_array_still_converts() {
        let toml_src = "table = { slots = [09:00:00, 17:00:00] }\n";
        let holder: Holder = toml::from_str(toml_src).unwrap();
        let slots = holder
            .table
            .as_map()
            .get("slots")
            .and_then(Value::as_array)
            .expect("slots must still be an array");
        assert_eq!(
            slots.as_slice(),
            [
                Value::String("09:00:00".to_string()),
                Value::String("17:00:00".to_string())
            ]
        );
    }

    /// Only the exact one-key shape converts: a real key of the same name
    /// beside a second key is left alone.
    #[test]
    fn a_real_key_of_the_same_name_beside_another_is_left_alone() {
        let raw = json!({"at": {"$__toml_private_datetime": "09:00:00", "extra": true}});
        let table: DogTable = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&table).unwrap(), raw);
    }

    #[test]
    fn a_number_is_refused() {
        let err = toml::from_str::<Holder>("table = 5\n").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn a_string_is_refused() {
        assert!(toml::from_str::<Holder>("table = \"x\"\n").is_err());
    }

    #[test]
    fn an_array_is_refused() {
        assert!(toml::from_str::<Holder>("table = [1]\n").is_err());
    }

    /// fails if a wire `dogs` that is not a table stops being refused.
    ///
    /// `AppConfig` decodes from a peer as well as from a Flockfile, and both
    /// paths read the key through the shared table reader. The wire refused a
    /// null map before, in serde's words: what changed is the words, not what
    /// is refused. A real table still decodes.
    #[test]
    fn a_wire_dogs_that_is_not_a_table_is_still_refused() {
        let src = r#"{"name":"web","script":"./srv","dogs":null}"#;
        let err = serde_json::from_str::<crate::config::AppConfig>(src)
            .expect_err("a wire dogs that is not a table must be refused, as it always was");
        let msg = err.to_string();
        assert!(msg.contains("`dogs` must be a table"), "{msg}");

        let ok: crate::config::AppConfig = serde_json::from_str(
            r#"{"name":"web","script":"./srv","dogs":{"jobs":{"concurrency":2}}}"#,
        )
        .expect("a real dogs table on the wire still decodes");
        assert_eq!(ok.dogs.len(), 1);
    }

    #[test]
    fn debug_prints_the_key_count_and_never_a_value() {
        // Exact string pinned so a lazy derive(Debug) refactor fails here:
        // a derived Debug would print the credential this table might hold.
        let table = table_of([("token", json!("hunter2"))]);
        assert_eq!(format!("{table:?}"), "DogTable(<1 key>)");

        let table = table_of([
            ("concurrency", json!(2)),
            ("merge", json!("ask")),
            ("token", json!("hunter2")),
        ]);
        assert_eq!(format!("{table:?}"), "DogTable(<3 keys>)");
        assert!(!format!("{table:?}").contains("hunter2"));
    }

    #[test]
    fn a_json_round_trip_is_byte_identical() {
        let table = table_of([("concurrency", json!(2)), ("merge", json!("ask"))]);
        let json = serde_json::to_string(&table).unwrap();
        let holder: Holder = serde_json::from_str(&format!("{{\"table\":{json}}}")).unwrap();
        assert_eq!(serde_json::to_string(&holder.table).unwrap(), json);
    }
}
