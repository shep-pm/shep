//! Flockfile: discovery and multi-format parsing
//!
//! One document shape across formats: a list of app tables. The key is
//! `app`, `[[app]]` in TOML. Parsing is strict serde, no code execution.
//! `.js` configs are the CLI's job, which feeds node's JSON through
//! [`FlockFormat::Json`].
//!
//! `discovery` finds the file and `format` says what it is. `parse` runs
//! the per-format dispatch, with `json5` guarding the depth that backend
//! cannot survive. `raw` is the shape serde accepts, `file` the result.

mod declared_app;
mod discovery;
mod error;
mod file;
mod format;
mod json5;
mod parse;
mod raw;
#[cfg(feature = "schema")]
mod schema;

pub use declared_app::DeclaredApp;
pub use discovery::{DISCOVERY_ORDER, discover};
pub use error::FlockfileError;
pub use file::Flockfile;
pub use format::FlockFormat;
#[cfg(feature = "schema")]
// `COMMITTED` with them: it was reachable as `config::flockfile::COMMITTED`
// before the split, since this was one `pub mod` file, and shep-core is
// published. A split must not quietly shrink the public surface.
pub use schema::{COMMITTED, flockfile_schema_json, flockfile_schema_string};
