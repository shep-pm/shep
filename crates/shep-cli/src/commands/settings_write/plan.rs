//! What one `shep set` would write, decided before any of it is sent: the
//! keys a value lands on, the one request that writes them, and what the value
//! has to pass first.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value as Json};
use shep_core::config::DogTable;
use toml_edit::DocumentMut;

use super::edit::{document_of, dotted, set_toml};
use super::value::table_json;
use super::{Plan, Write, WriteRefusal};
use crate::commands::copy::address::{self, DEFAULTS};
use crate::commands::copy::plan::{Change, ChangeKind, collect_changes, set_path, walk};
use crate::dog_settings::table::{default_refusals, table_refusal, touched_refusal};

/// The plan for one dog's table on one sheep.
///
/// `keys` is the path under the dog's table, empty for the whole table.
///
/// # Errors
/// [`WriteRefusal::Refused`] when the write would have to go through a value
/// that is not a table, or when the whole table is named and the value is not
/// one.
pub(super) fn plan_dog_table(
    sheep: &str,
    dog: &str,
    keys: &[String],
    config: &Json,
    value: &Json,
) -> Result<Plan, WriteRefusal> {
    let before = Json::Object(
        address::dog_table(config, dog)
            .and_then(Json::as_object)
            .cloned()
            .unwrap_or_default(),
    );
    let edit = plan_table(&format!("{sheep}.dogs.{dog}"), before, keys, value)?;
    Ok(Plan {
        changes: edit.changes,
        write: Write::SheepDogTable {
            dog: dog.to_string(),
            table: DogTable::from(edit.table.clone()),
            before: edit.before,
        },
        table: edit.table,
        schema: None,
        touched: touched_paths(keys, value),
    })
}

/// The plan for one dog's defaults.
///
/// `held` is the default as the shepherd answered it, and `sheep` is each
/// sheep carrying a table for the dog, by its own table: the schema check
/// layers the new default under each, as the dog reads it.
///
/// # Errors
/// As [`plan_dog_table`].
pub(super) fn plan_dog_defaults(
    dog: &str,
    keys: &[String],
    held: Json,
    sheep: BTreeMap<String, DogTable>,
    value: &Json,
) -> Result<Plan, WriteRefusal> {
    let edit = plan_table(&format!("{dog}.{DEFAULTS}"), held, keys, value)?;
    Ok(Plan {
        changes: edit.changes,
        write: Write::DogDefaults {
            dog: dog.to_string(),
            table: DogTable::from(edit.table.clone()),
            before: edit.before,
            sheep,
        },
        table: edit.table,
        schema: None,
        touched: touched_paths(keys, value),
    })
}

/// What writing `value` at `keys` does to one table.
struct TableEdit {
    /// One entry per key that moves.
    changes: Vec<Change>,
    /// The table before the write.
    before: Json,
    /// The table after it.
    table: Map<String, Json>,
}

/// `before` with `value` put at `keys`, and the diff between the two.
///
/// `at` is where the table sits, as the operator spells it, for the
/// refusals.
fn plan_table(
    at: &str,
    before: Json,
    keys: &[String],
    value: &Json,
) -> Result<TableEdit, WriteRefusal> {
    let mut after = before.clone();
    // A table the sheep already holds is not replaced by a value that is not
    // one, which is the same rule the `dogs.toml` half follows through
    // [`set_toml`]: a key that holds a table holds it until an operator names
    // a table to put there.
    if !keys.is_empty() && !value.is_object() && walk(&before, keys).is_some_and(Json::is_object) {
        return Err(WriteRefusal::Refused(format!(
            "`{at}.{}` holds a table, and `shep set` does not replace a table with a value that \
                 is not one",
            keys.join(".")
        )));
    }
    set_path(&mut after, keys, value.clone()).map_err(|held| {
        WriteRefusal::Refused(format!(
            "`{at}{}` holds a value that is not a table, and `shep set` cannot write `{}` \
             through it",
            dotted(&held),
            keys.join(".")
        ))
    })?;
    let Some(table) = after.as_object().cloned() else {
        return Err(WriteRefusal::Refused(format!(
            "`{at}` names a whole dog table: the value has to be one, as `{{ keep = 1 }}`"
        )));
    };
    let mut changes = Vec::new();
    collect_changes(
        walk(&before, keys),
        walk(&Json::Object(table.clone()), keys),
        "",
        &mut changes,
    );
    name_the_path(&mut changes, keys);
    Ok(TableEdit {
        changes,
        before,
        table,
    })
}

/// The plan for one dog's section, edited in place.
///
/// The document is what the shepherd answered, so the text this hands back is
/// that text with one key changed: every comment the operator wrote inside
/// the section survives. `keys` empty is the whole section, the one write
/// that replaces a document rather than editing one, and the one that loses
/// the comments in it.
///
/// # Errors
/// [`WriteRefusal::Refused`] when the write would go through a value that is
/// not a table, erase a table with one, or replace an array of tables.
/// [`WriteRefusal::Usage`] when the whole section is named and the value is
/// not a table.
pub(super) fn plan_section(
    doc: DocumentMut,
    name: &str,
    keys: &[String],
    value: &Json,
) -> Result<Plan, WriteRefusal> {
    let mut doc = doc;
    // The text the shepherd answered, kept so the write can check the section
    // still answers it. `toml_edit` renders a parsed document back to the text
    // it parsed from, comment for comment.
    let answered = doc.to_string();
    let before = table_json(doc.as_table());
    if keys.is_empty() {
        let Json::Object(map) = value else {
            return Err(WriteRefusal::Usage(
                "a whole section takes a whole table: the value has to read as `{ keep = 1 }`, or \
                 name a key inside the section, as `kelpie.worker.timeout`"
                    .to_string(),
            ));
        };
        doc = document_of(map).map_err(|held| held.refusal(name, keys))?;
    } else {
        set_toml(doc.as_table_mut(), keys, "", value).map_err(|held| held.refusal(name, keys))?;
    }
    let toml = doc.to_string();
    // The table this document renders is the table the shepherd parses back out
    // of that text: `toml_edit` renders a parsed document so it re-parses to
    // the same tree, and the change list and the schema check both read the
    // tree rather than the text.
    let after = table_json(doc.as_table());
    let mut changes = Vec::new();
    collect_changes(
        walk(&Json::Object(before), keys),
        walk(&Json::Object(after.clone()), keys),
        "",
        &mut changes,
    );
    name_the_path(&mut changes, keys);
    Ok(Plan {
        changes,
        write: Write::DogSection {
            toml,
            before: answered,
        },
        table: after,
        schema: None,
        touched: touched_paths(keys, value),
    })
}

/// The keys a write to a sheep's own field changes: one, or none where the
/// sheep already holds the value.
pub(super) fn field_changes(field: &str, config: &Json, value: &Json) -> Vec<Change> {
    let old = config.get(field).cloned();
    if old.as_ref() == Some(value) {
        return Vec::new();
    }
    let kind = match old {
        Some(_) => ChangeKind::Changed,
        None => ChangeKind::Added,
    };
    vec![Change {
        key: field.to_string(),
        kind,
        old,
        new: Some(value.clone()),
    }]
}

/// The diff [`collect_changes`] built under the written path, named the way the
/// operator named it, for [`crate::commands::copy::plan::plan_dog`]'s reason: a
/// change list whose only row is a blank names nothing the operator can act on.
pub(super) fn name_the_path(changes: &mut [Change], keys: &[String]) {
    let Some(last) = keys.last() else {
        return;
    };
    for change in changes {
        if change.key.is_empty() {
            change.key = last.clone();
        }
    }
}

/// Every property path under `keys` that `value` puts a leaf at.
///
/// A scalar is one path. A table is one path per leaf inside it, because the
/// schema door walks leaves.
///
/// Empty `keys` is the whole-table write, which answers the empty set.
pub(super) fn touched_paths(keys: &[String], value: &Json) -> BTreeSet<Vec<String>> {
    let mut touched = BTreeSet::new();
    if !keys.is_empty() {
        leaves(keys.to_vec(), value, &mut touched);
    }
    touched
}

/// [`touched_paths`] walking `value`, with the path it has reached so far.
fn leaves(path: Vec<String>, value: &Json, out: &mut BTreeSet<Vec<String>>) {
    let Json::Object(map) = value else {
        out.insert(path);
        return;
    };
    if map.is_empty() {
        out.insert(path);
        return;
    }
    for (key, nested) in map {
        let mut step = path.clone();
        step.push(key.clone());
        leaves(step, nested, out);
    }
}

/// The refusal for a value the dog's own schema turns down, or `None` when no
/// rule it published is broken by the plan.
///
/// A plan that replaces a whole table is asked whole: every key in it is one
/// the operator chose. A plan that writes one key is asked about that key and
/// the leaves under it.
pub(super) fn schema_refusal(plan: &Plan, name: &str, path: &str) -> Option<WriteRefusal> {
    let schema = plan.schema.as_ref()?;
    if let Write::DogDefaults { table, sheep, .. } = &plan.write {
        return default_refusal(schema, table, sheep, &plan.touched, name, path);
    }
    let table = &plan.table;
    let refused = if plan.touched.is_empty() {
        table_refusal(schema, table)
    } else {
        touched_refusal(schema, table, &plan.touched)
    }?;
    Some(WriteRefusal::Invalid(format!(
        "nothing was written: `{name}.{path}` is not a value the flock's schema takes. {}",
        refused.text
    )))
}

/// The refusal for a default some sheep's layered table would break, naming
/// each sheep and the rule it breaks, or `None` when every one is taken.
///
/// A default no sheep carries a table for has nothing to be checked against.
fn default_refusal(
    schema: &Json,
    default: &DogTable,
    sheep: &BTreeMap<String, DogTable>,
    touched: &BTreeSet<Vec<String>>,
    name: &str,
    path: &str,
) -> Option<WriteRefusal> {
    let refused = default_refusals(schema, default, sheep, touched);
    if refused.is_empty() {
        return None;
    }
    let each: Vec<String> = refused
        .iter()
        .map(|(sheep, refusal)| format!("`{sheep}` would hold {}", refusal.text))
        .collect();
    Some(WriteRefusal::Invalid(format!(
        "nothing was written: `{name}.{path}` is not a default the flock's schema takes, for \
         {} of the sheep it reaches: {}",
        refused.len(),
        each.join("; ")
    )))
}
