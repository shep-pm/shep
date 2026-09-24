//! What a dog's own tests compare its config schema against: the keys it
//! declares, and the keys its `--print-config` block writes out.

use crate::dogs::{DogConfig, config_schema};

/// `T`'s top-level keys as its schema names them, sorted.
///
/// # Panics
///
/// When `T`'s schema has no `properties`, which a derived struct always has.
#[track_caller]
#[must_use]
pub fn schema_keys<T: DogConfig + schemars::JsonSchema>() -> Vec<String> {
    let schema = config_schema::<T>();
    let Some(properties) = schema
        .as_value()
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        panic!("{schema:?} has no properties, so it is not a struct's schema");
    };
    let mut keys: Vec<String> = properties.keys().cloned().collect();
    keys.sort();
    keys
}

/// The keys a `--print-config` block sets, sorted: one `#key = value` line
/// each, commented out so the defaults stay the dog's.
///
/// A line opening `# ` is prose, not a setting, and is skipped.
#[must_use]
pub fn printed_keys(block: &str) -> Vec<String> {
    let mut keys: Vec<String> = block
        .lines()
        .filter_map(|line| line.strip_prefix('#'))
        .filter(|setting| !setting.starts_with(char::is_whitespace))
        .filter_map(|setting| {
            setting
                .split(|c: char| c == '=' || c.is_whitespace())
                .next()
        })
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .collect();
    keys.sort();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogs::dog_config;

    #[dog_config]
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
    struct Rotate {
        interval: String,
        #[serde(rename = "keep")]
        generations: u32,
    }

    const PRINTED: &str = "\
# How often to look.
#interval = \"1h\"

# Rotated files to keep.
#keep=7
";

    #[test]
    fn the_schema_names_its_keys_as_serde_spells_them() {
        assert_eq!(schema_keys::<Rotate>(), ["interval", "keep"]);
    }

    #[test]
    fn a_printed_block_names_its_settings_and_not_its_prose() {
        assert_eq!(printed_keys(PRINTED), ["interval", "keep"]);
    }

    /// The comparison a dog writes: two-way, so a stale printed key fails
    /// as surely as a missing one.
    #[test]
    fn a_stale_printed_key_fails_the_comparison() {
        let stale = format!("{PRINTED}#buffer_line = 5\n");
        assert_ne!(printed_keys(&stale), schema_keys::<Rotate>());
        assert_eq!(printed_keys(PRINTED), schema_keys::<Rotate>());
    }
}
