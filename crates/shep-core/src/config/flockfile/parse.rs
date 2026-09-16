//! The one per-format deserializer dispatch, and the unknown-key callback.
//!
//! Both public entry points route through here, so a format added in one
//! place reaches both, and a key no field claims is reported the same way
//! whichever format wrote it.

use super::{
    error::FlockfileError,
    format::FlockFormat,
    json5::{MAX_JSON5_NESTING_DEPTH, json5_nesting_depth},
};

// Its one caller deserializes into `serde_json::Value`, which claims every
// key, so `parse_into_ignoring`'s callback never fires here; delegating
// costs nothing behaviorally and drops a second four-arm format dispatch.
pub(super) fn parse_into<T: serde::de::DeserializeOwned>(
    source: &str,
    format: FlockFormat,
) -> Result<T, FlockfileError> {
    parse_into_ignoring(source, format, |_| {})
}

// The one four-arm format dispatch, shared by `parse_into` (an empty
// `on_ignored`) and `RawFlockfile::new` (a real one). Each format's
// `Deserializer` routes through `serde_ignored::deserialize`, calling
// `on_ignored` once per key the target type did not claim, recursing into
// nested structs (a Flockfile's `readiness_probe`/`liveness_probe` tables
// included). The real callback is used only by `Flockfile::parse` and
// `parse_declared`: those are the two places a document really is a
// hand-written file, where an unrecognized key means a typo. Everywhere else
// the same `AppConfig`/`ProbeConfig` shape rides the wire, where it means a
// newer peer instead, which is why the two types dropped
// `deny_unknown_fields` rather than this function replacing it everywhere.
pub(super) fn parse_into_ignoring<T: serde::de::DeserializeOwned>(
    source: &str,
    format: FlockFormat,
    mut on_ignored: impl FnMut(&str),
) -> Result<T, FlockfileError> {
    match format {
        FlockFormat::Toml => serde_ignored::deserialize(toml::Deserializer::new(source), |path| {
            on_ignored(&path.to_string());
        })
        .map_err(|e| FlockfileError::Toml(e.to_string())),
        // Default options, so YAML 1.1 resolution stays on for the whole
        // document: `autorestart: yes` is a bool, and an unquoted `yes` under
        // `env` is the string "true". Quote an env value whose text matters.
        FlockFormat::Yaml => serde_saphyr::with_deserializer_from_str(source, |de| {
            serde_ignored::deserialize(de, |path| on_ignored(&path.to_string()))
        })
        .map_err(|e| FlockfileError::Yaml(e.to_string())),
        FlockFormat::Json => {
            let mut de = serde_json::Deserializer::from_str(source);
            let value = serde_ignored::deserialize(&mut de, |path| on_ignored(&path.to_string()))
                .map_err(|e| FlockfileError::Json(e.to_string()))?;
            // `serde_json::from_str` checks this too, to catch trailing
            // garbage after a value that otherwise parsed fine.
            de.end().map_err(|e| FlockfileError::Json(e.to_string()))?;
            Ok(value)
        }
        FlockFormat::Json5 => {
            if json5_nesting_depth(source) > MAX_JSON5_NESTING_DEPTH {
                return Err(FlockfileError::Json5(
                    "nesting depth exceeds 64".to_string(),
                ));
            }
            let mut de = json5::Deserializer::from_str(source)
                .map_err(|e| FlockfileError::Json5(e.to_string()))?;
            serde_ignored::deserialize(&mut de, |path| on_ignored(&path.to_string()))
                .map_err(|e| FlockfileError::Json5(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flockfile::file::Flockfile;

    #[test]
    fn yaml_deep_nesting_is_rejected_without_crashing() {
        // 5000-deep flow-style nesting must return Err from serde-saphyr,
        // never overflow the stack.
        let deep = "[".repeat(5000);
        let result = Flockfile::parse(&deep, FlockFormat::Yaml);
        assert!(matches!(result, Err(FlockfileError::Yaml(_))));
    }

    #[test]
    fn yaml_alias_bomb_is_bounded() {
        // Billion-laughs shape: each level aliases the previous twice. The
        // backend must reject it or resolve it bounded; completing
        // quickly is the assertion.
        let mut bomb = String::from("a: &a [\"x\",\"x\"]\n");
        for i in 1..9 {
            bomb.push_str(&format!(
                "{c}: &{c} [*{p},*{p}]\n",
                c = (b'a' + i) as char,
                p = (b'a' + i - 1) as char
            ));
        }
        let result = Flockfile::parse(&bomb, FlockFormat::Yaml);
        assert!(result.is_err(), "alias bomb must not produce a valid flock");
    }

    /// A raw boolean or number anywhere under `env` reads as its string form,
    /// in every format. This is the format-dispatch half of the coercion:
    /// the `deserialize_with` on `AppConfig::env` is the one code path, so a
    /// raw scalar must land at `config.env` through TOML, YAML, JSON, and
    /// JSON5 without a format-specific branch.
    #[test]
    fn a_raw_scalar_env_value_is_a_string_in_every_format() {
        let cases: [(FlockFormat, &str); 4] = [
            (
                FlockFormat::Toml,
                "[[app]]\nname = \"web\"\nscript = \"./srv\"\nenv = { SOME_BOOL = true, PORT = 8080 }\n",
            ),
            (
                FlockFormat::Yaml,
                "app:\n  - name: web\n    script: ./srv\n    env:\n      SOME_BOOL: true\n      PORT: 8080\n",
            ),
            (
                FlockFormat::Json,
                r#"{"app":[{"name":"web","script":"./srv","env":{"SOME_BOOL":true,"PORT":8080}}]}"#,
            ),
            (
                FlockFormat::Json5,
                "{ app: [{ name: \"web\", script: \"./srv\", env: { SOME_BOOL: true, PORT: 8080 } }] }",
            ),
        ];
        for (format, text) in cases {
            let flock = Flockfile::parse(text, format)
                .unwrap_or_else(|e| panic!("{format:?} refused a raw scalar env value: {e}"));
            let env = &flock.apps[0].env;
            assert_eq!(
                env.get("SOME_BOOL").map(String::as_str),
                Some("true"),
                "{format:?}: SOME_BOOL"
            );
            assert_eq!(
                env.get("PORT").map(String::as_str),
                Some("8080"),
                "{format:?}: PORT"
            );
        }
    }

    /// YAML resolves a bare scalar before serde sees it, so an unquoted `yes`
    /// under `env` arrives as the string "true" and `0x1F` as "31". That is
    /// the cost of the same resolution making `autorestart: yes` a bool, and
    /// quoting is the operator's way out. Pinned so it cannot drift unseen.
    #[test]
    fn yaml_resolves_a_bare_env_scalar_before_serde_sees_it() {
        let text = "app:\n  - name: web\n    script: ./srv\n    autorestart: yes\n    env:\n      BARE: yes\n      HEX: 0x1F\n      QUOTED: 'yes'\n";
        let flock = Flockfile::parse(text, FlockFormat::Yaml).unwrap();
        let app = &flock.apps[0];
        assert!(app.autorestart, "`autorestart: yes` must stay a bool");
        assert_eq!(app.env.get("BARE").map(String::as_str), Some("true"));
        assert_eq!(app.env.get("HEX").map(String::as_str), Some("31"));
        assert_eq!(app.env.get("QUOTED").map(String::as_str), Some("yes"));
    }

    /// fails if a format other than TOML loses the unknown-key refusal.
    /// `parse_into_ignoring` dispatches over all four; a regression here
    /// means a format bypassed `serde_ignored` on the way through.
    #[test]
    fn a_misspelled_key_is_refused_in_every_parse_format() {
        let cases: [(FlockFormat, &str); 4] = [
            (
                FlockFormat::Toml,
                "[[app]]\nname = \"web\"\nscript = \"./srv\"\nmax_restrts = 5\n",
            ),
            (
                FlockFormat::Yaml,
                "app:\n  - name: web\n    script: ./srv\n    max_restrts: 5\n",
            ),
            (
                FlockFormat::Json,
                r#"{"app":[{"name":"web","script":"./srv","max_restrts":5}]}"#,
            ),
            (
                FlockFormat::Json5,
                "{ app: [{ name: \"web\", script: \"./srv\", max_restrts: 5 }] }",
            ),
        ];
        for (format, text) in cases {
            let err = Flockfile::parse(text, format).expect_err("a typo must be refused");
            assert!(
                matches!(err, FlockfileError::UnknownKeys { .. }),
                "{format:?}: got {err:?}"
            );
        }
    }
}
