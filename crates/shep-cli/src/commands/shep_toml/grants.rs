//! The read side of `[daemon.dog_grants]`, where an operator records what a
//! dog may ask of the shepherd.

use shep_core::permissions::Grants;
use toml_edit::{Array, InlineTable, Item, Table, Value};

use super::{ShepToml, ShepTomlError};

impl ShepToml {
    /// The permissions recorded for `name` under
    /// `[daemon.dog_grants.<name>] granted`, or `None` when the dog has no
    /// entry and is undeclared, with full access.
    ///
    /// Read as strictly as the shepherd reads it at boot, so a hand-typed
    /// typo is refused where the operator is looking rather than recorded as
    /// a grant that matches nothing.
    ///
    /// # Errors
    /// [`ShepTomlError::BadGrants`] when the entry is not a table holding
    /// only a `granted` array of strings, or names a permission shep cannot
    /// read.
    pub fn granted(&self, name: &str) -> Result<Option<Grants>, ShepTomlError> {
        // Table-like, not `as_table`: `dog_grants = { otel = { granted = [] } }`
        // is the same TOML as the headed form and the boot reads both, so a
        // reader that saw only headed tables would send a dog granted nothing
        // as undeclared, with full access.
        let Some(entry) = self
            .doc
            .get("daemon")
            .and_then(Item::as_table_like)
            .and_then(|daemon| daemon.get("dog_grants"))
            .and_then(Item::as_table_like)
            .and_then(|grants| grants.get(name))
        else {
            return Ok(None);
        };
        let bad = |reason: String| ShepTomlError::BadGrants {
            path: self.path.clone(),
            dog: name.to_string(),
            reason,
        };
        let entry = entry
            .as_table_like()
            .ok_or_else(|| bad("must be a table holding `granted`".to_string()))?;
        // The boot's `DogGrantsSection` denies unknown keys, so a stray one
        // is refused here, where the operator is looking, and not at the next
        // boot.
        if let Some((key, _)) = entry.iter().find(|(key, _)| *key != "granted") {
            return Err(bad(format!("`{key}` is not a key shep reads here")));
        }
        let names = entry
            .get("granted")
            .and_then(Item::as_array)
            .and_then(|names| names.iter().map(|v| v.as_str()).collect::<Option<Vec<_>>>())
            .ok_or_else(|| bad("`granted` must be a list of permission names".to_string()))?;
        Grants::parse(names)
            .map(Some)
            .map_err(|err| bad(err.to_string()))
    }

    /// Records `grants` as everything `name` may ask of the shepherd, under
    /// `[daemon.dog_grants.<name>]` beside `adopted_dogs`, replacing what was
    /// there.
    ///
    /// Only the CLI writes this table: a dog's own section lives in
    /// `dogs.toml` so that a dog can never grant itself anything. An empty
    /// `grants` is written as `granted = []`, which grants nothing; leaving
    /// the entry out would make the dog undeclared, with full access.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] when `daemon` or `dog_grants` holds
    /// something other than a table.
    pub fn set_granted(&mut self, name: &str, grants: &Grants) -> Result<(), ShepTomlError> {
        let path = self.path.clone();
        let item = self
            .daemon_table_mut()?
            .entry("dog_grants")
            .or_insert_with(|| {
                let mut table = Table::new();
                // Only `[daemon.dog_grants.<name>]` is printed, not a bare
                // `[daemon.dog_grants]` above it.
                table.set_implicit(true);
                Item::Table(table)
            });
        let found = item.type_name();
        let names: Array = grants.names().collect();
        match item {
            Item::Table(table) => {
                let mut entry = Table::new();
                entry.insert("granted", Item::Value(Value::Array(names)));
                table.insert(name, Item::Table(entry));
            }
            Item::Value(Value::InlineTable(table)) => {
                let mut entry = InlineTable::new();
                entry.insert("granted", Value::Array(names));
                table.insert(name, Value::InlineTable(entry));
            }
            _ => {
                return Err(ShepTomlError::WrongShape {
                    path,
                    key: "dog_grants",
                    expected: "a table",
                    found,
                });
            }
        }
        Ok(())
    }
}
