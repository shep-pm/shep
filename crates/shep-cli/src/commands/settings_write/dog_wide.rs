//! A dog-wide key is the dog's own section's, so a sheep's table refuses it
//! before anything is sent.

use serde_json::Value as Json;

use super::WriteRefusal;
use crate::dog_settings::standard::Standard;

/// The refusal for a write that would put a dog-wide key in `sheep`'s table
/// for `dog`, or [`None`] when it puts none.
///
/// `keys` is the path under the dog's table, empty for the whole table, whose
/// `value` is then checked key by key.
pub(super) fn dog_wide_refusal(
    standard: &Standard,
    sheep: &str,
    dog: &str,
    keys: &[String],
    value: &Json,
) -> Option<WriteRefusal> {
    let key = match keys.first() {
        Some(first) => standard.is_dog_wide(first).then_some(first.as_str())?,
        None => value
            .as_object()?
            .keys()
            .find(|key| standard.is_dog_wide(key))?
            .as_str(),
    };
    Some(WriteRefusal::Refused(format!(
        "`{key}` is dog-wide: every sheep `{dog}` serves shares one value, so `{sheep}` cannot \
         hold its own. `shep set {dog}.{key} <value>` sets it for all of them"
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shep_core::dogs::manifest::Serves;

    use super::*;

    fn jobs() -> Standard {
        Standard::recognise(
            &json!({ "properties": {
                "poll": { "x-shep-dog-wide": true },
                "model": {},
            } }),
            Serves::OptedIn,
        )
        .expect("a standard dog")
    }

    fn keys(path: &[&str]) -> Vec<String> {
        path.iter().map(|key| (*key).to_string()).collect()
    }

    #[test]
    fn a_dog_wide_key_names_the_dog_level_address() {
        let refusal = dog_wide_refusal(&jobs(), "api", "jobs", &keys(&["poll"]), &json!("5m"))
            .expect("refused");
        let WriteRefusal::Refused(message) = refusal else {
            panic!("not a refusal of the key");
        };
        assert!(message.contains("shep set jobs.poll <value>"), "{message}");
        assert!(message.contains("`api`"), "{message}");
    }

    #[test]
    fn a_key_the_sheep_may_override_is_not_refused() {
        assert_eq!(
            dog_wide_refusal(&jobs(), "api", "jobs", &keys(&["model"]), &json!("x")),
            None
        );
    }

    #[test]
    fn a_whole_table_holding_a_dog_wide_key_is_refused() {
        let value = json!({ "model": "x", "poll": "5m" });
        assert!(dog_wide_refusal(&jobs(), "api", "jobs", &[], &value).is_some());
        assert_eq!(
            dog_wide_refusal(&jobs(), "api", "jobs", &[], &json!({ "model": "x" })),
            None
        );
    }
}
