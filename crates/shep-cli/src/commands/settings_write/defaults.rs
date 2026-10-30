//! `shep set <dog>.defaults[.<key>] <value>`: the one table of values every
//! sheep carrying a table for the dog reads beneath its own.
//!
//! The shepherd holds the default and layers it at read time, so this is the
//! same write as a sheep's table with a different holder: read it, plan the
//! one key, check the schema against every sheep it reaches, send the whole
//! table.

use std::collections::BTreeMap;

use serde_json::Value as Json;
use shep_client::Client;
use shep_core::config::DogTable;
use shep_core::protocol::{Request, Response};

use super::flow::run;
use super::plan::plan_dog_defaults;
use super::schema::dog_schema;
use super::{Ask, Plan, WriteRefusal};
use crate::commands::copy;
use crate::commands::copy::address::{self, DEFAULTS, DefaultsAddress};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::settings_read;
use crate::exit::ExitCode;
use crate::lookout::probe_sheep_dogs::sheep_schema;
use crate::output::Streams;

/// One dog's defaults, or one key inside them.
pub(super) async fn set_defaults(
    client: &Client,
    streams: &mut Streams<'_>,
    defaults: &DefaultsAddress,
    ask: &Ask<'_>,
) -> ExitCode {
    let dog = defaults.dog.as_str();
    if let Err(code) = refuse_a_shadowed_address(client, streams, dog).await {
        return code;
    }
    let held = match read_default(client, streams, dog).await {
        Ok(held) => held,
        Err(code) => return code,
    };
    let keys = address::resolve_keys(&defaults.keys, Some(&held), None);
    // The per-sheep half of the dog's schema speaks for a table a sheep holds
    // for the dog, and the dog's own checks run on each of those tables, so
    // every sheep the default reaches is read.
    let schema = dog_schema(dog, ask.paths).and_then(|root| sheep_schema(&root));
    let sheep = match reached(client, streams, dog).await {
        Ok(sheep) => sheep,
        Err(code) => return code,
    };
    let plan = match plan_dog_defaults(dog, &keys, held, sheep, &ask.value) {
        Ok(plan) => Plan { schema, ..plan },
        Err(refusal) => return refusal.report(streams),
    };
    let path = keys
        .iter()
        .fold(DEFAULTS.to_string(), |path, key| format!("{path}.{key}"));
    run(client, streams, ask.paths, dog, &path, &plan, ask.flags).await
}

/// `dog`'s default as the shepherd holds it, an empty table for a dog with
/// none.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(super) async fn read_default(
    client: &Client,
    streams: &mut Streams<'_>,
    dog: &str,
) -> Result<Json, ExitCode> {
    match client
        .request(Request::DogDefaults {
            dog: dog.to_string(),
        })
        .await
    {
        Ok(Response::DogDefaults { table }) => Ok(Json::Object(
            table.map(DogTable::into_map).unwrap_or_default(),
        )),
        Ok(_other) => Err(unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// Every sheep carrying a table for `dog`, by its own table as it is stored,
/// which is what a default is layered under.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
async fn reached(
    client: &Client,
    streams: &mut Streams<'_>,
    dog: &str,
) -> Result<BTreeMap<String, DogTable>, ExitCode> {
    let names = match client
        .request(Request::DogSheepSettings {
            dog: dog.to_string(),
        })
        .await
    {
        Ok(Response::DogSheepSettings { tables }) => tables,
        Ok(_other) => return Err(unexpected_response(streams)),
        Err(err) => return Err(client_error(streams, &err)),
    };
    let mut own = BTreeMap::new();
    for name in names.into_keys() {
        let config = copy::read_config(client, streams, &name).await?;
        if let Some(table) = address::dog_table(&config, dog).and_then(Json::as_object) {
            own.insert(name, DogTable::from(table.clone()));
        }
    }
    Ok(own)
}

/// Refuses an address that the dog's own section would also answer.
///
/// A section holding a key spelled `defaults` is not shadowed by the
/// defaults address: `<dog>.defaults` would read as either, so it reads as
/// neither.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
async fn refuse_a_shadowed_address(
    client: &Client,
    streams: &mut Streams<'_>,
    dog: &str,
) -> Result<(), ExitCode> {
    let section = settings_read::read_dog_section(client, streams, dog).await?;
    let holds_the_key = section
        .parse::<toml_edit::DocumentMut>()
        .is_ok_and(|doc| doc.as_table().contains_key(DEFAULTS));
    if holds_the_key {
        return Err(WriteRefusal::Refused(format!(
            "nothing was written: `{dog}`'s own section holds a key named `{DEFAULTS}`, and \
             `{dog}.{DEFAULTS}` is where its defaults are addressed. Rename the key in \
             `dogs.toml`, and `shep set {dog}.{DEFAULTS}.<key> <value>` writes a default"
        ))
        .report(streams));
    }
    Ok(())
}
