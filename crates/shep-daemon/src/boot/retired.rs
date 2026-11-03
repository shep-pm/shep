//! Files an older shepherd wrote that this one no longer reads

use shep_core::paths::ShepPaths;

/// Warns once when `dog-defaults.json` is still in `$SHEP_HOME`.
///
/// The file held `<dog>.defaults`, which a dog's own `[<dog>]` section in
/// `dogs.toml` replaced. It is left on disk for the operator to read or
/// delete.
pub(super) fn warn_of_dog_defaults(paths: &ShepPaths) {
    let file = paths.home.join("dog-defaults.json");
    if file.exists() {
        tracing::warn!(
            file = %file.display(),
            "dog-defaults.json is no longer read: a dog's own section in dogs.toml is the value \
             for every sheep, so move any value you still want there and delete the file"
        );
    }
}
