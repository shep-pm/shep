//! A form's shape, read off a JSON Schema.
//!
//! Every config pane in lookout renders one of these. A JSON Schema is
//! already a field list with types, defaults and descriptions, which is
//! exactly what a form needs, so this is the common shape rather than an
//! abstraction invented to share code. The Flockfile schema, a dog's own
//! `--schema` answer, and a hand-built list for `shep.toml` all become a
//! [`FieldSet`], and one renderer draws all three.

mod bounds;
#[cfg(test)]
mod fixtures;
mod init;
mod schema;

pub use bounds::Bounds;

use serde_json::{Map, Value};

use bounds::bounds_of;
use init::{neighbours, strings, suggestions};
use schema::{kind_of, render_default, value_kind_of};

/// What the widget for one field is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// `type: boolean`. Cycles.
    Bool,
    /// `type: integer`. Typed.
    Integer,
    /// `type: string`, or a `$ref` that resolves to one. Typed.
    Text,
    /// A closed set: `enum`, or `oneOf` of `const`s. Cycles.
    Choice(Vec<String>),
    /// `init.suggest` on a `Text` field. Cycles like a choice and types
    /// like text: the values are offered, not enforced, because the
    /// grammar stays open.
    Suggested(Vec<String>),
    /// `type: object` with `additionalProperties`. Opens a sub-screen.
    Map,
    /// `type: array` of a shape the editor can parse back. Opens a list
    /// sub-screen.
    List(ListItem),
    /// Anything else, including a nested object. Read-only, shown as JSON.
    Opaque,
}

/// What an array's elements are, so the editor can parse one back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListItem {
    /// `items: {type: string}`. Each element is typed as written.
    Text,
    /// `items: {type: integer}`. Each element is parsed back to a number.
    Integer,
}

/// Which of shep-core's own string grammars a [`FieldKind::Text`] field
/// actually holds, so the pane can show what a bare number means instead
/// of the digits an operator typed.
///
/// Read off the schema's `$ref` name rather than guessed from the field
/// key: a dog's own schema can reuse either grammar under any name it
/// likes, and the Flockfile schema already names both types for exactly
/// this reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// `$ref: MemSize`. A bare number is bytes.
    MemSize,
    /// `$ref: UpDuration`. A bare number is milliseconds.
    UpDuration,
}

/// One field of a form.
///
/// `Debug` is derived rather than redacted (IR-41): this is a schema, and a
/// schema describes a value without carrying one. A secret's shape is not a
/// secret.
///
/// `default` is the one field that could weaken that, since it is a value
/// rather than a description of one, and it does not: a schema's `default`
/// comes from a static constant, either the committed
/// `crates/shep-core/assets/flockfile.schema.json` or a dog's own
/// `--schema` answer, which is its binary describing itself. Neither has
/// ever seen this flock. A live value reaches the pane through
/// `ConfigPane`'s own values map instead, which is why that type's `Debug`
/// is redacted by hand while this one is derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The property name, which is also the key a write carries.
    pub key: String,
    /// What the operator reads beside it: `init.blurb`, else `description`,
    /// else the key.
    pub help: String,
    /// `init.group`, where the schema assigns one.
    pub group: Option<String>,
    /// The widget.
    pub kind: FieldKind,
    /// Which shep-core grammar a [`FieldKind::Text`] field's string is,
    /// when it is one of the two the pane knows how to resolve. `None`
    /// for every other field, [`FieldKind::Text`] included.
    pub value_kind: Option<ValueKind>,
    /// The schema's own `default`, rendered as the pane will show it. `None`
    /// for an absent or `null` default.
    pub default: Option<String>,
    /// The schema's own `default`, in the shape a write carries. `None` for
    /// an absent or `null` default, the same two cases [`Self::default`]
    /// collapses to one for: a field with no schema default and a field
    /// whose default genuinely is `null` (an `Option<T>` unset by design)
    /// both restore the same way, by filing `Value::Null`, so neither needs
    /// its own entry here. Kept alongside the rendered string rather than
    /// parsed back out of it: `kill_timeout`'s default is the string
    /// `"1600"` and `args`' is `[]`, and neither round-trips through
    /// display text.
    pub default_value: Option<Value>,
    /// `x-shep-secret`. The pane shows `<set>` and never reads the value.
    pub secret: bool,
    /// Whether the pane may edit it. `false` for [`FieldKind::Opaque`], and
    /// for anything a caller marks read-only after the fact.
    pub editable: bool,
    /// `init.example`, one concrete value a reader can copy.
    pub example: Option<String>,
    /// `init.accepts`, the forms this field takes, in the operator's
    /// words. Empty when the field carries none, in which case
    /// [`super::validation::bullets`] falls back to the type table.
    pub accepts: Vec<String>,
    /// `init.refuses`, the forms it turns down. Empty is the common case.
    pub refuses: Vec<String>,
    /// `init.neighbours`, the fields this one interacts with. Empty is the
    /// common case, and an entry missing either half is dropped.
    pub neighbours: Vec<Neighbour>,
    /// What the schema says the value must satisfy, beyond its type.
    /// Checked on the keystroke that applies an edit, by
    /// [`super::validation::refusal`].
    pub bounds: Bounds,
}

/// One field this field interacts with, and how.
///
/// `Debug` is derived (IR-41): two names, no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbour {
    /// The other field's key. Tested to name a real field.
    pub field: String,
    /// What the interaction is, in one clause.
    pub note: String,
}

/// An ordered set of fields, grouped.
///
/// The groups themselves are not stored. A renderer reads each field's own
/// [`Field::group`] as it walks the list, which is what a scrolled window
/// needs anyway: a pane whose top row is the middle of `control` has to
/// draw that header from the row, not from a list of every group the set
/// has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSet {
    fields: Vec<Field>,
}

impl FieldSet {
    /// Reads a schema's `properties`, resolving one level of `$ref` into
    /// `defs`, and orders the result by `group_order`.
    ///
    /// Within a group, fields keep the order `properties` yields them.
    /// `serde_json::Map` without `preserve_order` yields alphabetical, which
    /// is what the Flockfile schema already is on disk. A field whose group
    /// is not in `group_order` sorts after every group that is; a field with
    /// no group sorts last.
    #[must_use]
    pub fn from_properties(
        properties: &Map<String, Value>,
        defs: &Map<String, Value>,
        group_order: &[&str],
    ) -> Self {
        let fields = properties
            .iter()
            .map(|(key, schema)| field_from(key, schema, defs))
            .collect();
        Self::from_fields(fields, group_order)
    }

    /// Orders an already-built list by `group_order`, for a caller that has
    /// no schema (the settings screen builds its six by hand).
    #[must_use]
    pub fn from_fields(mut fields: Vec<Field>, group_order: &[&str]) -> Self {
        // Every group `group_order` does not name, in the order it first
        // appears. Without this, all of them ranked `(1, 0)` alike, and
        // two distinct unknown groups stayed interleaved, so a renderer
        // pushing a header on every group change drew each name twice.
        let mut unknown: Vec<String> = Vec::new();
        for field in &fields {
            if let Some(group) = field.group.as_deref()
                && !group_order.contains(&group)
                && !unknown.iter().any(|seen| seen == group)
            {
                unknown.push(group.to_owned());
            }
        }
        let rank = |group: Option<&str>| -> (usize, usize) {
            match group {
                None => (2, 0),
                Some(g) => match group_order.iter().position(|known| *known == g) {
                    Some(i) => (0, i),
                    None => (
                        1,
                        unknown
                            .iter()
                            .position(|seen| seen == g)
                            .unwrap_or(usize::MAX),
                    ),
                },
            }
        };
        // Stable, so within-group order is whatever the caller gave. The
        // sort is also what makes every group contiguous, which every
        // renderer relies on.
        fields.sort_by_key(|f| rank(f.group.as_deref()));
        Self { fields }
    }

    /// Every field, in display order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The field named `key`.
    #[must_use]
    pub fn by_key(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// How many fields.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

fn field_from(key: &str, schema: &Value, defs: &Map<String, Value>) -> Field {
    let init = schema.get("init");
    let help = init
        .and_then(|i| i.get("blurb"))
        .or_else(|| schema.get("description"))
        .and_then(Value::as_str)
        .map_or_else(|| key.to_owned(), str::to_owned);
    let group = init
        .and_then(|i| i.get("group"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let kind = kind_of(schema, defs);
    let value_kind = (kind == FieldKind::Text)
        .then(|| value_kind_of(schema))
        .flatten();
    let kind = match (kind, suggestions(init)) {
        (FieldKind::Text, Some(names)) if !names.is_empty() => FieldKind::Suggested(names),
        (kind, _) => kind,
    };
    let editable = kind != FieldKind::Opaque;
    let example = init
        .and_then(|i| i.get("example"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Field {
        key: key.to_owned(),
        help,
        group,
        kind,
        value_kind,
        default: render_default(schema.get("default")),
        default_value: schema
            .get("default")
            .filter(|value| !value.is_null())
            .cloned(),
        secret: schema
            .get(shep_core::dogs::SECRET_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false),
        editable,
        example,
        accepts: strings(init, "accepts"),
        refuses: strings(init, "refuses"),
        neighbours: neighbours(init),
        bounds: bounds_of(schema, defs),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::fixtures::{props, real_field_set};
    use super::*;
    use serde_json::json;

    /// The groups the set's fields carry, in the order they first appear.
    /// A group that appeared twice would show up twice, which is the
    /// contiguity a renderer's one-header-per-group rule depends on.
    fn groups_of(set: &FieldSet) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for field in set.fields() {
            if let Some(group) = &field.group
                && seen.last() != Some(group)
            {
                seen.push(group.clone());
            }
        }
        seen
    }

    #[test]
    fn help_prefers_the_blurb_then_the_description_then_the_key() {
        let p = props(json!({
            "a": { "type": "boolean", "description": "desc", "init": { "blurb": "blurb" } },
            "b": { "type": "boolean", "description": "desc" },
            "c": { "type": "boolean" },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert_eq!(set.by_key("a").unwrap().help, "blurb");
        assert_eq!(set.by_key("b").unwrap().help, "desc");
        assert_eq!(set.by_key("c").unwrap().help, "c");
    }

    /// They all rank equal, so a stable sort leaves them exactly where the
    /// caller put them, and a renderer that pushes a header on every group
    /// change would draw each of these twice. `GROUP_ORDER` names all four
    /// Flockfile groups, so only a dog's own `--schema` answer reaches
    /// this.
    #[test]
    fn two_groups_the_order_does_not_name_still_come_out_contiguous() {
        let p = props(json!({
            "a": { "type": "boolean", "init": { "group": "zebra" } },
            "b": { "type": "boolean", "init": { "group": "aardvark" } },
            "c": { "type": "boolean", "init": { "group": "zebra" } },
            "d": { "type": "boolean", "init": { "group": "aardvark" } },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let keys: Vec<&str> = set.fields().iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            ["a", "c", "b", "d"],
            "first appearance wins, and neither group is split"
        );
        assert_eq!(
            groups_of(&set),
            ["zebra", "aardvark"],
            "each name appears once, which is what a header per change needs"
        );
    }

    #[test]
    fn fields_sort_by_group_rank_then_schema_order_and_groups_lists_those_present() {
        let p = props(json!({
            "zeta": { "type": "boolean", "init": { "group": "control" } },
            "alpha": { "type": "boolean", "init": { "group": "process" } },
            "beta": { "type": "boolean", "init": { "group": "control" } },
            "nogroup": { "type": "boolean" },
            "odd": { "type": "boolean", "init": { "group": "unknown" } },
        }));
        let set =
            FieldSet::from_properties(&p, &Default::default(), &["process", "inputs", "control"]);
        let keys: Vec<&str> = set.fields().iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["alpha", "beta", "zeta", "odd", "nogroup"]);
        assert_eq!(
            groups_of(&set),
            ["process", "control", "unknown"],
            "and each group is contiguous, so a renderer draws its header once"
        );
    }

    #[test]
    fn the_secret_marker_is_read_off_the_extension_key() {
        let p = props(json!({
            "url": { "type": "string", "x-shep-secret": true },
            "path": { "type": "string" },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert!(set.by_key("url").unwrap().secret);
        assert!(!set.by_key("path").unwrap().secret);
    }

    #[test]
    fn the_real_flockfile_schema_yields_forty_two_fields_in_eight_groups() {
        let set = real_field_set();
        assert_eq!(set.len(), 42);
        assert_eq!(
            groups_of(&set),
            [
                "process",
                "logging",
                "inputs",
                "restart",
                "readiness",
                "shutdown",
                "watch",
                "cron"
            ]
        );
        assert!(
            set.fields().iter().all(|f| f.group.is_some()),
            "every field carries a group"
        );
        assert_eq!(set.by_key("env").unwrap().kind, FieldKind::Map);
        assert_eq!(set.by_key("autorestart").unwrap().kind, FieldKind::Bool);
    }

    #[test]
    fn no_group_holds_more_than_a_third_of_the_fields() {
        let set = real_field_set();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for field in set.fields() {
            *counts
                .entry(field.group.as_deref().unwrap_or(""))
                .or_default() += 1;
        }
        let (worst, count) = counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .expect("fields exist");
        assert!(*count <= 13, "{worst} holds {count} of {}", set.len());
    }

    /// Every neighbour named by the real schema has to be a real field. This
    /// is the only failure mode a hand written cross reference has.
    #[test]
    fn every_neighbour_in_the_real_schema_names_a_real_field() {
        let schema = shep_core::config::flockfile_schema_json().to_value();
        let props = schema
            .pointer("/$defs/AppConfig/properties")
            .and_then(serde_json::Value::as_object)
            .expect("app config properties must exist");
        let defs = schema
            .pointer("/$defs")
            .and_then(serde_json::Value::as_object)
            .expect("defs must exist");
        let set = FieldSet::from_properties(props, defs, shep_core::config::GROUP_ORDER);
        for field in set.fields() {
            for neighbour in &field.neighbours {
                assert!(
                    set.by_key(&neighbour.field).is_some(),
                    "{} names {}, which is not a field",
                    field.key,
                    neighbour.field
                );
            }
        }
    }
}
