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
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;
// `PathBuf` backs `lock_path` below, gated the same way for both platform
// arms of `SecretLock`.
#[cfg(any(unix, windows))]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::config::template;

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
/// Non-empty, at most [`MAX_KEY_BYTES`], not starting with `.`, and drawn
/// from `[A-Za-z0-9._-]`. Excludes `/`, so a name can never contain the
/// separator [`SecretRef::parse`] splits a namespace from a key on.
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
        /// enough that a lock held for a normal `set`/`unset`'s duration (a
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

/// Reads whichever version of the store `path` currently names.
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

/// Every `{{secret:...}}` reference `config` names, exactly as the operator
/// wrote it (`KEY` or `namespace/KEY`, no braces), deduplicated.
///
/// Walks `env`'s values, `args`, `out_file` and `err_file` through
/// `template`'s own tokenizer, the same one [`template::render`] resolves
/// against at spawn: a value this misses is one `render` would not touch
/// either, and a positional token (`{{instance}}`, `{{name}}`) contributes
/// nothing.
#[must_use]
pub fn references(config: &AppConfig) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut scan = |value: &str| {
        let _ = template::walk::<core::convert::Infallible>(value, |segment| {
            if let template::Segment::Token(token) = segment
                && let Some(reference) = template::secret_reference(token)
            {
                found.insert(match reference.namespace {
                    Some(namespace) => format!("{namespace}/{}", reference.key),
                    None => reference.key.to_string(),
                });
            }
            Ok(())
        });
    };
    for value in config.env.values() {
        scan(value);
    }
    for value in &config.args {
        scan(value);
    }
    if let Some(value) = &config.out_file {
        scan(value);
    }
    if let Some(value) = &config.err_file {
        scan(value);
    }
    found
}

/// The provider namespaces [`references`] names, derived with
/// [`SecretRef::parse`] and kept to the namespaced half.
///
/// No I/O of its own: the seam boot-dependency ordering asks "which
/// provider namespaces does this sheep depend on" through.
#[must_use]
pub fn namespaces_of(config: &AppConfig) -> BTreeSet<String> {
    references(config)
        .iter()
        .filter_map(|reference| SecretRef::parse(reference))
        .filter_map(|reference| reference.namespace.map(str::to_string))
        .collect()
}

/// `namespace -> key -> environment -> value`, every provider dog's pushed
/// values.
pub type NamespaceValues = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

/// `namespace -> environments`, the pairs a provider dog has actually
/// pushed for.
///
/// A push carries one namespace and one environment
/// (`Request::PutSecrets`), so a provider that has pushed `production` and
/// not yet `staging` has one entry here, not two. That distinction is what
/// [`Resolution::MissingNamespace`] is keyed on.
pub type PushedPairs = BTreeMap<String, BTreeSet<String>>;

/// What provider dogs have pushed: the values, and which
/// `(namespace, environment)` pairs carry a push at all.
///
/// The two travel together because they are read together and must come
/// from one moment: a values map from before a push read beside a pair set
/// from after it would call a key permanently missing that the push had
/// just supplied.
///
/// An empty push is why the pair set is not derivable from the values. A
/// dog saying "I have nothing for staging" registers the pair and holds no
/// keys, which is a different answer from a dog that has not pushed.
///
/// Debug does not leak a value: it prints two counts.
#[derive(Default, Clone)]
pub struct ProviderCache {
    /// Every namespace's values.
    pub values: NamespaceValues,
    /// Every `(namespace, environment)` pair a push has landed for.
    pub pushed: PushedPairs,
}

/// Redacted (IR-41): `values` holds provider values in the clear.
impl fmt::Debug for ProviderCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderCache")
            .field("namespaces", &self.values.len())
            .field("pushed", &self.pushed.len())
            .finish()
    }
}

/// The on-disk shape of `secrets-cache.json`, mirrored from
/// `shep-daemon`'s own private writer so a reader on this side of the
/// crate boundary can stay in step with it without importing a published
/// binary crate's internals.
#[derive(Default, Deserialize)]
struct ProviderCacheFile {
    version: u32,
    #[serde(default)]
    namespaces: NamespaceValues,
    #[serde(default)]
    pushed: PushedPairs,
}

/// Redacted (IR-41), matching `shep-daemon`'s own `CacheFile`: `namespaces`
/// holds provider values in the clear.
impl fmt::Debug for ProviderCacheFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderCacheFile")
            .field("version", &self.version)
            .field("namespaces", &self.namespaces.len())
            .field("pushed", &self.pushed.len())
            .finish()
    }
}

/// The cache file version [`provider_cache_on_disk`] understands.
///
/// Matches `shep-daemon`'s own `CACHE_VERSION`; a mismatch there and here
/// is a drift this module cannot detect on its own; the two constants
/// carry the same comment for that reason.
const PROVIDER_CACHE_VERSION: u32 = 2;

/// The provider cache as `secrets-cache.json` currently holds it on disk,
/// or nothing when the file is missing, will not parse, or is a version
/// this build does not understand.
///
/// Best-effort, more so than [`all`]: a namespace whose provider pushed
/// with `persist = false` never reaches this file at all, so a caller here
/// can under-report `MissingNamespace` for a pair the running shepherd
/// currently holds in memory. A caller that needs the shepherd's live
/// answer has to ask it directly rather than read this file.
#[must_use]
pub fn provider_cache_on_disk(path: &Path) -> ProviderCache {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ProviderCache::default();
    };
    match serde_json::from_str::<ProviderCacheFile>(&raw) {
        Ok(file) if file.version == PROVIDER_CACHE_VERSION => ProviderCache {
            values: file.namespaces,
            pushed: file.pushed,
        },
        _ => ProviderCache::default(),
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
    providers: ProviderCache,
}

/// Redacted (IR-41): `store` and the provider cache hold secret values.
impl fmt::Debug for SecretView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretView")
            .field("environment", &self.environment)
            .field("keys", &self.store.len())
            .field("namespaces", &self.providers.values.len())
            .finish()
    }
}

impl SecretView {
    /// A view over `store` and `providers`, resolved for `environment`.
    #[must_use]
    pub fn new(
        environment: String,
        store: BTreeMap<String, BTreeMap<String, String>>,
        providers: ProviderCache,
    ) -> Self {
        Self {
            environment,
            store,
            providers,
        }
    }

    /// A view holding nothing, so a bare reference resolves to
    /// [`Resolution::MissingKey`] and a namespaced one to
    /// [`Resolution::MissingNamespace`].
    #[must_use]
    pub fn empty(environment: String) -> Self {
        Self::new(environment, BTreeMap::new(), ProviderCache::default())
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
    ///
    /// A miss on a namespaced reference is [`Resolution::MissingNamespace`]
    /// unless a provider has pushed this view's own environment for that
    /// namespace: a push carries one `(namespace, environment)` pair, so a
    /// dog part way through `production` then `staging` has said nothing
    /// about staging yet, and calling that a missing key would `Errored` a
    /// staging sheep permanently for a value arriving a second later.
    #[must_use]
    pub fn resolve(&self, reference: &SecretRef<'_>) -> Resolution<'_> {
        let table = match reference.namespace {
            None => Some(&self.store),
            Some(namespace) => self.providers.values.get(namespace),
        };
        if let Some(value) =
            table
                .and_then(|table| table.get(reference.key))
                .and_then(|by_environment| {
                    by_environment
                        .get(&self.environment)
                        .or_else(|| by_environment.get(ALL_ENVIRONMENTS))
                })
        {
            return Resolution::Found(value.as_str());
        }
        match reference.namespace {
            None => Resolution::MissingKey,
            Some(namespace) if self.is_pushed(namespace) => Resolution::MissingKey,
            Some(_) => Resolution::MissingNamespace,
        }
    }

    /// Whether a provider has pushed `namespace` for this view's own
    /// environment.
    fn is_pushed(&self, namespace: &str) -> bool {
        self.providers
            .pushed
            .get(namespace)
            .is_some_and(|environments| environments.contains(&self.environment))
    }
}

/// The outcome of resolving one [`SecretRef`].
///
/// [`Self::MissingKey`] and [`Self::MissingNamespace`] are kept apart
/// because they need different remedies: a pair no dog has pushed is a dog
/// that has not reported yet, which a retry fixes, while a missing key is a
/// person's to set.
///
/// Debug does not leak a value: [`Self::Found`] prints as `Found(..)`.
pub enum Resolution<'a> {
    /// The resolved value.
    Found(&'a str),
    /// The operator's store holds nothing for this key, or a provider has
    /// pushed this namespace for this environment and that push lacks the
    /// key.
    MissingKey,
    /// No provider dog has pushed this namespace for this environment yet.
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
    fn references_finds_every_secret_in_a_config_and_nothing_else() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".into(), "{{secret:ONE}}".into());
        config.env.insert("B".into(), "plain".into());
        config
            .env
            .insert("C".into(), "{{name}}-{{secret:vercel/TWO}}".into());
        config.args = vec!["--x={{secret:ONE}}".into()];

        let found = references(&config);
        assert_eq!(
            found,
            BTreeSet::from(["ONE".to_string(), "vercel/TWO".to_string()]),
            "deduplicated, and no positional tokens"
        );
    }

    #[test]
    fn namespaces_of_a_config_is_the_seam_boot_ordering_will_want() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".into(), "{{secret:ONE}}".into());
        config
            .env
            .insert("B".into(), "{{secret:vercel/TWO}}".into());
        assert_eq!(
            namespaces_of(&config),
            BTreeSet::from(["vercel".to_string()])
        );
    }

    #[test]
    fn provider_cache_on_disk_reads_a_real_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-cache.json");
        std::fs::write(
            &path,
            r#"{"version":2,"namespaces":{"vercel":{"API_KEY":{"production":"sk_live"}}},"pushed":{"vercel":["production"]}}"#,
        )
        .unwrap();
        let cache = provider_cache_on_disk(&path);
        assert_eq!(cache.values["vercel"]["API_KEY"]["production"], "sk_live");
        assert_eq!(
            cache.pushed["vercel"],
            BTreeSet::from(["production".to_string()])
        );
    }

    #[test]
    fn provider_cache_on_disk_is_empty_for_a_missing_or_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            provider_cache_on_disk(&dir.path().join("absent.json"))
                .values
                .is_empty()
        );

        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "not json").unwrap();
        assert!(provider_cache_on_disk(&broken).values.is_empty());

        let future = dir.path().join("future.json");
        std::fs::write(&future, r#"{"version":999,"namespaces":{}}"#).unwrap();
        assert!(provider_cache_on_disk(&future).values.is_empty());
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

        let view = SecretView::new("staging".to_string(), store, ProviderCache::default());
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

    /// A cache holding `vercel/PRESENT` for `production`, pushed as that
    /// one pair.
    fn vercel_production() -> ProviderCache {
        ProviderCache {
            values: BTreeMap::from([(
                "vercel".to_string(),
                BTreeMap::from([(
                    "PRESENT".to_string(),
                    BTreeMap::from([("production".to_string(), "v".to_string())]),
                )]),
            )]),
            pushed: BTreeMap::from([(
                "vercel".to_string(),
                BTreeSet::from(["production".to_string()]),
            )]),
        }
    }

    #[test]
    fn an_unpopulated_namespace_is_told_apart_from_a_missing_key() {
        let view = SecretView::new(
            "production".to_string(),
            BTreeMap::new(),
            vercel_production(),
        );

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

    /// The case this pair set exists for. A provider pushes `production`
    /// and then `staging`, which is the ordinary shape, and a staging sheep
    /// spawning between the two must wait on the restart ladder rather than
    /// `Errored` for good. Keying on the namespace alone calls the second
    /// push's keys permanently missing the moment the first push lands.
    #[test]
    fn a_namespace_pushed_for_another_environment_is_not_populated_for_this_one() {
        let view = SecretView::new("staging".to_string(), BTreeMap::new(), vercel_production());

        assert!(
            matches!(
                view.resolve(&SecretRef {
                    namespace: Some("vercel"),
                    key: "PRESENT"
                }),
                Resolution::MissingNamespace
            ),
            "staging has had no push, so waiting is what fixes this"
        );
    }

    /// The other half, and the one that must stay permanent: the pair has
    /// been pushed and the key is not in it, so the provider genuinely does
    /// not have it and no amount of waiting will produce one.
    #[test]
    fn a_pushed_pair_missing_a_key_stays_permanent() {
        let view = SecretView::new(
            "production".to_string(),
            BTreeMap::new(),
            vercel_production(),
        );

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "ABSENT"
            }),
            Resolution::MissingKey
        ));
    }

    /// An empty push is a dog saying it holds nothing for that pair, which
    /// is an answer. Deriving the pairs from the values would lose it and
    /// leave such a sheep retrying against a dog that has already spoken.
    #[test]
    fn an_empty_push_populates_the_pair_it_carried() {
        let view = SecretView::new(
            "production".to_string(),
            BTreeMap::new(),
            ProviderCache {
                values: BTreeMap::from([("vercel".to_string(), BTreeMap::new())]),
                pushed: BTreeMap::from([(
                    "vercel".to_string(),
                    BTreeSet::from(["production".to_string()]),
                )]),
            },
        );

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "ANY"
            }),
            Resolution::MissingKey
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
        let view = SecretView::new("production".to_string(), store, ProviderCache::default());
        let rendered = format!("{view:?}");
        assert_eq!(
            rendered,
            "SecretView { environment: \"production\", keys: 1, namespaces: 0 }"
        );
        assert!(!rendered.contains("hunter2"));
    }

    /// IR-41, the same guard for the on-disk shape everything else is built
    /// from. `SecretFile` is private, so `missing_debug_implementations`
    /// never forces it to keep a `Debug` impl at all; this is what stops a
    /// later edit from deriving one over the hand-written redaction.
    #[test]
    fn a_secret_file_debug_never_prints_a_value() {
        let file = SecretFile {
            version: SECRETS_VERSION,
            entries: BTreeMap::from([(
                "K".to_string(),
                BTreeMap::from([("production".to_string(), "hunter2".to_string())]),
            )]),
        };
        let rendered = format!("{file:?}");
        assert_eq!(rendered, "SecretFile { version: 1, keys: 1 }");
        assert!(!rendered.contains("hunter2"));
    }

    /// IR-41, the same guard for the on-disk shape [`provider_cache_on_disk`]
    /// reads: it mirrors `shep-daemon`'s `CacheFile`, values in the clear
    /// included.
    #[test]
    fn a_provider_cache_file_debug_never_prints_a_value() {
        let file = ProviderCacheFile {
            version: PROVIDER_CACHE_VERSION,
            namespaces: BTreeMap::from([(
                "vercel".to_string(),
                BTreeMap::from([(
                    "API_KEY".to_string(),
                    BTreeMap::from([("production".to_string(), "sk_live".to_string())]),
                )]),
            )]),
            pushed: BTreeMap::from([(
                "vercel".to_string(),
                BTreeSet::from(["production".to_string()]),
            )]),
        };
        let rendered = format!("{file:?}");
        assert_eq!(
            rendered,
            "ProviderCacheFile { version: 2, namespaces: 1, pushed: 1 }"
        );
        assert!(!rendered.contains("sk_live"));
    }

    /// IR-41 for the type the two halves travel in together.
    #[test]
    fn a_provider_cache_debug_never_prints_a_value() {
        let cache = vercel_production();
        assert_eq!(
            format!("{cache:?}"),
            "ProviderCache { namespaces: 1, pushed: 1 }"
        );
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
