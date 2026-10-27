//! The JSON Schema of `shep.toml`, emitted from the type serde reads it into.

use schemars::Schema;

use super::config::RawDaemonConfig;

/// The schema property that says what applying an edit to a field costs.
///
/// Its value is one of the `APPLY_*` names below. A field with none costs
/// nothing a reader has to be told about.
pub const APPLY_KEY: &str = "x-shep-apply";

/// The shepherd re-reads the file on `shep daemon reload`.
pub const APPLY_RELOAD: &str = "reload";
/// The shepherd has to be stopped and started: a reload never moves what the
/// field names.
pub const APPLY_RESTART: &str = "restart";
/// `shep whistle` has to be restarted. The shepherd never reads the field.
pub const APPLY_WHISTLE: &str = "whistle";
/// The CLI reads the file afresh on every command, and the shepherd never
/// reads the field.
pub const APPLY_NEXT_COMMAND: &str = "next-command";

/// Returns the JSON Schema of `shep.toml`.
///
/// Generated from the type [`DaemonConfig::load`](super::DaemonConfig::load)
/// deserializes into, so the schema and the loader cannot drift. Describes
/// the deserializer, not [`DaemonConfig::load`](super::DaemonConfig::load)'s
/// validation: a `max_cron_sleep` under the floor is a string here, and the
/// loader still refuses it.
///
/// `[dog.<name>]` is absent: each dog's own `--schema` describes its table.
#[must_use]
pub fn daemon_config_schema_json() -> Schema {
    schemars::schema_for!(RawDaemonConfig)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn schema() -> Value {
        serde_json::to_value(daemon_config_schema_json()).unwrap()
    }

    /// The section a `$ref` property points at, from `$defs`.
    fn section<'a>(schema: &'a Value, name: &str) -> &'a Value {
        let target = schema["properties"][name]["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .unwrap_or_else(|| panic!("{name} is a $ref to a section"));
        &schema["$defs"][target]
    }

    #[test]
    fn the_schema_names_every_section_of_shep_toml_and_no_dog_table() {
        let schema = schema();
        let mut top: Vec<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        top.sort_unstable();
        assert_eq!(
            top,
            [
                "daemon",
                "hosts",
                "interpreters",
                "secrets",
                "style",
                "whistle"
            ]
        );
    }

    #[test]
    fn every_scalar_leaf_says_what_applying_it_costs() {
        let schema = schema();
        let known = [
            APPLY_RELOAD,
            APPLY_RESTART,
            APPLY_WHISTLE,
            APPLY_NEXT_COMMAND,
        ];
        let mut leaves: Vec<(String, &Value)> = Vec::new();
        for name in ["daemon", "whistle", "secrets", "style"] {
            for (key, leaf) in section(&schema, name)["properties"].as_object().unwrap() {
                leaves.push((format!("{name}.{key}"), leaf));
            }
        }
        for name in ["hosts", "interpreters"] {
            leaves.push((name.to_owned(), &schema["properties"][name]));
        }
        for (path, leaf) in leaves {
            let apply = leaf[APPLY_KEY]
                .as_str()
                .unwrap_or_else(|| panic!("{path} has no {APPLY_KEY}"));
            assert!(known.contains(&apply), "{path}: {apply:?} is not a cost");
        }
    }

    #[test]
    fn the_schema_defaults_are_the_loaders_defaults() {
        let schema = schema();
        let daemon = &section(&schema, "daemon")["properties"];
        assert_eq!(daemon["environment"]["default"], "production");
        assert_eq!(daemon["log_json"]["default"], false);
    }

    #[test]
    fn a_closed_set_field_names_its_default() {
        let schema = schema();
        assert_eq!(
            section(&schema, "daemon")["properties"]["log_level"]["default"],
            "warn"
        );
    }
}
