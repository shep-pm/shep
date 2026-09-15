//! Fixtures and helpers shared by this module's tests.

use shep_core::paths::ShepPaths;
use shep_core::secrets::SecretView;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) fn test_paths() -> ShepPaths {
    ShepPaths {
        home: PathBuf::from("/home/ada/.shep"),
        daemon_config: PathBuf::from("/home/ada/.shep/shep.toml"),
        dogs_config: PathBuf::from("/home/ada/.shep/dogs.toml"),
        snapshot: PathBuf::from("/home/ada/.shep/flock.json"),
        logs: PathBuf::from("/home/ada/.shep/logs"),
        pids: PathBuf::from("/home/ada/.shep/pids"),
        run: PathBuf::from("/home/ada/.shep/run"),
        socket: PathBuf::from("/home/ada/.shep/run/shep.sock"),
        barks: PathBuf::from("/home/ada/.shep/barks.jsonl"),
        kv: PathBuf::from("/home/ada/.shep/kv.json"),
        overrides: PathBuf::from("/home/ada/.shep/overrides.json"),
        secrets: PathBuf::from("/home/ada/.shep/secrets.json"),
        secrets_cache: PathBuf::from("/home/ada/.shep/secrets-cache.json"),
    }
}

/// A view holding nothing, in the environment a host defaults to.
pub(super) fn no_secrets() -> SecretView {
    SecretView::empty("production".to_string())
}

/// A view holding exactly `key` in `environment`, and nothing else.
pub(super) fn view_with(environment: &str, key: &str, value: &str) -> SecretView {
    SecretView::new(
        environment.to_string(),
        BTreeMap::from([(
            key.to_string(),
            BTreeMap::from([(environment.to_string(), value.to_string())]),
        )]),
        shep_core::secrets::ProviderCache::default(),
    )
}
