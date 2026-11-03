//! Which keys of a dog's settings are dog-wide, read off the schema the
//! dog publishes.

use std::collections::BTreeSet;

use serde_json::Value;

use super::{DOG_WIDE_KEY, DogConfig, config_schema};

/// How many `allOf`, `anyOf`, `oneOf` and `$ref` steps are followed below the
/// root: a type nested deeper than any settings type reads as plain.
const MAX_DEPTH: usize = 8;

/// The property names `T` marks `#[shep(dog_wide)]`, at the root of its
/// schema and in each variant or flattened type it is made of.
pub(super) fn dog_wide_keys<T: DogConfig + schemars::JsonSchema>() -> BTreeSet<String> {
    let schema = config_schema::<T>();
    let mut keys = BTreeSet::new();
    collect(schema.as_value(), schema.as_value(), MAX_DEPTH, &mut keys);
    keys
}

fn collect(root: &Value, node: &Value, depth: usize, keys: &mut BTreeSet<String>) {
    let Some(depth) = depth.checked_sub(1) else {
        return;
    };
    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        keys.extend(
            properties
                .iter()
                .filter(|(_, property)| property.get(DOG_WIDE_KEY) == Some(&Value::Bool(true)))
                .map(|(name, _)| name.clone()),
        );
    }
    for branch in ["allOf", "anyOf", "oneOf"] {
        for child in node
            .get(branch)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect(root, child, depth, keys);
        }
    }
    let target = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| root.pointer(pointer));
    if let Some(target) = target {
        collect(root, target, depth, keys);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::dogs::{DOG_WIDE_KEY, SECRET_KEY, config_schema, dog_config};

    use super::dog_wide_keys;

    #[dog_config]
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
    struct Marked {
        #[shep(dog_wide)]
        poll: String,
        #[shep(secret, dog_wide)]
        token: String,
        #[shep(dog_wide)]
        #[shep(secret)]
        key: String,
        model: String,
    }

    #[dog_config]
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
    struct Plain {
        poll: String,
        token: String,
        key: String,
        model: String,
    }

    #[test]
    fn a_dog_wide_field_gains_the_marker_and_nothing_else() {
        let marked = config_schema::<Marked>();
        let plain = config_schema::<Plain>();

        let mut expected = plain
            .as_value()
            .pointer("/properties/poll")
            .cloned()
            .unwrap();
        expected[DOG_WIDE_KEY] = json!(true);
        assert_eq!(
            marked.as_value().pointer("/properties/poll"),
            Some(&expected)
        );
        assert_eq!(
            marked.as_value().pointer("/properties/model"),
            plain.as_value().pointer("/properties/model"),
            "an unmarked field is untouched"
        );
    }

    #[test]
    fn the_marker_key_is_the_one_shep_core_publishes() {
        assert_eq!(DOG_WIDE_KEY, "x-shep-dog-wide");
        assert_eq!(
            config_schema::<Marked>()
                .as_value()
                .pointer("/properties/poll/x-shep-dog-wide"),
            Some(&json!(true)),
            "`shep-macros` writes the key verbatim and has no way to name the constant"
        );
    }

    #[test]
    fn dog_wide_and_secret_ride_one_field_in_either_spelling() {
        let schema = config_schema::<Marked>();
        for field in ["token", "key"] {
            let property = schema
                .as_value()
                .pointer(&format!("/properties/{field}"))
                .unwrap();
            assert_eq!(property[DOG_WIDE_KEY], json!(true), "{field}");
            assert_eq!(property[SECRET_KEY], json!(true), "{field}");
        }
    }

    #[dog_config]
    #[derive(schemars::JsonSchema)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
    enum Backend {
        Local {
            #[shep(dog_wide)]
            root: String,
            quiet: bool,
        },
        Remote {
            host: String,
        },
    }

    #[dog_config]
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
    struct Shared {
        #[shep(dog_wide)]
        region: String,
    }

    #[dog_config]
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
    struct Flattens {
        #[serde(flatten)]
        shared: Shared,
        model: String,
    }

    #[dog_config]
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code, reason = "read by the generated schema, not by Rust")]
    struct Renamed {
        #[shep(dog_wide)]
        #[serde(rename = "poll-every")]
        poll: String,
    }

    fn keys<T: crate::dogs::DogConfig + schemars::JsonSchema>() -> Vec<String> {
        dog_wide_keys::<T>().into_iter().collect()
    }

    #[test]
    fn the_dog_wide_keys_are_the_marked_properties_and_no_others() {
        assert_eq!(keys::<Marked>(), ["key", "poll", "token"]);
        assert_eq!(keys::<Plain>(), Vec::<String>::new());
    }

    #[test]
    fn a_rename_changes_the_key_that_is_dog_wide() {
        assert_eq!(keys::<Renamed>(), ["poll-every"]);
    }

    #[test]
    fn a_marked_field_of_a_variant_or_a_flattened_type_is_found() {
        assert_eq!(keys::<Backend>(), ["root"]);
        assert_eq!(keys::<Flattens>(), ["region"]);
    }
}
