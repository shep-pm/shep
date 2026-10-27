//! Which dogs can hold a table on a sheep, and one dog's per-sheep schema.
//!
//! Both run off the UI task. Listing is cheap: a built-in dog is this binary
//! and answers in-process, and an adopted one is only named. Its binary is
//! spawned with the schema flag when its row is opened, never before.
//! Nothing here is stored.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use shep_core::dogs::SHEEP_SCHEMA_KEY;

use super::pane::{ListedDog, SheepDogEntry};
use crate::commands::dogs::{DogSchema, ask_schema};
use crate::commands::settings::dog_candidates;
use crate::commands::shep_toml::ShepToml;

/// One entry per dog `daemon_config` names or shep builds in. A built-in
/// answers with its per-sheep schema where it publishes one; an adopted
/// dog is named and left unasked, for [`probe_sheep_dog`].
///
/// A `shep.toml` that cannot be read lists no dog, which leaves every table
/// the sheep carries read-only: without a schema nothing is edited.
pub(super) fn list_sheep_dogs(daemon_config: &Path) -> Vec<ListedDog> {
    let Ok(doc) = ShepToml::read_only(daemon_config) else {
        return Vec::new();
    };
    dog_candidates(&doc)
        .into_iter()
        .map(|dog| match crate::dog::builtin_schema(&dog.name) {
            Some(schema) => ListedDog {
                entry: SheepDogEntry {
                    schema: sheep_schema(&schema),
                    name: dog.name,
                    adopted_path: dog.adopted_path,
                },
                asked: true,
            },
            None => ListedDog {
                asked: dog.adopted_path.is_none(),
                entry: SheepDogEntry {
                    name: dog.name,
                    adopted_path: dog.adopted_path,
                    schema: None,
                },
            },
        })
        .collect()
}

/// `dog`'s per-sheep schema, asked of its binary with the schema flag. The
/// binary has `budget` to answer, `VERSION_BUDGET` outside a test. A dog
/// that stays silent, or answers something unreadable, has none.
pub(super) fn probe_sheep_dog(
    home: &Path,
    dog: &str,
    adopted_path: Option<PathBuf>,
    budget: Duration,
) -> SheepDogEntry {
    let answer = match (crate::dog::builtin_schema(dog), &adopted_path) {
        (Some(schema), _) => Some(schema),
        (None, Some(path)) => match ask_schema(path, home, dog, budget) {
            DogSchema::Published(schema) => Some(schema),
            DogSchema::Silent | DogSchema::Unreadable => None,
        },
        (None, None) => None,
    };
    SheepDogEntry {
        schema: answer.as_ref().and_then(sheep_schema),
        name: dog.to_owned(),
        adopted_path,
    }
}

/// `pub(crate)`: `shep set` validates a dog's table on a sheep against the
/// same schema the pane edits its rows from (issue #1096).
///
/// `root`'s `x-shep-sheep` value, with `root`'s own `$defs` attached so each
/// `$ref` in it resolves where it points. [`None`] when there is no such key,
/// or when it is not a schema object. A definition the sheep schema carries
/// itself wins over the root's of the same name.
pub(crate) fn sheep_schema(root: &Value) -> Option<Value> {
    let mut sheep = root.get(SHEEP_SCHEMA_KEY)?.as_object()?.clone();
    if let Some(root_defs) = root.get("$defs").and_then(Value::as_object) {
        let defs = sheep
            .entry("$defs")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(defs) = defs {
            for (name, def) in root_defs {
                defs.entry(name.clone()).or_insert_with(|| def.clone());
            }
        }
    }
    Some(Value::Object(sheep))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_sheep_schema_carries_the_roots_defs() {
        let root = json!({
            "properties": { "poll": { "type": "string" } },
            "x-shep-sheep": { "$ref": "#/$defs/ProjectSettings" },
            "$defs": { "ProjectSettings": { "type": "object" } },
        });
        assert_eq!(
            sheep_schema(&root),
            Some(json!({
                "$ref": "#/$defs/ProjectSettings",
                "$defs": { "ProjectSettings": { "type": "object" } },
            }))
        );
    }

    #[test]
    fn a_schema_without_the_key_or_with_a_bare_value_has_none() {
        assert_eq!(sheep_schema(&json!({ "properties": {} })), None);
        assert_eq!(sheep_schema(&json!({ "x-shep-sheep": true })), None);
    }

    #[test]
    fn the_sheep_schemas_own_definition_wins() {
        let root = json!({
            "x-shep-sheep": { "$defs": { "Hours": { "type": "string" } } },
            "$defs": { "Hours": { "type": "object" }, "Other": {} },
        });
        let sheep = sheep_schema(&root).expect("a sheep schema");
        assert_eq!(sheep["$defs"]["Hours"], json!({ "type": "string" }));
        assert_eq!(sheep["$defs"]["Other"], json!({}));
    }

    #[test]
    fn a_shep_toml_that_does_not_parse_lists_no_dog() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("shep.toml");
        std::fs::write(&config, "[daemon\n").unwrap();
        assert!(list_sheep_dogs(&config).is_empty());
    }

    /// An adopted dog is listed unasked: its binary is not run to list it.
    /// A built-in answers in-process, with no sheep schema when it
    /// publishes none.
    #[cfg(unix)]
    #[test]
    fn listing_names_an_adopted_dog_and_never_runs_it() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let script = dir.path().join("shep-jobs");
        std::fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = dir.path().join("shep.toml");
        std::fs::write(
            &config,
            format!(
                "[daemon.adopted_dogs]\njobs = {:?}\n",
                script.display().to_string()
            ),
        )
        .unwrap();

        let dogs = list_sheep_dogs(&config);
        let jobs = dogs
            .iter()
            .find(|dog| dog.entry.name == "jobs")
            .expect("jobs");
        assert!(!jobs.asked);
        assert_eq!(jobs.entry.adopted_path.as_deref(), Some(script.as_path()));
        assert_eq!(jobs.entry.schema, None);
        let bark = dogs
            .iter()
            .find(|dog| dog.entry.name == "bark")
            .expect("a built-in");
        assert!(bark.asked);
        assert_eq!(bark.entry.schema, None);
        assert!(!marker.exists(), "listing ran the adopted binary");
    }

    /// An adopted dog is its own binary, asked with the schema flag.
    #[cfg(unix)]
    #[test]
    fn an_adopted_dog_is_asked_and_a_silent_one_has_no_schema() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("shep-jobs");
        let schema = json!({
            "properties": {},
            "x-shep-sheep": { "$ref": "#/$defs/Settings" },
            "$defs": { "Settings": { "type": "object" } },
        });
        std::fs::write(
            &script,
            format!("#!/bin/sh\n[ \"$1\" = --schema ] && echo '{schema}'\nexit 0\n"),
        )
        .unwrap();
        let quiet = dir.path().join("shep-quiet");
        std::fs::write(&quiet, "#!/bin/sh\nexit 0\n").unwrap();
        let broken = dir.path().join("shep-broken");
        std::fs::write(&broken, "#!/bin/sh\necho '{not json'\nexit 0\n").unwrap();
        for bin in [&script, &quiet, &broken] {
            std::fs::set_permissions(bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Thirty seconds, not the real one: a first spawn of a new script
        // on a loaded machine outruns a second, and this is not the bound.
        let budget = Duration::from_secs(30);
        let jobs = probe_sheep_dog(dir.path(), "jobs", Some(script.clone()), budget);
        assert_eq!(jobs.adopted_path.as_deref(), Some(script.as_path()));
        assert_eq!(
            jobs.schema,
            Some(json!({
                "$ref": "#/$defs/Settings",
                "$defs": { "Settings": { "type": "object" } },
            }))
        );
        for (name, bin) in [("quiet", quiet), ("broken", broken)] {
            let dog = probe_sheep_dog(dir.path(), name, Some(bin), budget);
            assert_eq!(dog.schema, None, "{name} answers no schema");
        }
    }
}
