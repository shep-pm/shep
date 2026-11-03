//! What the flock itself says about a dog: which names the shepherd will take
//! a section for, and which schema speaks for one.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde_json::Value as Json;
use shep_core::dogs::manifest::Serves;
use shep_core::paths::ShepPaths;

use crate::commands::dogs::{DogSchema, ManifestRead, VERSION_BUDGET, ask_schema, read_manifest};
use crate::commands::shep_toml::ShepToml;
use crate::dog::{builtin_manifest, builtin_schema};
use crate::dog_settings::standard::Standard;

/// A dog the shepherd knows, which is a wider set than the dogs it runs.
///
/// The same three sources the daemon builds its own guard from, because
/// [`shep_core::protocol::Request::SetDogConfig`] is guarded on that one: the
/// dog most in need of configuring is one that is disabled or has never started.
///
/// A name outside this set is refused here rather than sent.
pub(super) fn known_dogs(paths: &ShepPaths) -> BTreeSet<String> {
    let from_file = ShepToml::read_only(&paths.daemon_config)
        .map(|doc| {
            doc.adopted_dog_names()
                .into_iter()
                .chain(doc.enabled_dog_names())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    crate::dog::BUILT_IN_DOGS
        .iter()
        .map(|name| (*name).to_string())
        .chain(from_file)
        .collect()
}
/// The flock's schema for the dog `name`: from this binary for a built-in dog,
/// from the cached manifest when this exact adopted binary has been read
/// before, and from a probe otherwise.
///
/// [`None`] is a dog that publishes nothing, which is the state lookout calls
/// "without a schema nothing is edited": shep is not the authority on a dog's
/// config beyond what that dog printed.
pub(super) fn dog_schema(name: &str, paths: &ShepPaths) -> Option<Json> {
    dog_schema_serving(name, paths).map(|(schema, _)| schema)
}

/// [`dog_schema`], and which sheep the dog's manifest says it serves.
///
/// A dog without a manifest, and one probed for its schema alone, serves
/// [`Serves::OptedIn`].
pub(super) fn dog_schema_serving(name: &str, paths: &ShepPaths) -> Option<(Json, Serves)> {
    if let Some(answer) = builtin_schema_serving(name) {
        return Some(answer);
    }
    let adopted = ShepToml::read_only(&paths.daemon_config)
        .ok()
        .and_then(|doc| doc.adopted_dog_path(name));
    asked_schema_serving(&adopted?, &paths.home, name, VERSION_BUDGET)
}

/// [`dog_schema_serving`] for a dog built into this binary.
pub(crate) fn builtin_schema_serving(name: &str) -> Option<(Json, Serves)> {
    let schema = builtin_schema(name)?;
    let serves = builtin_manifest(name).map_or(Serves::OptedIn, |manifest| manifest.serves);
    Some((schema, serves))
}

/// [`dog_schema_serving`] for the adopted dog at `path`, which has `budget`
/// to answer each question it is asked.
pub(crate) fn asked_schema_serving(
    path: &Path,
    home: &Path,
    name: &str,
    budget: Duration,
) -> Option<(Json, Serves)> {
    if let Ok(ManifestRead::Read(manifest)) = read_manifest(path, home, name, budget) {
        let serves = manifest.serves;
        return manifest.settings.map(|schema| (schema, serves));
    }
    match ask_schema(path, home, name, budget) {
        DogSchema::Published(schema) => Some((schema, Serves::OptedIn)),
        DogSchema::Silent | DogSchema::Unreadable => None,
    }
}

/// `name`'s schema and the standard it follows, for a dog that follows it.
pub(crate) fn dog_standard(name: &str, paths: &ShepPaths) -> Option<(Json, Standard)> {
    let (schema, serves) = dog_schema_serving(name, paths)?;
    let standard = Standard::recognise(&schema, serves)?;
    Some((schema, standard))
}
