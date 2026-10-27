//! The settings screen's field tree: `shep.toml`'s schema, flattened.
//!
//! [`shep_toml_shape`] reads shep-core's own schema of the file, so a key
//! added to a section shows up here with no change in lookout. What it takes
//! out is the four keys that are about one dog each, which the dog's row
//! carries instead.

use std::sync::OnceLock;

use serde_json::Value;

use super::{Child, Flattened, flattened_with_maps};

/// The `[daemon]` keys that belong to one dog. Not in the settings tree.
pub(crate) const PER_DOG_KEYS: [&str; 4] = [
    "daemon.enabled_dogs",
    "daemon.adopted_dogs",
    "daemon.channel_dogs",
    "daemon.dog_grants",
];

/// The sections of `shep.toml`, in the order the settings screen draws
/// them. A section the schema gains and this list lacks sorts last.
const SECTION_ORDER: [&str; 6] = [
    "daemon",
    "whistle",
    "secrets",
    "style",
    "interpreters",
    "hosts",
];

/// `shep.toml`'s schema flattened into dotted rows, built once.
pub(crate) fn shep_toml_shape() -> &'static Flattened {
    static SHAPE: OnceLock<Flattened> = OnceLock::new();
    SHAPE.get_or_init(|| {
        let mut schema = shep_core::config::daemon_config_schema_json().to_value();
        drop_per_dog_keys(&mut schema);
        let mut shape = flattened_with_maps(&schema);
        let rank = |child: &Child| {
            let key = match child {
                Child::Leaf(index) => shape.fields.fields().get(*index).map(|f| f.key.as_str()),
                Child::Table(index) => shape.outline.tables.get(*index).map(|t| t.path.as_str()),
            };
            SECTION_ORDER
                .iter()
                .position(|section| Some(*section) == key)
                .unwrap_or(SECTION_ORDER.len())
        };
        let mut roots = shape.outline.roots.clone();
        roots.sort_by_key(rank);
        shape.outline.roots = roots;
        shape
    })
}

/// Removes [`PER_DOG_KEYS`] from the `[daemon]` section's properties.
fn drop_per_dog_keys(schema: &mut Value) {
    let Some(name) = schema
        .pointer("/properties/daemon/$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .map(str::to_owned)
    else {
        return;
    };
    if let Some(properties) = schema
        .pointer_mut(&format!("/$defs/{name}/properties"))
        .and_then(Value::as_object_mut)
    {
        for key in PER_DOG_KEYS {
            if let Some(leaf) = key.strip_prefix("daemon.") {
                properties.remove(leaf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::field::FieldKind;

    fn kind(key: &str) -> FieldKind {
        shep_toml_shape()
            .fields
            .by_key(key)
            .unwrap_or_else(|| panic!("{key} is a row"))
            .kind
            .clone()
    }

    #[test]
    fn the_four_per_dog_keys_are_not_rows() {
        let shape = shep_toml_shape();
        for key in PER_DOG_KEYS {
            assert!(shape.fields.by_key(key).is_none(), "{key} is a row");
        }
        assert!(shape.fields.by_key("daemon.log_level").is_some());
    }

    #[test]
    fn a_closed_set_is_a_choice_and_a_map_of_scalars_is_editable() {
        assert!(matches!(kind("daemon.log_level"), FieldKind::Choice(_)));
        assert!(matches!(kind("style.level"), FieldKind::Choice(_)));
        assert_eq!(kind("interpreters"), FieldKind::Map);
        assert!(
            shep_toml_shape()
                .fields
                .by_key("interpreters")
                .unwrap()
                .editable
        );
    }

    #[test]
    fn the_sections_come_in_the_order_the_screen_draws_them() {
        let shape = shep_toml_shape();
        let roots: Vec<String> = shape
            .outline
            .roots
            .iter()
            .map(|child| match child {
                Child::Leaf(index) => shape.fields.fields()[*index].key.clone(),
                Child::Table(index) => shape.outline.tables[*index].path.clone(),
            })
            .collect();
        assert_eq!(
            roots,
            [
                "daemon",
                "whistle",
                "secrets",
                "style",
                "interpreters",
                "hosts"
            ]
        );
    }

    #[test]
    fn a_row_carries_the_apply_cost_the_schema_names() {
        assert_eq!(
            shep_toml_shape()
                .fields
                .by_key("daemon.socket")
                .unwrap()
                .apply
                .as_deref(),
            Some("restart")
        );
    }
}
