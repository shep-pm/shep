//! The sheep field set, and how one stored value reads on the screen.
//!
//! The field list is the Flockfile schema rather than a second list kept in
//! step with it, so a property shep scaffolds is a property the pane can
//! edit the day it lands.

use serde_json::{Map, Value};
use shep_core::config::{AppConfig, ApplyGroup, GROUP_ORDER, apply_group, flockfile_schema_json};
use shep_core::values::{MemSize, UpDuration};

use super::super::field::{FieldSet, ValueKind};

// Link-only (IR-32): these are the pane methods that read what this module
// builds.
#[cfg(doc)]
use super::ConfigPane;

/// The text a config row shows for a value the config never carried, or
/// carries as `null`: what [`render_json`] mints for a `null`, and the text
/// every call site that has to tell an unset row from a set one matches on.
/// One name for both, so a rendering change cannot leave a call site seeding
/// an editor with the word `(unset)`.
pub(in crate::lookout) const UNSET: &str = "(unset)";

/// The text a row shows for a value equal to the schema's own default. The
/// sheep pane's read-only config column mints it, being the one screen that
/// compares a stored value against a default.
pub(in crate::lookout) const DEFAULT: &str = "(default)";

/// A JSON value rendered the way a config row draws it: a scalar shows
/// bare, `null` shows [`UNSET`], anything else shows compact JSON.
///
/// Shared by [`ConfigPane::value`], reading the stored value, and
/// [`ConfigPane::edited_value`], reading a filed one, so the two sides of
/// an `old -> new` cell are rendered by one rule rather than two that can
/// drift.
pub(super) fn render_json(value: &Value) -> String {
    match value {
        Value::Null => UNSET.to_owned(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// How many entries an array or map of tables holds, the whole of what its
/// row says: `3 entries`, `1 entry`, `0 entries`, or [`UNSET`] for an absent
/// or `null` value, or one that is neither an array nor an object.
pub(super) fn entries_text(value: Option<&Value>) -> String {
    let count = match value {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Object(map)) => map.len(),
        _ => return render_json(&Value::Null),
    };
    match count {
        1 => "1 entry".to_owned(),
        count => format!("{count} entries"),
    }
}

/// The field set and the value map a sheep's config renders as.
///
/// Shared by [`ConfigPane::sheep`] and the sheep pane's read-only listing,
/// so group order and the read-only marking on a `Structural` field cannot
/// differ between the two screens. Built from the Flockfile schema rather
/// than from a second list of names, for the reason
/// [`ConfigPane::sheep`]'s own doc gives.
pub(crate) fn sheep_fields(config: &AppConfig) -> (FieldSet, Map<String, Value>) {
    let schema = flockfile_schema_json().to_value();
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let properties = defs
        .get("AppConfig")
        .and_then(|app| app.get("properties"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let set = FieldSet::from_properties(&properties, &defs, GROUP_ORDER);
    // A Structural field is identity or flock shape, not a runtime knob:
    // `name` cannot drift without becoming a different sheep, and
    // `instances` is routed through `handle_scale` rather than through a
    // config write at all.
    let fields = FieldSet::from_fields(
        set.fields()
            .iter()
            .cloned()
            .map(|mut field| {
                if apply_group(&field.key) == ApplyGroup::Structural {
                    field.editable = false;
                }
                field
            })
            .collect(),
        GROUP_ORDER,
    );
    let values = serde_json::to_value(config)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    (fields, values)
}

/// `raw` resolved through `key`'s own grammar in `fields`, when `key` is one
/// of shep-core's unit types: a bare number is annotated with the unit an
/// operator would otherwise have to already know the convention for.
///
/// Shared by [`ConfigPane::display_value`] and the sheep pane's read-only
/// column, so two screens cannot disagree on a value's units. Display only:
/// [`ConfigPane::value`] is what an editor seeds and sends.
pub(crate) fn resolved_display(fields: &FieldSet, key: &str, raw: &str) -> String {
    resolved_by_kind(fields.by_key(key).and_then(|field| field.value_kind), raw)
}

/// [`resolved_display`] for a field whose [`ValueKind`] the caller already
/// holds.
pub(super) fn resolved_by_kind(kind: Option<ValueKind>, raw: &str) -> String {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return raw.to_owned();
    }
    match kind {
        Some(ValueKind::MemSize) => raw
            .parse::<MemSize>()
            .map_or_else(|_| raw.to_owned(), |_| format!("{raw} B")),
        Some(ValueKind::UpDuration) => raw
            .parse::<UpDuration>()
            .map_or_else(|_| raw.to_owned(), |_| format!("{raw}ms")),
        None => raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name every comparison reads is the name the row draws: a null
    /// renders as [`UNSET`], so a call site that matches on the text cannot
    /// drift from the text a row shows.
    #[test]
    fn the_unset_text_is_what_a_null_renders_as() {
        assert_eq!(render_json(&Value::Null), UNSET);
    }
}
