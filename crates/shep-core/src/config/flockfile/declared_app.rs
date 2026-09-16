//! [`DeclaredApp`]: one app as the document wrote it, keys and all.
//!
//! `#[serde(default)]` erases which keys a document named, so the key sets
//! here are the only record of what it actually declared.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::AppConfig;

/// One app as the document declared it: the validated config, plus the
/// keys the document literally wrote.
///
/// The key set cannot be recovered from [`AppConfig`] afterwards:
/// `#[serde(default)]` gives every field a value, so a document naming
/// four keys deserializes identically to one naming forty.
/// [`Flockfile::parse_declared`](crate::config::flockfile::Flockfile::parse_declared) carries the claim out for a later merge
/// that keys on what a template declares, not on its values.
///
/// `Serialize`/`Deserialize` since this type travels inside a wire
/// request, keyed on the same claim. Derived `Debug` is safe: `config`
/// redacts its own `env`, and `declared_env` holds only key names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredApp {
    /// The app, validated the same way [`Flockfile::parse`](crate::config::flockfile::Flockfile::parse) validates one
    pub config: AppConfig,
    /// Top-level keys this app's table wrote, whatever their values
    pub declared: BTreeSet<String>,
    /// Keys inside this app's `env` table. Empty when `env` was not declared.
    pub declared_env: BTreeSet<String>,
}
