use core::fmt;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// `key -> environment -> value`, one provider dog's pushed values.
pub type NamespaceTable = BTreeMap<String, BTreeMap<String, String>>;

/// `namespace -> key -> environment -> value`, every provider dog's pushed
/// values.
pub type NamespaceValues = BTreeMap<String, NamespaceTable>;

/// `namespace -> environments`, the pairs a provider dog has actually
/// pushed for.
///
/// A push carries one namespace and one environment
/// (`Request::PutSecrets`), so a provider that has pushed `production` and
/// not yet `staging` has one entry here, not two. That distinction is what
/// [`Resolution::MissingNamespace`](crate::secrets::Resolution::MissingNamespace) is keyed on.
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
    ///
    /// Private where [`Self::pushed`] is not. A [`NamespaceValues`] is a
    /// plain nested map, so printing one prints every value in it, and a
    /// public field is exactly what a caller reaches for after reading
    /// that this type redacts (IR-41). Read it through
    /// [`Self::namespace`], [`Self::namespaces`] or [`Self::into_parts`].
    values: NamespaceValues,
    /// Every `(namespace, environment)` pair a push has landed for.
    ///
    /// Names, never values, which is why this half stays public: the
    /// shepherd's boot log prints it as it stands.
    pub pushed: PushedPairs,
}

impl ProviderCache {
    /// A cache holding `values`, pushed as the pairs in `pushed`.
    #[must_use]
    pub fn new(values: NamespaceValues, pushed: PushedPairs) -> Self {
        Self { values, pushed }
    }

    /// One namespace's values, or nothing when no push has carried that
    /// namespace.
    ///
    /// Present and empty is a dog that pushed no keys. Whether that dog
    /// has spoken for a given environment is [`Self::pushed`]'s answer,
    /// not this one's.
    #[must_use]
    pub fn namespace(&self, name: &str) -> Option<&NamespaceTable> {
        self.values.get(name)
    }

    /// Every namespace and its values, in name order.
    pub fn namespaces(&self) -> impl Iterator<Item = (&String, &NamespaceTable)> {
        self.values.iter()
    }

    /// How many namespaces a push has carried.
    #[must_use]
    pub fn namespace_count(&self) -> usize {
        self.values.len()
    }

    /// Both halves, moved out, for a caller that owns them afterwards.
    #[must_use]
    pub fn into_parts(self) -> (NamespaceValues, PushedPairs) {
        (self.values, self.pushed)
    }
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
pub(super) struct ProviderCacheFile {
    pub(super) version: u32,
    #[serde(default)]
    pub(super) namespaces: NamespaceValues,
    #[serde(default)]
    pub(super) pushed: PushedPairs,
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

/// The `secrets-cache.json` format version this build reads and writes.
///
/// One constant for both sides: shep-daemon writes the file and stamps it
/// with this, and [`provider_cache_on_disk`] refuses anything else. Two
/// literals of the same value would let a bump on one side turn every read
/// on the other into an empty cache, with nothing to say why.
pub const PROVIDER_CACHE_VERSION: u32 = 2;

/// The provider cache as `secrets-cache.json` currently holds it on disk,
/// or nothing when the file is missing, will not parse, or is a version
/// this build does not understand.
///
/// Best-effort, more so than [`all`](crate::secrets::all): a namespace whose provider pushed
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
        Ok(file) if file.version == PROVIDER_CACHE_VERSION => {
            ProviderCache::new(file.namespaces, file.pushed)
        }
        _ => ProviderCache::default(),
    }
}

#[cfg(test)]
mod tests {

    use std::collections::BTreeSet;

    use super::super::testing::*;
    use super::*;

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
        assert_eq!(
            cache.namespace("vercel").unwrap()["API_KEY"]["production"],
            "sk_live"
        );
        assert_eq!(
            cache.pushed["vercel"],
            BTreeSet::from(["production".to_string()])
        );
    }

    #[test]
    fn provider_cache_on_disk_is_empty_for_a_missing_or_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            provider_cache_on_disk(&dir.path().join("absent.json")).namespace_count(),
            0
        );

        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "not json").unwrap();
        assert_eq!(provider_cache_on_disk(&broken).namespace_count(), 0);

        let future = dir.path().join("future.json");
        std::fs::write(&future, r#"{"version":999,"namespaces":{}}"#).unwrap();
        assert_eq!(provider_cache_on_disk(&future).namespace_count(), 0);
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
}
