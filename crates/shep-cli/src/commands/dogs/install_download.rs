//! The download half of `shep install`: fetch the prebuilt binary the index
//! hashes, check the hash, and move it into the dog's directory.
//!
//! Nothing reaches `$SHEP_HOME/dogs/<name>/` until the hash matches. The
//! bytes wait in a temporary file beside it, which is gone on any refusal.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use shep_core::paths::ShepPaths;

use crate::dog_index::{BUILD_TARGET, Prebuilt};
use crate::exit::ExitCode;
use crate::fetch::{self, RedirectPolicy};
use crate::output::Streams;

use super::vet::sha256_hex;

/// The budget for the whole download, both requests together.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// The cap on a dog's binary: 128 MiB, several times a release build of the
/// largest dog.
const DOWNLOAD_LIMIT: usize = 128 << 20;

/// Downloads `prebuilt` and installs it as `binary`.
///
/// Returns `Err` with the exit code once the refusal is on `streams`.
pub(super) async fn download(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    policy: &RedirectPolicy,
    prebuilt: &Prebuilt,
    binary: &Path,
) -> Result<(), ExitCode> {
    let url = fetch::url_for_message(&prebuilt.url);
    streams.aside("install", &format!("downloading {url} for {BUILD_TARGET}"));
    let bytes = fetch::get_asset(&prebuilt.url, policy, DOWNLOAD_LIMIT, DOWNLOAD_TIMEOUT)
        .await
        .map_err(|err| {
            streams.fail(
                ExitCode::Failure,
                &format!("downloading {url}: {err}; nothing was installed"),
            )
        })?;

    let part = write_part(paths, &bytes).map_err(|err| {
        streams.fail(
            ExitCode::Failure,
            &format!(
                "writing the download under {}: {err}",
                paths.dogs_dir.display()
            ),
        )
    })?;
    let got = sha256_hex(&part).unwrap_or_default();
    if got != prebuilt.sha256 {
        return Err(streams.fail(
            ExitCode::Failure,
            &format!(
                "the download of {url} hashes to {got}, but the index lists {} for \
                 {BUILD_TARGET}; nothing was installed",
                prebuilt.sha256
            ),
        ));
    }

    let placed = binary
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| part.persist(binary).map_err(|err| err.error));
    placed.map_err(|err| {
        streams.fail(
            ExitCode::Failure,
            &format!("moving the download to {}: {err}", binary.display()),
        )
    })
}

/// `bytes` in a new executable file under the dogs directory, closed so the
/// binary can be run once it is moved.
fn write_part(paths: &ShepPaths, bytes: &[u8]) -> std::io::Result<tempfile::TempPath> {
    let mut part = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(&paths.dogs_dir)?;
    part.write_all(bytes)?;
    part.flush()?;
    let part = part.into_temp_path();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(part)
}
