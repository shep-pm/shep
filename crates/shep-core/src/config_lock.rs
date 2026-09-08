//! The staging file a config is written through, and the old name for the
//! lock its writers hold.

// In shep-core rather than shep-cli because `dogs.toml` has a daemon-side
// writer, and a type the daemon cannot name is a type it cannot hold.
// shep-cli's three writers import both back through a `pub(super) use`.
use std::path::Path;

/// The lock a config file's writers hold across their read-modify-write.
///
/// The old name for [`crate::file_lock::FileLock`], which every store
/// under `$SHEP_HOME` now takes. Kept so a caller that named the lock
/// through this module keeps compiling.
pub use crate::file_lock::FileLock as ConfigLock;

/// Creates the staging file a config is written through, in `parent` so
/// the later `rename` stays within one filesystem.
///
/// The create-at-mode reasoning lives with
/// [`crate::atomic_file::create_staging_file`], which every store shares.
/// What is left here is the pair of names, and this wrapper is where they
/// stay: `commands::dog_migration` writes `dogs.toml` through the same
/// staging name as `shep.toml`, and two call sites spelling that pair out
/// separately is how the two would drift.
///
/// # Errors
/// The staging file could not be created in `parent`.
pub fn create_config_file(parent: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    crate::atomic_file::create_staging_file(parent, "shep", ".toml.tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_config_file_is_owner_only_named_for_the_pair_and_lands_where_asked() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = create_config_file(dir.path()).unwrap();
        assert_eq!(tmp.path().parent(), Some(dir.path()));
        let name = tmp.path().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("shep"), "{name}");
        assert!(name.ends_with(".toml.tmp"), "{name}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tmp.as_file().metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
