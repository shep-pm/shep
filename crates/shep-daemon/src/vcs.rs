//! Reading the revision a directory is checked out on, from the files git
//! keeps, without running git.
//!
//! The grammar is deliberately narrow: the directory is one an operator
//! points a sheep at, possibly owned by someone else, so every file read is
//! capped, a ref name that could leave the git dir is refused, and anything
//! this reader does not recognise is absent rather than guessed. Reftable
//! repositories, symbolic loose refs and a `packed-refs` over the cap all
//! read as absent. No `git` binary runs and no environment variable is read.

use std::fmt;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use shep_core::protocol::Vcs;

/// The most bytes any one file is read for.
///
/// A `HEAD`, a loose ref, a `gitdir:` pointer and a `commondir` are each under
/// 200 bytes; 4 KiB leaves room for a long path and bounds what a hostile
/// directory can make this read allocate.
const READ_CAP: u64 = 4096;

/// The length of a revision in lowercase hex: SHA-1, then SHA-256.
const HEX_LENGTHS: [usize; 2] = [40, 64];

/// Why a directory reported no revision, for the shepherd's debug log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Absent {
    /// No `.git` in the directory or any directory above it.
    NotACheckout,
    /// A file the read needed could not be opened or read.
    Unreadable,
    /// A file was there but held something outside the grammar above.
    Unrecognised,
}

impl fmt::Display for Absent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotACheckout => "not a checkout",
            Self::Unreadable => "unreadable",
            Self::Unrecognised => "unrecognised",
        })
    }
}

/// The revision checked out in `dir`, or in the nearest directory above it
/// that holds a `.git`.
///
/// `None` for anything that is not a SHA-1 or SHA-256 checkout this reader
/// recognises, including a reftable repository.
#[must_use]
pub fn read_revision(dir: &Path) -> Option<Vcs> {
    revision_of(dir).ok()
}

/// [`read_revision`], naming why when there is nothing to report.
pub(crate) fn revision_of(dir: &Path) -> Result<Vcs, Absent> {
    let git_dir = find_git_dir(dir)?;
    let common_dir = common_dir_of(&git_dir)?;
    if git_dir.join("reftable").is_dir() || common_dir.join("reftable").is_dir() {
        return Err(Absent::Unrecognised);
    }

    let head = read_capped(&git_dir.join("HEAD"))?.ok_or(Absent::Unreadable)?;
    let head = line(&head);
    if let Some(name) = head.strip_prefix("ref: ") {
        let revision = resolve_ref(name, &[&git_dir, &common_dir])?;
        let vcs = Vcs::new(revision);
        return Ok(match name.strip_prefix("refs/heads/") {
            Some(branch) => vcs.with_branch(branch),
            None => vcs,
        });
    }
    if is_revision(head) {
        return Ok(Vcs::new(head));
    }
    Err(Absent::Unrecognised)
}

/// The git dir of the nearest checkout at or above `dir`.
///
/// The nearest `.git` decides: one that is unreadable or unrecognised ends
/// the walk rather than sending it on to a checkout further up.
fn find_git_dir(dir: &Path) -> Result<PathBuf, Absent> {
    for candidate in dir.ancestors() {
        let dot_git = candidate.join(".git");
        let Ok(found) = fs::symlink_metadata(&dot_git) else {
            continue;
        };
        // A link is followed, as git does: one to a git dir is that dir, and
        // one that dangles is a `.git` that cannot be read.
        let meta = if found.is_symlink() {
            fs::metadata(&dot_git).map_err(|_| Absent::Unreadable)?
        } else {
            found
        };
        if meta.is_dir() {
            return Ok(dot_git);
        }
        let pointer = read_capped(&dot_git)?.ok_or(Absent::Unreadable)?;
        let target = line(&pointer)
            .strip_prefix("gitdir: ")
            .ok_or(Absent::Unrecognised)?;
        return Ok(candidate.join(target));
    }
    Err(Absent::NotACheckout)
}

/// Where a linked worktree keeps the refs it shares, or `git_dir` itself for
/// a checkout that is not one.
fn common_dir_of(git_dir: &Path) -> Result<PathBuf, Absent> {
    Ok(match read_capped(&git_dir.join("commondir"))? {
        Some(pointer) => git_dir.join(line(&pointer)),
        None => git_dir.to_path_buf(),
    })
}

/// The revision `name` points at: its loose file in the first of `dirs` that
/// has one, else its line in the first `packed-refs` that lists it.
fn resolve_ref(name: &str, dirs: &[&Path]) -> Result<String, Absent> {
    if !is_ref_name(name) {
        return Err(Absent::Unrecognised);
    }
    for dir in dirs {
        if let Some(loose) = read_capped(&dir.join(name))? {
            return revision_text(line(&loose));
        }
    }
    for dir in dirs {
        let Some(packed) = read_capped(&dir.join("packed-refs"))? else {
            continue;
        };
        let found = packed
            .lines()
            .filter_map(|entry| entry.split_once(' '))
            .find(|(_, packed_name)| *packed_name == name);
        if let Some((revision, _)) = found {
            return revision_text(revision);
        }
    }
    Err(Absent::Unrecognised)
}

/// `text` as an owned revision, or [`Absent::Unrecognised`].
fn revision_text(text: &str) -> Result<String, Absent> {
    if is_revision(text) {
        Ok(text.to_string())
    } else {
        Err(Absent::Unrecognised)
    }
}

/// `text` without the one newline git ends a one-line file with.
fn line(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

/// A ref name this reader will join onto a git dir: under `refs/`, every
/// component a plain name, with no `..`, no leading dot and no character a
/// path treats specially.
fn is_ref_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("refs/") else {
        return false;
    };
    !rest.is_empty()
        && rest.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.contains("..")
                && !part.ends_with(".lock")
                && part
                    .chars()
                    .all(|c| !c.is_control() && !matches!(c, '\\' | ':' | ' '))
        })
}

/// Whether `text` is a whole revision: 40 or 64 lowercase hex digits.
fn is_revision(text: &str) -> bool {
    HEX_LENGTHS.contains(&text.len())
        && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The contents of the regular file at `path`, or `None` when it does not exist.
///
/// A non-file (a directory, or a fifo that would block the open) and a file
/// over [`READ_CAP`] are [`Absent::Unrecognised`]; one that fails to read is
/// [`Absent::Unreadable`].
fn read_capped(path: &Path) -> Result<Option<String>, Absent> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Absent::Unreadable),
    };
    if !meta.is_file() {
        return Err(Absent::Unrecognised);
    }
    let mut text = String::new();
    let read = fs::File::open(path)
        .map_err(|_| Absent::Unreadable)?
        .take(READ_CAP + 1)
        .read_to_string(&mut text);
    match read {
        Ok(_) if text.len() as u64 > READ_CAP => Err(Absent::Unrecognised),
        Ok(_) => Ok(Some(text)),
        Err(err) if err.kind() == ErrorKind::InvalidData => Err(Absent::Unrecognised),
        Err(_) => Err(Absent::Unreadable),
    }
}

#[cfg(test)]
mod tests;
