//! Which keys of a dog's settings are dog-wide, read off the schema the
//! dog publishes.

#[cfg(all(test, feature = "schema"))]
mod tests {
    use serde_json::json;

    use crate::dogs::{DOG_WIDE_KEY, SECRET_KEY, config_schema, dog_config};

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

        let mut expected = plain.as_value().pointer("/properties/poll").cloned().unwrap();
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
}
