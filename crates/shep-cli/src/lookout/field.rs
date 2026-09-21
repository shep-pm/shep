//! A form's shape, read off a JSON Schema.
//!
//! Every config pane in lookout renders one of these. A JSON Schema is
//! already a field list with types, defaults and descriptions, which is
//! exactly what a form needs, so this is the common shape rather than an
//! abstraction invented to share code. The Flockfile schema, a dog's own
//! `--schema` answer, and a hand-built list for `shep.toml` all become a
//! [`FieldSet`], and one renderer draws all three.

use serde_json::{Map, Value};

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

/// The bounds a property's schema publishes beyond its type, for the two
/// keywords this pane can actually check a keystroke against.
///
/// Read off the resolved schema, so a `$ref` into `$defs` and an
/// `anyOf: [T, null]` are both followed first: `max_age` on a dog that
/// spells it `Option<UpDuration>` carries its pattern two hops away from
/// the property itself.
///
/// Empty for a field whose schema says nothing machine-checkable, which is
/// most of them. A bound this type does not carry is a bound
/// [`super::validation::refusal`] cannot enforce, and the pane files the
/// value: shep is not the authority on a dog's own config.
///
/// `Debug` is derived (IR-41): a schema's bounds describe a value without
/// carrying one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bounds {
    /// `pattern`, as the schema wrote it. A dog's own grammar reaches the
    /// pane only through this, since shep has no `FromStr` for a type it
    /// has never heard of.
    pub pattern: Option<String>,
    /// `minimum`, for a [`FieldKind::Integer`] field, normalised to the
    /// smallest `i64` the schema accepts. shep-log-rotate's `keep`
    /// publishes one for exactly this reason: without it the pane offers a
    /// `keep = 0` the dog refuses on its next tick, in a file the operator
    /// has already saved and moved on from.
    pub minimum: Option<i64>,
    /// `maximum`, normalised to the largest `i64` the schema accepts.
    pub maximum: Option<i64>,
    /// Whether the schema's own numeric range admits no integer at all.
    ///
    /// A dog that writes `range(min = 3, max = 1)` publishes a field
    /// nothing can set. Without this the pane refuses `1` for being under
    /// the floor and `3` for being over the ceiling, one sentence at a
    /// time, and never says that no answer exists.
    pub unsatisfiable: bool,
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

/// Follows one `$ref` of the form `#/$defs/Name` into `defs`.
fn resolve<'a>(schema: &'a Value, defs: &'a Map<String, Value>) -> &'a Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .and_then(|name| defs.get(name))
        .unwrap_or(schema)
}

/// The name of a `$ref: "#/$defs/<Name>"`, direct or inside an `anyOf`
/// arm, before [`resolve`] replaces it with the schema it points to.
///
/// [`resolve`] and [`strip_nullable`] both need the referenced schema's
/// body; [`kind_of`] calls them first and the ref name is gone by the
/// time it returns, so this reads the original, unresolved schema.
fn ref_name(schema: &Value) -> Option<&str> {
    let target = schema.get("$ref").or_else(|| {
        schema
            .get("anyOf")?
            .as_array()?
            .iter()
            .find_map(|arm| arm.get("$ref"))
    })?;
    target.as_str()?.strip_prefix("#/$defs/")
}

/// [`ValueKind::MemSize`] or [`ValueKind::UpDuration`] when `schema`
/// names one of those two defs, else [`None`].
fn value_kind_of(schema: &Value) -> Option<ValueKind> {
    match ref_name(schema)? {
        "MemSize" => Some(ValueKind::MemSize),
        "UpDuration" => Some(ValueKind::UpDuration),
        _ => None,
    }
}

/// `anyOf: [T, {type: null}]` is `T` with the field optional. Anything
/// else is left as it was.
fn strip_nullable<'a>(schema: &'a Value, defs: &'a Map<String, Value>) -> &'a Value {
    let Some(arms) = schema.get("anyOf").and_then(Value::as_array) else {
        return schema;
    };
    let non_null: Vec<&Value> = arms
        .iter()
        .filter(|arm| arm.get("type").and_then(Value::as_str) != Some("null"))
        .collect();
    match non_null.as_slice() {
        [one] => resolve(one, defs),
        _ => schema,
    }
}

/// The schema a property actually describes: a `$ref` followed into `defs`,
/// and an `anyOf: [T, null]` reduced to `T`.
///
/// One spelling of the two hops, shared by [`kind_of`] and [`bounds_of`],
/// because a reader that followed only one of them would report a
/// nullable field's bounds as absent.
fn resolved<'a>(schema: &'a Value, defs: &'a Map<String, Value>) -> &'a Value {
    strip_nullable(resolve(schema, defs), defs)
}

/// The [`Bounds`] the resolved schema publishes.
///
/// A numeric bound is read as an `f64` and normalised to the integer it
/// actually forbids, because JSON Schema's bounds are inclusive: a
/// `minimum` rounds UP and a `maximum` rounds DOWN, so `minimum: 0.5`
/// forbids `0` and `maximum: 2.5` forbids `3`.
///
/// Reading them with `as_i64` instead dropped both, and a dropped bound is
/// an accepted keystroke: the pane filed a `0` its own schema forbade.
/// Reading them as `f64` also keeps a bound written as `1e30`, which no
/// `i64` satisfies, apart from one written as `1`.
fn bounds_of(schema: &Value, defs: &Map<String, Value>) -> Bounds {
    let schema = resolved(schema, defs);
    let minimum = schema.get("minimum").and_then(Value::as_f64).map(f64::ceil);
    let maximum = schema
        .get("maximum")
        .and_then(Value::as_f64)
        .map(f64::floor);
    let (floor, ceiling) = (minimum.and_then(as_bound), maximum.and_then(as_bound));
    Bounds {
        pattern: schema
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::to_owned),
        minimum: floor,
        maximum: ceiling,
        // A bound past an `i64`'s reach is impossible in one direction and
        // says nothing in the other: a `minimum` above `i64::MAX` admits
        // nothing, while one below `i64::MIN` admits every integer there
        // is, which is what `as_bound` already answered `None` for.
        unsatisfiable: minimum.is_some_and(|value| value > i64::MAX as f64)
            || maximum.is_some_and(|value| value < i64::MIN as f64)
            || matches!((floor, ceiling), (Some(low), Some(high)) if low > high),
    }
}

/// `value` as the `i64` bound it names, or [`None`] when it sits outside
/// what an `i64` can hold and so bounds nothing this pane can compare.
///
/// The comparison is in `f64`, which cannot name every `i64` exactly near
/// its ends. A bound within a few units of `i64::MAX` therefore saturates
/// to it rather than being refused, which is the right answer for a field
/// no operator is typing sixteen digits into.
fn as_bound(value: f64) -> Option<i64> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "guarded by the range check, and a float-to-int cast saturates"
    )]
    (value >= i64::MIN as f64 && value <= i64::MAX as f64).then_some(value as i64)
}

/// An array's `items` schema, or `Value::Null` when it declares none.
fn items(schema: &Value) -> &Value {
    schema.get("items").unwrap_or(&Value::Null)
}

/// The `type` keyword, which may be a string or a `[T, "null"]` list.
fn type_of(schema: &Value) -> Option<&str> {
    match schema.get("type")? {
        Value::String(s) => Some(s.as_str()),
        Value::Array(arr) => arr.iter().filter_map(Value::as_str).find(|t| *t != "null"),
        _ => None,
    }
}

fn kind_of(schema: &Value, defs: &Map<String, Value>) -> FieldKind {
    let schema = resolved(schema, defs);
    if let Some(consts) = schema.get("oneOf").and_then(Value::as_array) {
        let names: Vec<String> = consts
            .iter()
            .filter_map(|arm| arm.get("const").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        if !names.is_empty() && names.len() == consts.len() {
            return FieldKind::Choice(names);
        }
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let names: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if names.len() == values.len() {
            return FieldKind::Choice(names);
        }
    }
    match type_of(schema) {
        Some("boolean") => FieldKind::Bool,
        Some("integer") => FieldKind::Integer,
        Some("string") => FieldKind::Text,
        // An array of anything else stays `Opaque`, which is what keeps an
        // array of nested objects read-only rather than half-editable.
        Some("array") => match type_of(strip_nullable(resolve(items(schema), defs), defs)) {
            Some("string") => FieldKind::List(ListItem::Text),
            Some("integer") => FieldKind::List(ListItem::Integer),
            _ => FieldKind::Opaque,
        },
        Some("object")
            if schema.get("additionalProperties").is_some()
                && schema.get("properties").is_none() =>
        {
            FieldKind::Map
        }
        _ => FieldKind::Opaque,
    }
}

/// Renders a default the way the pane will show the value: bare for a
/// scalar, compact JSON for anything else, `None` for `null`.
fn render_default(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

/// The `init.suggest` values, when every entry is a string.
fn suggestions(init: Option<&Value>) -> Option<Vec<String>> {
    let values = init?.get("suggest")?.as_array()?;
    let names: Vec<String> = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    (names.len() == values.len()).then_some(names)
}

/// The `init.<key>` values, when every entry is a string. Empty (not
/// `None`) when `init` carries no such key, since [`Field::accepts`] and
/// [`Field::refuses`] are lists rather than options.
fn strings(init: Option<&Value>, key: &str) -> Vec<String> {
    let Some(values) = init.and_then(|i| i.get(key)).and_then(Value::as_array) else {
        return Vec::new();
    };
    let names: Vec<String> = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if names.len() == values.len() {
        names
    } else {
        Vec::new()
    }
}

/// The `init.neighbours` entries that carry both `field` and `note` as
/// strings. An entry missing either half is dropped rather than half
/// rendered.
fn neighbours(init: Option<&Value>) -> Vec<Neighbour> {
    let Some(values) = init
        .and_then(|i| i.get("neighbours"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|entry| {
            let field = entry.get("field")?.as_str()?.to_owned();
            let note = entry.get("note")?.as_str()?.to_owned();
            Some(Neighbour { field, note })
        })
        .collect()
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

    use super::*;
    use serde_json::json;

    fn props(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        v.as_object().unwrap().clone()
    }

    /// The real Flockfile schema's fields, in the order a pane would build them.
    fn real_field_set() -> FieldSet {
        let schema = shep_core::config::flockfile_schema_json().to_value();
        let defs = schema["$defs"].as_object().unwrap();
        let props = defs["AppConfig"]["properties"].as_object().unwrap();
        FieldSet::from_properties(props, defs, shep_core::config::GROUP_ORDER)
    }

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
    fn a_bool_an_integer_and_a_string_get_their_kinds() {
        let p = props(json!({
            "watch": { "type": "boolean", "default": false },
            "max_restarts": { "type": "integer", "format": "uint32", "default": 16 },
            "cwd": { "type": ["string", "null"], "default": null },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert_eq!(set.by_key("watch").unwrap().kind, FieldKind::Bool);
        assert_eq!(set.by_key("max_restarts").unwrap().kind, FieldKind::Integer);
        assert_eq!(set.by_key("cwd").unwrap().kind, FieldKind::Text);
    }

    #[test]
    fn a_ref_into_defs_takes_the_named_types_kind() {
        let p = props(json!({
            "kill_timeout": { "$ref": "#/$defs/UpDuration", "default": "1600" },
        }));
        let d = props(json!({
            "UpDuration": { "type": "string", "pattern": "^\\d+(ms|h|m|s)?$" },
        }));
        let set = FieldSet::from_properties(&p, &d, &[]);
        assert_eq!(set.by_key("kill_timeout").unwrap().kind, FieldKind::Text);
        assert_eq!(
            set.by_key("kill_timeout").unwrap().default.as_deref(),
            Some("1600")
        );
    }

    #[test]
    fn any_of_with_null_is_the_other_arm() {
        let p = props(json!({
            "max_memory": {
                "anyOf": [{ "$ref": "#/$defs/MemSize" }, { "type": "null" }],
                "default": null,
            },
        }));
        let d = props(json!({ "MemSize": { "type": "string" } }));
        let set = FieldSet::from_properties(&p, &d, &[]);
        assert_eq!(set.by_key("max_memory").unwrap().kind, FieldKind::Text);
        assert_eq!(set.by_key("max_memory").unwrap().default, None);
    }

    #[test]
    fn one_of_consts_is_a_choice_in_schema_order() {
        let p = props(json!({
            "kind": {
                "oneOf": [
                    { "type": "string", "const": "http" },
                    { "type": "string", "const": "tcp" },
                    { "type": "string", "const": "exec" },
                ],
            },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert_eq!(
            set.by_key("kind").unwrap().kind,
            FieldKind::Choice(vec!["http".into(), "tcp".into(), "exec".into()])
        );
    }

    /// The `$ref` name has to survive `strip_nullable` unwrapping the
    /// `anyOf`, and it names `MemSize`/`UpDuration` regardless of which of
    /// the two schema shapes carries it.
    #[test]
    fn a_text_field_records_which_shep_core_grammar_it_holds() {
        let d = props(json!({
            "MemSize": { "type": "string" },
            "UpDuration": { "type": "string" },
        }));
        let p = props(json!({
            "max_memory": {
                "anyOf": [{ "$ref": "#/$defs/MemSize" }, { "type": "null" }],
            },
            "kill_timeout": { "$ref": "#/$defs/UpDuration" },
            "cwd": { "type": "string" },
        }));
        let set = FieldSet::from_properties(&p, &d, &[]);
        assert_eq!(
            set.by_key("max_memory").unwrap().value_kind,
            Some(ValueKind::MemSize)
        );
        assert_eq!(
            set.by_key("kill_timeout").unwrap().value_kind,
            Some(ValueKind::UpDuration)
        );
        assert_eq!(set.by_key("cwd").unwrap().value_kind, None);
    }

    #[test]
    fn a_string_map_is_a_map_and_a_nested_object_is_opaque() {
        let p = props(json!({
            "env": { "type": "object", "additionalProperties": { "type": "string" } },
            "liveness_probe": {
                "anyOf": [{ "$ref": "#/$defs/ProbeConfig" }, { "type": "null" }],
            },
        }));
        let d = props(json!({
            "ProbeConfig": { "type": "object", "properties": { "kind": {} } },
        }));
        let set = FieldSet::from_properties(&p, &d, &[]);
        assert_eq!(set.by_key("env").unwrap().kind, FieldKind::Map);
        assert_eq!(
            set.by_key("liveness_probe").unwrap().kind,
            FieldKind::Opaque
        );
        assert!(!set.by_key("liveness_probe").unwrap().editable);
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

    /// Every hop [`resolved`] follows, because a reader that followed one
    /// and not the other reports a nullable field's bounds as absent, and
    /// nothing downstream can tell that from a schema that published none.
    ///
    /// Three spellings of the same `pattern`: inline on the property, one
    /// `$ref` away, and behind the `anyOf: [{$ref}, {type: null}]` that a
    /// dog writes for an `Option<UpDuration>`. That third one is the shape
    /// `shep-log-rotate` publishes, so it is the shape the gate in
    /// [`super::validation::refusal`] actually meets.
    #[test]
    fn a_pattern_is_read_through_every_hop_a_schema_can_put_it_behind() {
        let defs = props(json!({
            "UpDuration": { "type": "string", "pattern": r"^\d+(ms|h|m|s)?$" },
        }));
        let p = props(json!({
            "inline": { "type": "string", "pattern": r"^\d+(ms|h|m|s)?$" },
            "one_hop": { "$ref": "#/$defs/UpDuration" },
            "two_hops": { "anyOf": [{ "$ref": "#/$defs/UpDuration" }, { "type": "null" }] },
            "plain": { "type": "string" },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        for key in ["inline", "one_hop", "two_hops"] {
            assert_eq!(
                set.by_key(key).unwrap().bounds.pattern.as_deref(),
                Some(r"^\d+(ms|h|m|s)?$"),
                "{key}"
            );
        }
        // The negative control: bounds that came back non-empty for a
        // schema publishing none would pass every assertion above.
        assert_eq!(set.by_key("plain").unwrap().bounds, Bounds::default());
    }

    /// The numeric half, and the reason it is here rather than only in the
    /// gate's own tests: `shep-log-rotate` publishes `minimum: 1` on
    /// `keep`, so the floor an operator is held to is one this reader has
    /// to lift off somebody else's schema.
    ///
    /// A fractional bound is normalised to the integer it forbids, not
    /// dropped: JSON Schema's bounds are inclusive, so a `minimum` rounds
    /// up and a `maximum` rounds down. Dropping them accepted a `0` under
    /// a `minimum` of `0.5`, and the pane filed it.
    #[test]
    fn integer_bounds_are_read_and_a_fractional_one_is_normalised() {
        let p = props(json!({
            "keep": { "type": "integer", "minimum": 1 },
            "ratio": { "type": "integer", "minimum": 0.5, "maximum": 2.5 },
            "both": { "type": "integer", "minimum": -3, "maximum": 64 },
            "plain": { "type": "integer" },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert_eq!(set.by_key("keep").unwrap().bounds.minimum, Some(1));
        assert_eq!(set.by_key("both").unwrap().bounds.minimum, Some(-3));
        assert_eq!(set.by_key("both").unwrap().bounds.maximum, Some(64));
        let ratio = &set.by_key("ratio").unwrap().bounds;
        assert_eq!(ratio.minimum, Some(1), "0.5 rounds up, so 0 is refused");
        assert_eq!(ratio.maximum, Some(2), "2.5 rounds down, so 3 is refused");
        assert!(!ratio.unsatisfiable);
        assert_eq!(set.by_key("plain").unwrap().bounds, Bounds::default());
    }

    /// A range no integer satisfies, by each of the three routes into one.
    /// The pane says so once rather than bouncing an operator between a
    /// floor and a ceiling that cannot both be met.
    #[test]
    fn a_range_with_nothing_in_it_is_recorded_as_such() {
        let p = props(json!({
            "inverted": { "type": "integer", "minimum": 3, "maximum": 1 },
            "inverted_after_rounding": {
                "type": "integer", "minimum": 1.2, "maximum": 1.8,
            },
            "past_i64": { "type": "integer", "minimum": 1e30 },
            "under_i64": { "type": "integer", "maximum": -1e30 },
            "wide_open": { "type": "integer", "minimum": -1e30, "maximum": 1e30 },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        for key in [
            "inverted",
            "inverted_after_rounding",
            "past_i64",
            "under_i64",
        ] {
            assert!(set.by_key(key).unwrap().bounds.unsatisfiable, "{key}");
        }
        // The other direction of the same overflow bounds nothing: every
        // integer is above -1e30 and below 1e30, so the field is free.
        let open = &set.by_key("wide_open").unwrap().bounds;
        assert!(!open.unsatisfiable);
        assert_eq!((open.minimum, open.maximum), (None, None));
    }

    /// The bounds the real Flockfile schema publishes, so a change to it
    /// that moved one lands here rather than on an operator.
    ///
    /// `max_memory` and `kill_timeout` carry their grammars through
    /// `$defs`, and `max_restarts` a floor of its own. The two string
    /// grammars go through their own `FromStr` in the gate rather than
    /// through these patterns, so this test is about the reader reaching
    /// them at all.
    #[test]
    fn the_real_schema_publishes_the_bounds_the_gate_reads() {
        let set = real_field_set();
        assert_eq!(
            set.by_key("max_memory").unwrap().bounds.pattern.as_deref(),
            Some(r"^\d+(G|M|K)?$")
        );
        assert_eq!(
            set.by_key("kill_timeout")
                .unwrap()
                .bounds
                .pattern
                .as_deref(),
            Some(r"^\d+(ms|h|m|s)?$")
        );
        assert_eq!(set.by_key("max_restarts").unwrap().bounds.minimum, Some(0));
        assert_eq!(
            set.by_key("cwd").unwrap().bounds,
            Bounds::default(),
            "a plain path field is held to nothing"
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
    fn a_default_is_rendered_the_way_the_pane_will_show_it() {
        let p = props(json!({
            "b": { "type": "boolean", "default": true },
            "n": { "type": "integer", "default": 16 },
            "s": { "type": "string", "default": "1s" },
            "l": { "type": "array", "default": [], "items": { "type": "string" } },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert_eq!(set.by_key("b").unwrap().default.as_deref(), Some("true"));
        assert_eq!(set.by_key("n").unwrap().default.as_deref(), Some("16"));
        assert_eq!(set.by_key("s").unwrap().default.as_deref(), Some("1s"));
        assert_eq!(set.by_key("l").unwrap().default.as_deref(), Some("[]"));
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

    #[test]
    fn the_real_flockfile_schema_marks_every_mem_size_and_up_duration_field() {
        let set = real_field_set();
        assert_eq!(
            set.by_key("max_memory").unwrap().value_kind,
            Some(ValueKind::MemSize)
        );
        for key in [
            "kill_timeout",
            "listen_timeout",
            "min_uptime",
            "graceful_timeout",
            "action_timeout",
            "restart_delay",
            "watch_delay",
            "exp_backoff_restart_delay",
        ] {
            assert_eq!(
                set.by_key(key).unwrap().value_kind,
                Some(ValueKind::UpDuration),
                "{key}"
            );
        }
        assert_eq!(set.by_key("cwd").unwrap().value_kind, None);
    }

    #[test]
    fn a_field_with_init_suggest_cycles_and_still_types() {
        let schema = json!({
            "type": ["string", "null"],
            "init": { "suggest": ["SIGTERM", "SIGINT"] }
        });
        let field = field_from("kill_signal", &schema, &Map::new());
        assert_eq!(
            field.kind,
            FieldKind::Suggested(vec!["SIGTERM".into(), "SIGINT".into()])
        );
        assert!(field.editable, "a suggestion is not a constraint");
    }

    #[test]
    fn kill_signal_and_cron_restart_both_carry_suggestions() {
        let set = real_field_set();
        for key in ["kill_signal", "cron_restart"] {
            let field = set.by_key(key).unwrap_or_else(|| panic!("no {key}"));
            assert!(
                matches!(field.kind, FieldKind::Suggested(ref names) if !names.is_empty()),
                "{key}: {:?}",
                field.kind
            );
        }
    }

    #[test]
    fn an_array_of_strings_is_a_list_and_an_array_of_integers_knows_its_item() {
        let set = real_field_set();
        assert_eq!(
            set.by_key("args").map(|f| f.kind.clone()),
            Some(FieldKind::List(ListItem::Text))
        );
        assert_eq!(
            set.by_key("stop_exit_codes").map(|f| f.kind.clone()),
            Some(FieldKind::List(ListItem::Integer))
        );
        assert!(
            set.by_key("args").is_some_and(|f| f.editable),
            "an array is editable now"
        );
    }

    #[test]
    fn a_field_carries_its_example_from_the_init_block() {
        let set = FieldSet::from_properties(
            &props(json!({
                "cwd": { "type": ["string", "null"],
                         "init": { "example": "/srv/app", "group": "process" } }
            })),
            &Map::new(),
            &["process"],
        );
        assert_eq!(
            set.by_key("cwd").unwrap().example.as_deref(),
            Some("/srv/app")
        );
    }

    #[test]
    fn a_field_carries_its_accepted_and_refused_forms() {
        let set = FieldSet::from_properties(
            &props(json!({
                "cwd": { "type": ["string", "null"], "init": {
                    "group": "process",
                    "accepts": ["an absolute or relative path", "~ expands, $VARS do not"],
                    "refuses": ["a path the daemon's user cannot enter"]
                } }
            })),
            &Map::new(),
            &["process"],
        );
        let field = set.by_key("cwd").unwrap();
        assert_eq!(field.accepts.len(), 2);
        assert_eq!(field.refuses.len(), 1);
    }

    #[test]
    fn a_neighbour_carries_a_field_name_and_a_note() {
        let set = FieldSet::from_properties(
            &props(json!({
                "cwd": { "type": ["string", "null"], "init": { "group": "process",
                    "neighbours": [{ "field": "script", "note": "resolved against this cwd" }] } }
            })),
            &Map::new(),
            &["process"],
        );
        let neighbours = &set.by_key("cwd").unwrap().neighbours;
        assert_eq!(neighbours[0].field, "script");
        assert_eq!(neighbours[0].note, "resolved against this cwd");
    }

    /// An entry missing either half is dropped rather than half rendered.
    #[test]
    fn a_malformed_neighbour_entry_is_dropped() {
        let set = FieldSet::from_properties(
            &props(json!({
                "cwd": { "type": ["string", "null"], "init": { "group": "process",
                    "neighbours": [{ "field": "script" }, { "note": "orphan" }] } }
            })),
            &Map::new(),
            &["process"],
        );
        assert!(set.by_key("cwd").unwrap().neighbours.is_empty());
    }

    /// A field carrying none of the three keys renders no headings, which is
    /// the same "nothing rather than an empty one" rule the detail pane's
    /// `cfg` cell follows.
    #[test]
    fn a_field_without_the_new_keys_carries_empty_lists() {
        let set = FieldSet::from_properties(
            &props(json!({ "cwd": { "type": ["string", "null"] } })),
            &Map::new(),
            &[],
        );
        let field = set.by_key("cwd").unwrap();
        assert!(field.example.is_none());
        assert!(field.accepts.is_empty());
        assert!(field.refuses.is_empty());
        assert!(field.neighbours.is_empty());
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
