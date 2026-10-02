//! Flattens a per-sheep schema into dotted rows.
//!
//! A dog's per-sheep table can nest a table inside a table
//! (`[app.dogs.jobs].hours = { start = ... }`), and the pane that edits it
//! has no sub-screen for that: [`super::FieldKind::Opaque`] is read-only.
//! [`flattened`] walks every nested table's own `properties` instead of
//! treating it as one opaque leaf, so `hours.start` becomes its own row,
//! built by the same [`super::field_from`] every other schema-driven pane
//! uses. An array of strings or integers stays a list row, which the pane
//! writes whole at its dotted path. An array of tables and a map of scalars
//! or tables stay one `Opaque` row: neither has a shape [`flattened`] can
//! walk back into a set of writes.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::schema::marked;
use super::{Field, FieldKind, FieldSet};

/// A per-sheep schema flattened into dotted rows.
pub(crate) struct Flattened {
    /// One row per leaf, each table's properties in key order, depth
    /// first, no groups.
    pub(crate) fields: FieldSet,
    /// The dotted display key each row was built under, to the real path
    /// of property names it came from. A property literally named `a.b`
    /// keeps the one-element path `["a.b"]`.
    pub(crate) paths: BTreeMap<String, Vec<String>>,
}

/// Flattens `schema`, the `x-shep-sheep` value with the root's own `$defs`
/// attached, into [`Flattened`].
///
/// A property whose resolved schema is an object with `properties` is not
/// a leaf: its own properties become rows instead, prefixed with this
/// property's key, however many tables deep the schema nests. Every other
/// property becomes one leaf row, built by [`super::field_from`] so its
/// kind, help and secret mark come out exactly as they would in any other
/// schema-driven pane. A leaf whose kind is [`FieldKind::Map`] is shown as
/// [`FieldKind::Opaque`] instead: a map stays read-only here, since it has
/// no shape this flatten can write back into by path. An array of tables
/// is already [`FieldKind::Opaque`]; an array of strings or integers is a
/// [`FieldKind::List`], filed whole.
#[must_use]
pub(crate) fn flattened(schema: &Value) -> Flattened {
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let root = super::schema::resolved(schema, &defs);
    let properties = root
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut fields = Vec::new();
    let mut paths = BTreeMap::new();
    walk(
        &properties,
        &defs,
        &[],
        marked(schema) || marked(root),
        &mut vec![root],
        &mut fields,
        &mut paths,
    );

    Flattened {
        fields: FieldSet::from_fields(fields, &[]),
        paths,
    }
}

/// Whether `schema` (already `$ref`- and `anyOf`-resolved) is a nested
/// table rather than a leaf: an object schema that names its own
/// properties, not a map of arbitrary keys. `additionalProperties: false`
/// is what `deny_unknown_fields` emits, and closes a table rather than
/// making it a map.
fn is_nested_table(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
        && schema.get("properties").is_some()
        && matches!(
            schema.get("additionalProperties"),
            None | Some(Value::Bool(false))
        )
}

/// `key` as one segment of a dotted row key: bare when TOML would write it
/// bare, quoted otherwise, so a property named `a.b` shows as `"a.b"` and
/// can never collide with a nested `a` holding `b`.
fn segment(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        key.to_owned()
    } else {
        serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\""))
    }
}

/// `open` holds the nested tables on the current path, by identity: a
/// schema is finite, so only a `$ref` back to one of them can loop, and
/// that table is shown as one read-only leaf rather than walked again.
///
/// `inherited` is whether a table on the current path is marked secret.
/// `#[shep(secret)]` on a struct field sits beside its `$ref`, not on the
/// struct's own fields, so every row the table flattens into carries it.
fn walk<'a>(
    properties: &'a Map<String, Value>,
    defs: &'a Map<String, Value>,
    prefix: &[String],
    inherited: bool,
    open: &mut Vec<&'a Value>,
    fields: &mut Vec<Field>,
    paths: &mut BTreeMap<String, Vec<String>>,
) {
    for (key, property_schema) in properties {
        let mut path = prefix.to_vec();
        path.push(key.clone());

        let resolved = super::schema::resolved(property_schema, defs);
        let looped = open.iter().any(|seen| core::ptr::eq(*seen, resolved));
        if !looped
            && is_nested_table(resolved)
            && let Some(nested) = resolved.get("properties").and_then(Value::as_object)
        {
            let secret = inherited || marked(property_schema) || marked(resolved);
            open.push(resolved);
            walk(nested, defs, &path, secret, open, fields, paths);
            open.pop();
            continue;
        }

        let dotted = path
            .iter()
            .map(|key| segment(key))
            .collect::<Vec<_>>()
            .join(".");
        let mut field = super::field_from(key, property_schema, defs);
        field.key.clone_from(&dotted);
        if looped || field.kind == FieldKind::Map {
            field.kind = FieldKind::Opaque;
            field.editable = false;
        }
        // `field_from` already masks a row with a secret anywhere beneath
        // it; what it cannot see is a secret table above this one.
        field.secret |= inherited;
        fields.push(field);
        paths.insert(dotted, path);
    }
}

/// Reads `table`'s values at every path `paths` names, keyed by the same
/// dotted keys [`flattened`] built its rows under.
///
/// A path `flattened` named that `table` does not carry, at any depth,
/// reads as `Value::Null`: an unset leaf, not a fault.
#[must_use]
pub(crate) fn flatten_values(
    table: &Map<String, Value>,
    paths: &BTreeMap<String, Vec<String>>,
) -> Map<String, Value> {
    paths
        .iter()
        .map(|(dotted, path)| (dotted.clone(), value_at(table, path)))
        .collect()
}

fn value_at(table: &Map<String, Value>, path: &[String]) -> Value {
    let Some((first, rest)) = path.split_first() else {
        return Value::Null;
    };
    let Some(value) = table.get(first) else {
        return Value::Null;
    };
    if rest.is_empty() {
        return value.clone();
    }
    match value.as_object() {
        Some(nested) => value_at(nested, rest),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::fixtures::props;
    use super::*;
    use crate::lookout::field::ListItem;

    /// A per-sheep schema three tables deep, through a `$ref` chain, the
    /// way a dog's own `--schema` answer nests one: `models` names
    /// `Models`, which names `Worker` through its own `$ref`. `merge` is a
    /// closed choice, `tags` an array, `labels` a scalar map, and `a.b` is
    /// a property literally named with a dot, to prove the join does not
    /// re-split it.
    fn sheep_schema() -> Value {
        json!({
            "$ref": "#/$defs/ProjectSettings",
            "$defs": {
                "ProjectSettings": {
                    "type": "object",
                    "properties": {
                        "concurrency": { "type": "integer" },
                        "merge": { "enum": ["ask", "auto"] },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "labels": {
                            "type": "object",
                            "additionalProperties": { "type": "string" },
                        },
                        "a.b": { "type": "string" },
                        "models": { "$ref": "#/$defs/Models" },
                    },
                },
                "Models": {
                    "type": "object",
                    "properties": {
                        "worker": { "$ref": "#/$defs/Worker" },
                    },
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

    #[test]
    fn a_nested_table_three_deep_through_a_ref_chain_becomes_dotted_rows() {
        let flat = flattened(&sheep_schema());
        assert!(flat.fields.by_key("models.worker.model").is_some());
        assert!(flat.fields.by_key("models.worker.token").is_some());
        assert_eq!(
            flat.paths.get("models.worker.model").map(Vec::as_slice),
            Some(["models", "worker", "model"].map(str::to_owned).as_slice())
        );
    }

    #[test]
    fn a_leaf_keeps_its_kind_and_its_secret_mark() {
        let flat = flattened(&sheep_schema());
        assert_eq!(
            flat.fields.by_key("concurrency").unwrap().kind,
            FieldKind::Integer
        );
        assert_eq!(
            flat.fields.by_key("merge").unwrap().kind,
            FieldKind::Choice(vec!["ask".to_owned(), "auto".to_owned()])
        );
        assert!(flat.fields.by_key("models.worker.token").unwrap().secret);
        assert!(!flat.fields.by_key("models.worker.model").unwrap().secret);
    }

    #[test]
    fn a_scalar_map_stays_one_opaque_row() {
        let flat = flattened(&sheep_schema());
        let labels = flat.fields.by_key("labels").unwrap();
        assert_eq!(labels.kind, FieldKind::Opaque);
        assert!(!labels.editable);
    }

    #[test]
    fn a_list_of_strings_or_integers_stays_a_list_row() {
        let flat = flattened(&sheep_schema());
        let tags = flat.fields.by_key("tags").unwrap();
        assert_eq!(tags.kind, FieldKind::List(ListItem::Text));
        assert!(tags.editable);

        let flat = flattened(&json!({
            "type": "object",
            "properties": {
                "ports": { "type": "array", "items": { "type": "integer" } },
            },
        }));
        assert_eq!(
            flat.fields.by_key("ports").unwrap().kind,
            FieldKind::List(ListItem::Integer)
        );
    }

    /// A dog's `Vec<NonBlank>` names its items through a `$ref`, one table
    /// down.
    #[test]
    fn a_list_inside_a_nested_table_with_ref_items_is_a_list_row() {
        let flat = flattened(&json!({
            "$ref": "#/$defs/Settings",
            "$defs": {
                "Settings": {
                    "type": "object",
                    "properties": { "worker": { "$ref": "#/$defs/Worker" } },
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
                "NonBlank": { "type": "string" },
            },
        }));
        let domains = flat.fields.by_key("worker.allowed_domains").unwrap();
        assert_eq!(domains.kind, FieldKind::List(ListItem::Text));
        assert!(domains.editable);
    }

    #[test]
    fn an_array_of_tables_stays_one_opaque_row() {
        let schema = json!({
            "$ref": "#/$defs/Root",
            "$defs": {
                "Root": {
                    "type": "object",
                    "properties": {
                        "hosts": {
                            "type": "array",
                            "items": { "$ref": "#/$defs/Host" },
                        },
                    },
                },
                "Host": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                },
            },
        });
        let flat = flattened(&schema);
        assert_eq!(flat.fields.by_key("hosts").unwrap().kind, FieldKind::Opaque);
        assert!(!flat.fields.by_key("hosts").unwrap().editable);
    }

    #[test]
    fn a_property_literally_named_with_a_dot_keeps_a_one_element_path() {
        let flat = flattened(&sheep_schema());
        assert_eq!(
            flat.paths.get("\"a.b\"").map(Vec::as_slice),
            Some(["a.b"].map(str::to_owned).as_slice())
        );
        assert!(flat.fields.by_key("\"a.b\"").is_some());
    }

    #[test]
    fn a_literal_dotted_key_and_a_nested_one_are_two_rows() {
        let flat = flattened(&json!({
            "type": "object",
            "properties": {
                "a.b": { "type": "string" },
                "a": { "type": "object", "properties": { "b": { "type": "integer" } } },
            },
        }));
        assert_eq!(flat.paths.get("\"a.b\"").map(Vec::len), Some(1));
        assert_eq!(flat.paths.get("a.b").map(Vec::len), Some(2));
        assert_eq!(flat.fields.len(), 2);
    }

    /// A dog's schema comes out of a stranger's binary, and a type that
    /// holds itself would otherwise recurse until the stack overflows.
    #[test]
    fn a_table_that_refers_to_itself_stops_at_one_read_only_row() {
        let flat = flattened(&json!({
            "$ref": "#/$defs/Node",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "child": { "anyOf": [{ "$ref": "#/$defs/Node" }, { "type": "null" }] },
                    },
                },
            },
        }));
        let child = flat.fields.by_key("child").expect("the loop is one row");
        assert_eq!(child.kind, FieldKind::Opaque);
        assert!(!child.editable);
        assert!(flat.fields.by_key("name").is_some());
        assert_eq!(flat.fields.len(), 2);
    }

    /// `deny_unknown_fields` on a nested settings struct emits
    /// `additionalProperties: false`, which closes the table rather than
    /// making it a map, so its fields still become rows.
    #[test]
    fn a_closed_nested_table_still_becomes_dotted_rows() {
        let flat = flattened(&json!({
            "type": "object",
            "properties": {
                "hours": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "start": { "type": "string" } },
                },
            },
        }));
        let start = flat.fields.by_key("hours.start").expect("a row per field");
        assert!(start.editable);
        assert!(flat.fields.by_key("hours").is_none());
    }

    #[test]
    fn a_loop_below_the_root_stops_where_it_closes() {
        let flat = flattened(&json!({
            "type": "object",
            "properties": { "tree": { "$ref": "#/$defs/Node" } },
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "child": { "anyOf": [{ "$ref": "#/$defs/Node" }, { "type": "null" }] },
                    },
                },
            },
        }));
        let keys: Vec<&str> = flat
            .fields
            .fields()
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(keys, ["tree.child", "tree.name"]);
    }

    /// `#[shep(secret)]` on a struct field puts the marker beside the
    /// field's `$ref` (or its `anyOf`, for an `Option`), and none on the
    /// struct's own fields, so each row the table flattens into is masked
    /// by the table's mark, however deep.
    #[test]
    fn a_secret_table_masks_every_row_it_flattens_into() {
        let flat = flattened(&json!({
            "type": "object",
            "properties": {
                "creds": { "$ref": "#/$defs/Creds", "x-shep-secret": true },
                "backup": {
                    "anyOf": [{ "$ref": "#/$defs/Creds" }, { "type": "null" }],
                    "x-shep-secret": true,
                },
                "plain": { "$ref": "#/$defs/Creds" },
            },
            "$defs": {
                "Creds": {
                    "type": "object",
                    "properties": {
                        "user": { "type": "string" },
                        "tls": { "$ref": "#/$defs/Tls" },
                    },
                },
                "Tls": {
                    "type": "object",
                    "properties": { "key": { "type": "string" } },
                },
            },
        }));
        for key in [
            "creds.user",
            "creds.tls.key",
            "backup.user",
            "backup.tls.key",
        ] {
            assert!(flat.fields.by_key(key).expect(key).secret, "{key}");
        }
        for key in ["plain.user", "plain.tls.key"] {
            assert!(!flat.fields.by_key(key).expect(key).secret, "{key}");
        }
    }

    /// A read-only row draws its value as JSON, so a secret inside it has
    /// to mask the whole row: the marker sits on an item's own field, or on
    /// the `$defs` entry a property names, never on the row itself.
    #[test]
    fn a_row_that_reaches_a_secret_is_masked_whole() {
        let flat = flattened(&json!({
            "type": "object",
            "properties": {
                "hosts": { "type": "array", "items": { "$ref": "#/$defs/Host" } },
                "pools": { "type": "array", "items": { "$ref": "#/$defs/Pool" } },
                "token": { "$ref": "#/$defs/Token" },
                "names": { "type": "array", "items": { "type": "string" } },
            },
            "$defs": {
                "Host": {
                    "type": "object",
                    "properties": { "key": { "type": "string", "x-shep-secret": true } },
                },
                "Pool": {
                    "type": "object",
                    "properties": { "primary": { "$ref": "#/$defs/Host" } },
                },
                "Token": { "type": "string", "x-shep-secret": true },
            },
        }));
        assert!(flat.fields.by_key("hosts").unwrap().secret);
        assert!(flat.fields.by_key("pools").unwrap().secret, "two hops");
        assert!(flat.fields.by_key("token").unwrap().secret);
        assert!(!flat.fields.by_key("names").unwrap().secret);
    }

    #[test]
    fn flatten_values_reads_a_table_by_the_same_dotted_keys() {
        let flat = flattened(&sheep_schema());
        let table = props(json!({
            "concurrency": 2,
            "models": { "worker": { "model": "gpt", "token": "hunter2" } },
            "a.b": "literal",
        }));

        let values = flatten_values(&table, &flat.paths);
        assert_eq!(values.get("concurrency"), Some(&json!(2)));
        assert_eq!(values.get("models.worker.model"), Some(&json!("gpt")));
        assert_eq!(values.get("models.worker.token"), Some(&json!("hunter2")));
        assert_eq!(values.get("\"a.b\""), Some(&json!("literal")));
        assert_eq!(
            values.get("merge"),
            Some(&Value::Null),
            "a path the table does not carry reads as null, not a fault"
        );

        let scalar = props(json!({ "models": "gpt" }));
        let values = flatten_values(&scalar, &flat.paths);
        assert_eq!(
            values.get("models.worker.model"),
            Some(&Value::Null),
            "a scalar where a table belongs reads as unset below it"
        );
    }
}
