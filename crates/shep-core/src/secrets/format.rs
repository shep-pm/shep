use core::fmt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The on-disk format's version.
///
/// A store carrying a higher version is refused rather than read or
/// replaced ([`SecretError::FutureVersion`](crate::secrets::SecretError::FutureVersion)): there is no undo for a
/// downgrade that overwrites an operator's credentials.
pub const SECRETS_VERSION: u32 = 1;

/// Longest key, namespace or environment name this store accepts, in bytes.
pub const MAX_KEY_BYTES: usize = 128;

/// Longest value this store accepts, in bytes.
///
/// The store is read whole on every access; a cap keeps it from becoming an
/// unbounded blob store. A 4096-bit RSA private key in PEM is 3272 bytes.
pub const MAX_VALUE_BYTES: usize = 4096;

/// The environment name that covers every environment.
///
/// A value here is used when the sheep's own environment has no slot of its
/// own. Cannot be a sheep's `environment`, which `AppConfig` refuses.
pub const ALL_ENVIRONMENTS: &str = "all";

/// The file's shape: a version and a key to environment to value map.
///
/// `BTreeMap` throughout so two writes of the same content produce
/// byte-identical files.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct SecretFile {
    pub(super) version: u32,
    pub(super) entries: BTreeMap<String, BTreeMap<String, String>>,
}

/// Redacted (IR-41): `entries` is the whole point of this type.
impl fmt::Debug for SecretFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretFile")
            .field("version", &self.version)
            .field("keys", &self.entries.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::super::error::SecretError;
    use super::super::store::set;

    use super::*;

    #[test]
    fn an_oversized_value_is_refused_by_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let big = "x".repeat(MAX_VALUE_BYTES + 1);
        let err = set(&path, "K", "production", &big).unwrap_err();
        assert!(matches!(err, SecretError::ValueTooLong { len, .. } if len == big.len()));
    }
}
