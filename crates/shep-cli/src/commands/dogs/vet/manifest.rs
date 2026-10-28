//! Reading a dog's manifest for `shep adopt` and `shep enable`: asked of the
//! binary with [`MANIFEST_FLAG`], and cached by the binary's sha256 so a
//! build is asked once.

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use shep_core::atomic_file;
use shep_core::dogs::{DogVersion, MANIFEST_FLAG, manifest::Manifest};

use super::{AdoptRefusal, DogSchema, ask};

/// The directory under `$SHEP_HOME` holding one `<sha256>.json` per binary
/// that answered [`MANIFEST_FLAG`].
const CACHE_DIR: &str = "manifests";

/// What a binary's manifest answered. Three answers, in the shape
/// [`super::DogSchema`] uses one flag over.
///
/// The line is between a dog that has no manifest and a record of one this
/// machine cannot read. A dog that gave nothing declared nothing.
///
/// A cache file is the other thing: shep wrote it out of a probe that answered.
// `large_enum_variant` allowed, not fixed: clippy's remedy is to box
// `Manifest`, and that buys nothing. This enum lives for one read of one
// manifest inside one command, its payload is read field by field and
// dropped, and nothing here holds a collection of them.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum ManifestRead {
    /// The manifest, from the cache or from the binary.
    Read(Manifest),
    /// The binary gave no manifest: it printed nothing, printed something that
    /// is not a manifest, exited with a failure, or was still running when
    /// `budget` ran out. Nothing was ever recorded for this build, so no
    /// declaration is being missed.
    Absent,
    /// The cache holds a file for this exact binary, and neither that file nor
    /// a fresh probe says what the manifest was.
    Unreadable,
}

impl ManifestRead {
    /// The manifest, where there is one to read.
    #[must_use]
    pub(crate) fn manifest(&self) -> Option<&Manifest> {
        match self {
            Self::Read(manifest) => Some(manifest),
            Self::Absent | Self::Unreadable => None,
        }
    }
}

/// What `path` declares, from the cache when this exact binary has been read
/// before and from the binary otherwise.
///
/// A binary that gives nothing gives no declaration, and a cache file for this
/// exact binary is a record of one: [`ManifestRead::Unreadable`] is only where
/// a record exists and a fresh probe could not replace it, which is also what
/// heals one. A dog that gains a manifest is noticed on the next call, because
/// neither [`ManifestRead::Absent`] nor [`ManifestRead::Unreadable`] is cached.
/// The cache is derived and safe to delete, and a binary that changed while it
/// was probed is not cached.
///
/// # Errors
/// [`AdoptRefusal::WillNotExec`], and only that, from the probe.
pub(crate) fn read_manifest(
    path: &Path,
    home: &Path,
    name: &str,
    budget: Duration,
) -> Result<ManifestRead, AdoptRefusal> {
    let digest = sha256_hex(path);
    let cached = digest
        .as_ref()
        .map(|digest| home.join(CACHE_DIR).join(format!("{digest}.json")));
    if let Some(manifest) = cached.as_deref().and_then(read_cached) {
        return Ok(ManifestRead::Read(manifest));
    }
    // A file filed under this digest is a manifest some earlier read of this
    // exact binary printed. Ask the binary once more before calling this a dog
    // with nothing to say: the answer is what a corrupt record is healed by.
    let record = cached.as_deref().is_some_and(cache_record);
    let Some(text) = ask(path, MANIFEST_FLAG, home, name, budget)? else {
        // No answer at all: still running when the budget ran out, or a run
        // that failed. `docs/dogs.md` asks for a manifest on stdout and exit 0,
        // so a dog that could not answer said nothing about what it wants.
        return Ok(no_manifest(record));
    };
    // Empty output is a dog with no manifest, not a manifest that failed to
    // parse: the same guard `ask_schema` makes, so the ordinary case earns no
    // reading meant for a broken one.
    if text.trim().is_empty() {
        return Ok(no_manifest(record));
    }
    let Ok(manifest) = serde_json::from_str::<Manifest>(&text) else {
        return Ok(no_manifest(record));
    };
    // A binary swapped while it ran may have answered as the new build, and
    // the old digest would file that answer under the wrong key.
    if let Some(file) = cached.filter(|_| sha256_hex(path) == digest) {
        let _ = store(&file, &text);
    }
    Ok(ManifestRead::Read(manifest))
}

/// What a binary that gave no manifest answered, which is no manifest at all
/// unless a record of one was sitting there to be read.
fn no_manifest(record: bool) -> ManifestRead {
    if record {
        ManifestRead::Unreadable
    } else {
        ManifestRead::Absent
    }
}

/// What the cache already holds for `path`, and nothing else: this never
/// probes the binary.
///
/// [`read_manifest`] asks a dog that has not been asked before. Nothing here
/// does: `shep --help` and `shep completions` would otherwise spawn a dog.
///
/// `None` is ordinary: a dog adopted before manifests existed, or a moved binary.
pub(crate) fn cached_manifest(path: &Path, home: &Path) -> Option<Manifest> {
    read_cached(
        &home
            .join(CACHE_DIR)
            .join(format!("{}.json", sha256_hex(path)?)),
    )
}

/// The manifest filed at `file`, or `None`: no file, a file that will not
/// read, or one that will not parse.
fn read_cached(file: &Path) -> Option<Manifest> {
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// Whether a cache file sits at `file`, whatever is in it.
///
/// A read error that is not "no such file" counts as one: a file this process
/// cannot read is a record shep wrote and cannot read back.
fn cache_record(file: &Path) -> bool {
    match std::fs::metadata(file) {
        Ok(_) => true,
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Writes `text` to `file` the way every store under `$SHEP_HOME` is
/// written: staged beside it owner-only and renamed into place, so a reader
/// sees a whole manifest or none.
///
/// The manifest's `defaults` can hold a credential, so nobody else may read
/// the cache.
fn store(file: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    create_cache_dir(dir)?;
    let mut staged = atomic_file::create_staging_file(dir, ".manifest-", ".tmp")?;
    staged.write_all(text.as_bytes())?;
    atomic_file::publish(staged, file)
}

/// Creates `dir` and any missing parents, owner-only on unix at the `mkdir`
/// itself, as the rest of `$SHEP_HOME` is.
fn create_cache_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// `manifest` as the `--version` answer it replaces.
pub(super) fn version_of(manifest: &Manifest) -> DogVersion {
    DogVersion {
        version: manifest.version.clone(),
        protocol: Some(manifest.protocol),
        channel: manifest.channel,
    }
}

/// `manifest`'s settings schema as the `--schema` answer it replaces.
pub(super) fn schema_of(manifest: &Manifest) -> DogSchema {
    manifest
        .settings
        .clone()
        .map_or(DogSchema::Silent, DogSchema::Published)
}

/// The lowercase hex sha256 of the file at `path`, or `None` when it cannot
/// be read.
fn sha256_hex(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match file.read(&mut chunk).ok()? {
            0 => break,
            read => context.update(&chunk[..read]),
        }
    }
    Some(
        context
            .finish()
            .as_ref()
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            }),
    )
}

#[cfg(all(test, unix))]
mod tests;
