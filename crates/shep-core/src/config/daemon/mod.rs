//! Daemon-level configuration: `$SHEP_HOME/shep.toml`
//!
//! Layering (spec §5): file < `SHEP_*` env < CLI flags. This module applies
//! the first two; the CLI applies its flags onto the returned struct.

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
