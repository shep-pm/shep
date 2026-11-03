//! `$SHEP_HOME/dogs/<name>/receipt.json`: what `shep install` put on disk, from
//! which approval, and how.
//!
//! A record, never a cache. Nothing reads a schema from the manifest it
//! holds: anything that acts on a schema asks the binary fresh. It is there
//! so that "what was approved" can be answered after the index and the
//! binary have both moved on.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shep_core::dogs::manifest::Manifest;
use shep_core::paths::ShepPaths;

use crate::dog_index::AvailableDog;

/// The file name a dog's receipt has, inside its directory.
const RECEIPT_FILE: &str = "receipt.json";

/// What one install leaves behind.
///
/// `Debug` is derived and safe: every field is a public index value, an
/// argv shep built, or a hash, and the manifest carries no env (IR-41).
#[derive(Debug, Serialize)]
pub(super) struct Receipt<'a> {
    /// The index URL the row was read from, or a placeholder for one that
    /// may carry credentials.
    pub(super) index: &'a str,
    /// The row exactly as the index approved it.
    pub(super) row: &'a AvailableDog,
    /// The crate the binary comes from.
    pub(super) package: &'a str,
    /// The version or the ref and commit installed.
    #[serde(flatten)]
    pub(super) pinned: Pinned<'a>,
    /// How the binary got there.
    #[serde(flatten)]
    pub(super) installer: Installer<'a>,
    /// Where the install put the binary.
    pub(super) binary: &'a Path,
    /// Lowercase hex sha256 of that binary.
    pub(super) sha256: &'a str,
    /// The manifest the binary printed, or `None` for a dog that prints none.
    pub(super) manifest: Option<&'a Manifest>,
    /// When the receipt was written, as UTC RFC 3339.
    pub(super) installed_at: String,
}

/// What was installed, written as `version`, or as `ref` and `commit`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum Pinned<'a> {
    /// The index's exact version.
    Version {
        /// The version.
        version: &'a str,
    },
    /// A branch, tag or commit of the row's repository, which the index has
    /// not reviewed.
    Ref {
        /// What the operator typed after `--ref`.
        #[serde(rename = "ref")]
        git_ref: &'a str,
        /// The full commit cargo built.
        commit: &'a str,
    },
}

/// What put the binary on disk, written as `command` or `url` in the receipt.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Installer<'a> {
    /// The installer's whole command line, program first.
    Command(&'a [String]),
    /// The URL the binary was downloaded from.
    Url(&'a str),
}

/// The time a receipt written now carries.
pub(super) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Where `name`'s receipt lives.
pub(super) fn receipt_path(paths: &ShepPaths, name: &str) -> PathBuf {
    paths.dogs_dir.join(name).join(RECEIPT_FILE)
}

/// Writes `receipt` as `name`'s receipt, replacing the one before it.
///
/// # Errors
/// The directory could not be made, or the write failed.
pub(super) fn write(paths: &ShepPaths, name: &str, receipt: &Receipt<'_>) -> std::io::Result<()> {
    let path = receipt_path(paths, name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    shep_core::atomic_file::write_json(&path, "receipt", receipt)
}

/// The part of a receipt the SOURCE and VERSION columns read.
///
/// `Debug` is derived and safe: a package name, a repository URL and two
/// version strings (IR-41).
#[derive(Debug, Deserialize)]
pub(crate) struct Installed {
    /// The crate the binary came from.
    pub(crate) package: String,
    /// The repository the index row named.
    pub(crate) repo: String,
    /// The version the manifest reported at install, else the index version
    /// the install pinned. `None` for a ref install of a dog with no
    /// manifest.
    pub(crate) version: Option<String>,
}

/// The receipt's shape as written, narrowed to what [`Installed`] keeps.
#[derive(Deserialize)]
struct Written {
    package: String,
    row: WrittenRow,
    version: Option<String>,
    manifest: Option<WrittenManifest>,
}

/// The index row inside a receipt.
#[derive(Deserialize)]
struct WrittenRow {
    repo: String,
}

/// The manifest inside a receipt.
#[derive(Deserialize)]
struct WrittenManifest {
    version: String,
}

/// What `name`'s receipt says, or `None` for a dog with no receipt or one
/// that cannot be read.
///
/// A listing never fails over a record, so an unreadable receipt counts as
/// none.
pub(crate) fn installed(paths: &ShepPaths, name: &str) -> Option<Installed> {
    let bytes = std::fs::read(receipt_path(paths, name)).ok()?;
    let written: Written = serde_json::from_slice(&bytes).ok()?;
    Some(Installed {
        package: written.package,
        repo: written.row.repo,
        version: written.manifest.map(|m| m.version).or(written.version),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::dog_index::{AvailableDog, DogSourceKind};

    fn row() -> AvailableDog {
        AvailableDog {
            name: "Bella".to_owned(),
            package: "shep-log-rotate".to_owned(),
            adopt_as: "log-rotate".to_owned(),
            description: "Rotates logs.".to_owned(),
            repo: "https://github.com/shep-pm/shep-log-rotate".to_owned(),
            license: "MIT".to_owned(),
            category: "logs".to_owned(),
            source: DogSourceKind::Cargo {
                version: Some("0.2.4".to_owned()),
                sha256: BTreeMap::new(),
            },
        }
    }

    fn write_for(paths: &ShepPaths, manifest: Option<&Manifest>, pinned: Pinned<'_>) {
        write(
            paths,
            "log-rotate",
            &Receipt {
                index: "https://shep-pm.com/dogs.json",
                row: &row(),
                package: "shep-log-rotate",
                pinned,
                installer: Installer::Url("https://example.test/asset"),
                binary: Path::new("/bin/dog"),
                sha256: "ab",
                manifest,
                installed_at: now(),
            },
        )
        .unwrap();
    }

    fn manifest(version: &str) -> Manifest {
        serde_json::from_str(&format!(
            r#"{{"manifest":1,"name":"log-rotate","version":"{version}","protocol":12}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_receipt_the_writer_made_reads_back_with_the_manifest_version() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        write_for(
            &paths,
            Some(&manifest("0.2.3")),
            Pinned::Version { version: "0.2.4" },
        );

        let read = installed(&paths, "log-rotate").unwrap();

        assert_eq!(read.package, "shep-log-rotate");
        assert_eq!(read.repo, "https://github.com/shep-pm/shep-log-rotate");
        assert_eq!(read.version.as_deref(), Some("0.2.3"), "the manifest wins");
    }

    #[test]
    fn a_receipt_without_a_manifest_falls_back_to_the_pinned_version() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        write_for(&paths, None, Pinned::Version { version: "0.2.4" });
        assert_eq!(
            installed(&paths, "log-rotate").unwrap().version.as_deref(),
            Some("0.2.4")
        );

        write_for(
            &paths,
            None,
            Pinned::Ref {
                git_ref: "main",
                commit: "3f9c2a1b",
            },
        );
        assert_eq!(installed(&paths, "log-rotate").unwrap().version, None);
    }

    #[test]
    fn no_receipt_and_an_unreadable_one_both_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        assert!(installed(&paths, "log-rotate").is_none());

        let path = receipt_path(&paths, "log-rotate");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(installed(&paths, "log-rotate").is_none());
    }
}
