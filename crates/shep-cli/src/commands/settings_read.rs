//! `shep get <name>[.<path>]`: one read for any settings in the flock.
//!
//! A sheep's name addresses its config and the `dogs.<dog>` tables inside
//! it; a dog's name addresses its own section of `dogs.toml`. A sheep is
//! looked for first, and anything no sheep is goes to `dogs.toml`, where the
//! dog is the longest run of segments the shepherd answers a section for.
//!
//! The answer is the TOML the address names, spelled as that file spells it,
//! so a dog's comments inside its section survive the read.

use std::fmt;

use serde_json::Value as Json;
use shep_client::Client;
use shep_core::protocol::{Request, Response};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value as TomlValue};

use crate::cli::{Format, GetArgs};
use crate::commands::copy::address::FlockNames;
use crate::commands::copy::{self, address};
use crate::commands::rpc::{typed_request_error, unexpected_response};
use crate::exit::ExitCode;
use crate::output::{SettingsAnswer, Streams, emit, write_outcome};

/// The one path root that is not an `AppConfig` field name.
const DOGS: &str = "dogs";

/// The one field whose values never reach a client.
const ENV: &str = "env";

/// Which half of the flock an address named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Holder {
    /// A sheep: its config, and the `dogs.<dog>` tables inside it.
    Sheep,
    /// A dog: its own section of `dogs.toml`.
    Dog,
}

impl Holder {
    /// `sheep` or `dog`, the word `shep get`'s envelope and `shep set`'s both
    /// carry for the half of the flock an address named.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Sheep => "sheep",
            Self::Dog => "dog",
        }
    }
}

/// An address split into the name it named and the segments after it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Written {
    holder: Holder,
    name: String,
    /// The segments past the name, in order, empty for a read of everything
    /// that name holds.
    rest: Vec<String>,
}

/// Why a read printed nothing.
///
/// Every variant carries the sentence the operator reads, because the reason
/// is the remedy: an unknown name names `shep kv`, `env` names `shep import
/// env`, and an empty path names the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadRefusal {
    /// No sheep and no dog in this flock answers that name.
    Unknown(String),
    /// The address is malformed, or the path is no field and no dog table.
    Usage(String),
    /// The holder is known and nothing is set at the path named.
    Empty(String),
    /// A key this verb never reads.
    Refused(String),
}

impl ReadRefusal {
    /// The sentence the operator reads.
    fn message(&self) -> &str {
        match self {
            Self::Unknown(message)
            | Self::Usage(message)
            | Self::Empty(message)
            | Self::Refused(message) => message,
        }
    }

    /// The code that reports it.
    ///
    /// [`ExitCode::NotFound`] for a name the flock does not have and for a
    /// path nothing is set at: both are "no such setting", which is the code
    /// `shep kv get` gives a key it does not have.
    const fn code(&self) -> ExitCode {
        match self {
            Self::Unknown(_) | Self::Empty(_) => ExitCode::NotFound,
            Self::Usage(_) => ExitCode::Usage,
            Self::Refused(_) => ExitCode::InvalidConfig,
        }
    }

    /// Writes the refusal and hands back the code it wrote.
    ///
    /// `pub(crate)`: `shep set` splits an address with this verb's own
    /// [`segments`] and refuses what this verb refuses (issue #1096).
    pub(crate) fn report(&self, streams: &mut Streams<'_>) -> ExitCode {
        streams.fail(self.code(), self.message())
    }
}

impl fmt::Display for ReadRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl core::error::Error for ReadRefusal {}

/// `shep get <name>[.<path>]`.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: a name no sheep or dog
/// answers, a malformed address, a key this verb never reads, a path nothing
/// is set at, or a shepherd that refused the read.
pub(crate) async fn get(client: &Client, streams: &mut Streams<'_>, args: &GetArgs) -> ExitCode {
    let flock = match copy::flock_names(client, streams).await {
        Ok(names) => names,
        Err(code) => return code,
    };
    let segments = match segments(&args.address) {
        Ok(segments) => segments,
        Err(refusal) => return refusal_report(streams, &refusal),
    };
    if let Some(name) = address::longest_in(&segments, &flock.sheep) {
        let depth = name.split('.').count();
        let written = Written {
            holder: Holder::Sheep,
            name,
            rest: segments[depth..].to_vec(),
        };
        return get_sheep(client, streams, &flock, written).await;
    }
    get_dog(client, streams, &flock, &segments).await
}

/// The address split on `.`.
///
/// `pub(crate)`: `shep set` takes the same address (issue #1096), and one
/// splitter is what keeps `shep get api.dogs.kelpie` and `shep set
/// api.dogs.kelpie.git` naming the same thing.
///
/// # Errors
/// [`ReadRefusal::Usage`] for an address with an empty part.
pub(crate) fn segments(raw: &str) -> Result<Vec<String>, ReadRefusal> {
    let parts: Vec<String> = raw.split('.').map(str::to_string).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(ReadRefusal::Usage(format!(
            "`{raw}` has an empty part: a name and a path segment are each at least one character"
        )));
    }
    Ok(parts)
}

/// Reports a refusal, named separately from the refusals themselves so no
/// arm of `get` has to know which variant it holds.
fn refusal_report(streams: &mut Streams<'_>, refusal: &ReadRefusal) -> ExitCode {
    refusal.report(streams)
}

/// One sheep's config, or the fragment of it the address named.
async fn get_sheep(
    client: &Client,
    streams: &mut Streams<'_>,
    flock: &FlockNames,
    written: Written,
) -> ExitCode {
    let config = match copy::read_config(client, streams, &written.name).await {
        Ok(config) => config,
        Err(code) => return code,
    };
    // The target config is `Null`: a read resolves a dotted dog name against
    // the one sheep it names, where a copy resolves against two.
    let dogs = address::DogNames::new(flock, &config, &Json::Null);
    let steps = match sheep_steps(&written.name, &written.rest, &dogs, &config) {
        Ok(steps) => steps,
        Err(refusal) => return refusal_report(streams, &refusal),
    };
    let toml = match sheep_document(&written, &config, &steps) {
        Ok(toml) => toml,
        Err(refusal) => return refusal_report(streams, &refusal),
    };
    print(streams, &written, &toml)
}

/// One dog's own section, or the fragment of it the address named.
///
/// The name is the longest run of segments the shepherd answers a section for,
/// which is what tells `kelpie` and `kelpie.worker` apart.
///
/// The section is read as a document and re-rooted, which keeps the comments
/// the operator wrote inside it.
async fn get_dog(
    client: &Client,
    streams: &mut Streams<'_>,
    flock: &FlockNames,
    segments: &[String],
) -> ExitCode {
    // A dog in the flock with nothing written for it is a different answer
    // from a name nothing in the product knows: the first says the settings
    // are all defaults, the second says the name is wrong. The name kept is
    // the candidate the flock carries, so the sentence names the dog it found.
    let mut named: Option<String> = None;
    for name in dog_candidates(segments) {
        let section = match read_dog_section(client, streams, &name).await {
            Ok(section) => section,
            Err(code) => return code,
        };
        if named.is_none() && flock.dogs.iter().any(|dog| *dog == name) {
            named = Some(name.clone());
        }
        if section.trim().is_empty() {
            continue;
        }
        let Ok(doc) = section.parse::<DocumentMut>() else {
            return streams.fail(
                ExitCode::InvalidConfig,
                &format!(
                    "`{name}`'s section of `dogs.toml` does not parse: the shepherd read it, so \
                     this binary cannot say what it holds",
                ),
            );
        };
        let depth = name.split('.').count();
        let written = Written {
            holder: Holder::Dog,
            name,
            rest: segments[depth..].to_vec(),
        };
        let steps = dog_steps(doc.as_table(), &written.rest);
        return match dog_document(&written, &doc, &steps) {
            Ok(toml) => print(streams, &written, &toml),
            Err(refusal) => refusal_report(streams, &refusal),
        };
    }
    let refusal = match named {
        Some(dog) => ReadRefusal::Empty(all_defaults(&dog, segments)),
        None => ReadRefusal::Unknown(no_such_name(segments)),
    };
    refusal_report(streams, &refusal)
}

/// Every name the address could be, longest first.
///
/// `pub(crate)`: `shep set` resolves a dog's name the same way (issue #1096).
///
/// `kelpie.worker.token` offers `kelpie.worker.token`, `kelpie.worker` and
/// `kelpie`, in that order: the longest one `dogs.toml` holds a section for is
/// the dog, and what is left over is its path.
pub(crate) fn dog_candidates(segments: &[String]) -> Vec<String> {
    (1..=segments.len())
        .rev()
        .map(|len| segments[..len].join("."))
        .collect()
}

/// The sentence for a name no sheep in the flock is and no dog has a section
/// for, which is where the key/value store `shep get` used to read went.
fn no_such_name(segments: &[String]) -> String {
    format!(
        "no sheep or dog named `{}`: `shep flock` lists the flock and `shep dogs` lists the dogs. \
         The key/value store `shep get` used to read lives at `shep kv get`",
        segments.join(".")
    )
}

/// The sentence for a dog the flock has and `dogs.toml` says nothing about.
///
/// `dog` is the name the flock carries and `address` is what the operator
/// typed, kept apart on purpose: `kelpie.worker.token` names a path inside
/// `kelpie`.
fn all_defaults(dog: &str, address: &[String]) -> String {
    let typed = address.join(".");
    let path = if typed == dog {
        String::new()
    } else {
        format!(" `{typed}` names nothing inside it either.")
    };
    format!(
        "`{dog}` is a dog with nothing set for it: `dogs.toml` holds no section, so every setting \
         it runs on is a default.{path} `shep enable {dog}` writes one",
    )
}

/// The TOML `shep get` prints for a dog: its whole section re-rooted under
/// the dog's name, or the fragment of it `steps` names.
///
/// # Errors
/// [`ReadRefusal::Empty`] when `steps` leads nowhere.
fn dog_document(
    written: &Written,
    doc: &DocumentMut,
    steps: &[String],
) -> Result<String, ReadRefusal> {
    let leaf = if steps.is_empty() {
        let mut root = doc.as_table().clone();
        // The header this document is about to grow is the dog's own, and a
        // root table never prints one: `implicit` is what keeps `[kelpie]`
        // off the page.
        root.set_implicit(false);
        clear_decor(Item::Table(root))
    } else {
        match walk_toml(doc.as_table(), steps) {
            Some(item) => clear_decor(item),
            None => return Err(ReadRefusal::Empty(empty(written))),
        }
    };
    let mut out = DocumentMut::new();
    let mut spine = vec![written.name.clone()];
    spine.extend(steps.iter().cloned());
    attach(&mut out, &spine, leaf);
    Ok(out.to_string())
}

/// The key path inside `config` that the segments after a sheep's name name.
///
/// An empty `rest` is the whole config, and a bare `dogs` is every dog table
/// in it: unlike `shep copy`, this verb has nothing to write through, so a
/// map it can print whole is not a refusal.
///
/// # Errors
/// [`ReadRefusal::Usage`] for a multi-segment path that is not a dog's table,
/// or a name no field of a sheep's config is. [`ReadRefusal::Refused`] for
/// `env`.
fn sheep_steps(
    sheep: &str,
    rest: &[String],
    dogs: &address::DogNames,
    config: &Json,
) -> Result<Vec<String>, ReadRefusal> {
    let Some(first) = rest.first() else {
        return Ok(Vec::new());
    };

    if *first == DOGS {
        let mut steps = vec![DOGS.to_string()];
        if rest.len() == 1 {
            return Ok(steps);
        }
        let tail = &rest[1..];
        let dog = dogs.longest(tail);
        let depth = dog.split('.').count();
        steps.push(dog.clone());
        steps.extend(address::resolve_keys(
            &tail[depth..],
            address::dog_table(config, &dog),
            None,
        ));
        return Ok(steps);
    }

    if rest.len() > 1 {
        return Err(ReadRefusal::Usage(format!(
            "a path is one field, `{DOGS}.<dog>`, or `{DOGS}.<dog>.<key>`; `{sheep}.{}` is none \
             of those",
            rest.join(".")
        )));
    }
    if *first == ENV {
        return Err(ReadRefusal::Refused(format!(
            "`env` is not read: a client is never sent a sheep's env values, so there is nothing \
             here to print. `shep describe {sheep}` lists the keys, and `shep import env` writes \
             them"
        )));
    }
    if !address::app_fields().any(|name| name == first.as_str()) {
        return Err(ReadRefusal::Usage(format!(
            "`{first}` is not a field a sheep's config has; `shep describe {sheep}` shows the \
             config `shep get` reads"
        )));
    }
    Ok(vec![first.clone()])
}

/// The key path inside a dog's section that the segments after its name name.
///
/// Each step takes the longest run of segments one of the tables actually
/// holds as one key, the rule [`address::resolve_keys`] applies to a sheep's dog table.
///
/// `shep set` writes at the path this names, so the key it changes is the
/// key `shep get` printed.
pub(crate) fn dog_steps(root: &Table, rest: &[String]) -> Vec<String> {
    let mut steps = Vec::new();
    let mut current: Option<&dyn toml_edit::TableLike> = Some(root);
    let mut next = 0;
    while next < rest.len() {
        let taken = longest_toml_key(&rest[next..], current);
        // One lookup for the key `longest_toml_key` found, not one per segment
        // of it. A section can hold a key spelled with a dot in it
        // (`[kelpie."api.token"]`), and looking for `api` then `token` loses
        // the table that key names.
        let key = taken.join(".");
        steps.push(key.clone());
        current = current
            .and_then(|table| table.get(key.as_str()))
            .and_then(Item::as_table_like);
        next += taken.len();
    }
    steps
}

/// The longest run of `rest` `table` holds as one key, or one segment.
fn longest_toml_key(rest: &[String], table: Option<&dyn toml_edit::TableLike>) -> Vec<String> {
    for len in (1..=rest.len()).rev() {
        let holds = table.is_some_and(|table| table.contains_key(rest[..len].join(".").as_str()));
        if holds {
            return rest[..len].to_vec();
        }
    }
    rest[..1].to_vec()
}

/// The item at `steps` inside `root`, cloned out of the document it came from.
fn walk_toml(root: &Table, steps: &[String]) -> Option<Item> {
    let mut current = root.get(steps.first()?.as_str())?;
    for step in &steps[1..] {
        current = current.as_table_like()?.get(step.as_str())?;
    }
    if current.is_none() {
        return None;
    }
    Some(current.clone())
}

/// Drops the blank line and the comment a table's header line carries, which
/// belong to the text around the fragment rather than to the fragment.
///
/// Comments inside the table are on its own items and stay.
fn clear_decor(item: Item) -> Item {
    match item {
        Item::Table(mut table) => {
            *table.decor_mut() = toml_edit::Decor::default();
            Item::Table(table)
        }
        other => other,
    }
}

/// The TOML document `shep get` prints for a sheep.
///
/// `steps` empty is the whole config, with `env` left out: the shepherd
/// answers a config with `env` already emptied, so printing its header would
/// advertise a table this verb can never fill.
///
/// # Errors
/// [`ReadRefusal::Empty`] when nothing is set at `steps`.
fn sheep_document(
    written: &Written,
    config: &Json,
    steps: &[String],
) -> Result<String, ReadRefusal> {
    let mut out = DocumentMut::new();
    if steps.is_empty() {
        for (key, value) in config.as_object().into_iter().flatten() {
            if key == ENV {
                continue;
            }
            if let Some(item) = json_item(value) {
                let item = if *key == DOGS {
                    dogs_container(item)
                } else {
                    item
                };
                out.as_table_mut().insert(key.as_str(), item);
            }
        }
        return Ok(out.to_string());
    }

    let mut current = config;
    for step in steps {
        let Some(next) = current.get(step.as_str()) else {
            return Err(ReadRefusal::Empty(empty(written)));
        };
        current = next;
    }
    let Some(item) = json_item(current) else {
        return Err(ReadRefusal::Empty(empty(written)));
    };
    // `shep get <sheep>.dogs` names the container itself, which is the one
    // table here that prints no header of its own.
    let item = if steps.len() == 1 && steps[0] == DOGS {
        dogs_container(item)
    } else {
        item
    };
    let mut spine = vec![written.name.clone()];
    spine.extend(steps.iter().cloned());
    // The sheep's own name is not part of the document a Flockfile holds:
    // `[dogs.kelpie]` is what the file spells, not `[api.dogs.kelpie]`.
    spine.remove(0);
    attach(&mut out, &spine, item);
    Ok(out.to_string())
}

/// The refusal sentence for a path nothing is set at.
fn empty(written: &Written) -> String {
    format!(
        "nothing is set at `{}.{}`: the field is unset, or the key is not in the table",
        written.name,
        written.rest.join(".")
    )
}

/// Writes `leaf` at the end of `spine`, creating the tables above it.
///
/// Every table this creates is implicit: it exists to hold what is under it,
/// and a header for it would name a table the operator never asked for.
///
/// # Panics
/// If `spine` is empty, which no caller here builds.
#[track_caller]
fn attach(doc: &mut DocumentMut, spine: &[String], leaf: Item) {
    let (last, parents) = spine
        .split_last()
        .expect("a spine always has its leaf's key");
    let mut table = doc.as_table_mut();
    for step in parents {
        let entry = table.entry(step).or_insert(Item::Table(Table::new()));
        let step_table = entry
            .as_table_mut()
            .expect("a spine step this function created is a table");
        step_table.set_implicit(true);
        table = step_table;
    }
    table.insert(last, leaf);
}

/// The sheep's own `dogs` container, the one table of a sheep's config that
/// prints no header of its own.
///
/// A Flockfile spells `[app.dogs.kelpie]` and never `[app.dogs]`, and the
/// shepherd answers an empty `dogs` for a sheep with no per-sheep dog settings,
/// which is the same config as one whose Flockfile says nothing about dogs. A
/// table inside it is the operator's own and keeps its header.
fn dogs_container(item: Item) -> Item {
    match item {
        Item::Table(mut table) => {
            table.set_implicit(true);
            Item::Table(table)
        }
        other => other,
    }
}

/// One `serde_json` value as a table entry.
///
/// A `Null` is no entry at all: TOML has no null, and an unset `Option`
/// field of a sheep's config is a field the operator never wrote.
fn json_item(value: &Json) -> Option<Item> {
    Some(match value {
        Json::Object(map) => {
            let mut table = Table::new();
            for (key, entry) in map {
                if let Some(item) = json_item(entry) {
                    table.insert(key.as_str(), item);
                }
            }
            // A map with no keys at all is a table the operator wrote a header
            // for and left empty: `[app.dogs.kelpie.options]` with no entries.
            // A header is the only way to say it is there, and a dog's settings
            // are opaque to shep, so an empty table is not a missing setting.
            let a_step_down = !map.is_empty() && table.iter().all(|(_, item)| item.is_table());
            table.set_implicit(a_step_down);
            Item::Table(table)
        }
        other => Item::Value(json_value(other)?),
    })
}

/// One `serde_json` value as a TOML value, for inside an array or an inline
/// table, where a nested map has no room to become a table of its own.
fn json_value(value: &Json) -> Option<TomlValue> {
    Some(match value {
        Json::Null => return None,
        Json::Bool(flag) => TomlValue::from(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(integer) => TomlValue::from(integer),
            None => TomlValue::from(number.as_f64()?),
        },
        Json::String(text) => TomlValue::from(text.as_str()),
        Json::Array(list) => {
            let mut array = Array::new();
            for element in list {
                array.push(json_value(element)?);
            }
            TomlValue::Array(array)
        }
        Json::Object(map) => {
            let mut inline = InlineTable::new();
            for (key, entry) in map {
                inline.insert(key.as_str(), json_value(entry)?);
            }
            TomlValue::InlineTable(inline)
        }
    })
}

/// Prints the answer: the TOML text itself, or the one envelope `--format json`
/// owes.
fn print(streams: &mut Streams<'_>, written: &Written, toml: &str) -> ExitCode {
    if streams.fmt == Format::Table {
        return write_outcome(streams.out.write_all(toml.as_bytes()));
    }
    write_outcome(emit(
        &mut *streams.out,
        Format::Json,
        "get",
        SettingsAnswer {
            kind: written.holder.as_str(),
            name: written.name.clone(),
            path: written.rest.join("."),
            toml: toml.to_string(),
        },
        streams.style,
    ))
}

/// One dog's section as the shepherd holds it, `{{secret:...}}` references
/// still references.
///
/// `pub(crate)`: `shep set` reads the same section before it writes it back
/// (issue #1096), and the unresolved read is what keeps a `{{secret:...}}`
/// reference out of the text a write would send.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(crate) async fn read_dog_section(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
) -> Result<String, ExitCode> {
    match client
        .request(Request::DogConfig {
            name: name.to_string(),
        })
        .await
    {
        Ok(Response::DogSection { toml }) => Ok(toml.as_str().to_string()),
        Ok(_other) => Err(unexpected_response(streams)),
        Err(err) => Err(typed_request_error(streams, &err)),
    }
}

#[cfg(test)]
mod tests;
