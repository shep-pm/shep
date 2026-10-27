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
//! `list` hold what it does when a key lands, and `choices` the checklist a
//! list of choices opens to; `dogs` holds a sheep's dogs tab
//! and `sheep_dog` the pane over one dog's table on it; `types`
//! and `fields` hold what the rest read and write; `section_write` turns a
//! filed set back into a dog's TOML text; `walk` holds the rows and the
//! cursor's walk through them.

mod choices;
mod config;
#[cfg(test)]
mod config_tests;
mod dogs;
mod edit;
#[cfg(test)]
mod edit_tests;
mod entries;
mod entries_write;
mod entry_add;
#[cfg(test)]
mod entry_edit;
mod entry_remove;
mod env;
mod fields;
#[cfg(test)]
mod fixtures;
mod list;
mod section_write;
#[cfg(test)]
mod section_write_entries;
mod sheep_dog;
mod tree;
mod types;
mod union;
#[cfg(test)]
mod union_tests;
mod walk;

pub(in crate::lookout) use config::toml_table;
pub use config::{ConfigPane, ReloadKind};
pub(in crate::lookout) use dogs::DogToggle;
pub(crate) use dogs::dog_names;
pub use dogs::{DogRow, DogTableState, ListedDog, SheepDogEntry};
pub use edit::PaneTyping;
pub(in crate::lookout) use entries::EntryBody;
pub use env::EnvTyping;
pub(in crate::lookout) use fields::{DEFAULT, UNSET};
pub(crate) use fields::{resolved_display, sheep_fields};
pub use list::{ListPane, ListRow};
pub(in crate::lookout) use tree::TreeRow;
pub(in crate::lookout) use types::PathStep;
pub use types::{FieldValue, Lock, PaneEdit, PaneRow, PaneTarget, SectionWrite};
