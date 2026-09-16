//! Turning a declared `AppConfig` into a [`ResolvedApp`] the daemon will run.
//!
//! Every rule that can refuse a config lives under here, and the proof that
//! they all ran is the [`ResolvedApp`] token: it has no public constructor,
//! so a caller holding one knows normalization happened.
//!
//! `resolved_app` is the pipeline and owns the bounds it enforces, `validate`
//! holds the checks reused across fields, and `tilde` expands paths and
//! renders the templates a log path may carry. `error` is what any of them
//! refuses with.

//! Validation and normalization: `AppConfig` -> `ResolvedApp`
//!
//! `ResolvedApp` is a proof token: constructing one is only possible through
//! [`normalize`], so daemon code can require it and skip re-validation.

#[cfg(test)]
use std::path::Path;

mod error;
mod resolved_app;
mod tilde;
mod validate;

pub use error::NormalizeError;
pub use resolved_app::{ResolvedApp, normalize, normalize_all, normalize_with_home};
pub use tilde::{TildeError, expand_home_tilde};

/// A shep home for the cases that never mention one, so a fixture
/// growing a `{{SHEP_HOME}}` later reads as the token rather than as a
/// refusal.
#[cfg(test)]
fn shep_home_fixture() -> Option<&'static Path> {
    Some(Path::new("/home/ada/.shep"))
}
