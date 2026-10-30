//! Resolving a log path's parent through the symlinks the daemon trusts
//!
//! `O_NOFOLLOW` and `RESOLVE_NO_SYMLINKS` refuse every link, including
//! `current -> releases/<sha>` and a symlinked home that
//! `log_path_security::loose_ancestor` waves through as the daemon's own.
//! [`resolve_parent`] follows exactly those links ahead of the open, so the
//! open itself can still refuse any link it meets.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::log_path_security::{SYMLINK_REFUSED, is_trusted_owner};

/// The most links one resolution follows: Linux's own `MAXSYMLINKS`, past
/// which `open(2)` answers `ELOOP`.
const MAX_LINKS: usize = 40;

/// `path` with every symlink above its last component replaced by what it
/// points at, each one owned by the daemon's user or by root.
///
/// The last component is not followed: a link there stays for the open to
/// refuse. A relative `path` is read from the working directory, as the open
/// reads it, and comes back absolute.
///
/// # Errors
///
/// The `lstat` or `readlink` failure on a component, `NotFound` for a parent
/// that is not there, `ELOOP` past [`MAX_LINKS`] links, and
/// [`io::ErrorKind::PermissionDenied`] naming a link owned by anyone else,
/// worded as [`SYMLINK_REFUSED`].
pub(super) fn resolve_parent(path: &Path, daemon_uid: u32) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        return Ok(absolute);
    };

    let mut resolved = PathBuf::from("/");
    let mut pending: VecDeque<OsString> = parent
        .components()
        .map(|component| component.as_os_str().to_owned())
        .collect();
    let mut followed = 0;
    while let Some(part) = pending.pop_front() {
        if part == "/" {
            resolved = PathBuf::from("/");
            continue;
        }
        if part == "." {
            continue;
        }
        if part == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&part);
        let meta = std::fs::symlink_metadata(&candidate)?;
        if !meta.file_type().is_symlink() {
            resolved = candidate;
            continue;
        }
        if !is_trusted_owner(meta.uid(), daemon_uid) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{SYMLINK_REFUSED} ({} is a symlink owned by uid {})",
                    candidate.display(),
                    meta.uid()
                ),
            ));
        }
        followed += 1;
        if followed > MAX_LINKS {
            return Err(io::Error::from_raw_os_error(nix::libc::ELOOP));
        }
        let target = std::fs::read_link(&candidate)?;
        for component in target.components().rev() {
            pending.push_front(component.as_os_str().to_owned());
        }
    }
    Ok(resolved.join(name))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::super::log_path_security::ROOT_UID;
    use super::super::testing::{FOREIGN_UID, me};
    use super::*;

    /// The tempdir with its own links (macOS's `/var`) already resolved, so
    /// the expected paths below are what a resolution should answer.
    fn real_dir(dir: &tempfile::TempDir) -> PathBuf {
        std::fs::canonicalize(dir.path()).unwrap()
    }

    #[test]
    fn a_path_with_no_links_resolves_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = real_dir(&dir);
        std::fs::create_dir(root.join("logs")).unwrap();

        let resolved = resolve_parent(&root.join("logs/web-out.log"), me()).unwrap();

        assert_eq!(resolved, root.join("logs/web-out.log"));
    }

    /// Fails if a link the daemon's user planted is refused: the layout
    /// `current -> releases/<sha>` has to keep working.
    #[test]
    fn a_link_the_daemon_owns_is_followed_to_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = real_dir(&dir);
        std::fs::create_dir(root.join("releases")).unwrap();
        symlink("releases", root.join("current")).unwrap();

        let resolved = resolve_parent(&root.join("current/web-out.log"), me()).unwrap();

        assert_eq!(resolved, root.join("releases/web-out.log"));
    }

    /// Fails if only the first link is checked or `..` is read lexically: the
    /// chain ends in a directory a textual walk would never reach.
    #[test]
    fn a_chain_of_links_with_dotdot_resolves_hop_by_hop() {
        let dir = tempfile::tempdir().unwrap();
        let root = real_dir(&dir);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        symlink("../../real", root.join("a/b/hop")).unwrap();
        symlink("a/b/hop", root.join("logs")).unwrap();

        let resolved = resolve_parent(&root.join("logs/web-out.log"), me()).unwrap();

        assert_eq!(resolved, root.join("real/web-out.log"));
    }

    /// Fails if a link owned by anyone else is followed: its owner can
    /// retarget it under the daemon. The refusal names the link.
    #[test]
    fn a_link_owned_by_another_user_is_refused_naming_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = real_dir(&dir);
        std::fs::create_dir(root.join("real")).unwrap();
        let link = root.join("logs");
        symlink("real", &link).unwrap();
        // As in `log_path_security`'s ownership cases: a root runner hands
        // the link away, because root is exempt whatever the daemon's uid is.
        let daemon_uid = if me() == ROOT_UID {
            std::os::unix::fs::lchown(&link, Some(FOREIGN_UID), None).unwrap();
            ROOT_UID
        } else {
            me() + 1
        };

        let error = resolve_parent(&link.join("web-out.log"), daemon_uid)
            .expect_err("a link its owner can retarget must not be followed");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let rendered = error.to_string();
        assert!(rendered.starts_with(SYMLINK_REFUSED), "{rendered}");
        assert!(rendered.contains(&link.display().to_string()), "{rendered}");
    }

    /// Fails if the last component is followed: the open must see the link
    /// and refuse it, so a log file that is a link keeps its target alone.
    #[test]
    fn a_link_at_the_last_component_is_left_for_the_open_to_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let root = real_dir(&dir);
        symlink("elsewhere.log", root.join("web-out.log")).unwrap();

        let resolved = resolve_parent(&root.join("web-out.log"), me()).unwrap();

        assert_eq!(resolved, root.join("web-out.log"));
    }

    /// `truncate_log` turns this into a successful no-op, so it must stay
    /// `NotFound` rather than become some other kind.
    #[test]
    fn a_parent_that_is_not_there_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = real_dir(&dir);

        let error = resolve_parent(&root.join("nope/web-out.log"), me()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// Fails if a loop of links, each one trusted, spins forever.
    #[test]
    fn a_loop_of_links_ends_in_eloop() {
        let dir = tempfile::tempdir().unwrap();
        let root = real_dir(&dir);
        symlink("b", root.join("a")).unwrap();
        symlink("a", root.join("b")).unwrap();

        let error = resolve_parent(&root.join("a/web-out.log"), me()).unwrap_err();

        assert_eq!(error.raw_os_error(), Some(nix::libc::ELOOP));
    }
}
