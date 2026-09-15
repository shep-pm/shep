//! Daemon-level configuration: `$SHEP_HOME/shep.toml`
//!
//! Layering (spec §5): file < `SHEP_*` env < CLI flags. [`DaemonConfig::load_layered`]
//! applies all three and validates the result; [`DaemonConfig::load`] applies
//! only the file and environment layers.

mod config;
mod error;
mod overrides;
mod sections;
#[cfg(test)]
mod testing;
pub use config::DaemonConfig;
pub use error::DaemonConfigError;
pub use overrides::{DaemonOverrides, parse_daemon_bool};
pub use sections::{DaemonSection, LogLevel, SecretsSection, StyleSection, WhistleSection};
