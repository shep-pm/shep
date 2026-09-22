//! Reading shep's own `init` block off a field's schema.
//!
//! A schema author writes these; no JSON Schema validator knows them.
//! Each reader drops what it cannot use whole rather than half rendering
//! it, since a pane showing half a neighbour is worse than one showing
//! none.

use serde_json::Value;

use super::Neighbour;

/// The `init.suggest` values, when every entry is a string.
pub(super) fn suggestions(init: Option<&Value>) -> Option<Vec<String>> {
    let values = init?.get("suggest")?.as_array()?;
    let names: Vec<String> = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    (names.len() == values.len()).then_some(names)
}

/// The `init.<key>` values, when every entry is a string. Empty (not
/// `None`) when `init` carries no such key, since [`super::Field::accepts`] and
/// [`super::Field::refuses`] are lists rather than options.
pub(super) fn strings(init: Option<&Value>, key: &str) -> Vec<String> {
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
pub(super) fn neighbours(init: Option<&Value>) -> Vec<Neighbour> {
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::{props, real_field_set};
    use super::super::{FieldKind, FieldSet, field_from};
    use serde_json::Map;
    use serde_json::json;

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
}
