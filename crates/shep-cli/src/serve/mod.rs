//! `shep serve`'s static-file surface: request-target resolution and the
//! filesystem containment it feeds.
//!
//! [`path`], `mime`, `conditional` and `listing` are pure: no `cfg`, no I/O,
//! and they compile on every target. `fs` and `worker` compile on every target
//! too, and are `async` over `tokio::fs`/`tokio::net`, since the
//! containment walk, the file open and the listener all cross a syscall.
//! `fs` branches by OS only for [`fs::open_regular`]'s open flags. `auth`
//! is pure except for its permission-mode check, isolated to one internal
//! function.

mod path;

// `conditional` is pure like `path`, `mime` and `listing`: the range
// grammar, the validators, and the status each combination answers with.
mod conditional;

pub(crate) mod fs;

mod mime;

mod listing;

// `pub(crate)`: `commands::serve`, a sibling module tree, builds
// `auth::load`'s `Credentials` and `worker::ServeConfig` and calls
// `worker::run`.
pub(crate) mod auth;

pub(crate) mod worker;
