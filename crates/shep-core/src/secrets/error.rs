use super::format::MAX_VALUE_BYTES;
use core::fmt;

/// Error type returned by this module.
///
/// `#[non_exhaustive]`: shep-core is published, so a new failure variant
/// must not break an out-of-tree `match`.
///
/// Wraps `io::Error`/`serde_json::Error` directly rather than stringifying
/// them, matching [`crate::kv::KvError`], so callers keep the underlying
/// diagnostic through [`core::error::Error::source`]; this type does not
/// derive `Clone`/`PartialEq`/`Eq` as a result.
///
/// No variant carries a secret's value: a message names the key, the
/// namespace or the environment, and nothing else.
#[non_exhaustive]
#[derive(Debug)]
pub enum SecretError {
    /// The store could not be read, written, or replaced.
    Io(std::io::Error),
    /// The store's JSON could not be parsed.
    ///
    /// Refused rather than repaired: a partial read would silently drop
    /// credentials still on disk, and a later write would erase them.
    Decode(serde_json::Error),
    /// A key outside the grammar; carries it verbatim so the message can
    /// quote what was typed.
    InvalidKey(String),
    /// An environment name outside the grammar; carries it verbatim.
    InvalidEnvironment(String),
    /// A value over [`MAX_VALUE_BYTES`].
    ValueTooLong {
        /// The key it was being stored under.
        key: String,
        /// Its length in bytes.
        len: usize,
    },
    /// The store on disk is a version this build does not understand; carries
    /// that version. Nothing was written.
    FutureVersion(u32),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "secret store I/O failed: {err}"),
            Self::Decode(err) => write!(f, "secret store failed to parse: {err}"),
            Self::InvalidKey(key) => write!(f, "`{key}` is not a valid secret key"),
            Self::InvalidEnvironment(environment) => {
                write!(f, "`{environment}` is not a valid environment name")
            }
            Self::ValueTooLong { key, len } => write!(
                f,
                "value for `{key}` is {len} bytes, over the {MAX_VALUE_BYTES}-byte limit"
            ),
            Self::FutureVersion(version) => write!(
                f,
                "secret store is version {version}, newer than this build understands"
            ),
        }
    }
}

impl core::error::Error for SecretError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Decode(err) => Some(err),
            Self::InvalidKey(_)
            | Self::InvalidEnvironment(_)
            | Self::ValueTooLong { .. }
            | Self::FutureVersion(_) => None,
        }
    }
}

impl From<std::io::Error> for SecretError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<serde_json::Error> for SecretError {
    fn from(source: serde_json::Error) -> Self {
        Self::Decode(source)
    }
}

#[cfg(test)]
mod tests {
    use super::super::format::{MAX_VALUE_BYTES, SECRETS_VERSION, SecretFile};
    use super::super::provider::{PROVIDER_CACHE_VERSION, ProviderCacheFile};
    use super::super::refs::sealed_keys;

    use std::collections::{BTreeMap, BTreeSet};

    use crate::config::AppConfig;

    use super::*;

    /// A value that only embeds a reference still comes from the store.
    #[test]
    fn an_embedded_reference_seals_its_key() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert(
            "DB_URL".into(),
            "postgres://u:{{secret:pg/PASSWORD}}@h".into(),
        );
        assert_eq!(sealed_keys(&config), vec!["DB_URL".to_string()]);
    }

    /// A positional token is not a secret: `{{name}}` and `{{instance}}` are
    /// filled from the sheep, not from the store.
    #[test]
    fn a_positional_token_does_not_seal_a_key() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("LOG".into(), "{{name}}.log".into());
        assert!(sealed_keys(&config).is_empty());
    }

    /// IR-41, the same guard for the on-disk shape everything else is built
    /// from. `SecretFile` is private, so `missing_debug_implementations`
    /// never forces it to keep a `Debug` impl at all; this is what stops a
    /// later edit from deriving one over the hand-written redaction.
    #[test]
    fn a_secret_file_debug_never_prints_a_value() {
        let file = SecretFile {
            version: SECRETS_VERSION,
            entries: BTreeMap::from([(
                "K".to_string(),
                BTreeMap::from([("production".to_string(), "hunter2".to_string())]),
            )]),
        };
        let rendered = format!("{file:?}");
        assert_eq!(rendered, "SecretFile { version: 1, keys: 1 }");
        assert!(!rendered.contains("hunter2"));
    }

    /// IR-41, the same guard for the on-disk shape [`provider_cache_on_disk`]
    /// reads: it mirrors `shep-daemon`'s `CacheFile`, values in the clear
    /// included.
    #[test]
    fn a_provider_cache_file_debug_never_prints_a_value() {
        let file = ProviderCacheFile {
            version: PROVIDER_CACHE_VERSION,
            namespaces: BTreeMap::from([(
                "vercel".to_string(),
                BTreeMap::from([(
                    "API_KEY".to_string(),
                    BTreeMap::from([("production".to_string(), "sk_live".to_string())]),
                )]),
            )]),
            pushed: BTreeMap::from([(
                "vercel".to_string(),
                BTreeSet::from(["production".to_string()]),
            )]),
        };
        let rendered = format!("{file:?}");
        assert_eq!(
            rendered,
            "ProviderCacheFile { version: 2, namespaces: 1, pushed: 1 }"
        );
        assert!(!rendered.contains("sk_live"));
    }

    /// Exact strings for both renderings (IR-41): every variant is meant to
    /// carry a name and never a value, and a substring check cannot see a
    /// field it was never told to look for.
    #[test]
    fn error_messages_name_the_key_and_never_a_value() {
        let too_long = SecretError::ValueTooLong {
            key: "K".to_string(),
            len: 9999,
        };
        assert_eq!(
            too_long.to_string(),
            format!("value for `K` is 9999 bytes, over the {MAX_VALUE_BYTES}-byte limit")
        );
        assert_eq!(
            format!("{too_long:?}"),
            "ValueTooLong { key: \"K\", len: 9999 }"
        );

        let bad_key = SecretError::InvalidKey("has space".to_string());
        assert_eq!(bad_key.to_string(), "`has space` is not a valid secret key");
        assert_eq!(format!("{bad_key:?}"), "InvalidKey(\"has space\")");

        for rendered in [too_long.to_string(), bad_key.to_string()] {
            assert!(
                !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
                "no em or en dash in copy a user reads: {rendered}"
            );
        }
    }
}
