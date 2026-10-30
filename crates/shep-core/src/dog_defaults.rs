//! `dog-defaults.json`: one operator-set default table per dog.
//!
//! A sheep carrying `[app.dogs.<dog>]` layers its own table over the
//! dog's default; a sheep with no table for the dog gets none. The shepherd
//! layers at read time and holds nothing of it in `AppConfig::dogs`.
//!
//! Same on-disk shape as [`crate::overrides`]: a read-modify-rename under a
//! [`crate::file_lock`].

use core::fmt;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::DogTable;
use crate::file_lock::FileLock;
use crate::versioned_file::{self, ReadError, Versioned};

/// The on-disk format's version.
///
/// A store carrying a higher version is refused rather than read or
/// replaced ([`DogDefaultsError::FutureVersion`]).
pub const DOG_DEFAULTS_VERSION: u32 = 1;

/// Every dog's default table, by dog name.
///
/// `Debug` prints key counts only, through [`DogTable`]'s own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DogDefaults(BTreeMap<String, DogTable>);

impl DogDefaults {
    /// `dog`'s default table, if one is set.
    #[must_use]
    pub fn get(&self, dog: &str) -> Option<&DogTable> {
        self.0.get(dog)
    }

    /// Whether no dog has a default.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Sets `dog`'s default table, or removes it with `None`.
    ///
    /// An empty table removes: it holds nothing to inherit.
    pub fn set(&mut self, dog: &str, table: Option<DogTable>) {
        match table.filter(|table| !table.as_map().is_empty()) {
            Some(table) => self.0.insert(dog.to_string(), table),
            None => self.0.remove(dog),
        };
    }

    /// A sheep's `dogs` map with each default layered under the table the
    /// sheep carries for that dog.
    ///
    /// Borrowed as it stands when no default reaches the sheep. A dog the
    /// sheep carries no table for gets no default, and an empty table takes
    /// every one.
    #[must_use]
    pub fn layer<'a>(
        &self,
        dogs: &'a BTreeMap<String, DogTable>,
    ) -> Cow<'a, BTreeMap<String, DogTable>> {
        if !dogs.keys().any(|dog| self.0.contains_key(dog)) {
            return Cow::Borrowed(dogs);
        }
        Cow::Owned(
            dogs.iter()
                .map(|(dog, own)| {
                    let table = match self.0.get(dog) {
                        Some(default) => layered(&[default, own]),
                        None => own.clone(),
                    };
                    (dog.clone(), table)
                })
                .collect(),
        )
    }
}

/// `layers` folded into one table, the lowest first: a later layer's value
/// wins.
///
/// Nested tables merge key by key. An array or a plain value replaces what
/// is under it whole, so a sheep's list never adds to a default's.
#[must_use]
pub fn layered(layers: &[&DogTable]) -> DogTable {
    let mut out = Map::new();
    for layer in layers {
        merge(&mut out, layer.as_map());
    }
    DogTable::from(out)
}

fn merge(under: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (key, value) in over {
        match (under.get_mut(key), value) {
            (Some(Value::Object(held)), Value::Object(incoming)) => merge(held, incoming),
            _ => {
                under.insert(key.clone(), value.clone());
            }
        }
    }
}

/// The file's shape: a version and a map of dog name to its default.
#[derive(Debug, Default, Serialize, Deserialize)]
struct DefaultsFile {
    version: u32,
    dogs: DogDefaults,
}

impl Versioned for DefaultsFile {
    fn version(&self) -> u32 {
        self.version
    }
}

/// Error type returned by this module.
///
/// `#[non_exhaustive]`: shep-core is published, so a new failure variant
/// must not break an out-of-tree `match`.
#[non_exhaustive]
#[derive(Debug)]
pub enum DogDefaultsError {
    /// The store could not be read, written, or replaced.
    Io(std::io::Error),
    /// The store's JSON could not be parsed.
    Decode(serde_json::Error),
    /// The store on disk is a version this build does not understand;
    /// carries that version. Nothing was written.
    FutureVersion(u32),
}

impl fmt::Display for DogDefaultsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "dog defaults store I/O failed: {err}"),
            Self::Decode(err) => write!(f, "dog defaults store failed to parse: {err}"),
            Self::FutureVersion(version) => write!(
                f,
                "dog defaults store is version {version}, newer than this build understands"
            ),
        }
    }
}

impl core::error::Error for DogDefaultsError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Decode(err) => Some(err),
            Self::FutureVersion(_) => None,
        }
    }
}

impl From<ReadError> for DogDefaultsError {
    fn from(source: ReadError) -> Self {
        match source {
            ReadError::Io(err) => Self::Io(err),
            ReadError::Decode(err) => Self::Decode(err),
            ReadError::FutureVersion(version) => Self::FutureVersion(version),
        }
    }
}

fn read_file(path: &Path) -> Result<DefaultsFile, DogDefaultsError> {
    versioned_file::read_versioned(path, DOG_DEFAULTS_VERSION).map_err(DogDefaultsError::from)
}

/// Every dog's default table.
///
/// # Errors
///
/// - [`DogDefaultsError::Io`]: the store could not be opened or read. An
///   absent store is not an error: it reads as empty.
/// - [`DogDefaultsError::Decode`]: the file is not the JSON this module
///   writes.
/// - [`DogDefaultsError::FutureVersion`]: the file's `version` is newer than
///   [`DOG_DEFAULTS_VERSION`].
pub fn read(path: &Path) -> Result<DogDefaults, DogDefaultsError> {
    let _lock = FileLock::acquire(path).map_err(DogDefaultsError::Io)?;
    Ok(read_file(path)?.dogs)
}

/// Sets `dog`'s default table, or removes it with `None`, and returns the
/// store as it now stands.
///
/// Writes nothing when the store already holds exactly that.
///
/// # Errors
///
/// The set [`read`] returns, and [`DogDefaultsError::Io`] for the lock, the
/// staging file, the `fsync` or the `rename`. Nothing is written on any of
/// them.
pub fn put(
    path: &Path,
    dog: &str,
    table: Option<DogTable>,
) -> Result<DogDefaults, DogDefaultsError> {
    let _lock = FileLock::acquire(path).map_err(DogDefaultsError::Io)?;
    let mut file = read_file(path)?;
    let before = file.dogs.clone();
    file.dogs.set(dog, table);
    if file.version == DOG_DEFAULTS_VERSION && file.dogs == before {
        return Ok(file.dogs);
    }
    file.version = DOG_DEFAULTS_VERSION;
    crate::atomic_file::write_json(path, "dog-defaults", &file).map_err(DogDefaultsError::Io)?;
    Ok(file.dogs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(value: serde_json::Value) -> DogTable {
        match value {
            serde_json::Value::Object(map) => DogTable::from(map),
            other => panic!("not a table: {other}"),
        }
    }

    #[test]
    fn a_sheep_value_wins_and_a_nested_table_merges_key_by_key() {
        let operator = table(json!({
            "model": "opus",
            "review": { "lines": 8, "tone": "terse" },
            "labels": ["a", "b"],
        }));
        let sheep = table(json!({
            "model": "sonnet",
            "review": { "lines": 3 },
        }));
        assert_eq!(
            layered(&[&operator, &sheep]),
            table(json!({
                "model": "sonnet",
                "review": { "lines": 3, "tone": "terse" },
                "labels": ["a", "b"],
            }))
        );
    }

    #[test]
    fn an_array_or_a_plain_value_in_the_sheep_table_replaces_the_default_whole() {
        let operator = table(json!({
            "labels": ["a", "b"],
            "review": { "lines": 8 },
            "budget": 5,
        }));
        let sheep = table(json!({
            "labels": ["c"],
            "review": "off",
            "budget": { "daily": 1 },
        }));
        assert_eq!(layered(&[&operator, &sheep]), sheep);
    }

    #[test]
    fn an_empty_sheep_table_takes_every_default_and_no_table_takes_none() {
        let operator = table(json!({ "model": "opus" }));
        let mut defaults = DogDefaults::default();
        defaults.set("jobs", Some(operator.clone()));

        let carrying = BTreeMap::from([("jobs".to_string(), table(json!({})))]);
        assert_eq!(
            defaults.layer(&carrying).into_owned(),
            BTreeMap::from([("jobs".to_string(), operator)])
        );

        let bare = BTreeMap::from([("deploy".to_string(), table(json!({})))]);
        assert_eq!(defaults.layer(&bare).into_owned(), bare);
    }

    #[test]
    fn a_put_default_reads_back_and_none_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dog-defaults.json");
        assert!(read(&path).unwrap().is_empty());

        let jobs = table(json!({ "model": "opus" }));
        put(&path, "jobs", Some(jobs.clone())).unwrap();
        put(&path, "deploy", Some(table(json!({ "region": "eu" })))).unwrap();
        assert_eq!(read(&path).unwrap().get("jobs"), Some(&jobs));

        put(&path, "jobs", None).unwrap();
        assert_eq!(read(&path).unwrap().get("jobs"), None);
        assert!(read(&path).unwrap().get("deploy").is_some());
    }

    #[test]
    fn an_empty_default_table_is_no_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dog-defaults.json");
        put(&path, "jobs", Some(table(json!({ "model": "opus" })))).unwrap();
        put(&path, "jobs", Some(table(json!({})))).unwrap();
        assert!(read(&path).unwrap().is_empty());
    }

    #[test]
    fn a_store_from_a_newer_build_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dog-defaults.json");
        let future = r#"{"version":99,"dogs":{}}"#;
        std::fs::write(&path, future).unwrap();
        assert!(matches!(
            put(&path, "jobs", Some(table(json!({ "a": 1 })))),
            Err(DogDefaultsError::FutureVersion(99))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), future);
    }
}
