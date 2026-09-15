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

/// A JSON value rendered the way a config row draws it: a scalar shows
/// bare, `null` shows `(unset)`, anything else shows compact JSON.
///
/// Shared by [`ConfigPane::value`], reading the stored value, and
/// [`ConfigPane::edited_value`], reading a filed one, so the two sides of
/// an `old -> new` cell are rendered by one rule rather than two that can
/// drift.
pub(super) fn render_json(value: &Value) -> String {
    match value {
        Value::Null => "(unset)".to_owned(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
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
    // config write at all. Read-only here, so the pane never offers an
    // edit the daemon would refuse.
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
/// column ([`super::view::sheep::field_value_text`]), the same move
/// [`sheep_fields`] made for the field set itself: two rows reading the same
/// value off two different screens and disagreeing on its units is exactly
/// the divergence a shared function forecloses rather than a pair of tests
/// happening to agree.
///
/// Display only: [`ConfigPane::value`] is what an editor still seeds and
/// sends, so a suffix minted here never travels back out as part of a
/// value.
///
/// A `raw` that fails to parse, including whatever is mid-edit, comes back
/// unchanged: this has no business guessing at a string shep is about to
/// refuse on its own.
///
/// Only a bare number is annotated. A value naming its own unit is the
/// operator's spelling and survives as written, so a `60s` on disk is never
/// redrawn as the `1m` its own `Display` would canonicalize it to.
pub(crate) fn resolved_display(fields: &FieldSet, key: &str, raw: &str) -> String {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return raw.to_owned();
    }
    match fields.by_key(key).and_then(|field| field.value_kind) {
        Some(ValueKind::MemSize) => raw
            .parse::<MemSize>()
            .map_or_else(|_| raw.to_owned(), |_| format!("{raw} B")),
        Some(ValueKind::UpDuration) => raw
            .parse::<UpDuration>()
            .map_or_else(|_| raw.to_owned(), |_| format!("{raw}ms")),
        None => raw.to_owned(),
    }
}
