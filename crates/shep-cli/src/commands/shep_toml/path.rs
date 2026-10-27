//! Reading and writing `shep.toml` by key path.
//!
//! The per-key setters in [`super`] each own one key a verb writes. The
//! settings screen writes whatever its schema names, so it goes by path:
//! `["daemon", "log_level"]`, or `["interpreters", "py"]` for one entry of a
//! map. A write replaces a value in place, so the comment trailing it stays.

use std::fmt;

use toml_edit::{Item, Table, TableLike, Value};

use super::ShepToml;

/// Why [`ShepToml::set_path`] could not write.
///
/// `Debug` is derived, not redacted: a key path and a TOML type name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathError {
    /// A key above the target is in the file as something other than a
    /// table, so there is nowhere to put the target.
    NotATable {
        /// The dotted path of that key.
        at: String,
        /// What it is instead, as TOML names the type.
        found: &'static str,
    },
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotATable { at, found } => {
                write!(f, "{at} is {found} in shep.toml, not a table")
            }
        }
    }
}

impl core::error::Error for PathError {}

impl ShepToml {
    /// Writes `value` at `path`, creating the tables above it.
    ///
    /// A value already there is replaced in place and keeps its decor, so a
    /// trailing comment survives. `value` is written as given, unchecked:
    /// [`shep_core::config::DaemonConfig::load`] decides whether the
    /// document still boots.
    ///
    /// # Errors
    /// [`PathError::NotATable`] if a key above the target is in the file as
    /// something other than a table. An inline table counts as one.
    pub(crate) fn set_path(&mut self, path: &[String], value: Value) -> Result<(), PathError> {
        let Some((last, parents)) = path.split_last() else {
            return Ok(());
        };
        let table = descend_creating(self.doc.as_table_mut(), parents)?;
        if let Some(Item::Value(old)) = table.get_mut(last) {
            let decor = old.decor().clone();
            *old = value;
            *old.decor_mut() = decor;
        } else {
            table.insert(last, Item::Value(value));
        }
        Ok(())
    }

    /// Removes the key at `path`. Does nothing when it, or a table above
    /// it, is absent or not a table. An inline table counts as one.
    pub(crate) fn unset_path(&mut self, path: &[String]) {
        let Some((last, parents)) = path.split_last() else {
            return;
        };
        let mut table: &mut dyn TableLike = self.doc.as_table_mut();
        for key in parents {
            let Some(next) = table.get_mut(key).and_then(Item::as_table_like_mut) else {
                return;
            };
            table = next;
        }
        table.remove(last);
    }

    /// The document as JSON, for reading a value by the same dotted keys a
    /// schema flattens to. A date or time reads as its text.
    pub(crate) fn to_json(&self) -> serde_json::Map<String, serde_json::Value> {
        table_to_json(self.doc.as_table())
    }
}

/// `table` descended through `keys`, creating each missing table.
fn descend_creating<'a>(
    mut table: &'a mut dyn TableLike,
    keys: &[String],
) -> Result<&'a mut dyn TableLike, PathError> {
    let mut walked = Vec::new();
    for key in keys {
        walked.push(key.as_str());
        let item = table
            .entry(key)
            .or_insert_with(|| Item::Table(Table::new()));
        let found = item.type_name();
        table = item
            .as_table_like_mut()
            .ok_or_else(|| PathError::NotATable {
                at: walked.join("."),
                found,
            })?;
    }
    Ok(table)
}

fn table_to_json(table: &Table) -> serde_json::Map<String, serde_json::Value> {
    table
        .iter()
        .map(|(key, item)| (key.to_owned(), item_to_json(item)))
        .collect()
}

fn item_to_json(item: &Item) -> serde_json::Value {
    match item {
        Item::None => serde_json::Value::Null,
        Item::Value(value) => value_to_json(value),
        Item::Table(table) => serde_json::Value::Object(table_to_json(table)),
        Item::ArrayOfTables(tables) => {
            serde_json::Value::Array(tables.iter().map(|t| table_to_json(t).into()).collect())
        }
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::String(text) => text.value().as_str().into(),
        Value::Integer(number) => (*number.value()).into(),
        Value::Float(number) => serde_json::Number::from_f64(*number.value())
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Boolean(flag) => (*flag.value()).into(),
        Value::Datetime(when) => when.value().to_string().into(),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::InlineTable(table) => serde_json::Value::Object(
            table
                .iter()
                .map(|(key, value)| (key.to_owned(), value_to_json(value)))
                .collect(),
        ),
    }
}
