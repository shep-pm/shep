//! An open config pane: what it is editing, its fields, and its cursor.
//!
//! The pane is a [`FieldSet`](super::field::FieldSet) over one target, plus
//! the values that target currently holds and a
//! [`Viewport`](super::viewport::Viewport) over the rows. It writes once:
//! every keystroke files a [`PaneEdit`] into [`Edits`](super::edits::Edits),
//! and the whole set leaves together when the pane closes. See [`PaneEdit`]
//! for why not `Request::ApplyConfig`.

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

pub(in crate::lookout) use choices::ChoiceRow;
pub(in crate::lookout) use config::toml_table;
pub use config::{ConfigPane, ReloadKind};
pub(in crate::lookout) use dogs::DogToggle;
pub(crate) use dogs::dog_names;
pub use dogs::{DogRow, DogTableState, ListedDog, SheepDogEntry};
pub use edit::PaneTyping;
pub(in crate::lookout) use entries::EntryBody;
pub(crate) use env::{ArmedEnv, EnvSubmit};
pub(in crate::lookout) use fields::{DEFAULT, UNSET};
pub(crate) use fields::{resolved_display, sheep_fields};
pub use list::{ListPane, ListRow};
pub(in crate::lookout) use tree::TreeRow;
pub(in crate::lookout) use types::PathStep;
pub use types::{FieldValue, Lock, PaneEdit, PaneRow, PaneTarget, SectionWrite};
