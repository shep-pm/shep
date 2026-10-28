use core::fmt;
use std::collections::BTreeMap;
// use schemars::generate
use serde::Deserializer;

use crate::config::table::deserialize_table;

/// One value an `env` table may carry: a string, or a bare boolean or whole
/// number an operator wrote without quoting.
///
/// Exists only to read a Flockfile, where a bare value is a plausible
/// shortcut. It never rides the wire: [`AppConfig`](crate::config::AppConfig)
/// serializes through impls that see only `String`. Debug does not leak an
/// env value.
pub(super) enum EnvValue {
    /// A quoted value, kept verbatim
    Str(String),
    /// A bare `true` or `false`
    Bool(bool),
    /// A whole number, signed or unsigned
    Int(i128),
}

impl fmt::Debug for EnvValue {
    /// Prints only the shape of the value, never its contents.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(_) => f.write_str("<str>"),
            Self::Bool(_) => f.write_str("<bool>"),
            Self::Int(_) => f.write_str("<int>"),
        }
    }
}

impl EnvValue {
    /// Renders the value as the string a process receives. Consuming: a borrow
    /// would force the `Str` arm to clone.
    #[must_use]
    fn into_string(self) -> String {
        match self {
            Self::Str(s) => s,
            Self::Bool(b) => b.to_string(),
            Self::Int(n) => n.to_string(),
        }
    }
}

/// Reads an `env` table, rendering each [`EnvValue`] as the string a process
/// receives. The `deserialize_with` on [`AppConfig::env`](crate::config::AppConfig::env).
///
/// The table goes through the shared reader in `config::table`, so a null
/// `env` or a bare TOML datetime is refused as `` `env` must be a table ``.
/// Absent is not refused: `AppConfig`'s `#[serde(default)]` fills it before
/// this runs.
pub(super) fn deserialize_env<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    Ok(deserialize_table::<D, EnvValue>(de, "env")?
        .into_iter()
        .map(|(k, v)| (k, v.into_string()))
        .collect())
}

impl<'de> serde::de::Deserialize<'de> for EnvValue {
    /// Reads one `env` value in whatever raw form it arrives.
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct EnvValueVisitor;

        impl serde::de::Visitor<'_> for EnvValueVisitor {
            type Value = EnvValue;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a string, boolean, or whole number")
            }

            /// A quoted value, kept verbatim.
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<EnvValue, E> {
                Ok(EnvValue::Str(v.to_string()))
            }

            /// A quoted value from a non-borrowed source.
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<EnvValue, E> {
                Ok(EnvValue::Str(v))
            }

            /// A bare `true` or `false`.
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<EnvValue, E> {
                Ok(EnvValue::Bool(v))
            }

            /// A whole signed number.
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<EnvValue, E> {
                // i128 holds the full i64 range losslessly.
                Ok(EnvValue::Int(i128::from(v)))
            }

            /// A whole unsigned number. Only JSON can produce one beyond
            /// `i64::MAX`; TOML's own spec bounds integers to signed 64-bit,
            /// so its `visit_u64` input is always within `i64::MAX` and the
            /// wider type is invisible from a TOML Flockfile. i128 holds
            /// whatever we receive losslessly, so no value is refused.
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<EnvValue, E> {
                Ok(EnvValue::Int(i128::from(v)))
            }

            /// A float, refused. `f64` carries no trailing zero and no
            /// written precision, so `1.10` would reach the process as
            /// `1.1`. The value is left out of the message: an `env` value
            /// never reaches a log.
            fn visit_f64<E: serde::de::Error>(self, _v: f64) -> Result<EnvValue, E> {
                Err(E::custom(
                    "a float env value loses its written form, quote it",
                ))
            }
        }

        de.deserialize_any(EnvValueVisitor)
    }
}

#[cfg(test)]
mod tests {

    // use schemars::generate

    use super::*;

    use crate::config::AppConfig;

    use super::super::testing::*;

    /// fails if a wire `env` that is not a table stops being refused.
    ///
    /// `AppConfig` decodes from a peer as well as from a Flockfile, and both
    /// paths read the table through the same reader. The wire refused a null
    /// map before, in serde's words: what changed is the words, not what is
    /// refused. A real table still decodes.
    #[test]
    fn a_wire_env_that_is_not_a_table_is_still_refused() {
        for (label, value) in [
            ("null", "null"),
            ("a bare string", "\"postgres://hunter2\""),
        ] {
            let src = format!(r#"{{"name":"web","script":"./srv","env":{value}}}"#);
            let err = serde_json::from_str::<AppConfig>(&src)
                .expect_err("a wire env that is not a table must be refused, as it always was")
                .to_string();
            assert!(err.contains("`env` must be a table"), "{label}: {err}");
            assert!(
                !err.contains("hunter2"),
                "the refusal leaked the value: {err}"
            );
        }

        let config: AppConfig =
            serde_json::from_str(r#"{"name":"web","script":"./srv","env":{"A":"b"}}"#)
                .expect("a real env table on the wire still decodes");
        assert_eq!(config.env["A"], "b");
    }

    /// `EnvValue::Debug` prints only the kind, never the value — the exact
    /// string is pinned so a derived `Debug` (which prints the contents) fails
    /// here. This is the unit half of the redaction guarantee.
    #[test]
    fn env_value_debug_never_prints_the_value() {
        let cases = [
            (EnvValue::Str("postgres://secret".to_string()), "<str>"),
            (EnvValue::Bool(true), "<bool>"),
            (EnvValue::Int(9_223_372_036_854_775_807), "<int>"),
        ];
        for (value, expected) in cases {
            assert_eq!(format!("{value:?}"), expected);
        }
    }

    /// fails if a clause starts or stops leaning on an enforcer outside
    /// this crate. The list is written twice on purpose: without a second
    /// copy, `Proof::Elsewhere` is a free pass past the test above.
    #[test]
    fn the_clauses_enforced_outside_normalize_are_the_ones_named() {
        let claims = refusal_claims();
        let outside: Vec<(&str, &str, &str)> = claims
            .iter()
            .filter_map(|claim| match claim.proof {
                Proof::Elsewhere(place) => Some((claim.field, claim.refusal, place)),
                Proof::Refused { .. } => None,
            })
            .collect();
        assert_eq!(
            outside,
            vec![
                (
                    "env",
                    "a float, since 1.10 would arrive as 1.1",
                    "EnvValue's Deserialize, before normalize sees the table",
                ),
                (
                    "dogs",
                    "a value that is not a table",
                    "DogTable's Deserialize, before normalize sees the table",
                ),
                (
                    "group",
                    "a name with no group entry",
                    "shep-daemon's privilege::resolve, at spawn",
                ),
                (
                    "group",
                    "another group, unless the shepherd runs as root",
                    "shep-daemon's privilege::resolve, at spawn",
                ),
                (
                    "user",
                    "a name with no passwd entry",
                    "shep-daemon's privilege::resolve, at spawn",
                ),
                (
                    "user",
                    "another user, unless the shepherd runs as root",
                    "shep-daemon's privilege::resolve, at spawn",
                ),
            ]
        );
    }
}
