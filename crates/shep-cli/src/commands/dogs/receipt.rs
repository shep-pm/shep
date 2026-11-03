//! `$SHEP_HOME/dogs/<name>/receipt.json`: what `shep install` put on disk, from
//! which approval, and how.
//!
//! A record, never a cache. Nothing reads a schema from the manifest it
//! holds: anything that acts on a schema asks the binary fresh. It is there
//! so that "what was approved" can be answered after the index and the
//! binary have both moved on.

use std::path::{Path, PathBuf};

use serde::Serialize;
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
