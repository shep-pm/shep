//! `[daemon.dog_grants]`, where an operator records what a dog may ask of
//! the shepherd, and what is left to decide.

use shep_core::permissions::Grants;
use toml_edit::{Array, InlineTable, Item, Table, TableLike, Value};

use super::{ShepToml, ShepTomlError};

/// The keys the boot's `DogGrantsSection` reads.
const KEYS: [&str; 5] = ["granted", "waiting", "revoked", "reviewed", "events"];

/// One dog's `[daemon.dog_grants.<name>]` entry.
///
/// The shepherd judges a dog by `granted` alone. The rest is the CLI's record
/// of the upgrade path, so that a dog never gains access without the operator
/// seeing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRecord {
    /// What the dog may ask.
    pub granted: Grants,
    /// What a newer binary declares that the operator has not approved. The
    /// dog runs without it.
    pub waiting: Grants,
    /// What the operator took away, so a binary that still declares it is not
    /// read as asking for something new.
    pub revoked: Grants,
    /// `false` for a list written at upgrade that the operator has not yet
    /// looked at.
    pub reviewed: bool,
    /// The event kinds the dog's manifest declares it publishes. Not a
    /// permission: the shepherd lets the dog publish these and no others,
    /// under its own name.
    pub events: Vec<String>,
}

impl GrantRecord {
    /// `granted`, looked at by the operator, with nothing waiting or revoked.
    #[must_use]
    pub fn reviewed(granted: Grants) -> Self {
        Self {
            granted,
            waiting: Grants::default(),
            revoked: Grants::default(),
            reviewed: true,
            events: Vec::new(),
        }
    }

    /// `granted`, written at upgrade for the operator to look at later.
    #[must_use]
    pub fn at_upgrade(granted: Grants) -> Self {
        Self {
            reviewed: false,
            ..Self::reviewed(granted)
        }
    }
}

impl GrantRecord {
    /// This record declaring `events` as the kinds the dog publishes.
    #[must_use]
    pub fn with_events(self, events: Vec<String>) -> Self {
        Self { events, ..self }
    }
}

impl ShepToml {
    /// The permissions recorded for `name` under
    /// `[daemon.dog_grants.<name>] granted`, or `None` when the dog has no
    /// entry and is undeclared, with full access.
    ///
    /// # Errors
    /// As [`Self::grant_record`].
    pub fn granted(&self, name: &str) -> Result<Option<Grants>, ShepTomlError> {
        Ok(self.grant_record(name)?.map(|record| record.granted))
    }

    /// Everything recorded for `name`, or `None` when the dog has no entry and
    /// is undeclared, with full access.
    ///
    /// Read as strictly as the shepherd reads it at boot, so a hand-typed
    /// typo is refused where the operator is looking rather than recorded as
    /// a grant that matches nothing.
    ///
    /// # Errors
    /// [`ShepTomlError::BadGrants`] when the entry is not a table holding
    /// only the keys the boot reads, when a list holds anything but strings
    /// or names a permission shep cannot read, or when `reviewed` is not a
    /// boolean, or `events` is not a list of strings.
    pub fn grant_record(&self, name: &str) -> Result<Option<GrantRecord>, ShepTomlError> {
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
        if let Some((key, _)) = entry.iter().find(|(key, _)| !KEYS.contains(key)) {
            return Err(bad(format!("`{key}` is not a key shep reads here")));
        }
        let list = |key: &str, required: bool| -> Result<Grants, ShepTomlError> {
            let Some(item) = entry.get(key) else {
                return if required {
                    Err(bad(format!("`{key}` must be a list of permission names")))
                } else {
                    Ok(Grants::default())
                };
            };
            let names = item
                .as_array()
                .and_then(|names| names.iter().map(|v| v.as_str()).collect::<Option<Vec<_>>>())
                .ok_or_else(|| bad(format!("`{key}` must be a list of permission names")))?;
            Grants::parse(names).map_err(|err| bad(format!("`{key}`: {err}")))
        };
        let reviewed = match entry.get("reviewed") {
            None => true,
            Some(item) => item
                .as_bool()
                .ok_or_else(|| bad("`reviewed` must be true or false".to_string()))?,
        };
        let events = match entry.get("events") {
            None => Vec::new(),
            Some(item) => item
                .as_array()
                .and_then(|kinds| {
                    kinds
                        .iter()
                        .map(|kind| kind.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| bad("`events` must be a list of event kinds".to_string()))?,
        };
        Ok(Some(GrantRecord {
            granted: list("granted", true)?,
            waiting: list("waiting", false)?,
            revoked: list("revoked", false)?,
            reviewed,
            events,
        }))
    }

    /// Records `grants` as everything `name` may ask of the shepherd, looked
    /// at, replacing what was there.
    ///
    /// # Errors
    /// As [`Self::set_grant_record`].
    #[cfg(all(test, unix))]
    pub fn set_granted(&mut self, name: &str, grants: &Grants) -> Result<(), ShepTomlError> {
        self.set_grant_record(name, &GrantRecord::reviewed(grants.clone()))
    }

    /// Records `record` for `name` under `[daemon.dog_grants.<name>]` beside
    /// `adopted_dogs`, replacing what was there.
    ///
    /// Only the CLI writes this table: a dog's own section lives in
    /// `dogs.toml` so that a dog can never grant itself anything. An empty
    /// `granted` is written as `granted = []`, which grants nothing; leaving
    /// the entry out would make the dog undeclared, with full access.
    /// `waiting`, `revoked`, `reviewed` and `events` are written only when
    /// they say something.
    ///
    /// # Errors
    /// [`ShepTomlError::WrongShape`] when `daemon` or `dog_grants` holds
    /// something other than a table.
    pub fn set_grant_record(
        &mut self,
        name: &str,
        record: &GrantRecord,
    ) -> Result<(), ShepTomlError> {
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
        match item {
            Item::Table(table) => {
                let mut entry = Table::new();
                fill(&mut entry, record);
                table.insert(name, Item::Table(entry));
            }
            Item::Value(Value::InlineTable(table)) => {
                let mut entry = InlineTable::new();
                fill(&mut entry, record);
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

/// Writes `record`'s keys into `entry`, in the order the boot documents them.
fn fill(entry: &mut dyn TableLike, record: &GrantRecord) {
    let list = |grants: &Grants| Item::Value(Value::Array(grants.names().collect::<Array>()));
    entry.insert("granted", list(&record.granted));
    if record.waiting.names().next().is_some() {
        entry.insert("waiting", list(&record.waiting));
    }
    if record.revoked.names().next().is_some() {
        entry.insert("revoked", list(&record.revoked));
    }
    if !record.reviewed {
        entry.insert("reviewed", Item::Value(Value::from(false)));
    }
    if !record.events.is_empty() {
        entry.insert(
            "events",
            Item::Value(Value::Array(
                record.events.iter().map(String::as_str).collect(),
            )),
        );
    }
}
