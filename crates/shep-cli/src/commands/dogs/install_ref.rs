//! The `--ref` half of `shep install`: builds a dog from a branch, tag or
//! commit of the index row's own repository.
//!
//! cargo installs into a staging root, which is moved to `ref-<sha>` once
//! `.crates2.json` has said which commit it built, so shep needs no `git`. A
//! `ref-<sha>` that already holds the binary is kept as it is.

use std::path::{Path, PathBuf};

use serde_json::Value;
use shep_core::paths::ShepPaths;

use super::install_cargo::{self, with_tail};
use super::install_keep::Kept;
use super::receipt;
use super::session::{Progress, Step};
use crate::dog_index::AvailableDog;
use crate::exit::ExitCode;
use crate::output::Streams;

/// Where cargo records what it installed, inside the root.
const CRATES2: &str = ".crates2.json";

/// The shortest and longest hex string that names a commit.
const COMMIT_LENGTH: std::ops::RangeInclusive<usize> = 7..=40;

/// The length of a full commit id, which is what `.crates2.json` carries.
const FULL_COMMIT: usize = 40;

/// A `--ref` install that is in place.
#[derive(Debug)]
pub(super) struct Acquired {
    /// The full commit cargo built, 40 lowercase hex characters.
    pub(super) commit: String,
    /// The cargo command line that built it, program first.
    pub(super) command: Vec<String>,
    /// The `ref-<sha>` directory it landed in.
    pub(super) root: PathBuf,
    /// The binary inside it.
    pub(super) binary: PathBuf,
    /// The receipt of the install it replaces, put back if this one is refused.
    pub(super) kept: Kept,
}

/// A cargo build that succeeded but is not yet in place.
#[derive(Debug)]
struct Built {
    commit: String,
    command: Vec<String>,
}

/// Whether `git_ref` is one cargo takes as a value: not empty, not a flag, and
/// without whitespace or control characters.
pub(super) fn is_acceptable(git_ref: &str) -> bool {
    !git_ref.is_empty()
        && !git_ref.starts_with('-')
        && !git_ref.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The cargo flags to try for `git_ref`, in order. A hex string of 7 to 40
/// characters is a commit; anything else is tried as a tag, then a branch.
fn flags_for(git_ref: &str) -> &'static [&'static str] {
    let is_hex =
        COMMIT_LENGTH.contains(&git_ref.len()) && git_ref.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex {
        &["--rev"]
    } else {
        &["--tag", "--branch"]
    }
}

/// Builds `dog` at `git_ref` and puts it in `ref-<sha>` under its directory.
pub(super) async fn acquire(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    cargo: &Path,
    dog: &AvailableDog,
    git_ref: &str,
    progress: Option<&Progress>,
) -> Result<Acquired, ExitCode> {
    let dog_dir = paths.dogs_dir.join(&dog.adopt_as);
    // A leading dot cannot be a version or a name the index passes.
    let staging = dog_dir.join(".ref-build");
    let built = build(
        streams,
        cargo,
        &staging,
        (&dog.repo, &dog.package, git_ref),
        progress,
    )
    .await?;
    let root = dog_dir.join(format!("ref-{}", built.commit));
    let binary = root
        .join("bin")
        .join(format!("{}{}", dog.package, std::env::consts::EXE_SUFFIX));
    let receipt_file = receipt::receipt_path(paths, &dog.adopt_as);
    let kept = Kept::park(&[&receipt_file]).map_err(|err| {
        streams.fail(
            ExitCode::Failure,
            &format!("setting aside the installed {}: {err}", dog.adopt_as),
        )
    })?;
    place(&staging, &root, &binary).map_err(|err| {
        streams.fail(
            ExitCode::Failure,
            &format!("moving the build to {}: {err}", root.display()),
        )
    })?;
    Ok(Acquired {
        commit: built.commit,
        command: built.command,
        root,
        binary,
        kept,
    })
}

/// Runs `cargo install --git <repo>` for `package` at `git_ref` into
/// `staging`, and reads the commit it built.
async fn build(
    streams: &mut Streams<'_>,
    cargo: &Path,
    staging: &Path,
    (repo, package, git_ref): (&str, &str, &str),
    progress: Option<&Progress>,
) -> Result<Built, ExitCode> {
    let _ = std::fs::remove_dir_all(staging);
    streams.aside(
        "install",
        &format!("building {package} from {git_ref} of {repo} with cargo"),
    );
    if let Some(progress) = progress {
        progress.tell(Step::Building);
    }
    let mut said = Vec::new();
    for flag in flags_for(git_ref) {
        let command: Vec<String> = [
            "cargo",
            "install",
            "--locked",
            "--git",
            repo,
            flag,
            git_ref,
            "--root",
            &staging.display().to_string(),
            package,
        ]
        .map(str::to_owned)
        .into();
        match install_cargo::run(cargo, &command[1..], progress).await {
            Ok(ran) if ran.status.success() => {
                let Some(commit) = commit_of(staging, package) else {
                    let _ = std::fs::remove_dir_all(staging);
                    return Err(streams.fail(
                        ExitCode::Failure,
                        &format!(
                            "cargo built {package} from {git_ref} but {CRATES2} names no commit \
                             for it; nothing was adopted"
                        ),
                    ));
                };
                return Ok(Built { commit, command });
            }
            Ok(ran) => {
                if !ran.tail.is_empty() {
                    said.push(format!("with {flag}:\n{}", ran.tail));
                }
                let _ = std::fs::remove_dir_all(staging);
            }
            Err(err) => {
                return Err(streams.fail(
                    ExitCode::SpawnFailed,
                    &format!("running {}: {err}", cargo.display()),
                ));
            }
        }
    }
    let tried = match flags_for(git_ref) {
        [_] => "a commit",
        _ => "a tag, then a branch",
    };
    Err(streams.fail(
        ExitCode::Failure,
        &with_tail(
            format!(
                "`cargo install` for {package} from {git_ref} of {repo} failed (tried as \
                 {tried}); nothing was adopted"
            ),
            &said.join("\n"),
        ),
    ))
}

/// Moves the staged build to `root`, unless `root` already holds `package`'s
/// binary, in which case that one stays and the staged one is dropped.
///
/// # Errors
/// `root` could not be cleared or the staged build could not be moved.
fn place(staging: &Path, root: &Path, binary: &Path) -> std::io::Result<()> {
    if binary.is_file() {
        return std::fs::remove_dir_all(staging);
    }
    match std::fs::remove_dir_all(root) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    std::fs::rename(staging, root)
}

/// The full commit `.crates2.json` in `root` records for `package`, if it
/// installed from git.
fn commit_of(root: &Path, package: &str) -> Option<String> {
    let document: Value = serde_json::from_slice(&std::fs::read(root.join(CRATES2)).ok()?).ok()?;
    let prefix = format!("{package} ");
    document
        .get("installs")?
        .as_object()?
        .keys()
        .filter(|key| key.starts_with(&prefix) && key.contains("(git+"))
        .find_map(|key| {
            let (_source, commit) = key.strip_suffix(')')?.rsplit_once('#')?;
            let is_full = commit.len() == FULL_COMMIT
                && commit
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            is_full.then(|| commit.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_to_forty_hex_characters_are_a_commit_and_anything_else_is_a_tag_then_a_branch() {
        for commit in ["3f9c2a1", "3F9C2A1", &"a".repeat(40)] {
            assert_eq!(flags_for(commit), ["--rev"], "{commit}");
        }
        for name in [
            "main",
            "v1.2.0",
            "3f9c2a",
            &"a".repeat(41),
            "release-abc1234",
        ] {
            assert_eq!(flags_for(name), ["--tag", "--branch"], "{name}");
        }
    }

    #[test]
    fn the_commit_is_the_fragment_of_the_git_install_for_the_package() {
        let dir = tempfile::tempdir().unwrap();
        let full = "0123456789abcdef0123456789abcdef01234567";
        let document = serde_json::json!({"installs": {
            "other 1.0.0 (git+https://example.com/o?branch=x#ffffffffffffffffffffffffffffffffffffffff)": {},
            "dog 0.2.4 (registry+https://github.com/rust-lang/crates.io-index)": {},
            format!("dog 0.2.4 (git+https://example.com/d?branch=main#{full})"): {},
        }});
        std::fs::write(dir.path().join(CRATES2), document.to_string()).unwrap();

        assert_eq!(commit_of(dir.path(), "dog").as_deref(), Some(full));
    }

    #[test]
    fn a_commit_that_is_not_forty_lowercase_hex_is_not_one_shep_will_name_a_path_for() {
        let dir = tempfile::tempdir().unwrap();
        for fragment in ["abc123", "../../x", &"A".repeat(40), &"a".repeat(41)] {
            let document = serde_json::json!({"installs": {
                format!("dog 0.2.4 (git+https://example.com/d#{fragment})"): {},
            }});
            std::fs::write(dir.path().join(CRATES2), document.to_string()).unwrap();

            assert_eq!(commit_of(dir.path(), "dog"), None, "{fragment}");
        }
    }

    #[test]
    fn no_file_or_a_registry_install_names_no_commit() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(commit_of(dir.path(), "dog"), None);
        std::fs::write(
            dir.path().join(CRATES2),
            r#"{"installs":{"dog 0.2.4 (registry+https://x)":{}}}"#,
        )
        .unwrap();
        assert_eq!(commit_of(dir.path(), "dog"), None);
    }
}
