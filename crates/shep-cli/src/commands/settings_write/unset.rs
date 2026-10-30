//! `shep unset <address>`: the undo for `shep set`.
//!
//! The address is `set`'s and the flow after it is [`run`]'s. What differs is
//! the plan: there is no value to write, and no schema to check against a
//! key that is simply gone.
//!
//! - A sheep's field is [`Request::ResetSheepField`]: the shepherd drops the
//!   operator's override and the Flockfile's value comes back.
//! - A key in a sheep's dog table leaves the table, so the dog's default applies.
//! - A key in a dog's own section leaves `dogs.toml`'s text, edited in place.

use std::collections::BTreeSet;

use serde_json::{Map, Value as Json};
use shep_client::Client;
use shep_core::config::DogTable;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{Request, Response};
use toml_edit::{Item, TableLike};

use super::plan::name_the_path;
use super::value::table_json;
use super::{Flags, Plan, Verb, Write, WriteRefusal, defaults_in, path_in, resolve_dog, run};
use crate::cli::UnsetArgs;
use crate::commands::copy;
use crate::commands::copy::address::{self, ConfigPath, DEFAULTS, FlockNames};
use crate::commands::copy::plan::{Change, ChangeKind, collect_changes, walk};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::settings_read;
use crate::exit::ExitCode;
use crate::output::Streams;

/// `shep unset <address>`.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: a malformed address, a name
/// no sheep or dog answers, a field with no override or a key nothing holds, a
/// key this verb never writes, a refusal to write with no terminal to ask on,
/// or a write the shepherd refused.
pub(crate) async fn unset(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &UnsetArgs,
) -> ExitCode {
    let flock = match copy::flock_names(client, streams).await {
        Ok(names) => names,
        Err(code) => return code,
    };
    let segments = match settings_read::segments(&args.address) {
        Ok(segments) => segments,
        Err(refusal) => return refusal.report(streams),
    };
    let flags = Flags {
        verb: Verb::Unset,
        yes: args.yes,
        dry_run: args.dry_run,
    };
    if let Some(name) = address::longest_in(&segments, &flock.sheep) {
        return unset_sheep(client, streams, paths, &name, &segments, &flock, flags).await;
    }
    unset_dog(client, streams, paths, &segments, &flock, flags).await
}

/// One sheep's field, or a key inside one of its dog tables.
async fn unset_sheep(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    segments: &[String],
    flock: &FlockNames,
    flags: Flags,
) -> ExitCode {
    let (config, overridden) = match read_view(client, streams, name).await {
        Ok(view) => view,
        Err(code) => return code,
    };
    let path = match path_in(name, segments, flock, &config) {
        Ok(path) => path,
        Err(refusal) => return refusal.report(streams),
    };
    let plan = match &path {
        ConfigPath::Field { name: field } => plan_field(name, field, &config, &overridden),
        ConfigPath::Env => Err(WriteRefusal::Refused(address::ENV_NOT_WRITTEN.to_string())),
        ConfigPath::Dog { dog, keys } => plan_dog_table(name, dog, keys, &config),
    };
    match plan {
        Ok(plan) => run(client, streams, paths, name, &path.dotted(), &plan, flags).await,
        Err(refusal) => refusal.report(streams),
    }
}

/// One key of a dog's own section of `dogs.toml`.
async fn unset_dog(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    segments: &[String],
    flock: &FlockNames,
    flags: Flags,
) -> ExitCode {
    if let Some(defaults) = defaults_in(segments, flock, paths) {
        return WriteRefusal::Refused(format!(
            "`shep unset` does not remove a key of `{dog}`'s defaults: `shep set {dog}.{DEFAULTS} \
             <table>` writes the whole default, and `shep set {dog}.{DEFAULTS} {{}}` removes it",
            dog = defaults.dog
        ))
        .report(streams);
    }
    let target = match resolve_dog(client, streams, segments, flock, paths).await {
        Ok(target) => target,
        Err(code) => return code,
    };
    let doc = match target.document(streams) {
        Ok(doc) => doc,
        Err(code) => return code,
    };
    let rest = target.rest(segments);
    let keys = settings_read::dog_steps(doc.as_table(), rest);
    match plan_section(doc, &target.name, &keys) {
        Ok(plan) => {
            run(
                client,
                streams,
                paths,
                &target.name,
                &rest.join("."),
                &plan,
                flags,
            )
            .await
        }
        Err(refusal) => refusal.report(streams),
    }
}

/// A sheep's config as JSON, and the names of the fields the operator
/// overrides.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
async fn read_view(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
) -> Result<(Json, Vec<String>), ExitCode> {
    match client
        .request(Request::SheepConfig {
            name: name.to_string(),
        })
        .await
    {
        Ok(Response::SheepConfig(view)) => {
            let config = serde_json::to_value(&view.config).map_err(|_| {
                streams.fail(
                    ExitCode::Internal,
                    "the shepherd's config answer did not serialize, so nothing was changed",
                )
            })?;
            Ok((config, view.overridden))
        }
        Ok(_other) => Err(unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// The plan for dropping one field's override.
///
/// The change list names the value the override holds now and no new one: the
/// value that returns is the Flockfile's, which only the shepherd keeps, and
/// the reply to the write says what it is.
///
/// # Errors
/// [`WriteRefusal::Refused`] for a field the operator does not override, which
/// already holds the Flockfile's value.
fn plan_field(
    sheep: &str,
    field: &str,
    config: &Json,
    overridden: &[String],
) -> Result<Plan, WriteRefusal> {
    if !overridden.iter().any(|name| name == field) {
        return Err(WriteRefusal::Refused(format!(
            "`{sheep}.{field}` has no override to drop: it already holds the Flockfile's value. \
             `shep get {sheep}.{field}` reads it"
        )));
    }
    let held = config.get(field).unwrap_or(&Json::Null);
    Ok(Plan {
        changes: removal_changes(Some(held), field, field),
        write: Write::ResetSheepField {
            key: field.to_string(),
        },
        table: Map::new(),
        schema: None,
        touched: BTreeSet::new(),
    })
}

/// The plan for removing a key from one dog's table on one sheep, or the whole
/// table when `keys` is empty.
///
/// # Errors
/// [`WriteRefusal::Refused`] when the sheep holds no table for the dog or the
/// table holds no such key.
pub(super) fn plan_dog_table(
    sheep: &str,
    dog: &str,
    keys: &[String],
    config: &Json,
) -> Result<Plan, WriteRefusal> {
    let at = |keys: &[String]| {
        if keys.is_empty() {
            format!("{sheep}.dogs.{dog}")
        } else {
            format!("{sheep}.dogs.{dog}.{}", keys.join("."))
        }
    };
    let Some(before) = address::dog_table(config, dog).filter(|table| table.is_object()) else {
        return Err(WriteRefusal::Refused(format!(
            "`{}` holds nothing to remove: `{sheep}` has no table for `{dog}`",
            at(&[])
        )));
    };
    let before = before.clone();
    let mut after = before.clone();
    let write = if keys.is_empty() {
        // The table goes whole, so what the holder ends up holding is nothing,
        // and that is what the dog is asked about.
        after = Json::Object(Map::new());
        Write::DropSheepDogTable {
            dog: dog.to_string(),
            before: before.clone(),
        }
    } else {
        if remove_path(&mut after, keys).is_none() {
            return Err(WriteRefusal::Refused(format!(
                "`{}` holds nothing to remove: the table has no such key",
                at(keys)
            )));
        }
        let Json::Object(table) = after.clone() else {
            return Err(WriteRefusal::Refused(format!(
                "`{}` is not a table",
                at(&[])
            )));
        };
        Write::SheepDogTable {
            dog: dog.to_string(),
            table: DogTable::from(table.clone()),
            before: before.clone(),
        }
    };
    let label = keys.last().map_or(dog, String::as_str);
    let mut changes = removal_changes(walk(&before, keys), "", label);
    name_the_path(&mut changes, keys);
    Ok(Plan {
        changes,
        write,
        // What the holder ends up holding, which is what the dog is asked
        // about: an empty map for a table that goes whole.
        table: after.as_object().cloned().unwrap_or_default(),
        schema: None,
        touched: BTreeSet::new(),
    })
}

/// The plan for removing one key from a dog's section, edited in place.
///
/// # Errors
/// [`WriteRefusal::Usage`] when the address names the whole section.
/// [`WriteRefusal::Refused`] when the section holds no such key.
pub(super) fn plan_section(
    mut doc: toml_edit::DocumentMut,
    name: &str,
    keys: &[String],
) -> Result<Plan, WriteRefusal> {
    if keys.is_empty() {
        return Err(WriteRefusal::Usage(format!(
            "`{name}` names a whole section: `shep unset` removes a key inside it, as \
             `{name}.<key>`"
        )));
    }
    let answered = doc.to_string();
    let before = Json::Object(table_json(doc.as_table()));
    if remove_toml(doc.as_table_mut(), keys).is_none() {
        return Err(WriteRefusal::Refused(format!(
            "`{name}.{}` holds nothing to remove: the section has no such key",
            keys.join(".")
        )));
    }
    let after = table_json(doc.as_table());
    let mut changes = removal_changes(
        walk(&before, keys),
        "",
        keys.last().map_or(name, String::as_str),
    );
    name_the_path(&mut changes, keys);
    Ok(Plan {
        changes,
        write: Write::DogSection {
            toml: doc.to_string(),
            before: answered,
        },
        table: after,
        schema: None,
        touched: BTreeSet::new(),
    })
}

/// Every key the removal of `before` drops, as dotted paths under `prefix`.
///
/// A value with nothing in it to list, such as an empty table, still has to be
/// dropped: it is one change named `label` holding the value itself, so the
/// plan is never mistaken for a write with nothing to do.
fn removal_changes(before: Option<&Json>, prefix: &str, label: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    collect_changes(before, None, prefix, &mut changes);
    if changes.is_empty()
        && let Some(old) = before
    {
        changes.push(Change {
            key: label.to_string(),
            kind: ChangeKind::Removed,
            old: Some(old.clone()),
            new: None,
        });
    }
    changes
}

/// Removes the value at `keys` under `root` and hands it back, `None` when any
/// step is missing or is not a table.
fn remove_path(root: &mut Json, keys: &[String]) -> Option<Json> {
    let (last, parents) = keys.split_last()?;
    let mut node = root;
    for key in parents {
        node = node.as_object_mut()?.get_mut(key)?;
    }
    node.as_object_mut()?.remove(last)
}

/// Removes the item at `keys` under `table`, `None` when any step is missing
/// or is not a table.
///
/// The removal is one `remove`, so a comment on the line above a neighbouring
/// key stays where it is.
fn remove_toml(table: &mut dyn TableLike, keys: &[String]) -> Option<Item> {
    let (head, rest) = keys.split_first()?;
    if rest.is_empty() {
        return table.remove(head);
    }
    let child = table.get_mut(head)?.as_table_like_mut()?;
    remove_toml(child, rest)
}

#[cfg(test)]
mod tests;
