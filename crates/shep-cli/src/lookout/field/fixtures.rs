//! Test fixtures shared by this module's four halves.
//!
//! [`real_field_set`] reads the Flockfile schema off disk, which every
//! half has a claim to make about: `bounds` that a range is published,
//! `schema` that a grammar is marked, `init` that a neighbour names a
//! real field. One copy, so a change to the schema moves one fixture.

use super::FieldSet;

pub(super) fn props(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().unwrap().clone()
}

/// The real Flockfile schema's fields, in the order a pane would build them.
pub(super) fn real_field_set() -> FieldSet {
    let schema = shep_core::config::flockfile_schema_json().to_value();
    let defs = schema["$defs"].as_object().unwrap();
    let props = defs["AppConfig"]["properties"].as_object().unwrap();
    FieldSet::from_properties(props, defs, shep_core::config::GROUP_ORDER)
}
