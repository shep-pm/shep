//! `shep set <address> <value>` and `shep unset <address>`: one write for any
//! settings in the flock, and the undo for it.
//!
//! The address is `copy`'s and `get`'s, and each kind is written through the
//! request the shepherd already answers: [`Request::SetSheepField`],
//! [`Request::SetSheepDogSettings`] or [`Request::SetDogConfig`], so
//! everything those refuse this refuses.
//!
//! The order is `copy`'s: resolve, read, plan, print the change list, ask,
//! write. Nothing is sent until the operator has seen every key it touches.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Map, Value as Json};
use shep_client::Client;
use shep_core::config::DogTable;
use shep_core::paths::ShepPaths;
#[cfg(doc)]
use shep_core::protocol::Request;
use toml_edit::DocumentMut;

use self::plan::{field_changes, plan_dog_table, plan_section};
use self::schema::{dog_schema, known_dogs};
use self::value::value_of;
use crate::cli::SetArgs;
use crate::commands::copy;
use crate::commands::copy::CopyRefusal;
use crate::commands::copy::address::{self, ConfigPath, FlockNames};
use crate::commands::copy::plan::Change;
use crate::commands::settings_read;
use crate::exit::ExitCode;
use crate::lookout::probe_sheep_dogs::sheep_schema;
use crate::output::Streams;

mod edit;
mod flow;
#[cfg(test)]
mod flow_stale_tests;
mod plan;
mod schema;
mod unset;
#[cfg(test)]
mod unset_plan_tests;
pub(crate) mod value;

use self::flow::run;
pub(crate) use self::unset::unset;

/// Which of the two verbs this run is, for the words it prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `shep set`.
    Set,
    /// `shep unset`.
    Unset,
}

impl Verb {
    /// The verb as typed, which is also the `command` its envelope carries.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Unset => "unset",
        }
    }
}

/// The verb and the two flags a run was given.
#[derive(Debug, Clone, Copy)]
struct Flags {
    verb: Verb,
    /// `--yes`: write without asking.
    yes: bool,
    /// `--dry-run`: print the changes and write nothing.
    dry_run: bool,
}

/// Why a write stopped before it wrote anything.
///
/// Every variant carries the sentence the operator reads, because the reason
/// is the remedy: an unknown name names `shep kv set`, `env` names `shep
/// import env`, and a schema refusal names the key path and the rule it
/// breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteRefusal {
    /// An address or a value is malformed for the path it was aimed at.
    Usage(String),
    /// No sheep and no dog in this flock answers that name.
    Unknown(String),
    /// A key this verb never writes, in the reason the shepherd gives for it.
    Refused(String),
    /// The dog's own schema turned the value down.
    Invalid(String),
    /// The dog itself refused the write, in its own words.
    ByDog(String),
    /// The holder moved between the read the plan was built from and the write.
    Stale(String),
}

impl WriteRefusal {
    /// The sentence the operator reads.
    fn message(&self) -> &str {
        match self {
            Self::Usage(message)
            | Self::Unknown(message)
            | Self::Refused(message)
            | Self::Invalid(message)
            | Self::ByDog(message)
            | Self::Stale(message) => message,
        }
    }

    /// The code that reports it.
    ///
    /// [`ExitCode::InvalidConfig`] for a refused key and for a schema refusal
    /// is the code the shepherd's own refusal of the same write carries.
    ///
    /// A holder that moved is [`ExitCode::Failure`]: nothing is invalid, only
    /// that the two are no longer the pair this plan described.
    const fn code(&self) -> ExitCode {
        match self {
            Self::Usage(_) => ExitCode::Usage,
            Self::Unknown(_) => ExitCode::NotFound,
            Self::Refused(_) | Self::Invalid(_) | Self::ByDog(_) => ExitCode::InvalidConfig,
            Self::Stale(_) => ExitCode::Failure,
        }
    }

    /// Writes the refusal and hands back the code it wrote.
    fn report(&self, streams: &mut Streams<'_>) -> ExitCode {
        streams.fail(self.code(), self.message())
    }
}

impl fmt::Display for WriteRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl core::error::Error for WriteRefusal {}

/// The request a plan turns into.
///
/// One per address shape, which is one per request the shepherd already
/// answers. A dog's section is the whole text rather than one key:
/// [`Request::SetDogConfig`] replaces the section, and the text is the one
/// this verb read, edited in place, so the comments the operator wrote inside
/// it survive the write.
#[derive(Debug, Clone)]
pub(super) enum Write {
    /// One `AppConfig` field, through [`Request::SetSheepField`].
    SheepField {
        /// The field name.
        key: String,
        /// The new value.
        value: Json,
    },
    /// One dog's whole table on one sheep, through
    /// [`Request::SetSheepDogSettings`].
    ///
    /// The whole table and not the one key: that request replaces a dog's
    /// table, so the merged result is what carries the keys this write left
    /// alone.
    SheepDogTable {
        /// The dog's name.
        dog: String,
        /// The table the sheep holds afterwards.
        table: DogTable,
        /// The table the sheep held when this plan was built, which is what
        /// [`flow::holder_unchanged`] checks the sheep still holds.
        before: Json,
    },
    /// One dog's whole table on one sheep, removed through
    /// [`Request::SetSheepDogSettings`] with no table.
    DropSheepDogTable {
        /// The dog's name.
        dog: String,
        /// The table the sheep held when this plan was built, which is what
        /// [`flow::holder_unchanged`] checks the sheep still holds.
        before: Json,
    },
    /// One `AppConfig` field's override, dropped through
    /// [`Request::ResetSheepField`].
    ResetSheepField {
        /// The field name.
        key: String,
    },
    /// One dog's whole section, through [`Request::SetDogConfig`].
    DogSection {
        /// The section text, edited in place from the text the shepherd
        /// answered.
        toml: String,
        /// The text the shepherd answered, which is what [`flow::holder_unchanged`]
        /// checks it still answers.
        before: String,
    },
}

/// What one `shep set` would write: the keys it changes, the one request that
/// writes them, and what the value has to pass before any of it is sent.
#[derive(Debug)]
pub(super) struct Plan {
    /// One entry per key the write touches, in dotted-key order.
    changes: Vec<Change>,
    write: Write,
    /// The table the write leaves behind, as JSON: the dog's table on the
    /// sheep, or the whole section. Empty for a sheep's own field, which is
    /// one value and not a table, and for a dog's table removed whole, which
    /// leaves nothing.
    table: Map<String, Json>,
    /// The schema `table` is checked against, `None` when no dog published one
    /// for it.
    schema: Option<Json>,
    /// The property paths this run chose, under `table`. Empty says the write
    /// replaces the table whole, so every key in it is in play.
    touched: BTreeSet<Vec<String>>,
}

/// What one run carries to whichever holder the address names: the operator's
/// address and value, and the flock and the files the address was resolved
/// against.
pub(super) struct Ask<'a> {
    /// The flags the operator typed: `--yes`, `--dry-run`.
    flags: Flags,
    /// The value, already read as TOML.
    value: Json,
    /// The names the running flock carries.
    flock: &'a FlockNames,
    /// `$SHEP_HOME`, for a dog's schema and `shep.toml`'s dog list.
    paths: &'a ShepPaths,
}

/// `shep set <address> <value>`.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: a malformed address, a name
/// no sheep or dog answers, a key this verb never writes, a value the dog's
/// schema turns down, a refusal to write with no terminal to ask on, or a
/// write the shepherd refused.
pub(crate) async fn set(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &SetArgs,
) -> ExitCode {
    let value = match value_of(&args.value) {
        Ok(value) => value,
        Err(refusal) => return refusal.report(streams),
    };
    let flock = match copy::flock_names(client, streams).await {
        Ok(names) => names,
        Err(code) => return code,
    };
    let segments = match settings_read::segments(&args.address) {
        Ok(segments) => segments,
        Err(refusal) => return refusal.report(streams),
    };
    let ask = Ask {
        flags: Flags {
            verb: Verb::Set,
            yes: args.yes,
            dry_run: args.dry_run,
        },
        value,
        flock: &flock,
        paths,
    };
    if let Some(name) = address::longest_in(&segments, &flock.sheep) {
        return set_sheep(client, streams, &name, &segments, &ask).await;
    }
    set_dog(client, streams, &segments, &ask).await
}

/// One sheep's field, or one key inside one of its dog tables.
async fn set_sheep(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
    segments: &[String],
    ask: &Ask<'_>,
) -> ExitCode {
    let config = match copy::read_config(client, streams, name).await {
        Ok(config) => config,
        Err(code) => return code,
    };
    let path = match path_in(name, segments, ask.flock, &config) {
        Ok(path) => path,
        Err(refusal) => return refusal.report(streams),
    };
    let plan = match &path {
        ConfigPath::Field { name: field } => Plan {
            changes: field_changes(field, &config, &ask.value),
            write: Write::SheepField {
                key: field.clone(),
                value: ask.value.clone(),
            },
            table: Map::new(),
            schema: None,
            touched: BTreeSet::new(),
        },
        ConfigPath::Env => {
            return CopyRefusal::Refused(address::ENV_NOT_WRITTEN.to_string()).report(streams);
        }
        ConfigPath::Dog { dog, keys } => {
            let plan = match plan_dog_table(name, dog, keys, &config, &ask.value) {
                Ok(plan) => plan,
                Err(refusal) => return refusal.report(streams),
            };
            // The per-sheep half of the dog's schema, the same one lookout
            // builds its rows from, and the only schema that speaks for a
            // sheep's table for a dog.
            let schema = dog_schema(dog, ask.paths).and_then(|root| sheep_schema(&root));
            Plan { schema, ..plan }
        }
    };
    run(
        client,
        streams,
        ask.paths,
        name,
        &path.dotted(),
        &plan,
        ask.flags,
    )
    .await
}

/// Where in sheep `name`'s settings the address points: one of its fields, or
/// a key inside one of its dog tables.
///
/// # Errors
/// The refusal [`address::path_of`] gives for a path no write reaches.
fn path_in(
    name: &str,
    segments: &[String],
    flock: &FlockNames,
    config: &Json,
) -> Result<ConfigPath, CopyRefusal> {
    let depth = name.split('.').count();
    let written = address::Written {
        sheep: name.to_string(),
        segments: segments[depth..].to_vec(),
    };
    // The same sheep on both sides of `path_of`: `copy` resolves a dotted dog
    // name against the two tables it is copying between, and this verb has the
    // one it is writing.
    let dogs = address::DogNames::new(flock, config, &Json::Null);
    address::path_of(&written, &dogs, config, config)
}

/// The dog an address names, and the section of `dogs.toml` it holds now.
pub(super) struct DogTarget {
    /// The dog's name, dotted where the dog is.
    name: String,
    /// The section text the shepherd answered, `None` for a dog the flock runs
    /// with no section yet.
    section: Option<String>,
}

impl DogTarget {
    /// The section as a document to edit in place, empty for a dog with no
    /// section.
    ///
    /// # Errors
    /// The [`ExitCode`] already reported to `streams`, for text that is not
    /// TOML.
    fn document(&self, streams: &mut Streams<'_>) -> Result<DocumentMut, ExitCode> {
        let Some(section) = &self.section else {
            return Ok(DocumentMut::new());
        };
        section
            .parse::<DocumentMut>()
            .map_err(|_| streams.fail(ExitCode::InvalidConfig, &unparsable(&self.name)))
    }

    /// The segments of the address after the dog's name.
    fn rest<'a>(&self, segments: &'a [String]) -> &'a [String] {
        &segments[self.name.split('.').count()..]
    }
}

/// One dog's own section of `dogs.toml`.
async fn set_dog(
    client: &Client,
    streams: &mut Streams<'_>,
    segments: &[String],
    ask: &Ask<'_>,
) -> ExitCode {
    let target = match resolve_dog(client, streams, segments, ask.flock, ask.paths).await {
        Ok(target) => target,
        Err(code) => return code,
    };
    let doc = match target.document(streams) {
        Ok(doc) => doc,
        Err(code) => return code,
    };
    let rest = target.rest(segments);
    let keys = settings_read::dog_steps(doc.as_table(), rest);
    match plan_section(doc, &target.name, &keys, &ask.value) {
        Ok(plan) => {
            let plan = Plan {
                schema: dog_schema(&target.name, ask.paths),
                ..plan
            };
            run(
                client,
                streams,
                ask.paths,
                &target.name,
                &rest.join("."),
                &plan,
                ask.flags,
            )
            .await
        }
        Err(refusal) => refusal.report(streams),
    }
}

/// The dog an address names.
///
/// The name is the longest run of segments the shepherd answers a section for,
/// which is what tells `kelpie` and `kelpie.worker` apart, as [`shep get`](settings_read::get) applies it.
///
/// A dog the flock runs with no section yet is the one case with nothing to
/// read, and a write creates it.
///
/// A section is not what makes a dog known: the candidate has to be a name the
/// daemon will take a `Request::SetDogConfig` for.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(super) async fn resolve_dog(
    client: &Client,
    streams: &mut Streams<'_>,
    segments: &[String],
    flock: &FlockNames,
    paths: &ShepPaths,
) -> Result<DogTarget, ExitCode> {
    let mut named: Option<String> = None;
    let mut sectioned: Option<String> = None;
    let known = known_dogs(paths);
    for name in settings_read::dog_candidates(segments) {
        let section = settings_read::read_dog_section(client, streams, &name).await?;
        let holds = flock.dogs.contains(&name) || known.contains(&name);
        if holds && named.is_none() {
            named = Some(name.clone());
        }
        if section.trim().is_empty() {
            continue;
        }
        sectioned = Some(name.clone());
        if !holds {
            // A shorter candidate may still be a dog this shepherd knows: a dog
            // adopted under the dotted name `kelpie.worker` is known while
            // `kelpie` is not.
            continue;
        }
        return Ok(DogTarget {
            name,
            section: Some(section),
        });
    }
    match (named, sectioned) {
        (Some(name), _) => Ok(DogTarget {
            name,
            section: None,
        }),
        (None, Some(name)) => Err(WriteRefusal::Unknown(no_known_dog(&name)).report(streams)),
        (None, None) => Err(WriteRefusal::Unknown(no_such_name(segments)).report(streams)),
    }
}

/// The sentence for a value that parses as TOML and has no place in the JSON the
/// shepherd carries.
///
/// `path` is where inside the value it sits, empty for the value itself.
pub(super) fn not_a_number(path: &str) -> String {
    let under = if path.is_empty() {
        String::new()
    } else {
        format!(" under `{path}`")
    };
    format!(
        "not a value `shep set` writes: TOML's `nan` and `inf`{under} have no place in the JSON \
         the shepherd carries, and the flock would be sent a null. Quote the text (`\"nan\"`) to \
         write it as a string"
    )
}
/// The sentence for a name no sheep is and no dog has a section for, which is
/// where the key/value store `shep set` used to write went.
pub(super) fn no_such_name(segments: &[String]) -> String {
    format!(
        "no sheep or dog named `{}`: `shep flock` lists the flock and `shep dogs` lists the dogs. \
         The key/value store `shep set` used to write lives at `shep kv set`",
        segments.join(".")
    )
}

/// The sentence for a `dogs.toml` section whose name this shepherd knows no dog
/// by.
///
/// `shep disable` and a hand edit both leave a section behind, and the daemon
/// guards [`Request::SetDogConfig`] on its own known set for the same reason
/// this refusal exists: writing the section of a dog nothing runs configures
/// nothing. The remedy is the daemon's own, so the sentence is too.
pub(super) fn no_known_dog(name: &str) -> String {
    format!(
        "nothing was written: `dogs.toml` has a `[{name}]` section, but no dog this shepherd knows \
         is named `{name}`. `shep adopt` or `shep enable` makes one known, and `shep dogs` lists \
         what is"
    )
}

/// The sentence for a `dogs.toml` section this verb cannot read as TOML.
///
/// The file is named rather than the settings, because the file is the thing
/// that is broken: the same text [`settings_read::get`] prints is what does not
/// parse, and `shep get` is how the operator sees it.
pub(super) fn unparsable(name: &str) -> String {
    format!(
        "`{name}`'s section of `dogs.toml` does not parse: nothing was written, and `shep get \
         {name}` reads the same text"
    )
}
#[cfg(test)]
mod tests;
#[cfg(test)]
mod value_tests;
