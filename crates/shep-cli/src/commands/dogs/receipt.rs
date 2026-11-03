//! `$SHEP_HOME/dogs/<name>/receipt.json`: what `shep install` put on disk, from
//! which approval, and how.
//!
//! A record, never a cache. Nothing reads a schema from the manifest it
//! holds: anything that acts on a schema asks the binary fresh. It is there
//! so that "what was approved" can be answered after the index and the
//! binary have both moved on.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// The install directory the last upgrade replaced, kept for
    /// `shep upgrade --rollback`. Absent for a first install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) previous: Option<&'a Path>,
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

/// Removes what `shep install` wrote for `name`: the directory
/// `$SHEP_HOME/dogs/<name>/`, binary and receipt together. Returns the
/// directory it removed, or `None` when there was nothing of install's to
/// remove.
///
/// The receipt is the proof install wrote the directory, so a directory
/// without one is left alone, and so is a `name` that is not one plain path
/// component. An adopted dog has no receipt and its file is never reached.
///
/// # Errors
/// The directory exists and could not be removed.
pub(super) fn remove_install(paths: &ShepPaths, name: &str) -> std::io::Result<Option<PathBuf>> {
    if !super::install::is_path_safe(name) || !receipt_path(paths, name).is_file() {
        return Ok(None);
    }
    let dir = paths.dogs_dir.join(name);
    std::fs::remove_dir_all(&dir)?;
    Ok(Some(dir))
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

/// What a dog's receipt on disk says, as far as `shep upgrade` reads it.
///
/// Holds the whole document, so a copy written back keeps every field this
/// shep does not read.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Held {
    document: serde_json::Map<String, Value>,
    /// What was installed.
    pub(super) pinned: HeldPin,
    /// The binary the install put on disk.
    pub(super) binary: PathBuf,
    /// The install directory the last upgrade replaced.
    pub(super) previous: Option<PathBuf>,
}

/// [`Pinned`], owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HeldPin {
    /// The index's exact version.
    Version(String),
    /// A ref of the row's repository and the commit it built.
    Ref {
        /// What the operator typed after `--ref`.
        git_ref: String,
        /// The full commit cargo built.
        commit: String,
    },
}

/// Why a receipt could not be read.
#[derive(Debug)]
pub(super) enum HeldError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file is not a receipt this shep wrote.
    Malformed,
}

impl core::fmt::Display for HeldError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Malformed => write!(
                f,
                "it names no version or ref, or no binary, so it is not a receipt `shep install` wrote"
            ),
        }
    }
}

impl core::error::Error for HeldError {}

impl Held {
    /// Reads `bytes` as a receipt.
    ///
    /// # Errors
    /// [`HeldError::Malformed`] when it is not a JSON object naming a binary
    /// and a version or a ref with its commit.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, HeldError> {
        let Ok(Value::Object(document)) = serde_json::from_slice::<Value>(bytes) else {
            return Err(HeldError::Malformed);
        };
        let text = |key: &str| document.get(key).and_then(Value::as_str).map(str::to_owned);
        let pinned = match (text("version"), text("ref"), text("commit")) {
            (Some(version), None, _) => HeldPin::Version(version),
            (None, Some(git_ref), Some(commit)) => HeldPin::Ref { git_ref, commit },
            _ => return Err(HeldError::Malformed),
        };
        let binary = text("binary")
            .map(PathBuf::from)
            .ok_or(HeldError::Malformed)?;
        let previous = text("previous").map(PathBuf::from);
        Ok(Self {
            document,
            pinned,
            binary,
            previous,
        })
    }

    /// The directory the install put its binary in: `<root>/bin/<binary>`.
    pub(super) fn root(&self) -> Option<&Path> {
        root_of(&self.binary)
    }

    /// This receipt as JSON with `previous` set, or removed for `None`.
    pub(super) fn with_previous(&self, previous: Option<&Path>) -> Value {
        let mut document = self.document.clone();
        match previous {
            Some(dir) => {
                document.insert(
                    "previous".to_owned(),
                    Value::String(dir.display().to_string()),
                );
            }
            None => {
                document.remove("previous");
            }
        }
        Value::Object(document)
    }
}

/// The install directory a binary at `<root>/bin/<file>` belongs to.
pub(super) fn root_of(binary: &Path) -> Option<&Path> {
    binary.parent()?.parent()
}

/// Reads `name`'s receipt, or `None` when it has none.
///
/// # Errors
/// The file exists and could not be read or is not a receipt.
pub(super) fn read(paths: &ShepPaths, name: &str) -> Result<Option<Held>, HeldError> {
    read_file(&receipt_path(paths, name))
}

/// [`read`] for the receipt at `path`.
pub(super) fn read_file(path: &Path) -> Result<Option<Held>, HeldError> {
    match std::fs::read(path) {
        Ok(bytes) => Held::parse(&bytes).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(HeldError::Io(err)),
    }
}

/// Writes `document` to `path`.
///
/// # Errors
/// The write failed.
pub(super) fn write_document(path: &Path, document: &Value) -> std::io::Result<()> {
    shep_core::atomic_file::write_json(path, "receipt", document)
}

/// Whether `dir` is a directory `shep install` makes for `name`: a version or
/// a `ref-<sha>` directly under its own, so a receipt naming anything else
/// can send nothing to be removed.
pub(super) fn is_install_dir(paths: &ShepPaths, name: &str, dir: &Path) -> bool {
    dir.parent() == Some(paths.dogs_dir.join(name).as_path())
        && dir
            .file_name()
            .and_then(|file| file.to_str())
            .is_some_and(super::install::is_path_safe)
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
    /// The index version the install pinned, which the index's own version
    /// is compared against. `None` for a ref install.
    pub(crate) pinned: Option<String>,
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
        version: written
            .manifest
            .map(|m| m.version)
            .or_else(|| written.version.clone()),
        pinned: written.version,
    })
}

#[cfg(test)]
mod held_tests {
    use super::*;

    const VERSION: &[u8] =
        br#"{"version":"0.2.4","binary":"/h/dogs/otel/0.2.4/bin/shep-otel","sha256":"ab","future":1}"#;
    const REF: &[u8] = br#"{"ref":"main","commit":"3f9c2a1b7d4e","binary":"/h/dogs/otel/ref-3f9c2a1/bin/shep-otel","previous":"/h/dogs/otel/0.2.4"}"#;

    #[test]
    fn a_version_receipt_reads_as_a_version_and_its_install_directory() {
        let held = Held::parse(VERSION).unwrap();

        assert_eq!(held.pinned, HeldPin::Version("0.2.4".to_owned()));
        assert_eq!(held.root(), Some(Path::new("/h/dogs/otel/0.2.4")));
        assert_eq!(held.previous, None);
    }

    #[test]
    fn a_ref_receipt_reads_as_a_ref_with_its_commit_and_the_directory_it_replaced() {
        let held = Held::parse(REF).unwrap();

        assert_eq!(
            held.pinned,
            HeldPin::Ref {
                git_ref: "main".to_owned(),
                commit: "3f9c2a1b7d4e".to_owned()
            }
        );
        assert_eq!(held.previous, Some(PathBuf::from("/h/dogs/otel/0.2.4")));
    }

    #[test]
    fn setting_previous_keeps_every_other_field_and_none_removes_it() {
        let held = Held::parse(VERSION).unwrap();

        let with = held.with_previous(Some(Path::new("/h/dogs/otel/0.2.3")));
        assert_eq!(with["previous"], "/h/dogs/otel/0.2.3");
        assert_eq!(with["future"], 1);
        assert_eq!(with["sha256"], "ab");

        let without = Held::parse(&serde_json::to_vec(&with).unwrap())
            .unwrap()
            .with_previous(None);
        assert!(without.get("previous").is_none());
    }

    #[test]
    fn a_document_without_a_version_or_ref_and_a_binary_is_not_a_receipt() {
        for bytes in [
            &b"[]"[..],
            b"not json",
            br#"{"binary":"/b/bin/x"}"#,
            br#"{"version":"1.0.0"}"#,
            br#"{"ref":"main","binary":"/b/bin/x"}"#,
        ] {
            assert!(
                matches!(Held::parse(bytes), Err(HeldError::Malformed)),
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn a_missing_file_is_no_receipt_and_a_corrupt_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipt.json");
        assert!(read_file(&path).unwrap().is_none());

        std::fs::write(&path, "{").unwrap();

        assert!(matches!(read_file(&path), Err(HeldError::Malformed)));
    }
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
                previous: None,
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
        assert_eq!(read.pinned.as_deref(), Some("0.2.4"));
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
        let read = installed(&paths, "log-rotate").unwrap();
        assert_eq!((read.version, read.pinned), (None, None));
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
