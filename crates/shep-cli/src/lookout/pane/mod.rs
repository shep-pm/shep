//! An open config pane: what it is editing, its fields, and its cursor.
//!
//! The pane is a [`FieldSet`](super::field::FieldSet) over one target, plus
//! the values that target currently holds and a
//! [`Viewport`](super::viewport::Viewport) over the rows. It writes too, and
//! it writes once: every keystroke files a [`PaneEdit`] into
//! [`Edits`](super::edits::Edits), and the whole set leaves together when
//! the pane closes, each entry as a `Request::SetSheepField` or, for `env`,
//! a `Request::SetSheepEnv`. Both write an operator override for one key;
//! neither pretends to be a template. See [`PaneEdit`] for why not
//! `Request::ApplyConfig`.
//!
//! `config` holds the pane and everything it answers; `edit`, `env` and
//! `list` hold what it does when a key lands; `types` and `fields` hold what
//! all four read and write.

mod config;
mod edit;
mod env;
mod fields;
#[cfg(test)]
mod fixtures;
mod list;
mod types;

pub use config::{ConfigPane, ReloadKind};
pub use edit::PaneTyping;
pub use env::EnvTyping;
pub(crate) use fields::{resolved_display, sheep_fields};
pub use list::{ListPane, ListRow};
pub use types::{FieldValue, Lock, PaneEdit, PaneRow, PaneTarget};
