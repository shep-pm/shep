//! `secrets.json`: the values a config refers to and never carries.
//!
//! A key holds one value per environment, so `production` and `staging`
//! differ without two config files. A `{{secret:NAME}}` reference resolves
//! through [`SecretView`], which reads the sheep's own environment and then
//! [`ALL_ENVIRONMENTS`], never another named environment.
//!
//! Same on-disk shape as [`crate::kv`]: a read-modify-rename under a
//! [`crate::file_lock`] on a sibling `secrets.json.lock`.

mod error;
mod format;
mod provider;
mod refs;
mod resolve_mod;
mod store;
#[cfg(test)]
mod testing;
pub use error::SecretError;
pub use format::{ALL_ENVIRONMENTS, MAX_KEY_BYTES, MAX_VALUE_BYTES, SECRETS_VERSION};
pub use provider::{
    NamespaceValues, PROVIDER_CACHE_VERSION, ProviderCache, PushedPairs, provider_cache_on_disk,
};
pub use refs::{SecretRef, namespaces_of, references, sealed_keys};
pub use resolve_mod::{Resolution, SecretView};
pub use store::{all, get, is_name, set, unset};
