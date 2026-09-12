//! The atomic file replace.
//!
//! Stage a fresh file beside the real one, write it, `fsync` it, `rename`
//! it over the target, then `fsync` the directory the rename landed in. A
//! reader sees the whole old file or the whole new one, never a fragment.
//! [`write_json`] is all of it for a store holding one serializable value;
//! a store that writes its own bytes stages with [`create_staging_file`]
//! and finishes with [`publish`].
//!
//! [`sync_dir`], which [`publish`] calls last, makes the rename durable,
//! where the temp file's own `fsync` does not, on unix only, and only
//! where the filesystem implements the flush.

use std::path::Path;

// `$SHEP_HOME` is already `0700`, but `shep.toml` and `dogs.toml` hold
// webhook URLs with a bearer token in the path, and a `tar` or `cp -p` of
// them carries this mode somewhere no directory mode follows.
/// Mode a file under `$SHEP_HOME` is created with: owner read/write,
/// nobody else.
///
/// # Platforms
///
/// Unix only. On Windows a file inherits the ACL of the directory it lands
/// in.
pub const OWNER_ONLY_FILE_MODE: u32 = 0o600;

// No mode parameter: everything staged here holds a credential or an
// `env` value, so one fixed mode is always right. Prefix and suffix
// refuse separators because `tempfile_in` joins them onto `parent`, and
// `../evil` would escape the directory this is contracted to stay inside.
/// Creates the staging file a store is rewritten through, in `parent` so
/// the later `rename` stays within one filesystem.
///
/// `prefix` and `suffix` bracket a unique middle `tempfile` picks: two
/// concurrent writers never share a name. Neither may contain a path
/// separator. Created [`OWNER_ONLY_FILE_MODE`] on unix at the `open`
/// itself, never by a later `chmod`. Carries no mode on Windows.
///
/// # Errors
/// - [`std::io::ErrorKind::InvalidInput`] if `prefix` or `suffix` contains `/` or `\`.
/// - Otherwise `parent` is missing or unwritable, or `tempfile` ran out of unique names.
pub fn create_staging_file(
    parent: &Path,
    prefix: &str,
    suffix: &str,
) -> std::io::Result<tempfile::NamedTempFile> {
    for (label, part) in [("prefix", prefix), ("suffix", suffix)] {
        if part.contains(['/', '\\']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("staging file {label} must not contain a path separator: {part:?}"),
            ));
        }
    }

    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix).suffix(suffix);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(OWNER_ONLY_FILE_MODE));
    }

    builder.tempfile_in(parent)
}

// `sync_all` on the staged file flushes its contents; the rename's
// directory entry is a separate change to the parent directory, which
// this flushes. Only an unclean shutdown can lose an entry a completed
// `rename` already made visible to every later process.
/// Flushes `dir`'s own metadata, making renames into it durable.
///
/// Call after the `rename` that installs a staged file: the directory
/// entry it created needs a separate flush to reach disk. Skipping this
/// keeps the atomicity guarantee and loses only durability, to a power cut.
///
/// # Platforms
/// Unix only. A no-op on Windows: as durable as NTFS makes it.
///
/// # Errors
/// - [`std::io::Error`] if `dir` could not be opened or flushed. `EINVAL`
///   is tolerated as "no such step"; never errors on Windows.
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        match std::fs::File::open(dir)?.sync_all() {
            Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            other => other,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Installs `tmp` at `path`, replacing whatever was there.
///
/// Returns only once both the contents and the rename that published them
/// have reached disk.
///
/// # Errors
/// - [`std::io::Error`] if the `fsync`, the rename, or the directory flush
///   failed. `path` keeps its old contents unless the rename succeeded.
pub fn publish(tmp: tempfile::NamedTempFile, path: &Path) -> std::io::Result<()> {
    tmp.as_file().sync_all()?;

    // `persist` is `rename(2)`. On failure the `NamedTempFile` comes back
    // inside the error and its `Drop` removes the staging file, so a failed
    // replace does not leave one behind.
    tmp.persist(path).map_err(|err| err.error)?;
    sync_dir(path.parent().unwrap_or_else(|| Path::new(".")))
}

/// Replaces `path` with `value` as pretty-printed JSON and one trailing
/// newline, atomically and durably.
///
/// The staging file lands beside `path` under `prefix`; see
/// [`create_staging_file`] for what a prefix may hold. `path` is left as it
/// was unless the whole value serialized and reached disk.
///
/// # Errors
/// - [`std::io::ErrorKind::InvalidInput`] if `prefix` holds `/` or `\`.
/// - [`std::io::Error`] carrying a `serde_json::Error` if `value` would not
///   serialize, or reporting a failed stage, write, `fsync` or rename.
pub fn write_json<T>(path: &Path, prefix: &str, value: &T) -> std::io::Result<()>
where
    T: serde::Serialize + ?Sized,
{
    use std::io::Write as _;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = create_staging_file(parent, prefix, ".tmp")?;

    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    tmp.write_all(json.as_bytes())?;
    tmp.write_all(b"\n")?;

    publish(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fails if the file lands outside `parent`, where the later `rename`
    /// would cross a filesystem and stop being atomic.
    #[test]
    fn the_staging_file_lands_in_the_parent_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = create_staging_file(dir.path(), "kv", ".tmp").unwrap();
        assert_eq!(tmp.path().parent(), Some(dir.path()));
    }

    /// fails if either part is dropped or swapped on the way to `tempfile`.
    #[test]
    fn the_name_carries_the_prefix_and_the_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = create_staging_file(dir.path(), "barks", ".tmp").unwrap();

        let name = tmp.path().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("barks"), "{name}");
        assert!(name.ends_with(".tmp"), "{name}");
        assert!(name.len() > "barks.tmp".len(), "no unique middle: {name}");
    }

    /// fails if a separator reaches `tempfile`. Both spellings, both
    /// platforms, so an argument cannot be legal on one and an escape on
    /// the other.
    #[test]
    fn a_path_separator_is_refused_in_either_argument() {
        let dir = tempfile::tempdir().unwrap();

        for (prefix, suffix) in [
            ("../escape", ".tmp"),
            ("kv", "/etc/passwd"),
            ("..\\escape", ".tmp"),
            ("kv", "\\tmp"),
        ] {
            let err = create_staging_file(dir.path(), prefix, suffix)
                .expect_err("a separator must not reach tempfile");
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::InvalidInput,
                "{prefix:?} {suffix:?}: {err:?}"
            );
        }
    }

    /// fails if a refused rename leaves a staging file in `$SHEP_HOME`, the
    /// claim `publish`'s own comment makes about `persist`.
    #[test]
    fn a_refused_rename_takes_the_staging_file_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let occupied = dir.path().join("a-directory");
        std::fs::create_dir(&occupied).unwrap();

        let tmp = create_staging_file(dir.path(), "kv", ".tmp").unwrap();
        publish(tmp, &occupied).expect_err("a rename over a directory must fail");

        assert!(occupied.is_dir(), "the target was replaced anyway");
        assert_eq!(entry_names(dir.path()), vec!["a-directory"]);
    }

    /// fails if the trailing newline or the pretty printing is dropped: four
    /// stores' on-disk bytes are this exact shape.
    #[test]
    fn write_json_writes_pretty_json_under_one_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.json");
        let value = std::collections::BTreeMap::from([("one", 1), ("two", 2)]);

        write_json(&path, "kv", &value).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"one\": 1,\n  \"two\": 2\n}\n"
        );
        assert_eq!(entry_names(dir.path()), vec!["store.json"]);
    }

    /// fails if `write_json` stops routing its prefix through
    /// `create_staging_file`, where the separator check lives.
    #[test]
    fn write_json_refuses_a_prefix_holding_a_separator() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_json(&dir.path().join("store.json"), "../escape", &1)
            .expect_err("a separator must not reach tempfile");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err:?}");
    }

    /// Every entry in `dir`, sorted, so a leftover staging file shows up as a
    /// mismatch naming itself.
    fn entry_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn sync_dir_accepts_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        // Windows returns `Ok` unconditionally, so this only has teeth on unix.
        sync_dir(dir.path()).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn sync_dir_reports_a_directory_that_is_not_there() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");

        // Guards the `EINVAL` tolerance from widening into swallowing every error.
        let err = sync_dir(&missing).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err:?}");
    }
}
