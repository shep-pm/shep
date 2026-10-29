//! Moving `[dog.<name>]` out of `shep.toml` and into `dogs.toml`, once.
//!
//! [`migrate_dog_sections`] runs at the top of every daemon boot, and
//! `shep adopt` runs it before seeding a manifest's `defaults`. It does
//! nothing on all but the first.
//!
//! `dogs.toml`'s other writers are [`seed_dog_defaults`] and
//! `shep_daemon::dogs::set_dog_section`. All rewrite the whole file, so all
//! hold `dogs.toml`'s [`ConfigLock`] across the read-modify-write, with
//! `shep.toml`'s lock as the outer one.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;

use shep_core::config::{DogsConfig, DogsConfigError};
use shep_core::paths::ShepPaths;
use toml_edit::{DocumentMut, Item, TableLike};

use super::shep_toml::{ConfigLock, ShepToml, ShepTomlError, create_config_file};

/// Moves every `[dog.<name>]` section into `dogs.toml`, naming those moved
///
/// # Errors
/// - [`DogMigrationError::WouldOverwrite`] when a name holds values in both
///   files; an empty section is not a value on either side.
/// - [`DogMigrationError::SectionsUnreadable`] when a name declared under
///   `[dog]` did not come back as a section to move.
/// - [`DogMigrationError::Parse`] when `dogs.toml` is not valid TOML.
/// - [`DogMigrationError::Read`], [`DogMigrationError::ReadDogs`],
///   [`DogMigrationError::Lock`], [`DogMigrationError::Write`] and
///   [`DogMigrationError::Toml`] for the underlying I/O.
pub(crate) fn migrate_dog_sections(paths: &ShepPaths) -> Result<Vec<String>, DogMigrationError> {
    let existing_source = match std::fs::read_to_string(&paths.daemon_config) {
        Ok(source) => source,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(DogMigrationError::Read(err)),
    };
    // Not a substring test: `[ dog.metrics ]` is the same section spelled
    // differently. `None` means this parser could not read the source, which
    // `ShepToml` below reports with file and line.
    let table = existing_source.parse::<toml::Table>().ok();
    let declared = table.as_ref().map(declared_dog_names);
    // Skipping the open is load-bearing: `ShepToml::save` renames a staged
    // temp file over the original, so opening an untouched `shep.toml` would
    // give it a fresh inode and mode on every boot after the first. A `[dog]`
    // holding only empty headers is not untouched: it is struck.
    let has_dog_table = table
        .as_ref()
        .is_some_and(|table| matches!(table.get("dog"), Some(toml::Value::Table(_))));
    if declared.as_ref().is_some_and(BTreeSet::is_empty) && !has_dog_table {
        return Ok(Vec::new());
    }
    move_sections(paths, declared.as_ref())
}

/// The locked half of [`migrate_dog_sections`], given the names its unlocked
/// read of `shep.toml` declared.
///
/// A separate function so a test can hand it a `declared` another writer has
/// since made stale.
fn move_sections(
    paths: &ShepPaths,
    declared: Option<&BTreeSet<String>>,
) -> Result<Vec<String>, DogMigrationError> {
    // `try_edit`, never `edit`: `edit` calls `save` unconditionally, so
    // refusing from inside it would strike the sections from `shep.toml` and
    // only then fail, leaving them in neither file.
    ShepToml::try_edit(&paths.daemon_config, |doc| {
        // Empty sections dropped here, matching what `declared_dog_names`
        // left out, so the two agree on what there was to move. They still
        // leave `shep.toml`: `take_dog_sections` removes the whole `[dog]`,
        // even when every section is empty.
        let incoming: BTreeMap<String, Item> = doc
            .take_dog_sections()
            .into_iter()
            .filter(|(_, section)| item_has_content(section))
            .collect();
        // `take_dog_sections` removes the whole `[dog]` table before deciding
        // what to hand back, so a count cannot see what it dropped: compare
        // names. With no readable source, fall back to whether any came back.
        let missing: Vec<String> = match declared {
            Some(names) => names
                .iter()
                .filter(|name| !incoming.contains_key(*name))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        // Nothing declared and nothing taken: only empty headers were there,
        // and they are struck. Judged on `incoming` as well, since `declared`
        // was read before the lock and another writer may have added a
        // section since.
        if incoming.is_empty() && declared.is_some_and(BTreeSet::is_empty) {
            return Ok(Vec::new());
        }
        if !missing.is_empty() || incoming.is_empty() {
            return Err(DogMigrationError::SectionsUnreadable { names: missing });
        }
        // Take `dogs.toml`'s lock, so the read and merge below are one
        // transaction against the other writers: a second boot here, and
        // `dogs::set_dog_section` behind the config pane. It nests inside
        // `shep.toml`'s, which `try_edit` holds.
        let _dogs_lock =
            ConfigLock::acquire(&paths.dogs_config).map_err(DogMigrationError::Lock)?;
        // A live document, not a `toml::Table`: a second migration writes into
        // a `dogs.toml` an operator has been hand-editing, and a
        // `toml::to_string` of a parsed map drops every comment in it.
        let mut merged = read_dogs_document(&paths.dogs_config)?;
        // Over values on both sides: a bare `[metrics]` in `dogs.toml` is not
        // a second value, so the section carrying values wins. A destination
        // `[[metrics]]` is not table-like, so it refuses.
        let collides = |name: &String| merged.get(name).is_some_and(item_has_content);
        if let Some(name) = incoming.keys().find(|name| collides(name)) {
            return Err(DogMigrationError::WouldOverwrite {
                name: name.clone(),
                holds_env: section_holds_env(&incoming[name]),
            });
        }
        // `incoming` is a `BTreeMap`, so its keys are already in name order.
        let moved: Vec<String> = incoming.keys().cloned().collect();

        // Appended: a table taken out of `shep.toml` still carries the
        // position it held there. The scan covers every positioned table, not
        // top-level `Item::Table`s alone, since `[bark.sinks]` is neither.
        let mut next = merged
            .iter()
            .filter_map(|(_, item)| last_position(item))
            .max()
            .map_or(0, |last| last + 1);
        for (name, mut section) in incoming {
            renumber_tables(&mut section, &mut next);
            merged.insert(&name, section);
        }
        // Written before this closure returns, so `save` strikes the old
        // sections only once the new file holds them, durable on unix but not
        // against a Windows power cut (`sync_dir` no-ops there).
        write_dogs_config(&paths.dogs_config, &merged.to_string())
            .map_err(DogMigrationError::Write)?;
        Ok(moved)
    })
}

/// Whether `dogs.toml` holds a section for `name`. A missing file is
/// `Ok(false)`.
///
/// `shep delete <dog>` and the hidden `shep rehome`'s only interest in this
/// file: they forget the adoption and leave the settings, so the answer
/// decides whether there is anything to tell the operator it kept. Takes no
/// lock and writes nothing, so a section appearing or vanishing under the
/// read changes a notice, never a file.
///
/// # Errors
/// [`DogMigrationError::ReadDogs`] if the file exists and could not be
/// read, and [`DogMigrationError::Parse`] if it is not valid TOML.
pub(crate) fn dog_section_exists(path: &Path, name: &str) -> Result<bool, DogMigrationError> {
    Ok(read_dogs_document(path)?.contains_key(name))
}

/// What `shep adopt` does with a manifest's `defaults`: writes them as
/// `name`'s section in `dogs.toml` when the file holds none.
///
/// `defaults` is TOML text for the dog's own section body, so it is parsed
/// as a document and inserted whole under `[name]`.
///
/// `dogs.toml` is read before the `defaults` are judged.
///
/// # Errors
/// - [`DogMigrationError::ReadDogs`] and [`DogMigrationError::Parse`] when
///   `dogs.toml` cannot be read or parsed.
/// - [`DogMigrationError::Lock`] and [`DogMigrationError::Write`] for the
///   write's I/O.
pub(crate) fn seed_dog_defaults(
    paths: &ShepPaths,
    name: &str,
    defaults: &str,
) -> Result<DefaultsOutcome, DogMigrationError> {
    // The gate read: its answer is discarded, its error is not. Judging
    // the operator's file first is what reports a broken `dogs.toml` even
    // when the dog's `defaults` are broken too.
    read_dogs_document(&paths.dogs_config)?;
    let Ok(parsed) = defaults.parse::<DocumentMut>() else {
        return Ok(DefaultsOutcome::Unreadable);
    };
    // Comments and whitespace are not a section: a dog that declared
    // nothing writes nothing.
    if parsed.is_empty() {
        return Ok(DefaultsOutcome::Empty);
    }
    // The lock is taken before the section is looked for, so `Kept` is
    // never decided from a read a concurrent writer has already replaced:
    // a section appearing is kept, and one vanishing mid-flight re-seeds
    // on the next adopt rather than being missed by a stale glance.
    let _lock = ConfigLock::acquire(&paths.dogs_config).map_err(DogMigrationError::Lock)?;
    let mut merged = read_dogs_document(&paths.dogs_config)?;
    if merged.contains_key(name) {
        return Ok(DefaultsOutcome::Kept);
    }
    let mut next = merged
        .iter()
        .filter_map(|(_, item)| last_position(item))
        .max()
        .map_or(0, |last| last + 1);
    // The parsed document's root table is implicit, holding the defaults'
    // keys and any sub-tables its own headers spelled. Made explicit and
    // named, it is the section. Cloned through `as_table` rather than
    // `DocumentMut::into_table`, which postdates the `toml_edit` floor.
    let mut section = parsed.as_table().clone();
    section.set_implicit(false);
    let mut item = Item::Table(section);
    renumber_tables(&mut item, &mut next);
    merged.insert(name, item);
    write_dogs_config(&paths.dogs_config, &merged.to_string()).map_err(DogMigrationError::Write)?;
    Ok(DefaultsOutcome::Written)
}

/// Which of the four things `shep adopt` did with a manifest's `defaults`
/// an operator should be told about.
#[derive(Debug)]
pub(crate) enum DefaultsOutcome {
    /// `dogs.toml` gained a `[<name>]` section holding the defaults.
    Written,
    /// `dogs.toml` already holds a `name` section, so it was not opened
    /// for writing at all.
    Kept,
    /// The `defaults` declared nothing, so nothing was written.
    Empty,
    /// The `defaults` are not valid TOML: the dog's bug, warned about, and
    /// nothing was written.
    Unreadable,
}

/// Reads `path` as an editable document, treating a missing file as an empty
/// one. A caller that goes on to write holds `path`'s [`ConfigLock`]: a read
/// outside it is the first half of a lost update.
///
/// A [`DocumentMut`] rather than a [`DogsConfig`], since the writer rewrites
/// the whole file and an operator hand-edits it: comments and inline tables
/// survive a `toml_edit` round trip and not a `toml::to_string` of a parsed
/// map. [`DogsConfig::load`] still gates it, being the stricter parse.
///
/// # Errors
/// - [`DogMigrationError::ReadDogs`] when the file exists and could not be
///   read, and [`DogMigrationError::Parse`] when it is not valid TOML.
fn read_dogs_document(path: &Path) -> Result<DocumentMut, DogMigrationError> {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DocumentMut::new());
        }
        Err(err) => return Err(DogMigrationError::ReadDogs(err)),
    };
    DogsConfig::load(Some(&source)).map_err(DogMigrationError::Parse)?;
    source.parse::<DocumentMut>().map_err(|_| {
        // Unreachable: `toml` is a thin layer over `toml_edit`, so a source
        // the gate accepted parses here too. Reported rather than panicked
        // on, since this runs at the top of every daemon boot.
        DogMigrationError::ReadDogs(std::io::Error::other(
            "dogs.toml parses as TOML values and not as an editable document",
        ))
    })
}

/// Every name `table` declares a value for under `[dog]`.
///
/// The only record of what was there before [`ShepToml::take_dog_sections`]
/// struck the table; empty also means do not open the file at all.
///
/// A name holding nothing is left out, so an empty `[dog.metrics]` cannot
/// refuse a boot.
fn declared_dog_names(table: &toml::Table) -> BTreeSet<String> {
    match table.get("dog") {
        Some(toml::Value::Table(dog)) => dog
            .iter()
            .filter(|(_, value)| !declares_nothing(value))
            .map(|(name, _)| name.clone())
            .collect(),
        // No `[dog]` at all, or a `dog` that is not a table: either way
        // nothing is declared and there is no reason to open the document.
        _ => BTreeSet::new(),
    }
}

/// Whether `item` holds something, rather than being a bare table header.
///
/// An `Item` that is not a table at all counts as content: `dog = 3` is a
/// section to carry over and refuse a collision on, not an empty one.
fn item_has_content(item: &Item) -> bool {
    !item.as_table_like().is_some_and(TableLike::is_empty)
}

/// Whether `item` names `env` among its keys.
///
/// The one thing that decides which reading of
/// [`DogMigrationError::WouldOverwrite`] an operator is told: `env` is the
/// only setting a dog never reads from its own section, so a section holding
/// one was written for the wrong file.
fn section_holds_env(item: &Item) -> bool {
    item.as_table_like()
        .is_some_and(|table| table.contains_key("env"))
}

/// Whether `value` holds nothing an operator could lose by not moving it.
///
/// An empty table, an empty array, and an array of tables that are all
/// empty. The last is why this recurses rather than testing
/// `Table::is_empty`. A `[[dog.metrics]]` that carries values still refuses:
/// there is no one section for it to become.
fn declares_nothing(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => table.is_empty(),
        toml::Value::Array(items) => items.iter().all(declares_nothing),
        _ => false,
    }
}

/// The highest render position any table inside `item` holds, or `None`
/// when it holds no positioned table at all.
///
/// [`renumber_tables`]'s read-only twin, walking the same three shapes:
/// `toml_edit` positions every table individually, and an implicit parent
/// has no position of its own, hence the descent into children.
fn last_position(item: &Item) -> Option<usize> {
    match item {
        Item::Table(table) => table
            .position()
            .into_iter()
            .chain(table.iter().filter_map(|(_, child)| last_position(child)))
            .max(),
        Item::ArrayOfTables(array) => array
            .iter()
            .flat_map(|table| {
                table
                    .position()
                    .into_iter()
                    .chain(table.iter().filter_map(|(_, child)| last_position(child)))
            })
            .max(),
        // Neither holds a positioned table: an inline `metrics = { .. }` is a
        // key/value pair and renders above the tables, with the other pairs.
        Item::Value(_) | Item::None => None,
    }
}

/// Walks `item` depth-first and gives every table it holds the next document
/// position, so a section moved out of `shep.toml` renders after everything
/// already in `dogs.toml`.
///
/// Over sub-tables too: `toml_edit` renders every table by its own position,
/// so renumbering only the top-level `bark` would leave `[bark.sinks]` behind.
fn renumber_tables(item: &mut Item, next: &mut usize) {
    match item {
        Item::Table(table) => {
            table.set_position(*next);
            *next += 1;
            for (_, child) in table.iter_mut() {
                renumber_tables(child, next);
            }
        }
        Item::ArrayOfTables(array) => {
            for table in array.iter_mut() {
                table.set_position(*next);
                *next += 1;
                for (_, child) in table.iter_mut() {
                    renumber_tables(child, next);
                }
            }
        }
        Item::Value(_) | Item::None => {}
    }
}

/// Writes `rendered` to `path`: staged in a sibling temp file at
/// `OWNER_ONLY_FILE_MODE`, `fsync`ed, then `rename`d over `path`.
///
/// The mode matters: this file holds dog webhook URLs, and a plain
/// `std::fs::write` would create it at the ambient umask. The rename avoids
/// the half-written file a plain write's `O_TRUNC` leaves on a crash.
fn write_dogs_config(path: &Path, rendered: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = create_config_file(parent)?;
    tmp.write_all(rendered.as_bytes())?;
    shep_core::atomic_file::publish(tmp, path)
}

/// Why `[dog.<name>]` could not be moved into `dogs.toml`
///
/// Derived `Debug`: every variant carries a dog name, an I/O error or a
/// serializer's complaint, never a section's contents. That holds only
/// because [`ShepTomlError`] and [`DogsConfigError`] redact their own
/// `Debug`, so a new variant wrapping a parse error needs the same check.
#[derive(Debug)]
#[non_exhaustive]
pub(crate) enum DogMigrationError {
    /// `shep.toml` itself could not be read.
    Read(std::io::Error),
    /// `dogs.toml` exists and could not be read.
    ///
    /// Its own variant rather than [`Self::Read`]: an operator's next move
    /// depends on which of the two files it was.
    ReadDogs(std::io::Error),
    /// `dogs.toml` already exists and is not valid TOML.
    Parse(DogsConfigError),
    /// `name` has a section carrying values in both files, so the move
    /// would silently pick one of the two.
    ///
    /// An empty section never raises this on either side: in the source it is
    /// skipped, in `dogs.toml` it is the header the moved section lands on.
    WouldOverwrite {
        /// The dog named in both `shep.toml` and `dogs.toml`.
        name: String,
        /// Whether the `shep.toml` section held `env`, which has no place in
        /// a dog's own settings and belongs on its `[daemon.adopted_dogs]`
        /// entry beside its path.
        holds_env: bool,
    },
    /// `shep.toml` declares names under `[dog]` that did not come back as
    /// sections to move.
    SectionsUnreadable {
        /// The names that were declared and did not come back, empty when
        /// the source could not be parsed closely enough to name them.
        names: Vec<String>,
    },
    /// `dogs.toml` could not be written.
    Write(std::io::Error),
    /// `dogs.toml`'s sibling lock file could not be created or locked.
    Lock(std::io::Error),
    /// Reading, locking or rewriting `shep.toml` failed.
    Toml(ShepTomlError),
}

/// The `[daemon.adopted_dogs]` line a dog's `env` belongs on, with only the
/// name filled in.
///
/// Placeholders rather than the operator's own values: the refusal runs
/// before anything is read closely enough to quote, and an `env` value is a
/// credential.
fn adopted_dogs_entry(name: &str) -> String {
    format!(r#"{name} = {{ path = "...", env = {{ KEY = "{{{{secret:name}}}}" }} }}"#)
}

impl fmt::Display for DogMigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(err) => write!(f, "shep.toml could not be read: {err}"),
            Self::ReadDogs(err) => write!(f, "dogs.toml could not be read: {err}"),
            Self::Parse(err) => write!(f, "dogs.toml could not be read: {err}"),
            Self::WouldOverwrite {
                name,
                holds_env: false,
            } => write!(
                f,
                "dog '{name}' is configured in both shep.toml and dogs.toml; \
                 delete one of the two sections and start the daemon again"
            ),
            Self::WouldOverwrite {
                name,
                holds_env: true,
            } => write!(
                f,
                "dog '{name}' is configured in both shep.toml and dogs.toml; a dog's env goes \
                 beside its path in [daemon.adopted_dogs], as {entry}; move env there, \
                 choose the remaining settings to keep in [{name}] in dogs.toml, then delete \
                 [dog.{name}] from shep.toml and start the daemon again",
                entry = adopted_dogs_entry(name),
            ),
            Self::SectionsUnreadable { names } if names.is_empty() => write!(
                f,
                "shep.toml has entries under [dog] that are not dog sections; \
                 give each dog a table of its own, or delete them"
            ),
            Self::SectionsUnreadable { names } => write!(
                f,
                "shep.toml has entries under [dog] that are not dog sections ({}); \
                 give each dog a table of its own, or delete them",
                names.join(", ")
            ),
            Self::Write(err) => write!(f, "dogs.toml could not be written: {err}"),
            Self::Lock(err) => write!(f, "dogs.toml could not be locked: {err}"),
            Self::Toml(err) => write!(f, "shep.toml could not be rewritten: {err}"),
        }
    }
}

impl core::error::Error for DogMigrationError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read(err) | Self::ReadDogs(err) | Self::Write(err) | Self::Lock(err) => Some(err),
            Self::Parse(err) => Some(err),
            Self::Toml(err) => Some(err),
            Self::WouldOverwrite { .. } | Self::SectionsUnreadable { .. } => None,
        }
    }
}

// `ShepToml::try_edit` is generic as `E: From<ShepTomlError>`: this is what
// lets its own setup failures (home dir, lock, parse) reach the caller as
// this module's error without a second wrapping layer.
impl From<ShepTomlError> for DogMigrationError {
    fn from(source: ShepTomlError) -> Self {
        Self::Toml(source)
    }
}

#[cfg(test)]
#[path = "dog_migration_tests.rs"]
mod tests;
