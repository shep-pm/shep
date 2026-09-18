//! Newtypes that exist so a derived `Debug` cannot print a secret (IR-41).

use core::fmt;

use serde::{Deserialize, Serialize};

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{Request, Response};
#[cfg(doc)]
use crate::config::AppConfig;

/// A dog's `[dog.<name>]` config section, carried as TOML text.
///
/// Travels over the socket rather than the child's environment: a dog's
/// section routinely holds webhook credentials, and the socket keeps them
/// out of the process table and out of crash dumps. The manual `Debug`
/// below prints only a length, since [`Response`] derives `Debug`.
///
/// [`Self::as_str`] is the only way out: a `Deref<Target = str>` would hand
/// the type `ToString` and defeat that `Debug`.
///
/// `#[serde(transparent)]`: the wire representation is a bare `String`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DogSectionToml(String);

impl DogSectionToml {
    /// The TOML text, empty when the file has no such section.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for DogSectionToml {
    fn from(toml: String) -> Self {
        Self(toml)
    }
}

/// Prints a length, never the section body. Pinned as an exact string by
/// `dog_section_toml_debug_does_not_leak`.
impl fmt::Debug for DogSectionToml {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DogSectionToml(<{} bytes>)", self.0.len())
    }
}

/// One environment variable's value, on its way to a sheep.
///
/// A newtype for one reason, the same one [`DogSectionToml`] exists for: an
/// env value is the single most secret-dense thing a client can send this
/// daemon (a database URL, an API token, a signing key), and a derived
/// `Debug` on [`Request`] would print it in the clear the moment anything
/// logs a request. Every other secret-bearing field on this wire is already
/// protected by its inner type ([`AppConfig`]'s own manual `Debug` prints
/// `env: <N vars>`), and a bare `String` here would have been the first
/// field in the enum without that protection.
///
/// One direction only. Nothing ever sends one back: [`Request::SheepConfig`]
/// answers with the env keys and no values at all.
///
/// [`Self::as_str`] is the only way out, for the reason
/// [`DogSectionToml`] gives: a `Deref<Target = str>` would hand the type
/// `ToString` too, and `.to_string()` would return the value in the clear,
/// defeating the redacted `Debug` below.
///
/// `#[serde(transparent)]` makes the wire representation identical to a
/// bare `String`, so this newtype changes nothing about
/// [`crate::protocol::PROTOCOL_VERSION`] or the pinned snapshot fixtures.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvValue(String);

impl EnvValue {
    /// The value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for EnvValue {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Debug prints a length and never the value (IR-41); see the type doc for
/// why. Exact-string-tested below (`env_value_debug_does_not_leak`) so a
/// future `#[derive(Debug)]` fails that test instead of silently reopening
/// the leak.
impl fmt::Debug for EnvValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EnvValue(<{} bytes>)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Request, Response};
    use super::*;
    use std::collections::BTreeMap;

    /// Also pins that the newtype protecting this field costs the wire
    /// nothing: a bare string either way, so no fixture and no protocol
    /// version moves for it.
    #[test]
    fn env_value_debug_does_not_leak() {
        let request = Request::SetSheepEnv {
            name: "web".to_string(),
            key: "DATABASE_URL".to_string(),
            value: Some("postgres://user:hunter2@localhost/app".to_string().into()),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("EnvValue(<37 bytes>)"), "{debug}");

        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains(r#""value":"postgres://user:hunter2@localhost/app""#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    }

    /// The exact `Debug` string, not the absence of one value: `entries` is
    /// a map of [`EnvValue`], so what is actually under test is that the
    /// nested redaction renders, and a `contains` check would pass just as
    /// well against a map that printed nothing at all.
    #[test]
    fn put_secrets_round_trips_and_hides_its_values() {
        let request = Request::PutSecrets {
            namespace: "vercel".into(),
            environment: "production".into(),
            entries: BTreeMap::from([(
                "API_KEY".to_string(),
                EnvValue::from("sk_live".to_string()),
            )]),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);
        assert_eq!(
            format!("{request:?}"),
            "PutSecrets { namespace: \"vercel\", environment: \"production\", \
             entries: {\"API_KEY\": EnvValue(<7 bytes>)} }"
        );
    }

    /// IR-41. `EnvValue` is what keeps the derive on `Request` safe, and this
    /// pins that the batch variant actually uses it.
    #[test]
    fn set_sheep_env_batch_debug_does_not_leak() {
        let request = Request::SetSheepEnvBatch {
            name: "web".to_string(),
            entries: BTreeMap::from([(
                "DB_PASSWORD".to_string(),
                EnvValue::from("hunter2".to_string()),
            )]),
            force: false,
            dry_run: true,
        };
        assert_eq!(
            format!("{request:?}"),
            "SetSheepEnvBatch { name: \"web\", entries: {\"DB_PASSWORD\": EnvValue(<7 bytes>)}, \
             force: false, dry_run: true }"
        );
    }

    #[test]
    fn dog_section_toml_debug_does_not_leak() {
        // A dog's section routinely holds webhook credentials. Pinned as an
        // exact string, so a `#[derive(Debug)]` on `DogSectionToml` fails
        // here.
        let toml: DogSectionToml =
            "webhook_url = \"https://discord.com/api/webhooks/1/super-secret-token\"\n"
                .to_string()
                .into();
        assert_eq!(format!("{toml:?}"), "DogSectionToml(<70 bytes>)");

        let response = Response::DogSection { toml };
        assert_eq!(
            format!("{response:?}"),
            "DogSection { toml: DogSectionToml(<70 bytes>) }"
        );
    }
}
