//! `secrets-cache.json`: what provider dogs have pushed.
//!
//! A `{{secret:vercel/API_KEY}}` reference reads the namespace `vercel`,
//! which is whatever the dog of that name last pushed over
//! `Request::PutSecrets`. The values live in memory; the cache file exists
//! so a shepherd that restarts still resolves them before the dog's next
//! poll comes round.
//!
//! Derived, never authored: a file that will not read is skipped rather
//! than refused, unlike [`shep_core::secrets`], which holds the operator's own store.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use shep_core::secrets::{
    ALL_ENVIRONMENTS, NamespaceValues, PROVIDER_CACHE_VERSION, ProviderCache, PushedPairs,
};

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

/// What a push stored.
#[derive(Debug, PartialEq, Eq)]
pub struct Pushed {
    /// How many entries are stored for the pushed `(namespace, environment)`
    pub accepted: u32,
    /// The keys whose value in the host environment moved: missing to found,
    /// found to missing, or one value to another
    ///
    /// Key names only, never a value (IR-41).
    pub changed: BTreeSet<String>,
}

/// The push landed in memory and the cache file could not be rewritten.
///
/// Carries what [`Pushed`] would have, because the values are resolvable
/// either way and a rotation in them has happened.
#[derive(Debug)]
pub struct CacheWriteFailed {
    /// Why the staging file, the `fsync` or the `rename` failed
    pub source: std::io::Error,
    /// As [`Pushed::changed`]
    pub changed: BTreeSet<String>,
}

impl fmt::Display for CacheWriteFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.source, f)
    }
}

impl core::error::Error for CacheWriteFailed {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Every provider dog's pushed values, and the cache file the persisting
/// ones survive a restart in.
///
/// Shared between the connection task that serves `Request::PutSecrets` and
/// the supervisor actor that reads a view out of it per spawn. `state` is
/// never held across file I/O, so the actor's [`Self::snapshot`] can never
/// wait on an `fsync`, and `writer` serializes the rewrites.
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
        let (namespaces, pushed) = read_cache(cache).into_parts();
        let persisted = namespaces.keys().cloned().collect();
        Self {
            state: Mutex::new(State {
                namespaces,
                pushed,
                persisted,
            }),
            writer: Mutex::new(()),
            cache: cache.to_path_buf(),
        }
    }

    /// Replaces `namespace`'s values for `environment` with `entries`,
    /// returning how many are stored for that pair and which keys now
    /// resolve to something else in `host_environment`.
    ///
    /// Replaces rather than merges: a key the provider has deleted is
    /// absent from the push, and must be absent here too. `persist` decides
    /// whether the namespace may reach `cache`, read per push rather than
    /// settled once. An empty `entries` still registers the namespace.
    ///
    /// # Errors
    /// [`CacheWriteFailed`] if the cache had to be rewritten and the write
    /// failed. The in-memory values are already updated in that case.
    pub fn put(
        &self,
        host_environment: &str,
        namespace: &str,
        environment: &str,
        entries: BTreeMap<String, String>,
        persist: bool,
    ) -> Result<Pushed, CacheWriteFailed> {
        // Saturating rather than fallible: `MAX_FRAME_BYTES` caps a request
        // long before a push can carry four billion entries.
        let accepted = u32::try_from(entries.len()).unwrap_or(u32::MAX);

        // Held for the whole call, the write included, so two pushes cannot
        // land their files out of order. `state` below is not: it is
        // released before the write, which is what keeps `snapshot` off the
        // disk.
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);

        let (pending, changed) = {
            let mut state = self.state();
            let changed = changed_keys(
                state.namespaces.get(namespace),
                host_environment,
                environment,
                &entries,
            );
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
            (rewrite.then(|| persisted_view(&state)), changed)
        };

        if let Some(file) = pending {
            write_cache(&self.cache, &file).map_err(|source| CacheWriteFailed {
                source,
                changed: changed.clone(),
            })?;
        }
        Ok(Pushed { accepted, changed })
    }

    /// Every namespace's values and every pair pushed for them, in the
    /// shape [`shep_core::secrets::SecretView::new`] takes.
    ///
    /// Cloned rather than borrowed: the supervisor holds the result across
    /// a whole resolution pass, and a guard held that long would be the
    /// thing a push waits on. Both halves come out under one lock.
    pub fn snapshot(&self) -> ProviderCache {
        let state = self.state();
        ProviderCache::new(state.namespaces.clone(), state.pushed.clone())
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

/// The keys of `table` and `entries` that resolve to something else in
/// `host_environment` once `entries` replaces `environment`'s slots.
///
/// Worked out from the slots rather than by applying the push to a copy, so
/// the hot path clones nothing. A push to an environment the host cannot
/// see moves no resolution.
fn changed_keys(
    table: Option<&BTreeMap<String, BTreeMap<String, String>>>,
    host_environment: &str,
    environment: &str,
    entries: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    let visible = environment == host_environment || environment == ALL_ENVIRONMENTS;
    if !visible {
        return BTreeSet::new();
    }
    let replaced = table
        .into_iter()
        .flatten()
        .filter(|(_, slots)| slots.contains_key(environment))
        .map(|(key, _)| key);
    replaced
        .chain(entries.keys())
        .filter(|key| {
            let slots = table.and_then(|table| table.get(*key));
            let slot = |name: &str| slots.and_then(|slots| slots.get(name)).map(String::as_str);
            let before = slot(host_environment).or_else(|| slot(ALL_ENVIRONMENTS));
            let pushed = entries.get(*key).map(String::as_str);
            let after = if environment == host_environment {
                pushed.or_else(|| slot(ALL_ENVIRONMENTS))
            } else {
                slot(host_environment).or(pushed)
            };
            before != after
        })
        .cloned()
        .collect()
}

/// The entries of `table` whose names are in `keys`.
///
/// A name in `keys` that `table` has no entry for is skipped: a namespace
/// marked persistable with nothing pushed for it writes no entry rather
/// than an empty one.
fn projected<V: Clone>(
    keys: &BTreeSet<String>,
    table: &BTreeMap<String, V>,
) -> BTreeMap<String, V> {
    keys.iter()
        .filter_map(|name| table.get(name).map(|value| (name.clone(), value.clone())))
        .collect()
}

/// The cache file `state` should hold: its persisting namespaces and no
/// others.
fn persisted_view(state: &State) -> CacheFile {
    CacheFile {
        version: PROVIDER_CACHE_VERSION,
        namespaces: projected(&state.persisted, &state.namespaces),
        pushed: projected(&state.persisted, &state.pushed),
    }
}

/// Whatever `path` holds, or nothing.
fn read_cache(path: &Path) -> ProviderCache {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ProviderCache::default();
    };
    match serde_json::from_str::<CacheFile>(&raw) {
        Ok(file) if file.version == PROVIDER_CACHE_VERSION => {
            ProviderCache::new(file.namespaces, file.pushed)
        }
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
    shep_core::atomic_file::write_json(path, "secrets-cache", file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "production";

    #[test]
    fn a_push_replaces_that_namespace_and_environment_rather_than_merging() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        let store = ProviderSecrets::load(&cache);

        store
            .put(
                HOST,
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "1".into()), ("B".into(), "2".into())]),
                false,
            )
            .unwrap();
        // B is gone at the provider, so it must be gone here too.
        store
            .put(
                HOST,
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "9".into())]),
                false,
            )
            .unwrap();

        let snap = store.snapshot();
        let vercel = snap.namespace("vercel").unwrap();
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
                HOST,
                "v",
                "production",
                BTreeMap::from([("A".into(), "p".into())]),
                false,
            )
            .unwrap();
        store
            .put(
                HOST,
                "v",
                "staging",
                BTreeMap::from([("A".into(), "s".into())]),
                false,
            )
            .unwrap();
        let snap = store.snapshot();
        let v = snap.namespace("v").unwrap();
        assert_eq!(v["A"]["production"], "p");
        assert_eq!(v["A"]["staging"], "s");
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
                HOST,
                "kept",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                true,
            )
            .unwrap();
        first
            .put(
                HOST,
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
                HOST,
                "unpersisted",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                false,
            )
            .unwrap();
        first
            .put(
                HOST,
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
                HOST,
                "v",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                true,
            )
            .unwrap();
        first
            .put(
                HOST,
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
            .put(HOST, "v", "production", BTreeMap::new(), false)
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
                HOST,
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
                HOST,
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                false,
            )
            .unwrap();

        let snap = store.snapshot();
        assert!(snap.namespace("vercel").is_some(), "the values are there");
        assert_eq!(
            snap.pushed["vercel"],
            BTreeSet::from(["production".to_string()])
        );
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
                HOST,
                "vercel",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                true,
            )
            .unwrap();
        first
            .put(HOST, "vercel", "staging", BTreeMap::new(), true)
            .unwrap();

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
                HOST,
                "unpersisted",
                "production",
                BTreeMap::from([("A".into(), "1".into())]),
                false,
            )
            .unwrap();
        first
            .put(
                HOST,
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

    #[test]
    fn projected_keeps_only_persisted_names_and_skips_absent_ones() {
        let keys = BTreeSet::from([
            "kept".to_string(),
            "absent".to_string(),
            "also-kept".to_string(),
        ]);
        let table = BTreeMap::from([
            ("kept".to_string(), 1u32),
            ("also-kept".to_string(), 2),
            ("unkeyed".to_string(), 3),
        ]);
        assert_eq!(
            projected(&keys, &table),
            BTreeMap::from([("kept".to_string(), 1u32), ("also-kept".to_string(), 2)]),
            "`absent` is in no table, `unkeyed` is in no persisted set"
        );
    }

    /// IR-41 for the on-disk shape, the way shep-core's mirror of it is
    /// pinned. `CacheFile` is private, so `missing_debug_implementations`
    /// never forces it to keep a `Debug` at all; this is what stops a later
    /// edit from deriving one over the hand-written redaction.
    #[test]
    fn a_cache_file_debug_never_prints_a_value() {
        let file = CacheFile {
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
                HOST,
                "v",
                "production",
                BTreeMap::from([("A".into(), "hunter2".into())]),
                false,
            )
            .unwrap();
        let rendered = format!("{store:?}");
        assert_eq!(rendered, "ProviderSecrets { namespaces: 1 }");
    }

    /// Only what a key resolves to in the host environment counts: the exact
    /// slot, then `all`. A push the host cannot see, or a value an exact
    /// slot shadows, moves nothing.
    #[test]
    fn a_push_reports_only_the_keys_whose_resolved_value_changed() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        let pairs = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect()
        };
        let changed = |keys: &[&str]| -> BTreeSet<String> {
            keys.iter().map(|key| (*key).to_string()).collect()
        };
        let put = |environment: &str, entries: &[(&str, &str)]| {
            store
                .put("production", "v", environment, pairs(entries), false)
                .unwrap()
                .changed
        };
        // Missing to found.
        assert_eq!(
            put(
                "production",
                &[("SAME", "1"), ("MOVED", "1"), ("DROPPED", "1")]
            ),
            changed(&["SAME", "MOVED", "DROPPED"]),
            "a key the host could not resolve before and can now"
        );
        // Unchanged, value to value, found to missing, missing to found.
        assert_eq!(
            put(
                "production",
                &[("SAME", "1"), ("MOVED", "2"), ("ADDED", "1")]
            ),
            changed(&["MOVED", "DROPPED", "ADDED"]),
        );
        assert_eq!(
            put(
                "production",
                &[("SAME", "1"), ("MOVED", "2"), ("ADDED", "1")]
            ),
            changed(&[])
        );
        // Another environment is invisible to the host.
        assert_eq!(
            put("staging", &[("SAME", "9"), ("OTHER", "9")]),
            changed(&[])
        );
        // `all` is the fallback: it moves what the host has no exact slot
        // for, and nothing an exact slot shadows.
        assert_eq!(
            put("all", &[("SAME", "9"), ("MOVED", "9"), ("FALLBACK", "9")]),
            changed(&["FALLBACK"]),
            "SAME and MOVED keep their production value"
        );
        // Dropping the exact slot uncovers the `all` value beneath it.
        assert_eq!(
            put("production", &[("SAME", "1"), ("ADDED", "1")]),
            changed(&["MOVED"]),
            "MOVED falls back to its `all` slot, which is a different value"
        );
        // A key with no `all` slot under it just goes missing.
        assert_eq!(put("production", &[("SAME", "1")]), changed(&["ADDED"]));
    }
}
