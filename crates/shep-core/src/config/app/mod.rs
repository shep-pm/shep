//! Per-app configuration schema: one sheep's Flockfile entry.

mod behavior;
mod env_value;
mod probe;
mod schema;
#[cfg(test)]
mod testing;
pub use probe::{ProbeConfig, ProbeKind};
pub use schema::AppConfig;
