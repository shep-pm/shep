//! `secrets-cache.json`: what provider dogs have pushed.
//!
//! A `{{secret:vercel/API_KEY}}` reference reads the namespace `vercel`,
//! which is whatever the dog of that name last pushed over
//! `Request::PutSecrets`. The values live in memory; the cache file exists
//! so a shepherd that restarts still resolves them before the dog's next
//! poll comes round.
//!
//! Derived, never authored: a file that will not read is skipped rather
//! than refused, unlike [`shep_core::secrets`], which holds the operator's
//! own store and refuses instead of guessing.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use shep_core::secrets::{NamespaceValues, ProviderCache, PushedPairs};

/// The cache file's format version.
///
/// A file carrying any other version is read as empty: nothing here is
/// authored, so a refetch costs one provider poll.
///
/// Matches `shep_core::secrets`'s own `PROVIDER_CACHE_VERSION`, which is
/// what `shep describe` reads this file with; a mismatch there and here is
/// a drift neither side can detect on its own, so the two constants carry
/// the same comment.
const CACHE_VERSION: u32 = 2;

/// The cache file's shape.
///
/// `BTreeMap` throughout so two writes of the same content produce
/// byte-identical files.
#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    namespaces: NamespaceValues,
    pushed: PushedPairs,
}

/// Redacted (IR-41): `namespaces` holds provider values in the clear.
impl fmt::Debug for CacheFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheFile")
            .field("version", &self.version)
            .field("namespaces", &self.namespaces.len())
            .field("pushed", &self.pushed.len())
            .finish()
    }
}

/// What is pushed, and what of it may reach disk.
#[derive(Default)]
struct State {
    /// Every namespace a dog has pushed to, empty push included.
    namespaces: NamespaceValues,
    /// Every `(namespace, environment)` pair a push has landed for. A push
    /// carries one pair, so a dog that has done `production` and not yet
    /// `staging` is present here for one of the two, which is what keeps a
    /// staging sheep on the restart ladder instead of `Errored`.
    pushed: PushedPairs,
    /// The namespaces whose most recent push asked to persist. Exactly what
    /// [`write_cache`] writes, so a namespace an operator set
    /// `persist = false` for is never carried along by a neighbour's write.
    persisted: BTreeSet<String>,
}

/// Every provider dog's pushed values, and the cache file the persisting
/// ones survive a restart in.
///
/// Shared between the connection task that serves `Request::PutSecrets` and
/// the supervisor actor that reads a view out of it per spawn. The two
/// mutexes exist to keep those apart: `state` is never held across file
/// I/O, so the actor's [`Self::snapshot`] can never wait on an `fsync`,
/// and `writer` serializes the rewrites so the file ends up holding the
/// last push rather than whichever write finished last.
///
/// Lock order is `writer` then `state`, never the reverse.
pub struct ProviderSecrets {
    state: Mutex<State>,
    writer: Mutex<()>,
    cache: PathBuf,
}

/// Redacted (IR-41): the values are the whole point of this type.
impl fmt::Debug for ProviderSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderSecrets")
            .field("namespaces", &self.state().namespaces.len())
            .finish()
    }
}

impl ProviderSecrets {
    /// Reads `cache`, or starts empty when there is nothing readable there.
    ///
    /// Everything loaded counts as persisting: it came off disk, so the
    /// setting that put it there was `persist = true` and stays so until a
    /// push says otherwise.
    pub fn load(cache: &Path) -> Self {
        let cached = read_cache(cache);
        let persisted = cached.values.keys().cloned().collect();
        Self {
            state: Mutex::new(State {
                namespaces: cached.values,
                pushed: cached.pushed,
                persisted,
            }),
            writer: Mutex::new(()),
            cache: cache.to_path_buf(),
        }
    }

    /// Replaces `namespace`'s values for `environment` with `entries`,
    /// returning how many are stored for that pair.
    ///
    /// Replaces rather than merges: a key the provider has deleted is
    /// absent from the push, and must be absent here too. Other
    /// environments of the same namespace are untouched.
    ///
    /// `persist` decides whether the namespace may reach `cache`, and it is
    /// read per push rather than settled once: a push that turns it off
    /// takes the namespace out of the file on the spot. An empty `entries`
    /// still registers the namespace, because "the dog has nothing" and "no
    /// dog has pushed" send an operator to different places.
    ///
    /// # Errors
    /// [`std::io::Error`] if the cache had to be rewritten and the staging
    /// file, the `fsync` or the `rename` failed. The in-memory values are
    /// already updated in that case: a spawn resolves against them, and
    /// only a restart loses them.
    pub fn put(
        &self,
        namespace: &str,
        environment: &str,
        entries: BTreeMap<String, String>,
        persist: bool,
    ) -> std::io::Result<u32> {
        // Saturating rather than fallible: `MAX_FRAME_BYTES` caps a request
        // long before a push can carry four billion entries.
        let accepted = u32::try_from(entries.len()).unwrap_or(u32::MAX);

        // Held for the whole call, the write included, so two pushes cannot
        // land their files out of order. `state` below is not: it is
        // released before the write, which is what keeps `snapshot` off the
        // disk.
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);

        let pending = {
            let mut state = self.state();
            let table = state.namespaces.entry(namespace.to_owned()).or_default();
            table.retain(|_, by_environment| {
                by_environment.remove(environment);
                !by_environment.is_empty()
            });
            for (key, value) in entries {
                table
                    .entry(key)
                    .or_default()
                    .insert(environment.to_owned(), value);
            }

            state
                .pushed
                .entry(namespace.to_owned())
                .or_default()
                .insert(environment.to_owned());

            let rewrite = if persist {
                state.persisted.insert(namespace.to_owned());
                true
            } else {
                state.persisted.remove(namespace)
            };
            rewrite.then(|| persisted_view(&state))
        };

        if let Some(file) = pending {
            write_cache(&self.cache, &file)?;
        }
        Ok(accepted)
    }

    /// Every namespace's values and every pair pushed for them, in the
    /// shape [`shep_core::secrets::SecretView::new`] takes.
    ///
    /// Cloned rather than borrowed: the supervisor holds the result across
    /// a whole resolution pass, and a guard held that long would be the
    /// thing a push waits on. Both halves come out under one lock, so a
    /// resolution never reads values from before a push beside the pairs
    /// from after it.
    pub fn snapshot(&self) -> ProviderCache {
        let state = self.state();
        ProviderCache {
            values: state.namespaces.clone(),
            pushed: state.pushed.clone(),
        }
    }

    /// Every `(namespace, environment)` pair a dog has pushed, values not
    /// included.
    pub fn pushed(&self) -> PushedPairs {
        self.state().pushed.clone()
    }

    /// A poisoned lock is recovered rather than propagated, as
    /// [`crate::rpc::KnownDogs`] does: a panic mid-push can leave a
    /// namespace half replaced, and the next push replaces it whole.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The cache file `state` should hold: its persisting namespaces and no
/// others.
fn persisted_view(state: &State) -> CacheFile {
    CacheFile {
        version: CACHE_VERSION,
        namespaces: state
            .persisted
            .iter()
            .filter_map(|name| {
                state
                    .namespaces
                    .get(name)
                    .map(|table| (name.clone(), table.clone()))
            })
            .collect(),
        pushed: state
            .persisted
            .iter()
            .filter_map(|name| {
                state
                    .pushed
                    .get(name)
                    .map(|environments| (name.clone(), environments.clone()))
            })
            .collect(),
    }
}

/// Whatever `path` holds, or nothing.
fn read_cache(path: &Path) -> ProviderCache {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ProviderCache::default();
    };
    match serde_json::from_str::<CacheFile>(&raw) {
        Ok(file) if file.version == CACHE_VERSION => ProviderCache {
            values: file.namespaces,
            pushed: file.pushed,
        },
        _ => ProviderCache::default(),
    }
}

/// Rewrites `path` to hold exactly `file`: staged owner-only beside it,
/// `fsync`ed, then renamed over the original.
///
/// # Errors
/// [`std::io::Error`] from the staging file, the write, either `fsync`, or
/// the `rename`. A failed rename leaves `path` as it was and removes the
/// staging file.
fn write_cache(path: &Path, file: &CacheFile) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = shep_core::atomic_file::create_staging_file(parent, "secrets-cache", ".tmp")?;

    let json = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    tmp.write_all(json.as_bytes())?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;

    tmp.persist(path).map_err(|err| err.error)?;
    shep_core::atomic_file::sync_dir(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_replaces_that_namespace_and_environment_rather_than_merging() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        let store = ProviderSecrets::load(&cache);

        store
            .put(
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "1".into()), ("B".into(), "2".into())]),
                false,
            )
            .unwrap();
        // B is gone at the provider, so it must be gone here too.
        store
            .put(
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "9".into())]),
                false,
            )
            .unwrap();

        let snap = store.snapshot();
        let vercel = &snap.values["vercel"];
        assert_eq!(vercel["A"]["production"], "9");
        assert!(
            !vercel.contains_key("B"),
            "a replaced push drops what it omits"
        );
    }

    #[test]
    fn one_environment_does_not_disturb_another() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        store
            .put(
                "v",
                "production",
                BTreeMap::from([("A".into(), "p".into())]),
                false,
            )
            .unwrap();
        store
            .put(
                "v",
                "staging",
                BTreeMap::from([("A".into(), "s".into())]),
                false,
            )
            .unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.values["v"]["A"]["production"], "p");
        assert_eq!(snap.values["v"]["A"]["staging"], "s");
        assert_eq!(
            snap.pushed["v"],
            BTreeSet::from(["production".to_string(), "staging".to_string()]),
            "both pairs carry a push"
        );
    }

    #[test]
    fn a_persisted_push_survives_a_reload_and_an_unpersisted_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");

        let first = ProviderSecrets::load(&cache);
        first
            .put(
                "kept",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                true,
            )
            .unwrap();
        first
            .put(
                "gone",
                "production",
                BTreeMap::from([("B".into(), "2".into())]),
                false,
            )
            .unwrap();

        let second = ProviderSecrets::load(&cache);
        assert!(second.pushed().contains_key("kept"));
        assert!(!second.pushed().contains_key("gone"));
    }

    /// The reverse order of the test above, which is the one that matters:
    /// the unpersisted namespace is already in memory when the persisted
    /// push rewrites the file, so it is a bystander the write could sweep
    /// in against the operator's `persist = false`.
    #[test]
    fn an_unpersisted_namespace_is_not_swept_in_by_a_later_persisted_write() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");

        let first = ProviderSecrets::load(&cache);
        first
            .put(
                "unpersisted",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                false,
            )
            .unwrap();
        first
            .put(
                "persisted",
                "production",
                BTreeMap::from([("B".into(), "2".into())]),
                true,
            )
            .unwrap();

        let second = ProviderSecrets::load(&cache);
        assert_eq!(
            second.pushed().into_keys().collect::<BTreeSet<_>>(),
            BTreeSet::from(["persisted".to_string()])
        );
        let raw = std::fs::read_to_string(&cache).unwrap();
        assert!(
            !raw.contains("unpersisted"),
            "the unpersisted namespace reached disk: {raw}"
        );
    }

    /// A namespace the operator has stopped persisting leaves the file on
    /// the push that says so, rather than lingering until some other
    /// namespace happens to rewrite it.
    #[test]
    fn dropping_persist_removes_that_namespace_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");

        let first = ProviderSecrets::load(&cache);
        first
            .put(
                "v",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                true,
            )
            .unwrap();
        first
            .put(
                "v",
                "production",
                BTreeMap::from([("A".into(), "2".into())]),
                false,
            )
            .unwrap();

        assert!(ProviderSecrets::load(&cache).pushed().is_empty());
    }

    #[test]
    fn a_namespace_is_known_as_soon_as_it_is_pushed_even_when_empty() {
        // MissingNamespace means "no dog has pushed here"; an empty push is
        // a dog saying it has nothing, which is a different answer.
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        store
            .put("v", "production", BTreeMap::new(), false)
            .unwrap();
        assert!(store.pushed()["v"].contains("production"));
    }

    #[test]
    #[cfg(unix)]
    fn the_cache_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        ProviderSecrets::load(&cache)
            .put(
                "v",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                true,
            )
            .unwrap();
        let mode = std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// A cache file this build cannot read is a refetch, not a boot
    /// failure: nothing in it is authored, and the dog that wrote it
    /// rewrites it on its next push.
    #[test]
    fn an_unreadable_cache_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        std::fs::write(&cache, "{ not json").unwrap();
        assert!(ProviderSecrets::load(&cache).pushed().is_empty());
    }

    /// A push carries one `(namespace, environment)` pair, so a provider
    /// part way through `production` then `staging` has said nothing about
    /// staging. The pair set is what says so, since the values alone cannot:
    /// a namespace with production keys in it looks populated from any
    /// environment.
    #[test]
    fn a_push_registers_the_pair_it_carried_and_not_the_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        store
            .put(
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                false,
            )
            .unwrap();

        let snap = store.snapshot();
        assert!(snap.values.contains_key("vercel"), "the values are there");
        assert_eq!(snap.pushed["vercel"], BTreeSet::from(["production".to_string()]));
    }

    /// The pairs ride the cache file, so a shepherd that restarts still
    /// tells "the dog has not pushed staging" from "staging has no such
    /// key". Deriving them from the values would lose an empty push.
    #[test]
    fn the_pairs_survive_a_reload_through_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        let first = ProviderSecrets::load(&cache);
        first
            .put(
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                true,
            )
            .unwrap();
        first.put("vercel", "staging", BTreeMap::new(), true).unwrap();

        let second = ProviderSecrets::load(&cache);
        assert_eq!(
            second.pushed()["vercel"],
            BTreeSet::from(["production".to_string(), "staging".to_string()]),
            "an empty push is a push"
        );
    }

    /// A namespace an operator stopped persisting leaves the file whole:
    /// its pairs must not linger there for a namespace whose values are
    /// gone.
    #[test]
    fn an_unpersisted_namespaces_pairs_do_not_reach_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        let first = ProviderSecrets::load(&cache);
        first
            .put(
                "unpersisted",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                false,
            )
            .unwrap();
        first
            .put(
                "persisted",
                "production",
                BTreeMap::from([("B".into(), "2".into())]),
                true,
            )
            .unwrap();

        let raw = std::fs::read_to_string(&cache).unwrap();
        assert!(
            !raw.contains("unpersisted"),
            "neither its values nor its pairs: {raw}"
        );
    }

    /// IR-41 for the on-disk shape, the way shep-core's mirror of it is
    /// pinned. `CacheFile` is private, so `missing_debug_implementations`
    /// never forces it to keep a `Debug` at all; this is what stops a later
    /// edit from deriving one over the hand-written redaction.
    #[test]
    fn a_cache_file_debug_never_prints_a_value() {
        let file = CacheFile {
            version: CACHE_VERSION,
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
            "CacheFile { version: 2, namespaces: 1, pushed: 1 }"
        );
        assert!(!rendered.contains("sk_live"));
    }

    /// IR-41.
    #[test]
    fn debug_never_prints_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        store
            .put(
                "v",
                "production",
                BTreeMap::from([("A".into(), "hunter2".into())]),
                false,
            )
            .unwrap();
        let rendered = format!("{store:?}");
        assert_eq!(rendered, "ProviderSecrets { namespaces: 1 }");
    }
}
