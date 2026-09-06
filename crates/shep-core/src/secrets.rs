//! `secrets.json`: the values a config refers to and never carries.
//!
//! A key holds one value per environment, so `production` and `staging`
//! differ without two config files. A `{{secret:NAME}}` reference resolves
//! through [`SecretView`], which reads the sheep's own environment and then
//! [`ALL_ENVIRONMENTS`], never another named environment.
//!
//! Same on-disk shape as [`crate::kv`]: a read-modify-rename under an
//! exclusive lock on a sibling `secrets.json.lock`, copied rather than
//! shared since `KvLock` is private to its module.

use core::fmt;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
// `PathBuf` backs `lock_path` below, gated the same way for both platform
// arms of `SecretLock`.
#[cfg(any(unix, windows))]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The on-disk format's version.
///
/// A store carrying a higher version is refused rather than read or
/// replaced ([`SecretError::FutureVersion`]): there is no undo for a
/// downgrade that overwrites an operator's credentials.
pub const SECRETS_VERSION: u32 = 1;

/// Longest key, namespace or environment name this store accepts, in bytes.
pub const MAX_KEY_BYTES: usize = 128;

/// Longest value this store accepts, in bytes.
///
/// The store is read whole on every access; a cap keeps it from becoming an
/// unbounded blob store. A 4096-bit RSA private key in PEM is 3272 bytes.
pub const MAX_VALUE_BYTES: usize = 4096;

/// The environment name that covers every environment.
///
/// A value here is used when the sheep's own environment has no slot of its
/// own. Cannot be a sheep's `environment`, which `AppConfig` refuses.
pub const ALL_ENVIRONMENTS: &str = "all";

/// The file's shape: a version and a key to environment to value map.
///
/// `BTreeMap` throughout so two writes of the same content produce
/// byte-identical files.
#[derive(Default, Serialize, Deserialize)]
struct SecretFile {
    version: u32,
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

/// Redacted (IR-41): `entries` is the whole point of this type.
impl fmt::Debug for SecretFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretFile")
            .field("version", &self.version)
            .field("keys", &self.entries.len())
            .finish()
    }
}

/// Error type returned by this module.
///
/// `#[non_exhaustive]`: shep-core is published, so a new failure variant
/// must not break an out-of-tree `match`.
///
/// Wraps `io::Error`/`serde_json::Error` directly rather than stringifying
/// them, matching [`crate::kv::KvError`], so callers keep the underlying
/// diagnostic through [`core::error::Error::source`]; this type does not
/// derive `Clone`/`PartialEq`/`Eq` as a result.
///
/// No variant carries a secret's value: a message names the key, the
/// namespace or the environment, and nothing else.
#[non_exhaustive]
#[derive(Debug)]
pub enum SecretError {
    /// The store could not be read, written, or replaced.
    Io(std::io::Error),
    /// The store's JSON could not be parsed.
    ///
    /// Refused rather than repaired: a partial read would silently drop
    /// credentials still on disk, and a later write would erase them.
    Decode(serde_json::Error),
    /// A key outside the grammar; carries it verbatim so the message can
    /// quote what was typed.
    InvalidKey(String),
    /// An environment name outside the grammar; carries it verbatim.
    InvalidEnvironment(String),
    /// A value over [`MAX_VALUE_BYTES`].
    ValueTooLong {
        /// The key it was being stored under.
        key: String,
        /// Its length in bytes.
        len: usize,
    },
    /// The store on disk is a version this build does not understand; carries
    /// that version. Nothing was written.
    FutureVersion(u32),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "secret store I/O failed: {err}"),
            Self::Decode(err) => write!(f, "secret store failed to parse: {err}"),
            Self::InvalidKey(key) => write!(f, "`{key}` is not a valid secret key"),
            Self::InvalidEnvironment(environment) => {
                write!(f, "`{environment}` is not a valid environment name")
            }
            Self::ValueTooLong { key, len } => write!(
                f,
                "value for `{key}` is {len} bytes, over the {MAX_VALUE_BYTES}-byte limit"
            ),
            Self::FutureVersion(version) => write!(
                f,
                "secret store is version {version}, newer than this build understands"
            ),
        }
    }
}

impl core::error::Error for SecretError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Decode(err) => Some(err),
            Self::InvalidKey(_)
            | Self::InvalidEnvironment(_)
            | Self::ValueTooLong { .. }
            | Self::FutureVersion(_) => None,
        }
    }
}

impl From<std::io::Error> for SecretError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<serde_json::Error> for SecretError {
    fn from(source: serde_json::Error) -> Self {
        Self::Decode(source)
    }
}

/// The grammar shared by keys, namespaces and environment names.
///
/// Excludes `/`, so a name can never contain the separator
/// [`SecretRef::parse`] splits a namespace from a key on.
fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_BYTES
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Checks one key against the grammar.
///
/// # Errors
/// [`SecretError::InvalidKey`]: empty, over [`MAX_KEY_BYTES`], starting with
/// `.`, or containing anything outside `[A-Za-z0-9._-]`.
fn check_key(key: &str) -> Result<(), SecretError> {
    if is_name(key) {
        Ok(())
    } else {
        Err(SecretError::InvalidKey(key.to_string()))
    }
}

/// Checks one environment name against the grammar.
///
/// # Errors
/// [`SecretError::InvalidEnvironment`]: the same conditions [`check_key`]
/// refuses, so a name can never contain a `/`.
fn check_environment(environment: &str) -> Result<(), SecretError> {
    if is_name(environment) {
        Ok(())
    } else {
        Err(SecretError::InvalidEnvironment(environment.to_string()))
    }
}

/// The lock file that guards `path`: its own name with `.lock` appended, so
/// it sits in `$SHEP_HOME` next to the store and inherits that directory's
/// `0700`.
///
/// `cfg(any(unix, windows))` alongside its two callers: [`SecretLock::acquire`]
/// names a real lock file on both platforms, unix through `flock(2)` and
/// windows through an exclusive `share_mode(0)` open.
#[cfg(any(unix, windows))]
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".lock");
    path.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

/// An exclusive advisory lock over one secret store, released when it drops,
/// including by the kernel if the process dies holding it.
///
/// On a sibling `secrets.json.lock`, never on the store itself: `rename`
/// replaces the store's inode, which would orphan a lock held on it.
struct SecretLock {
    /// `flock(2)` is released by this handle's `Drop`. Named with a leading
    /// underscore because it is held, never read.
    #[cfg(unix)]
    _flock: nix::fcntl::Flock<std::fs::File>,
    /// The lock file, opened with `share_mode(0)` so no other handle can
    /// open it while this one is live; released by `Drop`, the same role
    /// `_flock` plays on unix. Named with a leading underscore because it
    /// is held, never read.
    #[cfg(windows)]
    _handle: std::fs::File,
}

impl SecretLock {
    /// Blocks until this process holds the store's lock exclusively.
    ///
    /// # Errors
    /// The lock file could not be created beside `path`, or `flock` failed
    /// for a reason other than contention (contention blocks rather than
    /// failing).
    #[cfg(unix)]
    fn acquire(path: &Path) -> std::io::Result<Self> {
        use nix::fcntl::{Flock, FlockArg};
        use std::os::unix::fs::OpenOptionsExt as _;

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(crate::atomic_file::OWNER_ONLY_FILE_MODE)
            .open(lock_path(path))?;

        Flock::lock(file, FlockArg::LockExclusive)
            .map(|flock| Self { _flock: flock })
            .map_err(|(_file, errno)| std::io::Error::from(errno))
    }

    /// Blocks until this process holds the store's lock exclusively.
    ///
    /// `share_mode(0)` denies every other open, in this process or another,
    /// giving the same exclusivity as unix `flock`. A contended open fails
    /// immediately with `ERROR_SHARING_VIOLATION` rather than blocking, so
    /// this polls on a short sleep until it succeeds.
    ///
    /// # Errors
    /// The lock file could not be created beside `path`, or the open failed
    /// for a reason other than sharing contention (contention retries rather
    /// than failing).
    #[cfg(windows)]
    fn acquire(path: &Path) -> std::io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt as _;

        /// Windows' `ERROR_SHARING_VIOLATION`: another handle already holds
        /// share access this open's `share_mode(0)` denies. Hardcoded rather
        /// than pulled from `windows-sys`, since this crate has no other
        /// Windows-only dependency.
        const ERROR_SHARING_VIOLATION: i32 = 32;

        /// How long a contended retry sleeps before trying again. Short
        /// enough that a lock held for a normal `set`/`get`'s duration (a
        /// handful of small file operations) costs this loop only a few
        /// iterations, long enough not to spin the CPU while it waits.
        const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);

        let lock_path = lock_path(path);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(&lock_path)
            {
                Ok(handle) => return Ok(Self { _handle: handle }),
                Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Reads `path` under the lock the caller already holds.
///
/// A missing file reads as an empty, current-version store: reading against
/// a fresh `$SHEP_HOME` should not fail with `ENOENT`. Any other
/// `io::Error` propagates.
fn read_file(path: &Path) -> Result<SecretFile, SecretError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(SecretFile::default()),
        Err(err) => return Err(SecretError::Io(err)),
    };
    let file: SecretFile = serde_json::from_str(&raw)?;
    if file.version > SECRETS_VERSION {
        return Err(SecretError::FutureVersion(file.version));
    }
    Ok(file)
}

/// Rewrites `path` to hold exactly `file`, atomically: staged through a
/// temp file, then renamed over the original.
fn write_file(path: &Path, file: &SecretFile) -> Result<(), SecretError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = crate::atomic_file::create_staging_file(parent, "secrets", ".tmp")?;

    let json = serde_json::to_string_pretty(file)?;
    tmp.write_all(json.as_bytes())?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;

    // `persist` is `rename(2)`. On failure the `NamedTempFile` comes back
    // inside the error and its `Drop` removes the staging file, so a failed
    // replace does not leave one behind.
    tmp.persist(path)
        .map_err(|err| SecretError::Io(err.error))?;

    // `sync_all` above made the contents durable; this makes the rename
    // that published them durable too.
    crate::atomic_file::sync_dir(parent)?;
    Ok(())
}

/// Every key in the store with its per-environment values, in key order.
///
/// # Errors
///
/// - [`SecretError::Io`]: the store could not be opened or read. A store
///   that is simply absent is not an error: it reads as empty.
/// - [`SecretError::Decode`]: the file is not the JSON this module writes.
/// - [`SecretError::FutureVersion`]: the file's `version` is newer than
///   [`SECRETS_VERSION`]. Nothing is read and nothing is written.
pub fn all(path: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>, SecretError> {
    // Taking the lock here too costs one extra `open`, but it orders this
    // read against `set`/`unset`'s read-modify-rename instead of racing it.
    let _lock = SecretLock::acquire(path)?;
    Ok(read_file(path)?.entries)
}

/// The value stored under `key` for exactly `environment`, if there is one.
///
/// The stored slot, not the resolved value: there is no fallback to
/// [`ALL_ENVIRONMENTS`] here. [`SecretView::resolve`] is what a config
/// reference goes through.
///
/// # Errors
///
/// [`SecretError::InvalidKey`] and [`SecretError::InvalidEnvironment`] for
/// names outside the grammar (refused before the file is opened, so a
/// malformed name never creates one), plus `Io`, `Decode` and
/// `FutureVersion` exactly as [`all`] returns them.
pub fn get(path: &Path, key: &str, environment: &str) -> Result<Option<String>, SecretError> {
    check_key(key)?;
    check_environment(environment)?;
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
    check_key(key)?;
    check_environment(environment)?;
    if value.len() > MAX_VALUE_BYTES {
        return Err(SecretError::ValueTooLong {
            key: key.to_string(),
            len: value.len(),
        });
    }

    let _lock = SecretLock::acquire(path)?;
    let mut file = read_file(path)?;
    file.version = SECRETS_VERSION;
    file.entries
        .entry(key.to_string())
        .or_default()
        .insert(environment.to_string(), value.to_string());
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
    check_key(key)?;
    check_environment(environment)?;

    let _lock = SecretLock::acquire(path)?;
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

/// One `{{secret:...}}` reference: a key, and the namespace it came from.
///
/// A bare reference reads the operator's own store; a namespaced one reads
/// what the provider dog of that name pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretRef<'a> {
    /// The provider dog's name, or `None` for the operator's own store.
    pub namespace: Option<&'a str>,
    /// The key within that store.
    pub key: &'a str,
}

impl<'a> SecretRef<'a> {
    /// Parses the body of a `{{secret:...}}` token, braces and prefix
    /// already stripped.
    ///
    /// Returns `None` for anything outside the grammar, which is how a
    /// config refuses a bad reference before a sheep ever starts.
    #[must_use]
    pub fn parse(body: &'a str) -> Option<Self> {
        match body.split_once('/') {
            None if is_name(body) => Some(Self {
                namespace: None,
                key: body,
            }),
            Some((namespace, key)) if is_name(namespace) && is_name(key) => Some(Self {
                namespace: Some(namespace),
                key,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for SecretRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{{secret:")?;
        if let Some(namespace) = self.namespace {
            f.write_str(namespace)?;
            f.write_str("/")?;
        }
        f.write_str(self.key)?;
        f.write_str("}}")
    }
}

/// What one environment can see: the operator's store plus every provider
/// dog's, resolved against a single environment name.
///
/// Built once per resolution pass and read many times, so the maps are owned
/// rather than borrowed.
///
/// Debug does not leak a value: it prints the environment and two counts.
pub struct SecretView {
    environment: String,
    store: BTreeMap<String, BTreeMap<String, String>>,
    namespaces: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
}

/// Redacted (IR-41): `store` and `namespaces` hold secret values.
impl fmt::Debug for SecretView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretView")
            .field("environment", &self.environment)
            .field("keys", &self.store.len())
            .field("namespaces", &self.namespaces.len())
            .finish()
    }
}

impl SecretView {
    /// A view over `store` and `namespaces`, resolved for `environment`.
    #[must_use]
    pub fn new(
        environment: String,
        store: BTreeMap<String, BTreeMap<String, String>>,
        namespaces: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
    ) -> Self {
        Self {
            environment,
            store,
            namespaces,
        }
    }

    /// A view holding nothing, so every reference resolves to
    /// [`Resolution::MissingKey`].
    #[must_use]
    pub fn empty(environment: String) -> Self {
        Self::new(environment, BTreeMap::new(), BTreeMap::new())
    }

    /// The environment this view resolves against.
    #[must_use]
    pub fn environment(&self) -> &str {
        &self.environment
    }

    /// The value `reference` resolves to in this view's environment.
    ///
    /// Exact environment, then [`ALL_ENVIRONMENTS`], then nothing. There is
    /// deliberately no fallback to another named environment: filling an
    /// empty `staging` slot from `production` would hand a live credential
    /// to staging the first time somebody forgot to set one.
    #[must_use]
    pub fn resolve(&self, reference: &SecretRef<'_>) -> Resolution<'_> {
        let table = match reference.namespace {
            None => &self.store,
            Some(namespace) => match self.namespaces.get(namespace) {
                Some(table) => table,
                None => return Resolution::MissingNamespace,
            },
        };
        let Some(by_environment) = table.get(reference.key) else {
            return Resolution::MissingKey;
        };
        by_environment
            .get(&self.environment)
            .or_else(|| by_environment.get(ALL_ENVIRONMENTS))
            .map_or(Resolution::MissingKey, |value| {
                Resolution::Found(value.as_str())
            })
    }
}

/// The outcome of resolving one [`SecretRef`].
///
/// [`Self::MissingKey`] and [`Self::MissingNamespace`] are kept apart
/// because they need different remedies: a namespace nobody has pushed
/// under is a dog that has not reported yet, which a retry fixes, while a
/// missing key is a person's to set.
///
/// Debug does not leak a value: [`Self::Found`] prints as `Found(..)`.
pub enum Resolution<'a> {
    /// The resolved value.
    Found(&'a str),
    /// The store or namespace exists and holds no value for this key in
    /// this environment.
    MissingKey,
    /// No provider dog has ever pushed under this namespace.
    MissingNamespace,
}

/// Redacted (IR-41): [`Resolution::Found`] carries a secret's value.
impl fmt::Debug for Resolution<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Found(_) => "Found(..)",
            Self::MissingKey => "MissingKey",
            Self::MissingNamespace => "MissingNamespace",
        })
    }
}

#[cfg(test)]
mod tests {
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
    fn an_oversized_value_is_refused_by_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let big = "x".repeat(MAX_VALUE_BYTES + 1);
        let err = set(&path, "K", "production", &big).unwrap_err();
        assert!(matches!(err, SecretError::ValueTooLong { len, .. } if len == big.len()));
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

    #[test]
    fn a_reference_parses_with_and_without_a_namespace() {
        let bare = SecretRef::parse("DB_PASSWORD").unwrap();
        assert_eq!(bare.namespace, None);
        assert_eq!(bare.key, "DB_PASSWORD");

        let scoped = SecretRef::parse("vercel/DB_PASSWORD").unwrap();
        assert_eq!(scoped.namespace, Some("vercel"));
        assert_eq!(scoped.key, "DB_PASSWORD");

        for bad in ["", "/KEY", "ns/", "a/b/c", "ns/bad key", "bad ns/KEY"] {
            assert!(SecretRef::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn resolution_prefers_the_exact_environment_then_all_then_gives_up() {
        let mut store = BTreeMap::new();
        store.insert(
            "K".to_string(),
            BTreeMap::from([
                ("production".to_string(), "prod".to_string()),
                ("all".to_string(), "fallback".to_string()),
            ]),
        );
        store.insert(
            "ONLY_ALL".to_string(),
            BTreeMap::from([("all".to_string(), "everywhere".to_string())]),
        );
        store.insert(
            "ONLY_PROD".to_string(),
            BTreeMap::from([("production".to_string(), "prod".to_string())]),
        );

        let view = SecretView::new("staging".to_string(), store, BTreeMap::new());
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "K"
            }),
            Resolution::Found("fallback")
        ));
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "ONLY_ALL"
            }),
            Resolution::Found("everywhere")
        ));
        // The whole point: staging never falls back to production's value.
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "ONLY_PROD"
            }),
            Resolution::MissingKey
        ));
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "ABSENT"
            }),
            Resolution::MissingKey
        ));
    }

    #[test]
    fn an_unpopulated_namespace_is_told_apart_from_a_missing_key() {
        let namespaces = BTreeMap::from([(
            "vercel".to_string(),
            BTreeMap::from([(
                "PRESENT".to_string(),
                BTreeMap::from([("production".to_string(), "v".to_string())]),
            )]),
        )]);
        let view = SecretView::new("production".to_string(), BTreeMap::new(), namespaces);

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "PRESENT"
            }),
            Resolution::Found("v")
        ));
        // The dog is up and simply does not have this key: a person's problem.
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "ABSENT"
            }),
            Resolution::MissingKey
        ));
        // No dog has ever pushed under this name: transient, retry.
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vault"),
                key: "ANY"
            }),
            Resolution::MissingNamespace
        ));
    }

    #[test]
    fn a_reference_displays_the_way_an_operator_wrote_it() {
        assert_eq!(
            SecretRef {
                namespace: None,
                key: "K"
            }
            .to_string(),
            "{{secret:K}}"
        );
        assert_eq!(
            SecretRef {
                namespace: Some("vercel"),
                key: "K"
            }
            .to_string(),
            "{{secret:vercel/K}}"
        );
    }

    /// IR-41. Fails the moment somebody replaces the hand-written impl with
    /// a derive, which is the only way this leak comes back.
    #[test]
    fn debug_never_prints_a_value() {
        let store = BTreeMap::from([(
            "K".to_string(),
            BTreeMap::from([("production".to_string(), "hunter2".to_string())]),
        )]);
        let view = SecretView::new("production".to_string(), store, BTreeMap::new());
        let rendered = format!("{view:?}");
        assert_eq!(
            rendered,
            "SecretView { environment: \"production\", keys: 1, namespaces: 0 }"
        );
        assert!(!rendered.contains("hunter2"));
    }

    /// IR-41, the same guard for the type a resolved value travels in.
    #[test]
    fn a_resolution_debug_never_prints_the_value_it_found() {
        assert_eq!(format!("{:?}", Resolution::Found("hunter2")), "Found(..)");
        assert_eq!(format!("{:?}", Resolution::MissingKey), "MissingKey");
        assert_eq!(
            format!("{:?}", Resolution::MissingNamespace),
            "MissingNamespace"
        );
    }

    #[test]
    fn error_messages_name_the_key_and_never_a_value() {
        let err = SecretError::ValueTooLong {
            key: "K".to_string(),
            len: 9999,
        };
        let rendered = err.to_string();
        assert!(rendered.contains('K'), "{rendered}");
        assert!(rendered.contains("9999"), "{rendered}");
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }
}
