//! Flockfile: discovery and multi-format parsing
//!
//! One document shape across formats: a list of app tables under the `app`
//! key (`[[app]]` in TOML). Parsing is strict serde, no code execution;
//! `.js` configs are the CLI's job (it shells out to node and feeds the
//! resulting JSON through [`FlockFormat::Json`]).
//!
//! The modules follow the path a document takes. `discovery` finds the
//! file and `format` says what it is; `parse` runs the one per-format
//! deserializer dispatch, with `json5` guarding the depth that backend
//! cannot survive; `raw` is the shape serde accepts and `file` the one
//! callers get. `error` is what any of it refuses with.

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
