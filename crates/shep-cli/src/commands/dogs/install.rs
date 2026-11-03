//! `shep install <dog>`: builds the version the community index approves
//! with cargo, records a receipt, and hands the binary to `adopt`.
//!
//! The index row approves one exact version. Before cargo runs, crates.io is
//! asked which repository published that version, and a repository other
//! than the row's refuses. After the build, the grant question and the
//! enable are `adopt`'s own: install adds no prompt.
//!
//! Cargo rows only. Any other kind prints its instructions and installs
//! nothing, and a dog the index does not carry is `shep adopt <path>`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use shep_core::file_lock::FileLock;
use shep_core::paths::ShepPaths;

use crate::cli::{AdoptArgs, Format, InstallArgs};
use crate::dog_index::{self, AvailableDog, DogSourceKind, Origin};
use crate::exit::ExitCode;
use crate::fetch;
use crate::output::{AvailableDogRows, Streams, emit, write_outcome};

use super::adopt::{Flow, adopt_with, collides_with_a_verb, lookup_on_path};
use super::consent::Input;
use super::receipt::{self, Receipt};
use super::vet::{ManifestRead, VERSION_BUDGET, read_manifest, sha256_hex};

/// Where crates.io's API is, when nothing says otherwise.
const CRATES_IO: &str = "https://crates.io";

/// How long a second install of one dog waits for the first.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// The budget for the one crates.io request.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The cap on crates.io's answer for one version.
const LOOKUP_LIMIT: usize = 1 << 20;

/// [`emit_notice`](crate::output::emit_notice) code for a version crates.io
/// says was published with a token rather than from a repository's CI.
const TOKEN_PUBLISH_NOTICE: &str = "dog_token_publish";

/// What `install` reaches outside the index and `shep.toml`, so a test can
/// stand a stub in for each.
#[derive(Debug)]
pub(super) struct Reach {
    /// The `cargo` to run, or `None` when there is none on `PATH`.
    pub(super) cargo: Option<PathBuf>,
    /// Who to ask which repository published a version.
    pub(super) lookup: Lookup,
    /// How long to wait for another install of the same dog.
    pub(super) lock_wait: Duration,
}

/// Where `install` learns who published a version.
#[derive(Debug)]
pub(super) enum Lookup {
    /// crates.io's API at this base URL, with no trailing slash.
    CratesIo(String),
    /// A fixed answer, so a test of everything after the lookup needs no
    /// socket.
    #[cfg(test)]
    Canned(Published),
}

impl Lookup {
    async fn published(&self, package: &str, version: &str) -> Result<Published, String> {
        match self {
            Self::CratesIo(base) => published_from(base, package, version).await,
            #[cfg(test)]
            Self::Canned(published) => Ok(published.clone()),
        }
    }
}

impl Reach {
    /// The real `cargo` on this process's `PATH` and the real crates.io.
    fn real() -> Self {
        Self {
            cargo: lookup_on_path(Path::new("cargo"), std::env::var_os("PATH").as_deref()),
            lookup: Lookup::CratesIo(CRATES_IO.to_owned()),
            lock_wait: LOCK_WAIT,
        }
    }
}

/// `shep install [<dog>]`.
///
/// With no `<dog>`, lists the index with an install line under each dog
/// that can be installed. With one, builds it, records its receipt and
/// adopts it; see the module doc.
pub async fn install(streams: &mut Streams<'_>, paths: &ShepPaths, args: &InstallArgs) -> ExitCode {
    let url = dog_index::index_url();
    let loaded = match dog_index::cached_index(paths, &url, args.refresh).await {
        Ok(loaded) => loaded,
        Err(err) => {
            let message = format!(
                "reading the dog index from {}: {err}",
                fetch::url_for_message(&url)
            );
            return streams.fail(ExitCode::Failure, &message);
        }
    };
    if let Origin::Stale { age, cause } = &loaded.origin {
        streams.aside(
            "dogs_index_stale",
            &format!(
                "could not read the dog index from {}: {cause}; using the copy cached {} ago",
                fetch::url_for_message(&url),
                dog_index::describe_age(*age)
            ),
        );
    }
    let Some(wanted) = &args.dog else {
        return list(streams, loaded.index.dogs);
    };
    let Some(dog) = loaded.index.dogs.iter().find(|dog| &dog.adopt_as == wanted) else {
        return streams.fail(
            ExitCode::NotFound,
            &format!(
                "the dog index has no dog called `{wanted}`; `shep install` lists it, and a dog \
                 it does not carry is `shep adopt <path>`"
            ),
        );
    };
    install_dog(
        streams,
        paths,
        args,
        &Reach::real(),
        &mut Input::stdin(),
        dog,
        &url,
    )
    .await
}

/// Bare `shep install`: what `shep dogs --available` lists, and under each
/// dog that can be installed the line that installs it.
fn list(streams: &mut Streams<'_>, dogs: Vec<AvailableDog>) -> ExitCode {
    if streams.fmt != Format::Table {
        return write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            "install",
            AvailableDogRows(dogs),
            streams.style,
        ));
    }
    let result = (|| {
        let out = &mut *streams.out;
        for (at, dog) in dogs.iter().enumerate() {
            if at > 0 {
                writeln!(out)?;
            }
            writeln!(out, "{} . {} . {}", dog.name, dog.package, dog.category)?;
            writeln!(out, "  {}", dog.description)?;
            if dog.is_installable() {
                writeln!(out, "  shep install {}", dog.adopt_as)?;
            } else if dog.is_unversioned_cargo() {
                writeln!(out, "  not installable: the index approves no version")?;
            }
        }
        std::io::Result::Ok(())
    })();
    write_outcome(result)
}

/// Whether `text` may be one component of a path under `$SHEP_HOME/dogs`
/// and of a crates.io URL: crate-name and semver characters, and no leading
/// dot, so `..` and hidden names are out.
fn is_path_safe(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('.')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

/// Everything after the index named a row and the operator asked for it.
pub(super) async fn install_dog(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &InstallArgs,
    reach: &Reach,
    input: &mut Input,
    dog: &AvailableDog,
    index_url: &str,
) -> ExitCode {
    let DogSourceKind::Cargo {
        version: Some(version),
        ..
    } = &dog.source
    else {
        // The instructions `shep dogs --available <name>` prints, and nothing
        // installed.
        let _ = crate::commands::query::render_detail(&mut *streams.out, dog);
        return ExitCode::Failure;
    };
    let name = dog.adopt_as.as_str();
    let package = dog.package.as_str();
    for (what, text) in [("name", name), ("package", package), ("version", version)] {
        if !is_path_safe(text) {
            return streams.fail(
                ExitCode::InvalidConfig,
                &format!("the index row's {what} {text:?} is not one this shep will build"),
            );
        }
    }
    // Before building: `adopt` refuses the same name, and only after the
    // build has been paid for.
    if collides_with_a_verb(name) {
        return streams.fail(
            ExitCode::InvalidConfig,
            &format!(
                "`{name}` is already a shep verb or alias, so a dog by that name could never be \
                 reached; nothing was built"
            ),
        );
    }
    let Some(cargo) = &reach.cargo else {
        return streams.fail(
            ExitCode::Failure,
            &format!(
                "`shep install` builds {package} with cargo, and there is no `cargo` on PATH. \
                 Install a Rust toolchain from https://rustup.rs and run it again; shep never \
                 installs one"
            ),
        );
    };

    let published = match reach.lookup.published(package, version).await {
        Ok(published) => published,
        Err(message) => return streams.fail(ExitCode::Failure, &message),
    };
    let token_publish = match published {
        Published::From(repository) => {
            if !same_repository(&dog.repo, &repository) {
                return streams.fail(
                    ExitCode::Failure,
                    &format!(
                        "crates.io says {package} {version} was published from {repository}, but \
                         the index lists {} for {name}; nothing was built",
                        dog.repo
                    ),
                );
            }
            false
        }
        Published::ByToken => true,
        Published::Unreported => false,
    };

    if let Err(err) = std::fs::create_dir_all(&paths.dogs_dir) {
        return streams.fail(
            ExitCode::Failure,
            &format!("making {}: {err}", paths.dogs_dir.display()),
        );
    }
    let lock_target = paths.dogs_dir.join(name);
    let lock_wait = reach.lock_wait;
    let lock =
        tokio::task::spawn_blocking(move || FileLock::acquire_within(&lock_target, lock_wait))
            .await;
    let _held = match lock {
        Ok(Ok(Some(held))) => held,
        Ok(Ok(None)) => {
            return streams.fail(
                ExitCode::Failure,
                &format!(
                    "another `shep install {name}` is running: {} is held. Wait for it to finish",
                    paths.dogs_dir.join(format!("{name}.lock")).display()
                ),
            );
        }
        Ok(Err(err)) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("locking the install of {name}: {err}"),
            );
        }
        Err(err) => {
            return streams.fail(ExitCode::Internal, &format!("locking the install: {err}"));
        }
    };

    let root = paths.dogs_dir.join(name).join(version);
    let command = [
        "cargo".to_owned(),
        "install".to_owned(),
        "--locked".to_owned(),
        "--version".to_owned(),
        version.clone(),
        "--root".to_owned(),
        root.display().to_string(),
        package.to_owned(),
    ];
    streams.aside(
        "install",
        &format!("building {package} {version} with cargo"),
    );
    let status = tokio::process::Command::new(cargo)
        .args(&command[1..])
        .stdin(Stdio::null())
        // stdout is the verb's answer; cargo narrates on stderr.
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await;
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            return streams.fail(
                ExitCode::Failure,
                &format!(
                    "`cargo install` for {package} {version} failed ({status}); nothing was adopted"
                ),
            );
        }
        Err(err) => {
            return streams.fail(
                ExitCode::SpawnFailed,
                &format!("running {}: {err}", cargo.display()),
            );
        }
    }

    let binary = root
        .join("bin")
        .join(format!("{package}{}", std::env::consts::EXE_SUFFIX));
    let Some(sha256) = sha256_hex(&binary) else {
        return streams.fail(
            ExitCode::Failure,
            &format!(
                "cargo built no binary called {package} in {}; nothing was adopted",
                root.join("bin").display()
            ),
        );
    };
    // A binary that prints none, or that this read cannot take, records none:
    // `adopt` reads it again and reports whichever it was.
    let manifest = match read_manifest(&binary, &paths.home, name, VERSION_BUDGET) {
        Ok(ManifestRead::Read(manifest)) => Some(manifest),
        Ok(ManifestRead::Absent | ManifestRead::Unreadable) | Err(_) => None,
    };
    let written = receipt::write(
        paths,
        name,
        &Receipt {
            index: fetch::url_for_message(index_url),
            row: dog,
            package,
            version,
            command: &command,
            binary: &binary,
            sha256: &sha256,
            manifest: manifest.as_ref(),
            installed_at: receipt::now(),
        },
    );
    if let Err(err) = written {
        return streams.fail(
            ExitCode::Failure,
            &format!(
                "writing {}: {err}",
                receipt::receipt_path(paths, name).display()
            ),
        );
    }

    if token_publish {
        streams.aside(
            TOKEN_PUBLISH_NOTICE,
            &format!(
                "crates.io lists {package} {version} as published with an API token, not from \
                 {}'s CI, so the build is not tied to that repository",
                dog.repo
            ),
        );
    }
    let adopting = AdoptArgs {
        path: binary,
        name: Some(name.to_owned()),
        yes: args.yes,
    };
    let flow = Flow {
        verb: "install",
        enable: !args.no_enable,
    };
    adopt_with(streams, paths, &adopting, input, flow).await
}

/// What crates.io says about who published one version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Published {
    /// Trusted publishing from this repository.
    From(String),
    /// `trustpub_data` is `null`: published with an API token.
    ByToken,
    /// The response has no usable `trustpub_data`. crates.io marks the field
    /// unstable, so its absence is not a refusal.
    Unreported,
}

/// Asks crates.io at `base` for `package` `version`'s `trustpub_data`.
///
/// # Errors
/// The message to refuse with: crates.io could not be reached, has no such
/// version, or answered something that is not JSON.
pub(super) async fn published_from(
    base: &str,
    package: &str,
    version: &str,
) -> Result<Published, String> {
    let url = format!("{base}/api/v1/crates/{package}/{version}");
    let target = fetch::parse_url(&url).map_err(|err| format!("{err}"))?;
    let body = fetch::get(&target, LOOKUP_LIMIT, LOOKUP_TIMEOUT)
        .await
        .map_err(|err| {
            format!("asking crates.io who published {package} {version}: {err}; nothing was built")
        })?;
    parse_published(&body).map_err(|err| {
        format!("crates.io's answer for {package} {version} was not JSON: {err}; nothing was built")
    })
}

/// [`Published`] from the body of crates.io's `GET /api/v1/crates/<name>/<version>`.
fn parse_published(body: &[u8]) -> Result<Published, serde_json::Error> {
    let document: Value = serde_json::from_slice(body)?;
    let published = match document.get("version").and_then(|v| v.get("trustpub_data")) {
        None => Published::Unreported,
        Some(Value::Null) => Published::ByToken,
        Some(data) => match data.get("repository").and_then(Value::as_str) {
            Some(repository) => Published::From(repository.to_owned()),
            None => Published::Unreported,
        },
    };
    Ok(published)
}

/// Whether `published`, crates.io's `owner/repo` (or a URL), names the same
/// repository as the index's `row_repo` URL. Case-insensitive, and blind to
/// a trailing slash and `.git`.
fn same_repository(row_repo: &str, published: &str) -> bool {
    fn path_of(repo: &str) -> String {
        let repo = repo.trim().trim_end_matches('/');
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        let repo = match repo.split_once("://") {
            Some((_scheme, rest)) => rest.split_once('/').map_or("", |(_host, path)| path),
            None => repo,
        };
        repo.trim_matches('/').to_ascii_lowercase()
    }
    let row = path_of(row_repo);
    !row.is_empty() && row == path_of(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_matches_the_rows_url_however_it_is_spelled() {
        for published in [
            "shep-pm/shep-log-rotate",
            "Shep-PM/Shep-Log-Rotate",
            "https://github.com/shep-pm/shep-log-rotate",
            "shep-pm/shep-log-rotate.git",
        ] {
            assert!(
                same_repository("https://github.com/shep-pm/shep-log-rotate", published),
                "{published}"
            );
        }
    }

    #[test]
    fn another_repository_or_a_prefix_of_it_does_not_match() {
        for published in ["someone/shep-log-rotate", "shep-pm/shep", "", "shep-pm"] {
            assert!(
                !same_repository("https://github.com/shep-pm/shep-log-rotate", published),
                "{published:?}"
            );
        }
    }

    #[test]
    fn crates_io_says_who_published_a_version_in_three_ways() {
        let from = br#"{"version":{"trustpub_data":{"provider":"github","repository":"o/r"}}}"#;
        let token = br#"{"version":{"trustpub_data":null}}"#;
        let silent = br#"{"version":{"num":"0.2.4"}}"#;

        assert_eq!(
            parse_published(from).unwrap(),
            Published::From("o/r".into())
        );
        assert_eq!(parse_published(token).unwrap(), Published::ByToken);
        assert_eq!(parse_published(silent).unwrap(), Published::Unreported);
    }

    #[test]
    fn a_name_that_climbs_or_hides_is_not_a_path_component() {
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "a b", "a\u{1b}"] {
            assert!(!is_path_safe(bad), "{bad:?}");
        }
        for good in ["log-rotate", "0.2.4", "1.0.0-rc.1+build", "shep_log"] {
            assert!(is_path_safe(good), "{good:?}");
        }
    }
}
