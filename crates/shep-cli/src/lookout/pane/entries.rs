//! The rows under an array or map of tables on a dog's pane.
//!
//! A field of [`FieldKind::Entries`] is one row, and the entries under it
//! are rows the schema alone cannot name: how many there are and what each
//! is called come from the values. They are built once, when the pane is,
//! into an arena the pane's rows index, the way its outline indexes tables.
//! A field in an entry edits like any other row, filed under its full path
//! from the section root; see [`EntryBody::Leaf`].

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use super::super::field::{
    Child, Entries, Field, FieldKind, FieldSet, Flattened, flatten_values, segment,
};
use super::fields::{entries_text, render_json};
use super::{PaneRow, PathStep};

/// Every row under an [`FieldKind::Entries`] field of one pane.
///
/// No `Debug` (IR-41): the rows hold the values the pane withholds.
#[derive(Clone, Default)]
pub(super) struct EntryRows {
    /// The item schema of each entries field, by the field's dotted key,
    /// kept so [`Self::rebuilt`] can read new values.
    shapes: Arc<BTreeMap<String, Flattened>>,
    rows: Vec<EntryRow>,
    /// The entries directly under each [`FieldKind::Entries`] field, by the
    /// field's index, in the order they are drawn.
    roots: BTreeMap<usize, Vec<usize>>,
    /// The arena row of each [`EntryBody::Leaf`], by its key.
    leaves: BTreeMap<String, usize>,
    /// The [`EntryBody::New`] row under each entries field, by the field's
    /// index. It is not one of `roots`: it is no entry, and `d` and the
    /// cursor's landing count entries only.
    news: BTreeMap<usize, usize>,
    /// What the section holds for each entry, by the entry's path, which is
    /// the identity an edit under an array position is checked against.
    items: BTreeMap<String, Value>,
}

/// One row under an [`FieldKind::Entries`] field.
///
/// No `Debug`, for the reason [`EntryRows`] gives.
#[derive(Clone)]
pub(in crate::lookout) struct EntryRow {
    /// How many rows it is indented under, the same unit as a field's.
    pub(in crate::lookout) depth: usize,
    /// The entry's key, name or `#<index>`; for anything inside an entry,
    /// the property's last segment.
    pub(in crate::lookout) label: String,
    /// What the row is.
    pub(in crate::lookout) body: EntryBody,
    /// The row it sits under, for `←`.
    pub(super) parent: PaneRow,
}

/// What an [`EntryRow`] is.
#[derive(Clone)]
pub(in crate::lookout) enum EntryBody {
    /// An entry, or a table inside one. Opens and closes.
    Node {
        /// Its key in the pane's open set.
        path: String,
        /// The arena rows directly under it, in order.
        children: Vec<usize>,
        /// Whether it is an entry, and not a table inside one.
        entry: bool,
        /// Where the entry is in its entries field: its index in an array,
        /// its key in a map. [`None`] for a table inside an entry.
        step: Option<PathStep>,
    },
    /// The `+ new` row at the foot of an open entries field, which adds an
    /// entry. Holds no value and does not open.
    New {
        /// The entries field it adds to, by its index in the pane's fields.
        field: usize,
    },
    /// One of the item schema's own properties.
    Leaf {
        /// The property, for its type. Its key is the row's full path from
        /// the section root (`clients[1].limits.rate`), each segment quoted
        /// as [`segment`] quotes it, so an edit files into the pane's one
        /// [`Edits`](super::super::edits::Edits) set under it.
        field: Box<Field>,
        /// What the section holds there, or [`Value::Null`].
        stored: Value,
        /// Where the property is in the section.
        at: Vec<PathStep>,
        /// How many leading steps of `at` name the entry, which a write
        /// finds and never creates.
        entry_len: usize,
        /// The path of the entry it is in, a key of [`EntryRows::items`].
        entry: String,
        /// The value as a row draws it: a count for a nested
        /// [`FieldKind::Entries`].
        text: String,
    },
}

impl EntryRows {
    /// The rows under every [`FieldKind::Entries`] field in `fields`.
    ///
    /// `paths` and `values` are the pane's own, keyed by the fields' dotted
    /// keys; `shapes` is [`Flattened::shapes`].
    pub(super) fn build(
        fields: &FieldSet,
        paths: &BTreeMap<String, Vec<String>>,
        values: &Map<String, Value>,
        shapes: BTreeMap<String, Flattened>,
    ) -> Self {
        Self::built(fields, paths, values, Arc::new(shapes))
    }

    /// These rows over `values`, the shapes unchanged.
    pub(super) fn rebuilt(
        &self,
        fields: &FieldSet,
        paths: &BTreeMap<String, Vec<String>>,
        values: &Map<String, Value>,
    ) -> Self {
        Self::built(fields, paths, values, Arc::clone(&self.shapes))
    }

    fn built(
        fields: &FieldSet,
        paths: &BTreeMap<String, Vec<String>>,
        values: &Map<String, Value>,
        shapes: Arc<BTreeMap<String, Flattened>>,
    ) -> Self {
        let mut built = Self {
            shapes: Arc::clone(&shapes),
            ..Self::default()
        };
        for (index, field) in fields.fields().iter().enumerate() {
            let FieldKind::Entries(kind) = field.kind else {
                continue;
            };
            let (Some(shape), Some(path)) = (shapes.get(&field.key), paths.get(&field.key)) else {
                continue;
            };
            let at = Where {
                kind,
                shape,
                base: &field.key,
                steps: path.iter().cloned().map(PathStep::Key).collect(),
                depth: path.len(),
                parent: PaneRow::Field(index),
            };
            let roots = built.push_entries(&at, values.get(&field.key));
            built.roots.insert(index, roots);
            built.news.insert(index, built.rows.len());
            built.rows.push(EntryRow {
                depth: at.depth,
                label: NEW_LABEL.to_owned(),
                body: EntryBody::New { field: index },
                parent: PaneRow::Field(index),
            });
        }
        built
    }

    /// Row `index`.
    pub(super) fn row(&self, index: usize) -> Option<&EntryRow> {
        self.rows.get(index)
    }

    /// The entries directly under field `field`, or [`None`] when it is not
    /// an entries field with a shape.
    pub(super) fn roots(&self, field: usize) -> Option<&[usize]> {
        self.roots.get(&field).map(Vec::as_slice)
    }

    /// The field whose key is `key`, with where it is in the section, when
    /// `key` names a row under an entries field.
    pub(super) fn leaf(&self, key: &str) -> Option<EntryLeaf<'_>> {
        match &self.rows.get(*self.leaves.get(key)?)?.body {
            EntryBody::Leaf {
                field,
                stored,
                at,
                entry_len,
                ..
            } => Some(EntryLeaf {
                field,
                stored,
                at,
                entry_len: *entry_len,
            }),
            EntryBody::Node { .. } | EntryBody::New { .. } => None,
        }
    }

    /// Whether the edit filed under `key` still names the entry it was made
    /// in, once `next` holds the section as it is now.
    ///
    /// An entry of an array is named by its position, so it is the same entry
    /// only when `next` holds the same table at that position. An entry of a
    /// map is named by its key and always is. A key that names no leaf here
    /// is not an entry's edit and always stays.
    pub(super) fn names_same_entry(&self, key: &str, next: &Self) -> bool {
        let Some(EntryBody::Leaf { at, entry, .. }) = self.leaf_body(key) else {
            return true;
        };
        let by_position = at.iter().any(|step| matches!(step, PathStep::Index(_)));
        !by_position || self.items.get(entry) == next.items.get(entry)
    }

    /// The body of the leaf whose key is `key`.
    pub(super) fn leaf_body(&self, key: &str) -> Option<&EntryBody> {
        self.rows.get(*self.leaves.get(key)?).map(|row| &row.body)
    }

    /// The place in the section of entry row `row`, and the entries field it
    /// is directly under. [`None`] for a row that is not an entry.
    pub(super) fn entry_place(&self, row: usize) -> Option<(usize, &PathStep)> {
        let entry = self.rows.get(row)?;
        match (&entry.body, entry.parent) {
            (
                EntryBody::Node {
                    entry: true,
                    step: Some(step),
                    ..
                },
                PaneRow::Field(field),
            ) => Some((field, step)),
            _ => None,
        }
    }

    /// The open-set key of every entry and table inside one.
    pub(super) fn node_paths(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().filter_map(|row| match &row.body {
            EntryBody::Node { path, .. } => Some(path.as_str()),
            EntryBody::Leaf { .. } | EntryBody::New { .. } => None,
        })
    }

    /// The `+ new` row under field `field`, or [`None`] when it is not an
    /// entries field with a shape.
    pub(super) fn new_row(&self, field: usize) -> Option<usize> {
        self.news.get(&field).copied()
    }

    /// The entries field a `+ new` row adds to, or [`None`] when `row` is
    /// not one.
    pub(super) fn new_field(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)?.body {
            EntryBody::New { field } => Some(field),
            _ => None,
        }
    }

    /// The entry row directly under field `field` that sits at `step`.
    pub(super) fn entry_at(&self, field: usize, step: &PathStep) -> Option<usize> {
        self.roots(field)?
            .iter()
            .copied()
            .find(|&root| self.entry_place(root).is_some_and(|(_, held)| held == step))
    }

    /// What a new entry of the entries field `key` holds: each property of
    /// the item schema that declares a `default`, a nested table as an
    /// object of its own. [`None`] for a key that is not an entries field
    /// with a shape.
    pub(super) fn defaults(&self, key: &str) -> Option<Value> {
        let shape = self.shapes.get(key)?;
        let mut item = Map::new();
        for field in shape.fields.fields() {
            if let (Some(value), Some(path)) = (&field.default_value, shape.paths.get(&field.key)) {
                put_default(&mut item, path, value.clone());
            }
        }
        Some(Value::Object(item))
    }

    fn push_entries(&mut self, at: &Where<'_>, value: Option<&Value>) -> Vec<usize> {
        let mut entries: Vec<(String, String, PathStep, &Value)> = Vec::new();
        match (at.kind, value) {
            (Entries::Array, Some(Value::Array(items))) => {
                for (index, item) in items.iter().enumerate() {
                    let path = format!("{}[{index}]", at.base);
                    let label = array_label(at.shape, item, index);
                    entries.push((path, label, PathStep::Index(index), item));
                }
            }
            (Entries::Map, Some(Value::Object(map))) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    let quoted = serde_json::to_string(key).unwrap_or_default();
                    let path = format!("{}[{quoted}]", at.base);
                    entries.push((path, plain(key), PathStep::Key(key.clone()), &map[key]));
                }
            }
            _ => {}
        }
        let mut roots = Vec::new();
        for (path, label, step, item) in entries {
            let index = self.rows.len();
            self.rows.push(EntryRow {
                depth: at.depth,
                label,
                body: EntryBody::Node {
                    path: path.clone(),
                    children: Vec::new(),
                    entry: true,
                    step: Some(step.clone()),
                },
                parent: at.parent,
            });
            self.items.insert(path.clone(), item.clone());
            let table = item.as_object().cloned().unwrap_or_default();
            let values = flatten_values(&table, &at.shape.paths);
            let mut steps = at.steps.clone();
            steps.push(step);
            let children = self.push_children(
                at.shape,
                &values,
                &at.shape.outline.roots,
                &Under {
                    depth: at.depth,
                    entry: &path,
                    steps: &steps,
                    parent: PaneRow::Entry(index),
                },
            );
            self.set_children(index, children);
            roots.push(index);
        }
        roots
    }

    fn push_children(
        &mut self,
        shape: &Flattened,
        values: &Map<String, Value>,
        children: &[Child],
        under: &Under<'_>,
    ) -> Vec<usize> {
        let mut pushed = Vec::new();
        for child in children {
            let index = self.rows.len();
            match *child {
                Child::Leaf(field) => {
                    let Some(field) = shape.fields.fields().get(field) else {
                        continue;
                    };
                    let Some(path) = shape.paths.get(&field.key) else {
                        continue;
                    };
                    let value = values.get(&field.key);
                    let counted = matches!(field.kind, FieldKind::Entries(_));
                    let key = format!("{}.{}", under.entry, field.key);
                    let mut at = under.steps.to_vec();
                    at.extend(path.iter().cloned().map(PathStep::Key));
                    let mut field = field.clone();
                    field.key.clone_from(&key);
                    self.leaves.insert(key, index);
                    self.rows.push(EntryRow {
                        depth: under.depth + path.len(),
                        label: path.last().map(|last| segment(last)).unwrap_or_default(),
                        body: EntryBody::Leaf {
                            text: if counted {
                                entries_text(value)
                            } else {
                                render_json(value.unwrap_or(&Value::Null))
                            },
                            stored: value.cloned().unwrap_or(Value::Null),
                            at,
                            entry_len: under.steps.len(),
                            entry: under.entry.to_owned(),
                            field: Box::new(field),
                        },
                        parent: under.parent,
                    });
                }
                Child::Table(table) => {
                    let Some(table) = shape.outline.tables.get(table) else {
                        continue;
                    };
                    let path = format!("{}.{}", under.entry, table.path);
                    self.rows.push(EntryRow {
                        depth: under.depth + 1 + table.depth,
                        label: table.key.clone(),
                        body: EntryBody::Node {
                            path: path.clone(),
                            children: Vec::new(),
                            entry: false,
                            step: None,
                        },
                        parent: under.parent,
                    });
                    let below = self.push_children(
                        shape,
                        values,
                        &table.children,
                        &Under {
                            depth: under.depth,
                            entry: under.entry,
                            steps: under.steps,
                            parent: PaneRow::Entry(index),
                        },
                    );
                    self.set_children(index, below);
                }
            }
            pushed.push(index);
        }
        pushed
    }

    fn set_children(&mut self, index: usize, below: Vec<usize>) {
        if let Some(EntryRow {
            body: EntryBody::Node { children, .. },
            ..
        }) = self.rows.get_mut(index)
        {
            *children = below;
        }
    }
}

/// A field under an entries field, as [`EntryRows::leaf`] answers for it.
pub(super) struct EntryLeaf<'a> {
    /// The property, keyed by its full path.
    pub(super) field: &'a Field,
    /// What the section holds there, or [`Value::Null`].
    pub(super) stored: &'a Value,
    /// Where it is in the section.
    pub(super) at: &'a [PathStep],
    /// How many leading steps of `at` name the entry.
    pub(super) entry_len: usize,
}

/// What [`EntryRows::push_entries`] needs to know about the field it builds
/// under.
struct Where<'a> {
    kind: Entries,
    shape: &'a Flattened,
    /// The field's dotted key, which its entries' paths start with.
    base: &'a str,
    /// The field's own path in the section.
    steps: Vec<PathStep>,
    /// The entries' depth.
    depth: usize,
    parent: PaneRow,
}

/// Where [`EntryRows::push_children`] is in an entry.
struct Under<'a> {
    /// The entry's depth.
    depth: usize,
    /// The path of the entry the rows are in, which a leaf's key starts
    /// with.
    entry: &'a str,
    /// Where the entry is in the section.
    steps: &'a [PathStep],
    parent: PaneRow,
}

/// The row at the foot of an open entries field that adds an entry.
pub(super) const NEW_LABEL: &str = "+ new";

/// Sets `value` at `path` in `item`, making the tables on the way.
fn put_default(item: &mut Map<String, Value>, path: &[String], value: Value) {
    match path {
        [] => {}
        [last] => {
            item.insert(last.clone(), value);
        }
        [first, rest @ ..] => {
            if let Value::Object(table) = item
                .entry(first.clone())
                .or_insert_with(|| Value::Object(Map::new()))
            {
                put_default(table, rest, value);
            }
        }
    }
}

/// `text` with each control character as a space, so a label an operator's
/// data supplies cannot break the line it is drawn on.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The first of `name` and `id` that the item schema declares as a
/// non-secret string and the entry holds, else `#<index>`. A property marked
/// secret never labels a row.
fn array_label(shape: &Flattened, item: &Value, index: usize) -> String {
    ["name", "id"]
        .into_iter()
        .find_map(|key| {
            let field = shape.fields.by_key(key)?;
            if field.secret || !matches!(field.kind, FieldKind::Text | FieldKind::Suggested(_)) {
                return None;
            }
            item.get(key)?.as_str().filter(|text| !text.is_empty())
        })
        .map_or_else(|| format!("#{index}"), plain)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{entries_pane, entries_schema};
    use super::super::{ConfigPane, PaneRow};
    use super::EntryBody;
    use serde_json::json;

    /// The count a row reads, never the value: `clients` holds a credential.
    #[test]
    fn an_entries_row_reads_as_a_count() {
        let section = |text: &str| {
            ConfigPane::dog(
                "paddock".into(),
                None,
                entries_schema(),
                text.to_owned(),
                Vec::new(),
            )
        };
        let one = section("[[clients]]\nkey = \"hunter2\"\n");
        let none = section("clients = []\n");
        let absent = section("retries = 3\n");
        let table = |value: serde_json::Value| {
            let table = json!({ "clients": value });
            ConfigPane::sheep_dog(
                "web".into(),
                "paddock".into(),
                &entries_schema(),
                table.as_object().cloned().expect("a table"),
            )
        };
        assert_eq!(entries_pane().value("clients"), "3 entries");
        assert_eq!(one.value("clients"), "1 entry");
        assert_eq!(none.value("clients"), "0 entries");
        assert_eq!(absent.value("clients"), "(unset)");
        assert_eq!(table(json!(null)).value("clients"), "(unset)");
        assert_eq!(entries_pane().value("models"), "2 entries");
        assert_eq!(entries_pane().display_value("clients"), "3 entries");
    }

    /// The labels of every entry directly under `key`, once it is open.
    fn labels(pane: &mut ConfigPane, key: &str) -> Vec<String> {
        pane.move_to_key(key);
        pane.open_table();
        pane.rows()
            .into_iter()
            .filter_map(|row| match row {
                PaneRow::Entry(index) => pane.entry_row(index).filter(|entry| {
                    entry.depth == 1 && matches!(entry.body, EntryBody::Node { .. })
                }),
                _ => None,
            })
            .map(|entry| entry.label.clone())
            .collect()
    }

    #[test]
    fn a_map_is_labelled_by_its_keys_and_an_array_by_name_then_id_then_its_index() {
        assert_eq!(labels(&mut entries_pane(), "models"), ["large", "small"]);
        assert_eq!(
            labels(&mut entries_pane(), "clients"),
            ["alpha", "b-2", "#2"]
        );
    }

    #[test]
    fn a_secret_name_never_labels_an_entry() {
        let schema = json!({
            "properties": { "hosts": { "type": "array", "items": { "$ref": "#/$defs/Host" } } },
            "$defs": { "Host": { "type": "object", "properties": {
                "name": { "type": "string", "x-shep-secret": true },
            } } },
        });
        let mut pane = ConfigPane::dog(
            "paddock".into(),
            None,
            schema,
            "[[hosts]]\nname = \"hunter2\"\n".to_owned(),
            Vec::new(),
        );
        assert_eq!(labels(&mut pane, "hosts"), ["#0"]);
    }

    /// A mark on the field, not on anything inside the item: the entries
    /// still label as indexes and every leaf under them is masked.
    #[test]
    fn a_secret_entries_field_masks_unmarked_leaves_and_never_labels_by_name() {
        let schema = json!({
            "properties": { "clients": {
                "type": "array",
                "items": { "$ref": "#/$defs/Client" },
                "x-shep-secret": true,
            } },
            "$defs": { "Client": { "type": "object", "properties": {
                "name": { "type": "string" },
                "key": { "type": "string" },
            } } },
        });
        let mut pane = ConfigPane::dog(
            "paddock".into(),
            None,
            schema,
            "[[clients]]\nname = \"alpha\"\nkey = \"hunter2\"\n".to_owned(),
            Vec::new(),
        );
        assert_eq!(labels(&mut pane, "clients"), ["#0"]);
        pane.move_by(1);
        pane.open_table();
        let masked: Vec<bool> = pane
            .rows()
            .into_iter()
            .filter_map(|row| match row {
                PaneRow::Entry(index) => pane.entry_row(index),
                _ => None,
            })
            .filter_map(|row| match &row.body {
                EntryBody::Leaf { field, .. } => Some(field.secret),
                EntryBody::Node { .. } | EntryBody::New { .. } => None,
            })
            .collect();
        assert_eq!(masked, [true, true]);
    }

    #[test]
    fn an_entries_field_inside_an_entry_is_a_count_row_with_no_children() {
        let schema = json!({
            "properties": { "clients": { "type": "array", "items": { "$ref": "#/$defs/Client" } } },
            "$defs": {
                "Client": { "type": "object", "properties": {
                    "peers": { "type": "array", "items": { "$ref": "#/$defs/Peer" } },
                } },
                "Peer": { "type": "object", "properties": { "key": { "type": "string" } } },
            },
        });
        let mut pane = ConfigPane::dog(
            "paddock".into(),
            None,
            schema,
            "[[clients]]\n[[clients.peers]]\nkey = \"a\"\n[[clients.peers]]\nkey = \"b\"\n"
                .to_owned(),
            Vec::new(),
        );
        pane.move_to_key("clients");
        pane.open_table();
        pane.move_by(1);
        pane.open_table();
        let rows = pane.rows();
        assert_eq!(rows.len(), 4, "clients, #0, peers and + new: {rows:?}");
        let Some(PaneRow::Entry(peers)) = rows.get(2).copied() else {
            panic!("the third row is peers: {rows:?}");
        };
        let peers = pane.entry_row(peers).expect("the row exists");
        assert!(
            matches!(&peers.body, EntryBody::Leaf { text, .. } if text == "2 entries"),
            "a count and not a node"
        );
    }
}
