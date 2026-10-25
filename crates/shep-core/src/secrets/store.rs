use super::error::SecretError;
use super::format::{MAX_KEY_BYTES, MAX_VALUE_BYTES, SECRETS_VERSION, SecretFile};
use crate::file_lock::FileLock;
use crate::versioned_file;
use std::collections::BTreeMap;
use std::path::Path;

/// The grammar shared by keys, namespaces and environment names.
///
/// Non-empty, at most [`MAX_KEY_BYTES`], not starting with `.`, and drawn
/// from `[A-Za-z0-9._-]`. Excludes `/`, so a name can never contain the
/// separator [`SecretRef::parse`](crate::secrets::SecretRef::parse) splits a namespace from a key on.
///
/// Public because the daemon checks a peer's namespace and environment
/// against it before storing anything under either: a name outside this
/// grammar is one no `{{secret:...}}` reference could ever name, so
/// accepting it would be a silent no-op.
#[must_use]
pub fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_BYTES
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Checks one name against the grammar, building the refusal with `refuse`.
///
/// # Errors
/// Whatever `refuse` builds from the name: it is empty, over
/// [`MAX_KEY_BYTES`], starts with `.`, or contains anything outside
/// `[A-Za-z0-9._-]`. A name can never contain a `/`.
pub(super) fn check_name(
    value: &str,
    refuse: impl FnOnce(String) -> SecretError,
) -> Result<(), SecretError> {
    if is_name(value) {
        Ok(())
    } else {
        Err(refuse(value.to_string()))
    }
}

/// Checks one value against [`MAX_VALUE_BYTES`], naming `key` in the refusal.
///
/// # Errors
/// [`SecretError::ValueTooLong`] when `value` is over the limit.
fn check_value(key: &str, value: &str) -> Result<(), SecretError> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(SecretError::ValueTooLong {
            key: key.to_string(),
            len: value.len(),
        });
    }
    Ok(())
}

/// Reads whichever version of the store `path` currently names: shorthand
/// for [`crate::versioned_file::read_versioned`] at [`SECRETS_VERSION`].
pub(super) fn read_file(path: &Path) -> Result<SecretFile, SecretError> {
    versioned_file::read_versioned(path, SECRETS_VERSION).map_err(SecretError::from)
}

/// Rewrites `path` to hold exactly `file`.
pub(super) fn write_file(path: &Path, file: &SecretFile) -> Result<(), SecretError> {
    crate::atomic_file::write_json(path, "secrets", file).map_err(SecretError::Io)
}

/// Every key in the store with its per-environment values, in key order.
///
/// Takes no lock, so a caller that must not block never does: the daemon
/// reads this from inside its actor loop, once per spawn, once per app at
/// preflight, and once more each time a sheep's extras arm on the way to
/// `Online`. That is safe because a writer publishes by renaming a fully
/// written file over this one, so a reader sees the whole store either
/// before or after a `set`/`unset`, never a fragment of one. The lock
/// [`set`] and [`unset`] take is what orders those read-modify-writes
/// against each other.
///
/// # Errors
///
/// - [`SecretError::Io`]: the store could not be opened or read. A store
///   that is simply absent is not an error: it reads as empty.
/// - [`SecretError::Decode`]: the file is not the JSON this module writes.
/// - [`SecretError::FutureVersion`]: the file's `version` is newer than
///   [`SECRETS_VERSION`]. Nothing is read and nothing is written.
pub fn all(path: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>, SecretError> {
    Ok(read_file(path)?.entries)
}

/// The value stored under `key` for exactly `environment`, if there is one.
///
/// The stored slot, not the resolved value: there is no fallback to
/// [`ALL_ENVIRONMENTS`](crate::secrets::ALL_ENVIRONMENTS) here. [`SecretView::resolve`](crate::secrets::SecretView::resolve) is what a config
/// reference goes through.
///
/// # Errors
///
/// [`SecretError::InvalidKey`] and [`SecretError::InvalidEnvironment`] for
/// names outside the grammar (refused before the file is opened, so a
/// malformed name never creates one), plus `Io`, `Decode` and
/// `FutureVersion` exactly as [`all`] returns them.
pub fn get(path: &Path, key: &str, environment: &str) -> Result<Option<String>, SecretError> {
    check_name(key, SecretError::InvalidKey)?;
    check_name(environment, SecretError::InvalidEnvironment)?;
    Ok(all(path)?
        .remove(key)
        .and_then(|mut by_environment| by_environment.remove(environment)))
}

/// Stores `value` under `key` for `environment`, replacing any previous
/// value in that slot and leaving every other environment alone.
///
/// # Errors
///
/// - [`SecretError::InvalidKey`]: the key is outside the grammar.
/// - [`SecretError::InvalidEnvironment`]: the environment name is outside
///   the grammar.
/// - [`SecretError::ValueTooLong`]: the value exceeds [`MAX_VALUE_BYTES`].
/// - [`SecretError::FutureVersion`]: the store on disk is newer than this
///   build understands. Nothing is written.
/// - [`SecretError::Decode`]: the existing file could not be parsed.
/// - [`SecretError::Io`]: the lock, the temp file, the `fsync` or the
///   `rename` failed.
pub fn set(path: &Path, key: &str, environment: &str, value: &str) -> Result<(), SecretError> {
    check_name(key, SecretError::InvalidKey)?;
    check_name(environment, SecretError::InvalidEnvironment)?;
    check_value(key, value)?;

    let _lock = FileLock::acquire(path)?;
    let mut file = read_file(path)?;
    file.version = SECRETS_VERSION;
    file.entries
        .entry(key.to_string())
        .or_default()
        .insert(environment.to_string(), value.to_string());
    write_file(path, &file)
}

/// Stores every `(key, value)` pair for `environment` in one locked
/// read-modify-write, so the batch lands whole or not at all.
///
/// Each pair replaces any previous value in its slot and every other
/// environment is left alone. A key repeated in `entries` keeps its last
/// value. An empty `entries` returns `Ok(())` before anything else, the
/// environment name included, and never opens, locks or creates the store.
///
/// # Errors
///
/// - [`SecretError::InvalidEnvironment`]: the environment name is outside
///   the grammar.
/// - [`SecretError::InvalidKey`]: a key is outside the grammar.
/// - [`SecretError::ValueTooLong`]: a value exceeds [`MAX_VALUE_BYTES`].
/// - [`SecretError::FutureVersion`], [`SecretError::Decode`] and
///   [`SecretError::Io`] exactly as [`set`] returns them.
///
/// The first three are checked for every entry before the lock is taken, so
/// a refused batch never creates the store. The store is replaced by one
/// rename, so a batch refused before it, or failing to stage or `fsync` the
/// temp file, leaves the old store whole. An `Io` error from the directory
/// flush that follows the rename can arrive after the whole batch is already
/// in place, exactly as it can for [`set`].
pub fn set_many(
    path: &Path,
    environment: &str,
    entries: &[(&str, &str)],
) -> Result<(), SecretError> {
    if entries.is_empty() {
        return Ok(());
    }
    check_name(environment, SecretError::InvalidEnvironment)?;
    for &(key, value) in entries {
        check_name(key, SecretError::InvalidKey)?;
        check_value(key, value)?;
    }

    let _lock = FileLock::acquire(path)?;
    let mut file = read_file(path)?;
    file.version = SECRETS_VERSION;
    for &(key, value) in entries {
        file.entries
            .entry(key.to_string())
            .or_default()
            .insert(environment.to_string(), value.to_string());
    }
    write_file(path, &file)
}

/// Removes `key`'s value for `environment`, returning whether it was there.
///
/// A key whose last environment this removes goes with it, so the store
/// never accumulates keys that hold nothing.
///
/// # Errors
///
/// The same set [`set`] returns, minus [`SecretError::ValueTooLong`]:
/// `InvalidKey`, `InvalidEnvironment`, `FutureVersion`, `Decode`, `Io`.
pub fn unset(path: &Path, key: &str, environment: &str) -> Result<bool, SecretError> {
    check_name(key, SecretError::InvalidKey)?;
    check_name(environment, SecretError::InvalidEnvironment)?;

    let _lock = FileLock::acquire(path)?;
    let mut file = read_file(path)?;
    let Some(by_environment) = file.entries.get_mut(key) else {
        return Ok(false);
    };
    let was_present = by_environment.remove(environment).is_some();
    if was_present {
        if by_environment.is_empty() {
            file.entries.remove(key);
        }
        file.version = SECRETS_VERSION;
        write_file(path, &file)?;
    }
    Ok(was_present)
}

#[cfg(test)]
mod tests {
    use super::super::error::SecretError;
    use super::super::format::ALL_ENVIRONMENTS;

    use super::*;

    #[test]
    fn a_value_round_trips_through_one_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "DB_PASSWORD", "production", "hunter2").unwrap();
        assert_eq!(
            get(&path, "DB_PASSWORD", "production").unwrap().as_deref(),
            Some("hunter2")
        );
        assert_eq!(get(&path, "DB_PASSWORD", "staging").unwrap(), None);
    }

    #[test]
    fn a_missing_store_reads_as_empty_rather_than_enoent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        assert!(all(&path).unwrap().is_empty());
        assert_eq!(get(&path, "ANY", "production").unwrap(), None);
    }

    #[test]
    fn unset_removes_one_environment_and_leaves_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", "production", "p").unwrap();
        set(&path, "K", "staging", "s").unwrap();
        assert!(unset(&path, "K", "staging").unwrap());
        assert_eq!(get(&path, "K", "production").unwrap().as_deref(), Some("p"));
        assert_eq!(get(&path, "K", "staging").unwrap(), None);
        assert!(!unset(&path, "K", "staging").unwrap(), "already gone");
    }

    #[test]
    fn a_key_that_empties_is_removed_rather_than_left_as_an_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", "production", "p").unwrap();
        assert!(unset(&path, "K", "production").unwrap());
        assert!(all(&path).unwrap().is_empty(), "no empty husk left behind");
    }

    #[test]
    fn a_bad_key_is_refused_by_name_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        for key in ["", ".hidden", "has space", "has/slash", "has:colon"] {
            let err = set(&path, key, "production", "v").unwrap_err();
            assert!(
                matches!(&err, SecretError::InvalidKey(k) if k == key),
                "{key:?}: {err:?}"
            );
        }
        assert!(!path.exists(), "a refused set must not create the store");
    }

    #[test]
    fn a_key_and_an_environment_share_one_grammar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let longest = "a".repeat(MAX_KEY_BYTES);
        let too_long = "a".repeat(MAX_KEY_BYTES + 1);
        let refused = [
            "",
            ".hidden",
            "has space",
            "has/slash",
            "has:colon",
            too_long.as_str(),
        ];
        for name in refused {
            let as_key = [
                get(&path, name, "production").unwrap_err(),
                set(&path, name, "production", "v").unwrap_err(),
                unset(&path, name, "production").unwrap_err(),
            ];
            for err in as_key {
                assert!(
                    matches!(&err, SecretError::InvalidKey(k) if k == name),
                    "{name:?} as a key: {err:?}"
                );
            }
            let as_environment = [
                get(&path, "K", name).unwrap_err(),
                set(&path, "K", name, "v").unwrap_err(),
                unset(&path, "K", name).unwrap_err(),
            ];
            for err in as_environment {
                assert!(
                    matches!(&err, SecretError::InvalidEnvironment(e) if e == name),
                    "{name:?} as an environment: {err:?}"
                );
            }
        }
        assert!(!path.exists(), "a refused name must not create the store");

        set(&path, &longest, &longest, "v").unwrap();
        assert_eq!(
            get(&path, &longest, &longest).unwrap().as_deref(),
            Some("v")
        );
        assert!(unset(&path, &longest, &longest).unwrap());
    }

    #[test]
    fn the_all_slot_is_writable_like_any_other_environment() {
        // Writing the `all` slot is how a value covers every environment,
        // so `set` accepts it. Nothing else about the name is special.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", ALL_ENVIRONMENTS, "everywhere").unwrap();
        assert_eq!(
            get(&path, "K", "all").unwrap().as_deref(),
            Some("everywhere")
        );
    }

    #[test]
    fn get_does_not_fall_back_to_the_all_slot() {
        // `get` returns the stored slot only; `SecretView::resolve` is the
        // one place the `all` fallback lives. Nothing here should fail if
        // that boundary moves, which is exactly the point of pinning it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", ALL_ENVIRONMENTS, "everywhere").unwrap();
        assert_eq!(get(&path, "K", "staging").unwrap(), None);
    }

    #[test]
    fn an_environment_outside_the_grammar_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        for env in ["", "has space", "has/slash"] {
            let err = set(&path, "K", env, "v").unwrap_err();
            assert!(
                matches!(&err, SecretError::InvalidEnvironment(e) if e == env),
                "{env:?}: {err:?}"
            );
        }
    }

    #[test]
    fn set_many_writes_every_entry_and_leaves_other_environments_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "A", "staging", "kept").unwrap();
        set(&path, "A", "production", "old").unwrap();

        set_many(&path, "production", &[("A", "new"), ("B", "b"), ("C", "c")]).unwrap();

        assert_eq!(
            get(&path, "A", "production").unwrap().as_deref(),
            Some("new")
        );
        assert_eq!(get(&path, "B", "production").unwrap().as_deref(), Some("b"));
        assert_eq!(get(&path, "C", "production").unwrap().as_deref(), Some("c"));
        assert_eq!(get(&path, "A", "staging").unwrap().as_deref(), Some("kept"));
        assert_eq!(get(&path, "B", "staging").unwrap(), None);
    }

    #[test]
    fn set_many_with_a_bad_entry_after_good_ones_leaves_the_store_unchanged() {
        let too_long = "v".repeat(MAX_VALUE_BYTES + 1);
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent.json");
        let present = dir.path().join("present.json");
        set(&present, "EXISTING", "production", "e").unwrap();
        let before = std::fs::read(&present).unwrap();

        let refused = |environment: &str, entries: &[(&str, &str)]| {
            let err = set_many(&absent, environment, entries).unwrap_err();
            assert!(
                !absent.exists(),
                "a refused batch must not create the store"
            );
            assert!(set_many(&present, environment, entries).is_err());
            assert_eq!(std::fs::read(&present).unwrap(), before);
            err
        };

        let err = refused("production", &[("GOOD", "g"), ("bad key", "v")]);
        assert!(
            matches!(&err, SecretError::InvalidKey(k) if k == "bad key"),
            "{err:?}"
        );
        let err = refused("production", &[("GOOD", "g"), ("LONG", &too_long)]);
        assert!(
            matches!(&err, SecretError::ValueTooLong { key, .. } if key == "LONG"),
            "{err:?}"
        );
        let err = refused("bad env", &[("GOOD", "g")]);
        assert!(
            matches!(&err, SecretError::InvalidEnvironment(e) if e == "bad env"),
            "{err:?}"
        );
    }

    #[test]
    fn set_many_of_nothing_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set_many(&path, "production", &[]).unwrap();
        set_many(&path, "bad env", &[]).unwrap();
        assert!(!path.exists());
        assert!(!dir.path().join("secrets.json.lock").exists());
    }

    #[test]
    fn a_future_version_is_refused_rather_than_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, r#"{"version":999,"entries":{}}"#).unwrap();
        assert!(matches!(all(&path), Err(SecretError::FutureVersion(999))));
        assert!(matches!(
            set(&path, "K", "production", "v"),
            Err(SecretError::FutureVersion(999))
        ));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("999"), "the refused store is untouched");
    }

    #[test]
    #[cfg(unix)]
    fn the_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", "production", "v").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
