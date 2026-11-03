//! `shep get <sheep>.dogs.<dog>` for a dog that follows the dog settings
//! standard: the table the dog reads, with the keys the sheep leaves to the
//! dog's section marked as inherited.

use std::collections::BTreeSet;

use serde_json::{Map, Value as Json};
use shep_client::Client;
use shep_core::dogs::manifest::Serves;
use shep_core::paths::ShepPaths;
use toml_edit::{DocumentMut, Item, Key};

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
    /// The path of each value of that table the sheep's own table does not set.
    pub(super) inherited: BTreeSet<Vec<String>>,
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

/// The inherited paths an answer at `steps` covers, in order, each below the
/// dog's table.
///
/// An address above an inherited path covers it. An address inside an
/// inherited value is covered whole, and is the one path.
pub(super) fn covered(steps: &[String], inherited: &BTreeSet<Vec<String>>) -> Vec<Vec<String>> {
    let Some(address) = steps.get(2..) else {
        return Vec::new();
    };
    let mut paths: Vec<Vec<String>> = Vec::new();
    for path in inherited {
        let found = if path.starts_with(address) {
            path.clone()
        } else if address.starts_with(path) {
            address.to_vec()
        } else {
            continue;
        };
        if !paths.contains(&found) {
            paths.push(found);
        }
    }
    paths
}

/// `path` as the dotted key `shep get` would be asked for, a segment that
/// needs it quoted.
pub(super) fn dotted(path: &[String]) -> String {
    path.iter()
        .map(|segment| Key::new(segment.as_str()).to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Marks each inherited path with a trailing comment that names the dog's
/// section, in the document `shep get` is about to print.
///
/// A path inside an inline table marks the inline table, since a comment
/// cannot sit inside one.
pub(super) fn mark(toml: &str, steps: &[String], dog: &str, inherited: &[Vec<String>]) -> String {
    if inherited.is_empty() {
        return toml.to_string();
    }
    let Ok(mut doc) = toml.parse::<DocumentMut>() else {
        return toml.to_string();
    };
    let note = format!(" # inherited from [{dog}]");
    let dog_depth = 2;
    for path in inherited {
        let steps = steps.iter().take(dog_depth).chain(path);
        if let Some(item) = locate(doc.as_item_mut(), steps) {
            annotate(item, &note);
        }
    }
    doc.to_string()
}

/// The item `steps` leads to, or the inline table that holds it, or
/// [`None`] when the document has nothing there.
fn locate<'a, 'b>(
    root: &'a mut Item,
    steps: impl Iterator<Item = &'b String>,
) -> Option<&'a mut Item> {
    let mut item = root;
    for step in steps {
        if matches!(item, Item::Value(_)) {
            break;
        }
        item = item.as_table_like_mut()?.get_mut(step.as_str())?;
    }
    Some(item)
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
        assert_eq!(layering.inherited, BTreeSet::from([path(&["model"])]));
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

    fn path(steps: &[&str]) -> Vec<String> {
        steps.iter().map(|step| (*step).to_string()).collect()
    }

    #[test]
    fn the_whole_table_marks_each_inherited_key() {
        let toml = "[dogs.jobs]\nlimit = 9\nmodel = \"sonnet\"\n";
        let steps = vec!["dogs".to_string(), "jobs".to_string()];
        assert_eq!(
            mark(toml, &steps, "jobs", &[path(&["model"])]),
            "[dogs.jobs]\nlimit = 9\nmodel = \"sonnet\" # inherited from [jobs]\n"
        );
    }

    #[test]
    fn a_key_left_inside_a_table_the_sheep_sets_part_of_is_marked() {
        let toml = "[dogs.jobs.db]\nhost = \"b\"\nport = 1\n";
        let steps = path(&["dogs", "jobs"]);
        assert_eq!(
            mark(toml, &steps, "jobs", &[path(&["db", "port"])]),
            "[dogs.jobs.db]\nhost = \"b\"\nport = 1 # inherited from [jobs]\n"
        );
    }

    #[test]
    fn a_path_the_document_lacks_marks_nothing() {
        let toml = "[dogs.jobs]\nlimit = 9\n";
        let steps = path(&["dogs", "jobs"]);
        assert_eq!(mark(toml, &steps, "jobs", &[path(&["db", "port"])]), toml);
    }

    #[test]
    fn one_key_covers_only_itself() {
        let inherited = BTreeSet::from([path(&["model"])]);
        let key = |name: &str| path(&["dogs", "jobs", name]);
        assert_eq!(covered(&key("model"), &inherited), [path(&["model"])]);
        assert!(covered(&key("limit"), &inherited).is_empty());
    }

    #[test]
    fn a_table_covers_what_inside_it_is_inherited_and_a_key_inside_one_is_covered_whole() {
        let inherited = BTreeSet::from([path(&["db", "port"]), path(&["model"])]);
        assert_eq!(
            covered(&path(&["dogs", "jobs", "db"]), &inherited),
            [path(&["db", "port"])]
        );
        assert_eq!(
            covered(&path(&["dogs", "jobs", "db", "port", "x"]), &inherited),
            [path(&["db", "port", "x"])]
        );
        assert!(covered(&path(&["dogs", "jobs", "db", "host"]), &inherited).is_empty());
    }

    #[test]
    fn a_path_is_dotted_with_a_segment_that_needs_it_quoted() {
        assert_eq!(dotted(&path(&["db", "a.b"])), "db.\"a.b\"");
    }
}
