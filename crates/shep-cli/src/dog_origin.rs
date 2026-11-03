//! Where a dog came from and whether the cached index holds a newer version,
//! for the SOURCE and VERSION columns.
//!
//! Both are facts for the operator and gate nothing. Nothing here touches the
//! network: the version comes from the dog's receipt and the newer one from
//! the index copy already on disk.

use std::collections::BTreeMap;
use std::path::Path;

use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, ProcessInfo};

use crate::commands::dogs::{cached_manifest, installed};
use crate::dog_index::{self, DogSourceKind, Index};

/// The repository owner whose dogs read as `installed`, not `installed,
/// community`.
const FIRST_PARTY_REPOS: &str = "https://github.com/shep-pm/";

/// Where a dog's binary came from.
///
/// `Debug` is derived: four fieldless variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// An argv branch of the shep binary.
    BuiltIn,
    /// `shep install` put it on disk from a shep-pm repository.
    Installed,
    /// `shep install` put it on disk from any other repository.
    Community,
    /// A path the operator gave `shep adopt`, with no receipt.
    Adopted,
}

impl Source {
    /// The word the SOURCE column and the JSON `source` key carry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Installed => "installed",
            Self::Community => "installed, community",
            Self::Adopted => "adopted",
        }
    }
}

/// What the SOURCE and VERSION columns say about one dog.
///
/// `Debug` is derived: a source word and two version strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DogOrigin {
    /// Where the binary came from.
    pub source: Source,
    /// What the dog's manifest reports, or `None` when nothing recorded it.
    pub version: Option<String>,
    /// The version the cached index holds when it is newer than `version`.
    pub newer: Option<String>,
}

impl DogOrigin {
    /// A built-in dog, which is versioned with the binary it ships in.
    pub fn built_in() -> Self {
        Self {
            source: Source::BuiltIn,
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            newer: None,
        }
    }

    /// A dog adopted from a path, versioned by the manifest cached for that
    /// exact binary, which an adopt or enable read. `None` when none was.
    pub fn adopted(version: Option<String>) -> Self {
        Self {
            source: Source::Adopted,
            version,
            newer: None,
        }
    }

    /// The VERSION cell: the version, then `→ <new>` when the index has one.
    /// `-` when nothing recorded a version.
    pub fn version_cell(&self) -> String {
        match (&self.version, &self.newer) {
            (Some(version), Some(newer)) => format!("{version} \u{2192} {newer}"),
            (Some(version), None) => version.clone(),
            (None, _) => "-".to_owned(),
        }
    }
}

/// The origins of the dogs in one listing, by name.
///
/// A dog it holds no entry for reads as its [`DogSource`] says: built-in, or
/// adopted with no version. `Debug` is derived: origins carry no secret.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DogOrigins(BTreeMap<String, DogOrigin>);

impl DogOrigins {
    /// `dog`'s origin.
    pub fn of(&self, dog: &ProcessInfo) -> DogOrigin {
        self.0.get(&dog.name).cloned().unwrap_or_else(|| {
            if matches!(dog.dog, Some(DogSource::BuiltIn { .. })) {
                DogOrigin::built_in()
            } else {
                DogOrigin::adopted(None)
            }
        })
    }
}

impl FromIterator<(String, DogOrigin)> for DogOrigins {
    fn from_iter<I: IntoIterator<Item = (String, DogOrigin)>>(entries: I) -> Self {
        Self(entries.into_iter().collect())
    }
}

/// Every dog's origin, answered from one read of the cached index.
///
/// `Debug` is derived: the index is public data.
#[derive(Debug)]
pub(crate) struct Origins<'a> {
    paths: &'a ShepPaths,
    index: Option<Index>,
}

impl<'a> Origins<'a> {
    /// Reads the cached index for `index_url`, which may be absent.
    pub(crate) fn read(paths: &'a ShepPaths, index_url: &str) -> Self {
        Self {
            paths,
            index: dog_index::cached_only(paths, index_url),
        }
    }

    /// `name`'s origin: built-in when `adopted_path` is `None`, installed
    /// when it has a receipt, adopted otherwise.
    pub(crate) fn of(&self, name: &str, adopted_path: Option<&Path>) -> DogOrigin {
        let Some(path) = adopted_path else {
            return DogOrigin::built_in();
        };
        let Some(receipt) = installed(self.paths, name) else {
            return DogOrigin::adopted(
                cached_manifest(path, &self.paths.home).map(|manifest| manifest.version),
            );
        };
        let source = if receipt.repo.starts_with(FIRST_PARTY_REPOS) {
            Source::Installed
        } else {
            Source::Community
        };
        let newer = receipt.pinned.as_deref().and_then(|have| {
            let row = self
                .index
                .as_ref()?
                .dogs
                .iter()
                .find(|dog| dog.package == receipt.package)?;
            let DogSourceKind::Cargo {
                version: Some(listed),
                ..
            } = &row.source
            else {
                return None;
            };
            is_newer(listed, have).then(|| listed.clone())
        });
        DogOrigin {
            source,
            version: receipt.version,
            newer,
        }
    }
}

impl Origins<'_> {
    /// The origin of every dog in `procs`, sheep skipped.
    pub(crate) fn of_listing(&self, procs: &[ProcessInfo]) -> DogOrigins {
        procs
            .iter()
            .filter_map(|proc| {
                let path = match proc.dog.as_ref()? {
                    DogSource::BuiltIn { .. } => None,
                    DogSource::Adopted { path, .. } => Some(Path::new(path)),
                    _ => return None,
                };
                Some((proc.name.clone(), self.of(&proc.name, path)))
            })
            .collect()
    }
}

/// Whether `listed` is a higher semver than `have`. A version either side
/// that is not semver is never newer.
fn is_newer(listed: &str, have: &str) -> bool {
    match (semver::Version::parse(listed), semver::Version::parse(have)) {
        (Ok(listed), Ok(have)) => listed > have,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.test/dogs.json";

    struct Home {
        _dir: tempfile::TempDir,
        paths: ShepPaths,
    }

    impl Home {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let paths = ShepPaths::at(dir.path().to_path_buf());
            Self { _dir: dir, paths }
        }

        /// A receipt as `shep install` wrote it, narrowed to what is read.
        fn receipt(&self, name: &str, repo: &str, version: &str) {
            self.receipt_pinning(name, repo, version, version);
        }

        /// As [`Self::receipt`], with the index version the install pinned
        /// apart from what the manifest reported.
        fn receipt_pinning(&self, name: &str, repo: &str, pinned: &str, manifest_version: &str) {
            let dir = self.paths.dogs_dir.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let body = serde_json::json!({
                "package": "shep-log-rotate",
                "row": { "repo": repo },
                "version": pinned,
                "manifest": { "version": manifest_version },
            });
            std::fs::write(dir.join("receipt.json"), body.to_string()).unwrap();
        }

        /// Caches an index whose one row lists `version` of `shep-log-rotate`.
        fn index_listing(&self, version: &str) {
            let document = serde_json::json!({
                "version": 1,
                "dogs": [{
                    "name": "Bella",
                    "package": "shep-log-rotate",
                    "adopt_as": "log-rotate",
                    "description": "Rotates logs.",
                    "repo": "https://github.com/shep-pm/shep-log-rotate",
                    "license": "MIT",
                    "category": "logs",
                    "source": { "kind": "cargo", "version": version }
                }]
            });
            dog_index::write_cache(
                &self.paths.dog_index_cache,
                URL,
                document.to_string().as_bytes(),
            )
            .unwrap();
        }

        fn origin(&self, name: &str) -> DogOrigin {
            Origins::read(&self.paths, URL).of(name, Some(Path::new("/opt/dogs/bin")))
        }
    }

    const SHEP_PM: &str = "https://github.com/shep-pm/shep-log-rotate";
    const COMMUNITY: &str = "https://github.com/someone/shep-log-rotate";

    #[test]
    fn a_built_in_dog_reads_built_in_with_the_binarys_version() {
        let home = Home::new();
        let origin = Origins::read(&home.paths, URL).of("metrics", None);

        assert_eq!(origin.source.as_str(), "built-in");
        assert_eq!(origin.version_cell(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_receipt_for_a_shep_pm_repo_reads_installed() {
        let home = Home::new();
        home.receipt("log-rotate", SHEP_PM, "0.2.4");

        assert_eq!(home.origin("log-rotate").source.as_str(), "installed");
    }

    #[test]
    fn a_receipt_for_any_other_repo_reads_installed_community() {
        let home = Home::new();
        home.receipt("log-rotate", COMMUNITY, "0.2.4");

        assert_eq!(
            home.origin("log-rotate").source.as_str(),
            "installed, community"
        );
    }

    #[test]
    fn a_path_with_no_receipt_reads_adopted_with_no_version() {
        let home = Home::new();
        let origin = home.origin("log-rotate");

        assert_eq!(origin.source.as_str(), "adopted");
        assert_eq!(origin.version_cell(), "-");
    }

    #[test]
    fn an_adopted_binary_reads_the_version_its_cached_manifest_reported() {
        let home = Home::new();
        let binary = home.paths.home.join("shep-otel");
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        let digest = crate::commands::dogs::sha256_hex(&binary).unwrap();
        let cache = home.paths.home.join("manifests");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join(format!("{digest}.json")),
            r#"{"manifest":1,"name":"otel","version":"1.4.0","protocol":12}"#,
        )
        .unwrap();

        let origin = Origins::read(&home.paths, URL).of("otel", Some(&binary));

        assert_eq!(origin.source.as_str(), "adopted");
        assert_eq!(origin.version_cell(), "1.4.0");
    }

    #[test]
    fn a_repo_that_only_starts_like_shep_pm_is_community() {
        let home = Home::new();
        home.receipt(
            "log-rotate",
            "https://github.com/shep-pm-evil/shep-log-rotate",
            "0.2.4",
        );

        assert_eq!(
            home.origin("log-rotate").source.as_str(),
            "installed, community"
        );
    }

    #[test]
    fn a_cached_index_with_a_newer_version_shows_the_arrow() {
        let home = Home::new();
        home.receipt("log-rotate", SHEP_PM, "0.2.4");
        home.index_listing("0.3.0");

        assert_eq!(
            home.origin("log-rotate").version_cell(),
            "0.2.4 \u{2192} 0.3.0"
        );
    }

    #[test]
    fn a_cached_index_with_the_same_version_shows_none() {
        let home = Home::new();
        home.receipt("log-rotate", SHEP_PM, "0.2.4");
        home.index_listing("0.2.4");

        assert_eq!(home.origin("log-rotate").version_cell(), "0.2.4");
    }

    #[test]
    fn a_cached_index_with_an_older_version_shows_none() {
        let home = Home::new();
        home.receipt("log-rotate", SHEP_PM, "0.10.0");
        home.index_listing("0.9.0");

        assert_eq!(
            home.origin("log-rotate").version_cell(),
            "0.10.0",
            "compared as semver, not as text"
        );
    }

    #[test]
    fn the_arrow_compares_the_index_with_the_pinned_version_not_the_manifests() {
        let home = Home::new();
        home.receipt_pinning("log-rotate", SHEP_PM, "0.2.4", "0.2.3");
        home.index_listing("0.2.4");

        assert_eq!(
            home.origin("log-rotate").version_cell(),
            "0.2.3",
            "the crate the index lists is the one installed"
        );

        home.index_listing("0.3.0");
        assert_eq!(
            home.origin("log-rotate").version_cell(),
            "0.2.3 \u{2192} 0.3.0"
        );
    }

    #[test]
    fn no_cached_index_shows_the_manifest_version_alone() {
        let home = Home::new();
        home.receipt("log-rotate", SHEP_PM, "0.2.4");

        assert_eq!(home.origin("log-rotate").version_cell(), "0.2.4");
    }
}
