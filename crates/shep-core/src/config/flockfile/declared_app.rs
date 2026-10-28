//! [`DeclaredApp`]: one app as the document wrote it, keys and all.
//!
//! `#[serde(default)]` erases which keys a document named, so the key sets
//! here are the only record of what it actually declared.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::AppConfig;

/// One app as the document declared it: the config plus the keys written.
///
/// The key set cannot be recovered from [`AppConfig`] afterwards:
/// `#[serde(default)]` gives every field a value.
/// [`parse_declared`](crate::config::flockfile::Flockfile::parse_declared)
/// carries the claim out. `Debug` is derived: `config` redacts its own
/// `env`, and `declared_env` holds key names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredApp {
    /// The app, validated the same way [`Flockfile::parse`](crate::config::flockfile::Flockfile::parse) validates one
    pub config: AppConfig,
    /// Top-level keys this app's table wrote, whatever their values
    pub declared: BTreeSet<String>,
    /// Keys inside this app's `env` table. Empty when `env` was not declared.
    pub declared_env: BTreeSet<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact string pinned so a lazy `derive(Debug)` refactor fails here:
    /// the safety of the derive rests entirely on `AppConfig`'s own manual
    /// `Debug` redacting `env`, and `declared_env` holding only key names.
    #[test]
    fn debug_does_not_leak_an_env_value() {
        let mut config = AppConfig::minimal("web", "./srv");
        config
            .env
            .insert("DATABASE_URL".to_string(), "postgres://secret".to_string());
        let app = DeclaredApp {
            config,
            declared: BTreeSet::from(["name".to_string(), "script".to_string()]),
            declared_env: BTreeSet::from(["DATABASE_URL".to_string()]),
        };
        assert_eq!(
            format!("{app:?}"),
            "DeclaredApp { config: AppConfig { name: \"web\", script: \"./srv\", \
             env: <1 vars>, .. }, declared: {\"name\", \"script\"}, \
             declared_env: {\"DATABASE_URL\"} }"
        );
    }
}
