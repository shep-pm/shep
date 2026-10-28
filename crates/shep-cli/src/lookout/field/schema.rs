//! Reading standard JSON Schema: `$ref`, `anyOf`, `type` and `items`.
//!
//! Only the vocabulary a schema author did not invent. Everything shep
//! adds under its own keys is [`super::init`], and the bounds grammar is
//! [`super::bounds`].

use serde_json::{Map, Value};

use super::union::union_of;
use super::{Entries, FieldKind, ListItem, ValueKind, all_strings};

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

/// Whether `arm` is the `null` type alone, spelled `{"type": "null"}` or
/// `{"type": ["null"]}`.
fn is_null_arm(arm: &Value) -> bool {
    match arm.get("type") {
        Some(Value::String(t)) => t == "null",
        Some(Value::Array(types)) => {
            !types.is_empty() && types.iter().all(|t| t.as_str() == Some("null"))
        }
        _ => false,
    }
}

/// Whether a value of `schema` may be `null`: a `type` list naming
/// `"null"`, or an `anyOf` arm that is [`is_null_arm`], each read through
/// a `$ref` first.
///
/// [`strip_nullable`] drops the arms this finds, so the two checks cannot
/// disagree about what counts as null.
fn allows_null(schema: &Value, defs: &Map<String, Value>) -> bool {
    let schema = resolve(schema, defs);
    let in_type = schema
        .get("type")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|t| t.as_str() == Some("null")));
    let in_arm = schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|arms| arms.iter().any(|arm| is_null_arm(resolve(arm, defs))));
    in_type || in_arm || is_null_arm(schema)
}

/// `anyOf: [T, {type: null}]` is `T` with the field optional. Anything
/// else is left as it was.
fn strip_nullable<'a>(schema: &'a Value, defs: &'a Map<String, Value>) -> &'a Value {
    let Some(arms) = schema.get("anyOf").and_then(Value::as_array) else {
        return schema;
    };
    let non_null: Vec<&Value> = arms
        .iter()
        .filter(|arm| !is_null_arm(resolve(arm, defs)))
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

/// The names of a closed set of strings: a `oneOf` of `const`s, or an
/// `enum` of strings, in the order the schema wrote them. [`None`] for
/// anything else, including a set with a member that is not a string. An
/// empty `enum` is `Some` of nothing, so a caller can tell it from a schema
/// that is no closed set at all.
fn closed_set(schema: &Value) -> Option<Vec<String>> {
    if let Some(consts) = schema.get("oneOf").and_then(Value::as_array) {
        let named = all_strings(consts, |arm| arm.get("const").and_then(Value::as_str));
        if let Some(names) = named.filter(|names| !names.is_empty()) {
            return Some(names);
        }
    }
    let values = schema.get("enum").and_then(Value::as_array)?;
    all_strings(values, Value::as_str)
}

pub(super) fn kind_of(schema: &Value, defs: &Map<String, Value>) -> FieldKind {
    let schema = resolved(schema, defs);
    if let Some(names) = closed_set(schema) {
        // A choice with nothing to choose has no row worth drawing.
        return if names.is_empty() {
            FieldKind::Opaque
        } else {
            FieldKind::Choice(names)
        };
    }
    if let Some(union) = union_of(schema, defs) {
        return FieldKind::Union(union);
    }
    match type_of(schema) {
        Some("boolean") => FieldKind::Bool,
        Some("integer") => FieldKind::Integer,
        Some("number") => FieldKind::Number,
        Some("string") => FieldKind::Text,
        // An array of anything else stays `Opaque`, which is what keeps an
        // array of nested objects read-only rather than half-editable. So
        // does one whose items can be `null`: the list sub-screen would
        // turn a stored `null` into `"null"`.
        Some("array") if allows_null(items(schema), defs) => FieldKind::Opaque,
        Some("array") => {
            let item = resolved(items(schema), defs);
            if let Some(names) = closed_set(item) {
                return if names.is_empty() {
                    FieldKind::Opaque
                } else {
                    FieldKind::List(ListItem::Choice(names))
                };
            }
            match type_of(item) {
                Some("string") => FieldKind::List(ListItem::Text),
                Some("integer") => FieldKind::List(ListItem::Integer),
                Some("object") if item.get("properties").is_some() => {
                    FieldKind::Entries(Entries::Array)
                }
                _ => FieldKind::Opaque,
            }
        }
        // A map of scalars, like `env`, is `Map`. A map of tables with named
        // properties is `Entries`; a map of free-form objects, like `dogs`,
        // stays read-only, since `kind_of` has no editor for a value that is
        // itself a table.
        Some("object")
            if schema.get("additionalProperties").is_some()
                && schema.get("properties").is_none() =>
        {
            let value = schema
                .get("additionalProperties")
                .map(|additional| resolved(additional, defs));
            match value {
                Some(table) if type_of(table) == Some("object") => {
                    if table.get("properties").is_some() {
                        FieldKind::Entries(Entries::Map)
                    } else {
                        FieldKind::Opaque
                    }
                }
                _ => FieldKind::Map,
            }
        }
        _ => FieldKind::Opaque,
    }
}

/// The table schema an [`FieldKind::Entries`] field holds one of per entry:
/// an array's `items`, or a map's `additionalProperties`, followed through a
/// `$ref` and an `anyOf` with `null`. [`None`] for any other field.
pub(super) fn entry_schema<'a>(
    schema: &'a Value,
    defs: &'a Map<String, Value>,
) -> Option<&'a Value> {
    entry_slot(schema, defs).map(|item| resolved(item, defs))
}

/// Whether the secret marker sits on an [`FieldKind::Entries`] field's
/// entry slot itself, beside the `$ref` that [`entry_schema`] resolves past.
pub(super) fn entry_slot_marked(schema: &Value, defs: &Map<String, Value>) -> bool {
    entry_slot(schema, defs).is_some_and(marked)
}

/// The `items` or `additionalProperties` value as the schema wrote it.
fn entry_slot<'a>(schema: &'a Value, defs: &'a Map<String, Value>) -> Option<&'a Value> {
    let schema = resolved(schema, defs);
    match type_of(schema)? {
        "array" => schema.get("items"),
        "object" => schema.get("additionalProperties"),
        _ => None,
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

/// Whether `schema` itself carries the secret marker.
pub(super) fn marked(schema: &Value) -> bool {
    schema
        .get(shep_core::dogs::SECRET_KEY)
        .and_then(Value::as_bool)
        == Some(true)
}

/// Whether `schema` or anything it reaches, through a `$ref` or any nested
/// keyword, carries the secret marker.
pub(super) fn holds_secret(schema: &Value, defs: &Map<String, Value>) -> bool {
    reaches_secret(schema, defs, &mut Vec::new())
}

/// [`holds_secret`]'s walk. `seen` holds the `$defs` names already
/// followed, so a type that holds itself is read once.
fn reaches_secret<'a>(
    schema: &'a Value,
    defs: &'a Map<String, Value>,
    seen: &mut Vec<&'a str>,
) -> bool {
    match schema {
        Value::Object(map) => {
            if marked(schema) {
                return true;
            }
            let target = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"));
            if let Some(name) = target
                && !seen.contains(&name)
            {
                seen.push(name);
                if defs
                    .get(name)
                    .is_some_and(|def| reaches_secret(def, defs, seen))
                {
                    return true;
                }
            }
            map.values().any(|value| reaches_secret(value, defs, seen))
        }
        Value::Array(items) => items.iter().any(|item| reaches_secret(item, defs, seen)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::FieldSet;
    use super::super::fixtures::{props, real_field_set};
    use super::*;
    use serde_json::json;

    /// The mark is found on the property, through one `$ref` and two,
    /// under an array's items, and not where there is none. A type that
    /// holds itself, and a `$ref` to a missing definition, both end the
    /// walk rather than recurse or fault.
    #[test]
    fn holds_secret_finds_the_mark_wherever_the_value_reaches() {
        let defs = props(json!({
            "Token": { "type": "string", "x-shep-secret": true },
            "Creds": { "type": "object", "properties": { "token": { "$ref": "#/$defs/Token" } } },
            "Node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } },
            "Plain": { "type": "object" },
        }));
        let secret = |schema: Value| holds_secret(&schema, &defs);
        assert!(secret(json!({ "type": "string", "x-shep-secret": true })));
        assert!(
            secret(json!({
                "type": "object",
                "properties": { "key": { "type": "string", "x-shep-secret": true } },
            })),
            "inline, no $ref"
        );
        assert!(secret(json!({ "$ref": "#/$defs/Token" })));
        assert!(secret(json!({ "$ref": "#/$defs/Creds" })), "two hops");
        assert!(
            secret(json!({
                "$ref": "#/$defs/Plain",
                "properties": { "key": { "type": "string", "x-shep-secret": true } },
            })),
            "a plain $ref does not end the walk"
        );
        assert!(secret(
            json!({ "type": "array", "items": { "$ref": "#/$defs/Creds" } })
        ));
        assert!(!secret(json!({ "type": "string" })));
        assert!(!secret(json!({ "$ref": "#/$defs/Node" })), "a loop ends");
        assert!(!secret(json!({ "$ref": "#/$defs/Missing" })));
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

    /// A decimal field is a widget the pane can type into, not a row drawn as
    /// JSON. `shep-kelpie`'s `budget.per_item_usd` is one: `type: number`,
    /// `format: double`, `exclusiveMinimum: 0`, a money cap an operator could
    /// otherwise only change by hand in `dogs.toml`.
    #[test]
    fn a_number_is_a_number_whatever_way_the_schema_spells_it() {
        let defs = props(json!({
            "Money": { "type": "number", "format": "double" },
            "MaybeMoney": { "anyOf": [{ "$ref": "#/$defs/Money" }, { "type": "null" }] },
        }));
        let p = props(json!({
            "per_item_usd": { "type": "number", "format": "double", "exclusiveMinimum": 0 },
            "ratio": { "type": ["number", "null"] },
            "cap": { "$ref": "#/$defs/Money" },
            "optional_cap": { "$ref": "#/$defs/MaybeMoney" },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        for key in ["per_item_usd", "ratio", "cap", "optional_cap"] {
            let field = set.by_key(key).expect(key);
            assert_eq!(field.kind, FieldKind::Number, "{key}");
            assert!(field.editable, "{key} is read-only");
        }
    }

    /// A list of decimals keeps the read-only row it has always had: the list
    /// sub-screen parses an element as a string, an integer or a name, and has
    /// no float element to file.
    #[test]
    fn a_list_of_numbers_stays_opaque() {
        let defs = Map::new();
        for items in [
            json!({ "type": "number" }),
            json!({ "type": ["number", "null"] }),
            json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }),
        ] {
            let schema = json!({ "type": "array", "items": items });
            assert_eq!(kind_of(&schema, &defs), FieldKind::Opaque, "{schema}");
        }
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

    /// What `#[serde(tag = "kind")]` over a unit-like arm and two arms with
    /// fields comes out of schemars as.
    fn tagged_union() -> serde_json::Value {
        json!({
            "oneOf": [
                {
                    "description": "Skip the step.",
                    "type": "object",
                    "properties": { "kind": { "type": "string", "const": "none" } },
                    "required": ["kind"],
                },
                {
                    "description": "A skill in the repo.",
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "const": "path" },
                        "path": { "type": "string", "description": "Where it lives." },
                    },
                    "required": ["kind", "path"],
                },
                {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "const": "plugin" },
                        "plugin": { "type": "string" },
                        "skill": { "type": "string", "default": "retro" },
                    },
                    "required": ["kind", "plugin"],
                },
            ],
        })
    }

    #[test]
    fn a_one_of_of_objects_sharing_a_const_tag_is_a_union_with_a_tag_per_arm() {
        let p = props(json!({ "retro": tagged_union() }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let field = set.by_key("retro").unwrap();
        let FieldKind::Union(union) = &field.kind else {
            panic!("expected a union, got {:?}", field.kind);
        };
        assert!(field.editable);
        assert_eq!(union.tag, "kind");
        let tags: Vec<&str> = union.arms.iter().map(|arm| arm.tag.as_str()).collect();
        assert_eq!(tags, ["none", "path", "plugin"]);
        assert_eq!(union.arms[0].description.as_deref(), Some("Skip the step."));
        assert_eq!(union.arms[2].description, None);
        assert!(
            union.arms[0].fields.is_empty(),
            "the tag is not a field of its arm"
        );
        let path = &union.arms[1].fields[0];
        assert_eq!(path.key, "path");
        assert_eq!(path.kind, FieldKind::Text);
        assert_eq!(path.help, "Where it lives.");
        assert!(union.arms[1].requires("path"));
        let plugin = &union.arms[2];
        assert!(plugin.requires("plugin"));
        assert!(!plugin.requires("skill"));
        assert_eq!(plugin.fields[1].default.as_deref(), Some("retro"));
    }

    #[test]
    fn a_union_is_found_through_a_ref_and_through_a_nullable_any_of() {
        let defs = props(json!({ "Step": tagged_union() }));
        let p = props(json!({
            "by_ref": { "$ref": "#/$defs/Step" },
            "optional": { "anyOf": [{ "$ref": "#/$defs/Step" }, { "type": "null" }] },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        for key in ["by_ref", "optional"] {
            assert!(
                matches!(set.by_key(key).unwrap().kind, FieldKind::Union(_)),
                "{key}: {:?}",
                set.by_key(key).unwrap().kind
            );
        }
    }

    #[test]
    fn a_one_of_of_objects_with_no_shared_const_tag_stays_opaque() {
        let no_tag = json!({
            "oneOf": [
                { "type": "object", "properties": { "path": { "type": "string" } } },
                { "type": "object", "properties": { "url": { "type": "string" } } },
            ],
        });
        let tag_in_one_arm_only = json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "kind": { "const": "a" } },
                    "required": ["kind"],
                },
                { "type": "object", "properties": { "name": { "type": "string" } } },
            ],
        });
        let tag_not_required = json!({
            "oneOf": [
                { "type": "object", "properties": { "kind": { "const": "a" } } },
                { "type": "object", "properties": { "kind": { "const": "b" } } },
            ],
        });
        let tag_repeated = json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "kind": { "const": "a" } },
                    "required": ["kind"],
                },
                {
                    "type": "object",
                    "properties": { "kind": { "const": "a" }, "x": { "type": "string" } },
                    "required": ["kind"],
                },
            ],
        });
        for (name, schema) in [
            ("no_tag", no_tag),
            ("tag_in_one_arm_only", tag_in_one_arm_only),
            ("tag_not_required", tag_not_required),
            ("tag_repeated", tag_repeated),
        ] {
            let p = props(json!({ name: schema }));
            let set = FieldSet::from_properties(&p, &Default::default(), &[]);
            let field = set.by_key(name).unwrap();
            assert_eq!(field.kind, FieldKind::Opaque, "{name}");
            assert!(!field.editable, "{name}");
        }
    }

    #[test]
    fn an_arm_field_with_no_editor_is_opaque_and_a_secret_keeps_the_union_read_only() {
        let nested = json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "kind": { "const": "a" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "inner": tagged_union(),
                        "env": { "type": "object", "additionalProperties": { "type": "string" } },
                    },
                    "required": ["kind"],
                },
            ],
        });
        let p = props(json!({ "nested": nested }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let FieldKind::Union(union) = &set.by_key("nested").unwrap().kind else {
            panic!("expected a union");
        };
        let kinds: Vec<(&str, &FieldKind)> = union.arms[0]
            .fields
            .iter()
            .map(|field| (field.key.as_str(), &field.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("env", &FieldKind::Opaque),
                ("inner", &FieldKind::Opaque),
                ("tags", &FieldKind::List(ListItem::Text)),
            ]
        );

        let secret = json!({
            "oneOf": [{
                "type": "object",
                "properties": {
                    "kind": { "const": "a" },
                    "token": { "type": "string", "x-shep-secret": true },
                },
                "required": ["kind"],
            }],
        });
        let p = props(json!({ "secret": secret }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert_eq!(set.by_key("secret").unwrap().kind, FieldKind::Opaque);
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

    /// A map whose values are themselves objects, like `dogs`, is not
    /// `env`'s string-map editor: `additionalProperties` names a schema
    /// with its own `type`, direct or through a `$ref`, rather than
    /// `{type: string}` or `env`'s `anyOf` of scalars.
    #[test]
    fn a_map_of_tables_is_opaque_not_a_string_map() {
        let p = props(json!({
            "dogs": {
                "type": "object",
                "additionalProperties": { "$ref": "#/$defs/DogTable" },
            },
        }));
        let d = props(json!({
            "DogTable": { "type": "object", "additionalProperties": true },
        }));
        let set = FieldSet::from_properties(&p, &d, &[]);
        assert_eq!(set.by_key("dogs").unwrap().kind, FieldKind::Opaque);
        assert!(!set.by_key("dogs").unwrap().editable);
    }

    /// An array or a map whose entries are tables with named properties is
    /// `Entries`, followed through a `$ref` and an `anyOf` with `null`. A map
    /// of scalars, a map of free-form objects and an array of anything else
    /// keep the kind they had.
    #[test]
    fn an_array_or_map_of_tables_is_entries_and_nothing_else_changes() {
        let defs = props(json!({
            "Client": { "type": "object", "properties": { "name": { "type": "string" } } },
            "Clients": { "type": "array", "items": { "$ref": "#/$defs/Client" } },
            "Free": { "type": "object", "additionalProperties": true },
        }));
        let table = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        for (name, schema, want) in [
            (
                "array, inline",
                json!({ "type": "array", "items": table }),
                FieldKind::Entries(Entries::Array),
            ),
            (
                "array, $ref on items",
                json!({ "type": "array", "items": { "$ref": "#/$defs/Client" } }),
                FieldKind::Entries(Entries::Array),
            ),
            (
                "array, anyOf [array, null]",
                json!({ "anyOf": [{ "$ref": "#/$defs/Clients" }, { "type": "null" }] }),
                FieldKind::Entries(Entries::Array),
            ),
            (
                "map, inline",
                json!({ "type": "object", "additionalProperties": table }),
                FieldKind::Entries(Entries::Map),
            ),
            (
                "map, $ref on additionalProperties",
                json!({ "type": "object", "additionalProperties": { "$ref": "#/$defs/Client" } }),
                FieldKind::Entries(Entries::Map),
            ),
            (
                "map, anyOf [table, null] values",
                json!({
                    "type": "object",
                    "additionalProperties": { "anyOf": [{ "$ref": "#/$defs/Client" }, { "type": "null" }] },
                }),
                FieldKind::Entries(Entries::Map),
            ),
            (
                "map of strings",
                json!({ "type": "object", "additionalProperties": { "type": "string" } }),
                FieldKind::Map,
            ),
            (
                "map of free-form objects",
                json!({ "type": "object", "additionalProperties": { "$ref": "#/$defs/Free" } }),
                FieldKind::Opaque,
            ),
            (
                "array of strings",
                json!({ "type": "array", "items": { "type": "string" } }),
                FieldKind::List(ListItem::Text),
            ),
            (
                "array of free-form objects",
                json!({ "type": "array", "items": { "$ref": "#/$defs/Free" } }),
                FieldKind::Opaque,
            ),
        ] {
            assert_eq!(kind_of(&schema, &defs), want, "{name}");
        }
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

    /// An item that can be `null` is not a text or integer list: the
    /// list sub-screen rebuilds every element as a string, so filing any
    /// edit would turn a stored `null` into `"null"`.
    #[test]
    fn a_list_whose_items_can_be_null_is_opaque() {
        let defs = props(json!({
            "MaybeString": { "type": ["string", "null"] },
            "MaybeInteger": { "type": ["integer", "null"] },
        }));
        let list = |items: Value| json!({ "type": "array", "items": items });
        for (name, item, def) in [
            ("string", "string", "MaybeString"),
            ("integer", "integer", "MaybeInteger"),
        ] {
            let plain = list(json!({ "type": item }));
            let expected = if item == "string" {
                ListItem::Text
            } else {
                ListItem::Integer
            };
            assert_eq!(kind_of(&plain, &defs), FieldKind::List(expected), "{name}");
            let nullable = [
                json!({ "type": [item, "null"] }),
                json!({ "anyOf": [{ "type": item }, { "type": "null" }] }),
                json!({ "anyOf": [{ "type": item }, { "type": ["null"] }] }),
                json!({ "$ref": format!("#/$defs/{def}") }),
            ];
            for items in nullable {
                assert_eq!(
                    kind_of(&list(items.clone()), &defs),
                    FieldKind::Opaque,
                    "{name} items {items}"
                );
            }
        }
    }

    #[test]
    fn a_ref_to_a_def_that_is_any_of_null_is_nullable() {
        let defs = props(json!({
            "MaybeString": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
        }));
        let schema = json!({ "type": "array", "items": { "$ref": "#/$defs/MaybeString" } });
        assert_eq!(kind_of(&schema, &defs), FieldKind::Opaque);
    }

    /// `schemars` spells a nullable arm as `{"type": ["null"]}` in some
    /// versions (#592).
    #[test]
    fn strip_nullable_reduces_an_any_of_with_a_null_type_list_to_the_other_arm() {
        let defs = Map::new();
        let schema = json!({ "anyOf": [{ "type": "string" }, { "type": ["null"] }] });
        assert_eq!(strip_nullable(&schema, &defs), &json!({ "type": "string" }));
    }

    /// An array whose items are a closed set of strings is a checklist,
    /// however the set is spelled. One that can also be `null` stays
    /// read-only, as a nullable text list does.
    #[test]
    fn a_list_of_a_closed_set_is_a_choice_list_unless_the_set_can_be_null() {
        let defs = props(json!({
            "Kind": { "enum": ["a", "b", "c"] },
            "Mode": {
                "oneOf": [
                    { "type": "string", "const": "x" },
                    { "type": "string", "const": "y" },
                ],
            },
            "MaybeKind": { "anyOf": [{ "$ref": "#/$defs/Kind" }, { "type": "null" }] },
        }));
        let list = |items: Value| json!({ "type": "array", "items": items });
        let names = |names: &[&str]| {
            FieldKind::List(ListItem::Choice(
                names.iter().map(|name| (*name).to_owned()).collect(),
            ))
        };
        for (name, items, want) in [
            (
                "enum",
                json!({ "type": "string", "enum": ["a", "b", "c"] }),
                names(&["a", "b", "c"]),
            ),
            (
                "enum without a type",
                json!({ "enum": ["c", "a"] }),
                names(&["c", "a"]),
            ),
            (
                "oneOf of consts",
                json!({ "oneOf": [{ "const": "x" }, { "const": "y" }] }),
                names(&["x", "y"]),
            ),
            (
                "$ref to an enum",
                json!({ "$ref": "#/$defs/Kind" }),
                names(&["a", "b", "c"]),
            ),
            (
                "$ref to a oneOf",
                json!({ "$ref": "#/$defs/Mode" }),
                names(&["x", "y"]),
            ),
            (
                "enum that lists null",
                json!({ "enum": ["a", null] }),
                FieldKind::Opaque,
            ),
            (
                "type list naming null",
                json!({ "type": ["string", "null"], "enum": ["a", "b"] }),
                FieldKind::Opaque,
            ),
            (
                "anyOf with null",
                json!({ "anyOf": [{ "enum": ["a", "b"] }, { "type": "null" }] }),
                FieldKind::Opaque,
            ),
            (
                "$ref to an anyOf with null",
                json!({ "$ref": "#/$defs/MaybeKind" }),
                FieldKind::Opaque,
            ),
            ("empty enum", json!({ "enum": [] }), FieldKind::Opaque),
        ] {
            assert_eq!(kind_of(&list(items), &defs), want, "{name}");
        }
    }

    #[test]
    fn an_empty_enum_is_opaque_and_a_one_member_enum_is_a_choice() {
        let defs = Map::new();
        assert_eq!(kind_of(&json!({ "enum": [] }), &defs), FieldKind::Opaque);
        assert_eq!(
            kind_of(&json!({ "enum": ["only"] }), &defs),
            FieldKind::Choice(vec!["only".into()])
        );
    }

    /// A `oneOf` is a choice only when every arm names its value, so one arm
    /// without a `const` keeps the field from becoming one.
    #[test]
    fn a_one_of_with_an_arm_lacking_a_const_is_not_a_choice() {
        let schema = json!({
            "oneOf": [{ "const": "x" }, { "type": "string" }]
        });
        assert!(!matches!(
            kind_of(&schema, &Map::new()),
            FieldKind::Choice(_)
        ));
    }

    #[test]
    fn an_enum_with_a_non_string_member_is_not_a_choice() {
        let schema = json!({ "enum": ["a", 1] });
        assert!(!matches!(
            kind_of(&schema, &Map::new()),
            FieldKind::Choice(_)
        ));
    }
}
