//! The catalogue screen's state: the dog index as rows, and a cursor over
//! them that opens one entry.
//!
//! Rows are built by [`build_rows`], a plain function over the index, the
//! receipts under `$SHEP_HOME` and `shep.toml`, so nothing here needs a
//! terminal. Reading the cache and fetching run on the effect's side, in
//! [`read_cache`] and [`fetch`].

use core::fmt;
use std::path::Path;

use shep_core::paths::ShepPaths;

use super::*;
use crate::commands::dogs::installed;
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::{
    self, AvailableDog, CATEGORIES, DogSourceKind, Index, MAX_AGE, cached_with_age,
};
use crate::dog_origin::{DogOrigin, Origins};

#[cfg(test)]
mod tests;

/// How far along a dog is on this machine.
///
/// `Debug` is derived: three fieldless variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DogState {
    /// Neither a receipt nor an adopted entry names it.
    NotInstalled,
    /// A receipt or an adopted entry names it, and `[daemon] enabled_dogs`
    /// does not.
    Installed,
    /// Installed, and `[daemon] enabled_dogs` lists it.
    Enabled,
}

impl DogState {
    /// The word the STATE column carries.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not installed",
            Self::Installed => "installed",
            Self::Enabled => "enabled",
        }
    }
}

impl fmt::Display for DogState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One index row with what this machine adds to it.
///
/// `Debug` is derived: the row is public index data and a state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogueRow {
    /// The index row, every string already sanitised by the parser.
    pub(crate) dog: AvailableDog,
    /// The VERSION cell, as [`DogOrigin::version_cell`] words it.
    pub(crate) version: String,
    /// Whether it is installed and whether it is enabled.
    pub(crate) state: DogState,
}

/// The rows for `index`, ordered by [`CATEGORIES`] and then by name.
///
/// A dog's state reads its receipt, `config`'s adopted dogs and its enabled
/// list under the name the dog is adopted as. An installed dog's VERSION is
/// its own, with `→ <new>` when `index` is ahead. A dog not installed shows
/// the version the index approves, or `-` for a row that approves none.
pub(crate) fn build_rows(index: &Index, paths: &ShepPaths, config: &ShepToml) -> Vec<CatalogueRow> {
    let origins = Origins::over(paths, Some(index.clone()));
    let adopted = config.adopted_dog_names();
    let enabled = config.enabled_dog_names();
    let mut rows: Vec<CatalogueRow> = index
        .dogs
        .iter()
        .map(|dog| {
            let name = dog.adopt_as.as_str();
            let is_installed =
                adopted.iter().any(|adopted| adopted == name) || installed(paths, name).is_some();
            let (state, version) = if is_installed {
                let path = config.adopted_dog_path(name).unwrap_or_default();
                let origin = origins.of(name, Some(Path::new(&path)));
                let state = if enabled.iter().any(|enabled| enabled == name) {
                    DogState::Enabled
                } else {
                    DogState::Installed
                };
                (state, origin.version_cell())
            } else {
                (DogState::NotInstalled, listed_version(dog))
            };
            CatalogueRow {
                dog: dog.clone(),
                version,
                state,
            }
        })
        .collect();
    rows.sort_by_key(|row| (category_rank(&row.dog.category), row.dog.name.clone()));
    rows
}

/// The VERSION cell of a dog nothing installed: what the index approves.
fn listed_version(dog: &AvailableDog) -> String {
    let version = match &dog.source {
        DogSourceKind::Cargo { version, .. } => version.clone(),
        DogSourceKind::CargoGit { .. }
        | DogSourceKind::GoInstall { .. }
        | DogSourceKind::Manual { .. } => None,
    };
    DogOrigin::adopted(version).version_cell()
}

/// A category's place in [`CATEGORIES`], so the list reads in the docs
/// site's order. One the build does not know sorts last.
fn category_rank(category: &str) -> usize {
    CATEGORIES
        .iter()
        .position(|known| *known == category)
        .unwrap_or(CATEGORIES.len())
}

/// What one load of the catalogue found.
///
/// `Debug` is derived: the rows are public index data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogueRead {
    /// The rows, empty when there is no cache to read.
    pub(crate) rows: Vec<CatalogueRow>,
    /// Whether a fetch is due and has not landed yet.
    pub(crate) refreshing: bool,
}

/// The rows of `index` for the machine under `paths`.
///
/// # Errors
/// `shep.toml` exists and could not be read or parsed.
fn rows_for(paths: &ShepPaths, index: &Index) -> Result<Vec<CatalogueRow>, String> {
    let config = ShepToml::read_only(&paths.daemon_config).map_err(|err| err.to_string())?;
    Ok(build_rows(index, paths, &config))
}

/// Reads the cached index at `url` and no more: what [`Effect::LoadCatalogue`]
/// draws from at once.
///
/// With `fetch` set the answer says a fetch is due when no copy exists or the
/// copy is older than [`MAX_AGE`]. Never touches the network.
///
/// # Errors
/// `shep.toml` could not be read, for [`rows_for`].
pub(crate) fn read_cache(
    paths: &ShepPaths,
    url: &str,
    fetch: bool,
) -> Result<Box<CatalogueRead>, String> {
    let Some((index, age)) = cached_with_age(paths, url) else {
        return Ok(Box::new(CatalogueRead {
            rows: Vec::new(),
            refreshing: fetch,
        }));
    };
    Ok(Box::new(CatalogueRead {
        rows: rows_for(paths, &index)?,
        refreshing: fetch && age >= MAX_AGE,
    }))
}

/// Fetches the index through the cache, for [`Effect::FetchCatalogue`].
///
/// A failed fetch with a copy on disk answers with that copy.
///
/// # Errors
/// The fetch failed and no copy exists, or `shep.toml` could not be read.
pub(crate) async fn fetch(paths: ShepPaths, url: String) -> Result<Box<CatalogueRead>, String> {
    let loaded = dog_index::cached_index(&paths, &url, false)
        .await
        .map_err(|err| err.to_string())?;
    let rows = tokio::task::spawn_blocking(move || rows_for(&paths, &loaded.index))
        .await
        .map_err(|err| err.to_string())??;
    Ok(Box::new(CatalogueRead {
        rows,
        refreshing: false,
    }))
}

/// The catalogue screen's cursor and level.
///
/// `Debug` is derived: a position and a dog's name.
#[derive(Debug)]
pub(crate) struct CataloguePane {
    /// The row under the cursor, an index into [`App::catalogue_rows`].
    pub(crate) selected: usize,
    /// The dog whose entry is open, by the name it is adopted as, or `None`
    /// on the list. A name, so a refresh that reorders the rows keeps the
    /// entry on the same dog.
    pub(crate) entry: Option<String>,
    /// Whether a fetch is out and has not answered.
    pub(crate) refreshing: bool,
}

impl App {
    /// Switches the body to the catalogue and asks for its rows.
    ///
    /// Every way in calls this. Refused over `--host`, since the receipts
    /// and `shep.toml` it reads are this machine's.
    pub(crate) fn open_catalogue(&mut self) -> Effect {
        if self.refuse_over_host() {
            return Effect::None;
        }
        self.body = Body::Catalogue(CataloguePane {
            selected: 0,
            entry: None,
            refreshing: false,
        });
        Effect::LoadCatalogue { fetch: true }
    }

    /// The rows of the latest load, whether or not the screen is open, for
    /// a screen that lists the uninstalled dogs too.
    pub(crate) fn catalogue_rows(&self) -> &[CatalogueRow] {
        &self.catalogue
    }

    /// `Msg::Catalogue`'s arm.
    ///
    /// The rows are kept even with the screen closed. An `Err` keeps the
    /// rows it has and ends the wait, and says why only when there are none.
    /// A load that says a fetch is due raises it.
    pub(super) fn on_catalogue(&mut self, result: Result<Box<CatalogueRead>, String>) -> Effect {
        let read = match result {
            Ok(read) => read,
            Err(message) => {
                if let Body::Catalogue(pane) = &mut self.body {
                    pane.refreshing = false;
                }
                // With rows on screen the old ones say enough. With none, the
                // pane's one line cannot say why.
                if self.catalogue.is_empty() {
                    self.notice = Some(Notice::grave(message));
                }
                return Effect::None;
            }
        };
        let CatalogueRead { rows, refreshing } = *read;
        if let Body::Catalogue(pane) = &mut self.body {
            let under_cursor = self
                .catalogue
                .get(pane.selected)
                .map(|row| &row.dog.adopt_as);
            pane.selected = under_cursor
                .and_then(|name| rows.iter().position(|row| &row.dog.adopt_as == name))
                .unwrap_or(pane.selected)
                .min(rows.len().saturating_sub(1));
            if let Some(open) = &pane.entry
                && !rows.iter().any(|row| &row.dog.adopt_as == open)
            {
                pane.entry = None;
            }
            pane.refreshing = refreshing;
        }
        self.catalogue = rows;
        if refreshing {
            Effect::FetchCatalogue
        } else {
            Effect::None
        }
    }

    /// The line under the band: how many dogs, and whether a fetch is out.
    ///
    /// With no rows and no fetch out it is the one line that says there is
    /// no cache and no network.
    pub(crate) fn catalogue_caption(&self) -> String {
        let refreshing = matches!(&self.body, Body::Catalogue(pane) if pane.refreshing);
        match (self.catalogue.len(), refreshing) {
            (0, true) => "refreshing".to_owned(),
            (0, false) => "no index cached, no network".to_owned(),
            (count, true) => format!("{count} dogs in the index \u{b7} refreshing"),
            (count, false) => format!("{count} dogs in the index"),
        }
    }

    /// The catalogue's own key box, in force while `self.body` is
    /// `Body::Catalogue`.
    pub(super) fn on_catalogue_key(&mut self, key: KeyPress) -> Effect {
        let rows = self.catalogue.len();
        let Body::Catalogue(pane) = &mut self.body else {
            return Effect::None;
        };
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::Escape => {
                if pane.entry.take().is_none() {
                    self.body = Body::FlockTable;
                }
            }
            KeyPress::Confirm => {
                // On the list, `Enter` opens the entry. On an entry shep can
                // install and has not, it installs.
                let open = pane.entry.clone();
                match open {
                    None => {
                        pane.entry = self
                            .catalogue
                            .get(pane.selected)
                            .map(|row| row.dog.adopt_as.clone());
                    }
                    Some(name) => {
                        let installable = self
                            .catalogue
                            .iter()
                            .find(|row| row.dog.adopt_as == name)
                            .is_some_and(|row| self.can_install(row));
                        if installable {
                            return self.start_install(&name);
                        }
                    }
                }
            }
            KeyPress::SelectUp if pane.entry.is_none() => {
                pane.selected = pane.selected.saturating_sub(1);
            }
            KeyPress::SelectDown if pane.entry.is_none() => {
                pane.selected = (pane.selected + 1).min(rows.saturating_sub(1));
            }
            KeyPress::SelectFirst if pane.entry.is_none() => pane.selected = 0,
            KeyPress::SelectLast if pane.entry.is_none() => {
                pane.selected = rows.saturating_sub(1);
            }
            KeyPress::Help => return self.open_keymap(),
            // Nothing else means anything here. Listed rather than a
            // wildcard, so a new `KeyPress` variant cannot fall silently
            // into an arm that ignores it.
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast
            | KeyPress::Refresh
            | KeyPress::Action(_)
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextEdit(_)
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Settings
            | KeyPress::Catalogue
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::Cycle
            | KeyPress::Edit
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Collapse
            | KeyPress::Bleats
            | KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::ArmDelete
            | KeyPress::NextGroup
            | KeyPress::PrevGroup
            | KeyPress::NextField
            | KeyPress::PrevField
            | KeyPress::Group(_)
            | KeyPress::Undo
            | KeyPress::Continue => {}
        }
        Effect::None
    }
}
