//! `shep get <sheep>.dogs.<dog>` for a dog that follows the dog settings
//! standard: the table the dog reads, with the keys the sheep leaves to the
//! dog's section marked as inherited.

use std::collections::BTreeSet;

use serde_json::{Map, Value as Json};
use shep_client::Client;
use shep_core::dogs::manifest::Serves;
use shep_core::paths::ShepPaths;
use toml_edit::{DocumentMut, Item};

use crate::commands::copy::address;
use crate::commands::settings_write::dog_standard;
use crate::commands::settings_write::value::table_json;
use crate::dog_settings::standard::Standard;
use crate::exit::ExitCode;
use crate::output::Streams;

use super::{DOGS, read_dog_section};

/// A sheep's config with one dog's table read as the dog reads it.
#[derive(Debug)]
pub(super) struct Layering {
    /// The config, its `dogs.<dog>` replaced by the layered table.
    pub(super) config: Json,
    /// The top-level keys of that table the sheep's own table does not set.
    pub(super) inherited: BTreeSet<String>,
}

/// The layering for `dog` on the sheep whose config is `config`, or
/// [`None`] when `dog` does not follow the standard or does not serve the
/// sheep.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(super) async fn layer(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    dog: &str,
    config: &Json,
) -> Result<Option<Layering>, ExitCode> {
    let Some((_, standard)) = dog_standard(dog, paths) else {
        return Ok(None);
    };
    if !serves(&standard, config, dog) {
        return Ok(None);
    }
    let text = read_dog_section(client, streams, dog).await?;
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return Err(streams.fail(
            ExitCode::InvalidConfig,
            &format!(
                "`{dog}`'s section of `dogs.toml` does not parse: the shepherd read it, so this \
                 binary cannot say what it holds",
            ),
        ));
    };
    Ok(layer_config(
        &standard,
        config,
        dog,
        &table_json(doc.as_table()),
    ))
}

/// Whether `dog` reads a table for the sheep: every sheep for a dog serving
/// them all, the ones carrying a table otherwise.
fn serves(standard: &Standard, config: &Json, dog: &str) -> bool {
    standard.serves == Serves::EverySheep || address::dog_table(config, dog).is_some()
}

/// `config` with `dog`'s table replaced by `section` layered under it.
fn layer_config(
    standard: &Standard,
    config: &Json,
    dog: &str,
    section: &Map<String, Json>,
) -> Option<Layering> {
    if !serves(standard, config, dog) {
        return None;
    }
    let own = address::dog_table(config, dog)
        .and_then(Json::as_object)
        .cloned()
        .unwrap_or_default();
    let layered = standard.layer(section, &own);
    let mut config = config.clone();
    let dogs = config.as_object_mut()?;
    let dogs = dogs
        .entry(DOGS)
        .or_insert_with(|| Json::Object(Map::new()))
        .as_object_mut()?;
    dogs.insert(dog.to_string(), Json::Object(layered.table));
    Some(Layering {
        config,
        inherited: layered.inherited,
    })
}

/// The inherited keys an answer at `steps` covers, in order.
///
/// The whole dog table covers every one, a key inside it only itself.
pub(super) fn covered(steps: &[String], inherited: &BTreeSet<String>) -> Vec<String> {
    match steps {
        [_, _] => inherited.iter().cloned().collect(),
        [_, _, key, ..] => inherited.get(key).cloned().into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Marks each inherited key `covered` names with a trailing comment that
/// names the dog's section, in the document `shep get` is about to print.
pub(super) fn mark(toml: &str, steps: &[String], dog: &str, inherited: &[String]) -> String {
    if inherited.is_empty() {
        return toml.to_string();
    }
    let Ok(mut doc) = toml.parse::<DocumentMut>() else {
        return toml.to_string();
    };
    let note = format!(" # inherited from [{dog}]");
    let dog_depth = 2;
    let Some(table) = steps
        .iter()
        .take(dog_depth)
        .try_fold(doc.as_item_mut(), |item, step| item.get_mut(step.as_str()))
    else {
        return toml.to_string();
    };
    if steps.len() == dog_depth {
        for key in inherited {
            if let Some(item) = table.get_mut(key.as_str()) {
                annotate(item, &note);
            }
        }
    } else if let Some(item) = steps
        .get(dog_depth)
        .and_then(|key| table.get_mut(key.as_str()))
    {
        annotate(item, &note);
    }
    doc.to_string()
}

fn annotate(item: &mut Item, note: &str) {
    match item {
        Item::Value(value) => value.decor_mut().set_suffix(note),
        Item::Table(table) => {
            table.set_implicit(false);
            table.decor_mut().set_suffix(note);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn jobs(serves: Serves) -> Standard {
        let root = json!({ "properties": {
            "poll": { "x-shep-dog-wide": true },
            "model": {},
            "limit": {},
        } });
        let mut standard = Standard::recognise(&root, Serves::EverySheep).expect("standard");
        standard.serves = serves;
        standard
    }

    fn map(value: Json) -> Map<String, Json> {
        value.as_object().cloned().expect("a table")
    }

    fn api() -> Json {
        json!({ "name": "api", "dogs": { "jobs": { "limit": 9 } } })
    }

    fn section() -> Map<String, Json> {
        map(json!({ "poll": "5m", "model": "sonnet", "limit": 3 }))
    }

    #[test]
    fn the_layered_table_replaces_the_sheeps_and_names_what_it_inherits() {
        let layering = layer_config(&jobs(Serves::OptedIn), &api(), "jobs", &section())
            .expect("a sheep with a table is served");
        assert_eq!(
            layering.config["dogs"]["jobs"],
            json!({ "model": "sonnet", "limit": 9 })
        );
        assert_eq!(layering.inherited.iter().collect::<Vec<_>>(), ["model"]);
    }

    #[test]
    fn an_opted_in_dog_does_not_layer_a_sheep_with_no_table() {
        let bare = json!({ "name": "web", "dogs": {} });
        assert!(layer_config(&jobs(Serves::OptedIn), &bare, "jobs", &section()).is_none());
        let every = layer_config(&jobs(Serves::EverySheep), &bare, "jobs", &section())
            .expect("every sheep is served");
        assert_eq!(
            every.config["dogs"]["jobs"],
            json!({ "model": "sonnet", "limit": 3 })
        );
    }

    #[test]
    fn the_whole_table_marks_each_inherited_key() {
        let toml = "[dogs.jobs]\nlimit = 9\nmodel = \"sonnet\"\n";
        let steps = vec!["dogs".to_string(), "jobs".to_string()];
        assert_eq!(
            mark(toml, &steps, "jobs", &["model".to_string()]),
            "[dogs.jobs]\nlimit = 9\nmodel = \"sonnet\" # inherited from [jobs]\n"
        );
    }

    #[test]
    fn one_key_covers_only_itself() {
        let inherited: BTreeSet<String> = ["model".to_string()].into();
        let key = |name: &str| ["dogs", "jobs", name].map(String::from).to_vec();
        assert_eq!(covered(&key("model"), &inherited), ["model"]);
        assert!(covered(&key("limit"), &inherited).is_empty());
    }
}
