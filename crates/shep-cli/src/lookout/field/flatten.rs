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
//!
//! The rows are flat and keyed by the dotted path, so a write never needs
//! the nesting. The nesting is kept beside them as an [`Outline`], which is
//! what a pane draws its rows from.

use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    /// The tables the rows nest in, in the order `properties` iterates.
    pub(crate) outline: Outline,
    /// For each [`FieldKind::Entries`] row, by its dotted key, the item
    /// schema flattened the same way. Empty inside one of these, so an
    /// entry's own entries stop at a count and a type that holds itself
    /// does not recurse.
    pub(crate) shapes: BTreeMap<String, Flattened>,
}

/// The nesting [`flattened`] folded into dotted rows.
///
/// An arena rather than a tree of boxes, so a pane names a table by an index
/// that stays valid while it is open or closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Outline {
    /// Every nested table, parents before their children.
    pub(crate) tables: Vec<TableNode>,
    /// What sits at the top level, in order.
    pub(crate) roots: Vec<Child>,
}

impl Outline {
    /// The dotted path of every table at the top level.
    pub(crate) fn top_level_paths(&self) -> BTreeSet<String> {
        self.tables
            .iter()
            .filter(|table| table.parent.is_none())
            .map(|table| table.path.clone())
            .collect()
    }
}

/// One nested table of an [`Outline`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableNode {
    /// Its own property name, as one segment of a dotted key: quoted when
    /// TOML would quote it.
    pub(crate) key: String,
    /// Its full dotted key, the one its leaves' keys start with.
    pub(crate) path: String,
    /// How many tables sit above it.
    pub(crate) depth: usize,
    /// The table it sits in, or [`None`] at the top level.
    pub(crate) parent: Option<usize>,
    /// What sits directly in it, in order.
    pub(crate) children: Vec<Child>,
}

/// What sits in a table, or at the top level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Child {
    /// A leaf, by its index in [`Flattened::fields`].
    Leaf(usize),
    /// A nested table, by its index in [`Outline::tables`].
    Table(usize),
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
/// no shape this flatten can write back into by path. So is a secret list,
/// since the list sub-screen seeds its editor with the stored elements and
/// draws what is typed unmasked. An array of tables is already
/// [`FieldKind::Opaque`]; any other array of strings or integers is a
/// [`FieldKind::List`], filed whole.
#[must_use]
pub(crate) fn flattened(schema: &Value) -> Flattened {
    flatten_schema(schema, true, false, false)
}

/// [`flattened`], with a map of scalars left as an editable
/// [`FieldKind::Map`] row instead of read-only [`FieldKind::Opaque`].
///
/// For the one caller whose writes go by path into the document itself,
/// where an entry of a map has a path of its own: `[interpreters]`.
#[must_use]
pub(crate) fn flattened_with_maps(schema: &Value) -> Flattened {
    flatten_schema(schema, true, false, true)
}

/// [`flattened`], with `shapes` filled in only when `with_shapes` is set.
/// `secret_above` marks every leaf, for an entry shape whose field sits
/// under a secret. `keep_maps` leaves a [`FieldKind::Map`] leaf alone.
fn flatten_schema(
    schema: &Value,
    with_shapes: bool,
    secret_above: bool,
    keep_maps: bool,
) -> Flattened {
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

    let mut walker = Walker {
        defs: &defs,
        open: vec![root],
        fields: Vec::new(),
        paths: BTreeMap::new(),
        tables: Vec::new(),
        shapes: BTreeMap::new(),
        with_shapes,
        keep_maps,
    };
    let mut roots = walker.walk(
        &properties,
        &[],
        None,
        secret_above || marked(schema) || marked(root),
    );
    let Walker {
        fields,
        paths,
        tables,
        shapes,
        ..
    } = walker;

    // The leaves were numbered in walk order. The set sorts by group, which
    // a dog's schema does not set, but a schema that did would move them.
    let walked: Vec<String> = fields.iter().map(|field| field.key.clone()).collect();
    let fields = FieldSet::from_fields(fields, &[]);
    let sorted: HashMap<&str, usize> = fields
        .fields()
        .iter()
        .enumerate()
        .map(|(index, field)| (field.key.as_str(), index))
        .collect();
    let renumber = |children: &mut Vec<Child>| {
        for child in children {
            if let Child::Leaf(index) = child
                && let Some(&sorted_index) =
                    walked.get(*index).and_then(|key| sorted.get(key.as_str()))
            {
                *index = sorted_index;
            }
        }
    };
    let mut tables = tables;
    renumber(&mut roots);
    for table in &mut tables {
        renumber(&mut table.children);
    }

    Flattened {
        fields,
        paths,
        outline: Outline { tables, roots },
        shapes,
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
pub(crate) fn segment(key: &str) -> String {
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

/// What [`Walker::walk`] builds up as it descends.
struct Walker<'a> {
    defs: &'a Map<String, Value>,
    /// The nested tables on the current path, by identity: a schema is
    /// finite, so only a `$ref` back to one of them can loop, and that table
    /// is shown as one read-only leaf rather than walked again.
    open: Vec<&'a Value>,
    fields: Vec<Field>,
    paths: BTreeMap<String, Vec<String>>,
    tables: Vec<TableNode>,
    shapes: BTreeMap<String, Flattened>,
    /// Whether an [`FieldKind::Entries`] leaf gets a shape of its own.
    with_shapes: bool,
    /// Whether a [`FieldKind::Map`] leaf stays one.
    keep_maps: bool,
}

impl<'a> Walker<'a> {
    /// Walks `properties`, returning what sits directly in them.
    ///
    /// `inherited` is whether a table on the current path is marked secret.
    /// `#[shep(secret)]` on a struct field sits beside its `$ref`, not on
    /// the struct's own fields, so every row the table flattens into
    /// carries it.
    fn walk(
        &mut self,
        properties: &'a Map<String, Value>,
        prefix: &[String],
        parent: Option<usize>,
        inherited: bool,
    ) -> Vec<Child> {
        let mut children = Vec::new();
        for (key, property_schema) in properties {
            let mut path = prefix.to_vec();
            path.push(key.clone());
            let dotted = path
                .iter()
                .map(|key| segment(key))
                .collect::<Vec<_>>()
                .join(".");

            let resolved = super::schema::resolved(property_schema, self.defs);
            let looped = self.open.iter().any(|seen| core::ptr::eq(*seen, resolved));
            if !looped
                && is_nested_table(resolved)
                && let Some(nested) = resolved.get("properties").and_then(Value::as_object)
            {
                let secret = inherited || marked(property_schema) || marked(resolved);
                let index = self.tables.len();
                self.tables.push(TableNode {
                    key: segment(key),
                    path: dotted,
                    depth: prefix.len(),
                    parent,
                    children: Vec::new(),
                });
                self.open.push(resolved);
                let below = self.walk(nested, &path, Some(index), secret);
                self.open.pop();
                self.tables[index].children = below;
                children.push(Child::Table(index));
                continue;
            }

            let mut field = super::field_from(key, property_schema, self.defs);
            field.key.clone_from(&dotted);
            // `field_from` already masks a row with a secret anywhere
            // beneath it; what it cannot see is a secret table above this
            // one.
            field.secret |= inherited;
            let secret_list = field.secret && matches!(field.kind, FieldKind::List(_));
            if looped || secret_list || (field.kind == FieldKind::Map && !self.keep_maps) {
                field.kind = FieldKind::Opaque;
                field.editable = false;
            }
            if self.with_shapes
                && matches!(field.kind, FieldKind::Entries(_))
                && let Some(item) = super::schema::entry_schema(property_schema, self.defs)
            {
                let mut item = item.clone();
                if let Some(table) = item.as_object_mut() {
                    table.insert("$defs".to_owned(), Value::Object(self.defs.clone()));
                }
                let above = inherited
                    || marked(property_schema)
                    || marked(resolved)
                    || super::schema::entry_slot_marked(property_schema, self.defs);
                self.shapes
                    .insert(dotted.clone(), flatten_schema(&item, false, above, false));
            }
            children.push(Child::Leaf(self.fields.len()));
            self.fields.push(field);
            self.paths.insert(dotted, path);
        }
        children
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
    use crate::lookout::field::{Entries, ListItem};

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

    /// A list of `Option<String>` under a nested table is one read-only
    /// row, and a plain string list beside it stays a list row.
    #[test]
    fn a_nullable_item_list_under_a_nested_table_stays_one_read_only_row() {
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
                        "maybe": {
                            "type": "array",
                            "items": { "type": ["string", "null"] },
                        },
                        "plain": { "type": "array", "items": { "type": "string" } },
                    },
                },
            },
        }));
        let maybe = flat.fields.by_key("worker.maybe").unwrap();
        assert_eq!(maybe.kind, FieldKind::Opaque);
        assert!(!maybe.editable);
        let plain = flat.fields.by_key("worker.plain").unwrap();
        assert_eq!(plain.kind, FieldKind::List(ListItem::Text));
        assert!(plain.editable);
    }

    /// An array or a map of tables is one read-only `Entries` row, and its
    /// item schema is flattened beside it, so a pane can draw an entry as
    /// the rows of that schema.
    #[test]
    fn an_array_or_map_of_tables_is_one_read_only_row_with_its_item_shape() {
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
                        "models": {
                            "type": "object",
                            "additionalProperties": { "$ref": "#/$defs/Host" },
                        },
                    },
                },
                "Host": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "limits": {
                            "type": "object",
                            "properties": { "rate": { "type": "integer" } },
                        },
                        "peers": { "type": "array", "items": { "$ref": "#/$defs/Host" } },
                    },
                },
            },
        });
        let flat = flattened(&schema);
        for (key, entries) in [("hosts", Entries::Array), ("models", Entries::Map)] {
            let field = flat.fields.by_key(key).expect(key);
            assert_eq!(field.kind, FieldKind::Entries(entries), "{key}");
            assert!(!field.editable, "{key}");
            let item = flat.shapes.get(key).expect(key);
            let keys: Vec<&str> = item
                .fields
                .fields()
                .iter()
                .map(|field| field.key.as_str())
                .collect();
            assert_eq!(keys, ["limits.rate", "name", "peers"], "{key}");
            assert_eq!(item.outline.tables.len(), 1, "{key}");
        }
        let peers = flat.shapes["hosts"].fields.by_key("peers").expect("peers");
        assert_eq!(peers.kind, FieldKind::Entries(Entries::Array));
        assert!(
            flat.shapes["hosts"].shapes.is_empty(),
            "an entry's own entries are a count and nothing deeper"
        );
    }

    // The item shape is flattened on its own, so a mark above it has to be
    // handed in: on the field, on a table holding the field, or beside the
    // `$ref` on `items` or `additionalProperties`.
    #[test]
    fn a_secret_mark_above_the_entries_masks_every_leaf_of_the_item_shape() {
        let client = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "key": { "type": "string" },
                "limits": { "type": "object", "properties": { "rate": { "type": "integer" } } },
            },
        });
        let on_field = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/Client" },
            "x-shep-secret": true,
        });
        let beside_ref = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/Client", "x-shep-secret": true },
        });
        let on_map_slot = json!({
            "type": "object",
            "additionalProperties": { "$ref": "#/$defs/Client", "x-shep-secret": true },
        });
        let plain = json!({ "type": "array", "items": { "$ref": "#/$defs/Client" } });
        let schemas = [
            ("on the field", json!({ "clients": on_field })),
            ("beside the ref", json!({ "clients": beside_ref })),
            ("beside the map ref", json!({ "clients": on_map_slot })),
            (
                "on a table above",
                json!({
                    "vault": {
                        "type": "object",
                        "x-shep-secret": true,
                        "properties": { "clients": plain.clone() },
                    },
                }),
            ),
        ];
        for (case, properties) in schemas {
            let flat = flattened(&json!({
                "type": "object",
                "properties": properties,
                "$defs": { "Client": client.clone() },
            }));
            let key = if case == "on a table above" {
                "vault.clients"
            } else {
                "clients"
            };
            let item = flat.shapes.get(key).expect(case);
            assert!(item.fields.fields().len() >= 3, "{case}");
            for field in item.fields.fields() {
                assert!(field.secret, "{case}: {} must be masked", field.key);
            }
        }
        let flat = flattened(&json!({
            "type": "object",
            "properties": { "clients": plain },
            "$defs": { "Client": client },
        }));
        assert!(
            flat.shapes["clients"]
                .fields
                .fields()
                .iter()
                .all(|field| !field.secret),
            "an unmarked field masks nothing"
        );
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

    /// The list sub-screen seeds its editor with the stored element and
    /// draws what is typed unmasked, so a secret list has no editor.
    #[test]
    fn a_secret_list_stays_one_read_only_row() {
        let flat = flattened(&json!({
            "type": "object",
            "properties": {
                "keys": {
                    "type": "array",
                    "items": { "type": "string" },
                    "x-shep-secret": true,
                },
                "creds": { "$ref": "#/$defs/Creds", "x-shep-secret": true },
                "plain": { "type": "array", "items": { "type": "string" } },
            },
            "$defs": {
                "Creds": {
                    "type": "object",
                    "properties": {
                        "tokens": { "type": "array", "items": { "type": "string" } },
                    },
                },
            },
        }));
        for key in ["keys", "creds.tokens"] {
            let field = flat.fields.by_key(key).expect(key);
            assert_eq!(field.kind, FieldKind::Opaque, "{key}");
            assert!(!field.editable, "{key}");
        }
        assert_eq!(
            flat.fields.by_key("plain").unwrap().kind,
            FieldKind::List(ListItem::Text)
        );
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

    /// The keys of `outline`'s roots and below, depth first, a table as
    /// `[key]` and its children after it, a leaf as the key of the field it
    /// indexes.
    fn shape(flat: &Flattened) -> Vec<String> {
        fn visit(flat: &Flattened, children: &[Child], out: &mut Vec<String>) {
            for child in children {
                match *child {
                    Child::Leaf(index) => out.push(flat.fields.fields()[index].key.clone()),
                    Child::Table(index) => {
                        let table = &flat.outline.tables[index];
                        out.push(format!("[{}]", table.path));
                        visit(flat, &table.children, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        visit(flat, &flat.outline.roots, &mut out);
        out
    }

    #[test]
    fn the_outline_follows_property_order_through_a_table_two_levels_down() {
        let flat = flattened(&sheep_schema());
        assert_eq!(
            shape(&flat),
            [
                "\"a.b\"",
                "concurrency",
                "labels",
                "merge",
                "[models]",
                "[models.worker]",
                "models.worker.model",
                "models.worker.token",
                "tags",
            ]
        );
    }

    #[test]
    fn an_outline_table_holds_its_last_segment_its_depth_and_its_parent() {
        let flat = flattened(&sheep_schema());
        let tables = &flat.outline.tables;
        assert_eq!(tables.len(), 2);
        assert_eq!(
            (tables[0].key.as_str(), tables[0].depth, tables[0].parent),
            ("models", 0, None)
        );
        assert_eq!(
            (tables[1].key.as_str(), tables[1].depth, tables[1].parent),
            ("worker", 1, Some(0))
        );
        assert_eq!(tables[1].path, "models.worker");
    }

    /// The outline orders the rows; `paths` and `by_key` still answer by the
    /// dotted key alone, one entry per leaf and none per table.
    #[test]
    fn a_table_adds_no_path_and_no_field() {
        let flat = flattened(&sheep_schema());
        assert_eq!(flat.fields.len(), 7);
        assert_eq!(flat.paths.len(), 7);
        assert!(flat.paths.contains_key("models.worker.model"));
        assert!(flat.fields.by_key("models").is_none());
        assert!(flat.fields.by_key("models.worker").is_none());
    }

    #[test]
    fn a_loop_is_one_leaf_in_the_outline_and_no_table() {
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
        assert_eq!(shape(&flat), ["child", "name"]);
        assert!(flat.outline.tables.is_empty());
    }

    #[test]
    fn an_outline_leaf_indexes_its_field_after_the_groups_sort() {
        let flat = flattened(&json!({
            "type": "object",
            "properties": {
                "a": { "type": "string" },
                "b": { "type": "string", "init": { "group": "late" } },
                "t": { "type": "object", "properties": { "c": { "type": "string" } } },
            },
        }));
        let keys: Vec<&str> = flat
            .fields
            .fields()
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(
            keys,
            ["b", "a", "t.c"],
            "the set sorts the grouped field first"
        );
        assert_eq!(shape(&flat), ["a", "b", "[t]", "t.c"]);
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
