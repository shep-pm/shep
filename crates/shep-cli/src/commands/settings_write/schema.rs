//! What the flock itself says about a dog: which names the shepherd will take
//! a section for, and which schema speaks for one.

use std::collections::BTreeSet;

use serde_json::Value as Json;
use shep_core::paths::ShepPaths;

use crate::commands::dogs::{DogSchema, ManifestRead, VERSION_BUDGET, ask_schema, read_manifest};
use crate::commands::shep_toml::ShepToml;
use crate::dog::builtin_schema;

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
    if let Some(schema) = builtin_schema(name) {
        return Some(schema);
    }
    let adopted = ShepToml::read_only(&paths.daemon_config)
        .ok()
        .and_then(|doc| doc.adopted_dog_path(name));
    let path = adopted?;
    if let Ok(ManifestRead::Read(manifest)) =
        read_manifest(&path, &paths.home, name, VERSION_BUDGET)
    {
        return manifest.settings;
    }
    match ask_schema(&path, &paths.home, name, VERSION_BUDGET) {
        DogSchema::Published(schema) => Some(schema),
        DogSchema::Silent | DogSchema::Unreadable => None,
    }
}
