//! Reading standard JSON Schema: `$ref`, `anyOf`, `type` and `items`.
//!
//! Only the vocabulary a schema author did not invent. Everything shep
//! adds under its own keys is [`super::init`], and the bounds grammar is
//! [`super::bounds`].

use serde_json::{Map, Value};

use super::{FieldKind, ListItem, ValueKind};

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
pub(super) fn value_kind_of(schema: &Value) -> Option<ValueKind> {
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
/// One spelling of the two hops, shared by [`kind_of`] and [`super::bounds::bounds_of`],
/// because a reader that followed only one of them would report a
/// nullable field's bounds as absent.
pub(super) fn resolved<'a>(schema: &'a Value, defs: &'a Map<String, Value>) -> &'a Value {
    strip_nullable(resolve(schema, defs), defs)
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

pub(super) fn kind_of(schema: &Value, defs: &Map<String, Value>) -> FieldKind {
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
pub(super) fn render_default(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::FieldSet;
    use super::super::fixtures::{props, real_field_set};
    use super::*;
    use serde_json::json;

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
}
