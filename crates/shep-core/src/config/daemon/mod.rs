//! Daemon-level configuration: `$SHEP_HOME/shep.toml`
//!
//! Layering (spec §5): file < `SHEP_*` env < CLI flags. [`DaemonConfig::load_layered`]
//! applies all three and validates the result; [`DaemonConfig::load`] applies
//! only the file and environment layers.

mod config;
mod dog_grants;
#[cfg(test)]
mod dog_grants_state_tests;
mod error;
mod overrides;
#[cfg(feature = "schema")]
mod schema;
mod sections;
#[cfg(test)]
mod testing;
pub use config::DaemonConfig;
pub use dog_grants::DogGrantsSection;
pub use error::DaemonConfigError;
pub use overrides::{DaemonOverrides, parse_daemon_bool};
#[cfg(feature = "schema")]
pub use schema::{
    APPLY_KEY, APPLY_NEXT_COMMAND, APPLY_RELOAD, APPLY_RESTART, APPLY_WHISTLE,
    daemon_config_schema_json,
};
pub use sections::{DaemonSection, LogLevel, SecretsSection, StyleSection, WhistleSection};
