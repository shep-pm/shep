use core::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// What an operator is told when a log path turns out to be a symlink.
///
/// One owner for the sentence, cited by [`open_log_path`] and by both
/// openers' tests, so the operator reads a remedy rather than a bare
/// `ELOOP`. The path is not in here: every caller already prefixes one.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) const SYMLINK_REFUSED: &str = "refusing to follow a symlink at this log path; shep \
     opens log files with O_NOFOLLOW, so point out_file/err_file at the real file";

/// Opens `path` through `options`, refusing a symlink at the path itself
///
/// The one opener of a log file in this crate: the pump's append handle and
/// `shep flush`'s truncating one both come through here.
/// [`check_log_ancestry`] runs before it at both call sites, ahead of
/// `open_append`'s `mkdir`. `O_NOFOLLOW` guards only the final component, so
/// a symlinked parent still resolves; only that check covers it.
///
/// # Errors
///
/// Whatever the open reported, with `ELOOP` relabelled to
/// [`SYMLINK_REFUSED`]: `NotFound` passes through untouched.
pub(crate) async fn open_log_path(
    options: &mut tokio::fs::OpenOptions,
    path: &Path,
) -> io::Result<tokio::fs::File> {
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_NOFOLLOW);
    options.open(path).await.map_err(name_the_symlink)
}

/// Relabels the `ELOOP` an `O_NOFOLLOW` open answers with, leaving every
/// other error exactly as the OS reported it.
///
/// One errno covers both supported platforms: POSIX specifies `ELOOP` and
/// Darwin's `open(2)` matches. The kind is carried over; only the message
/// changes.
#[cfg(unix)]
pub(super) fn name_the_symlink(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(nix::libc::ELOOP) {
        io::Error::new(error.kind(), SYMLINK_REFUSED)
    } else {
        error
    }
}

/// The non-unix arm of [`name_the_symlink`]: no `O_NOFOLLOW`, so no refusal
/// to relabel.
#[cfg(not(unix))]
pub(super) fn name_the_symlink(error: io::Error) -> io::Error {
    error
}

/// Refuses a log path another local user could redirect, under root only
///
/// [`open_log_path`]'s other half, run before it (and before any `mkdir`)
/// at both call sites. A loose ancestry escalates only under a privileged
/// daemon, so root refuses and everyone else is warned once per path.
/// [`loose_ancestor`] defines loose, and the window between the check and
/// the open stays open (`docs/specs/deferred.md`).
///
/// # Errors
///
/// [`io::ErrorKind::PermissionDenied`] naming the loose ancestor, when the
/// daemon's effective uid is root. The message carries no path of its own.
pub(crate) fn check_log_ancestry(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        check_log_ancestry_as(path, crate::server::daemon_uid())
    }
    // Windows has neither the uid model this reads nor the `shep flush`
    // surface that reaches it, so there is nothing to check.
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// The effective uid a shepherd has to be running as for a loose ancestor to
/// be an escalation rather than a footgun.
#[cfg(unix)]
pub(super) const ROOT_UID: u32 = 0;

/// The permission bit that lets every local user create entries in a
/// directory. Narrower than `boot`'s socket-directory check (`0o022`, group
/// or world): a group-writable log directory names accounts an operator
/// chose, while this bit names everyone.
#[cfg(unix)]
pub(super) const WORLD_WRITABLE: u32 = 0o002;

/// Log paths whose loose ancestry has already been reported, so an
/// unprivileged shepherd says it once rather than on every open.
///
/// Keyed by the log path, not by the offending ancestor: that is what an
/// operator asking which of their apps this is about is asking. Bounded by
/// the number of distinct log paths in the flock.
#[cfg(unix)]
pub(super) static WARNED_LOOSE_LOG_PATHS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<PathBuf>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// [`check_log_ancestry`] with the daemon's effective uid supplied, so the
/// privileged arm is reachable from a test that is not running as root.
///
/// # Errors
///
/// [`check_log_ancestry`]'s.
#[cfg(unix)]
pub(super) fn check_log_ancestry_as(path: &Path, daemon_uid: u32) -> io::Result<()> {
    let Some(loose) = loose_ancestor(path, daemon_uid) else {
        return Ok(());
    };
    if daemon_uid == ROOT_UID {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to open a log file below {}, which {}; a shepherd running as root \
                 writes only below directories its own user owns",
                loose.path.display(),
                loose.reason,
            ),
        ));
    }
    let first_time = WARNED_LOOSE_LOG_PATHS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf());
    if first_time {
        tracing::warn!(
            path = %path.display(),
            ancestor = %loose.path.display(),
            reason = %loose.reason,
            "log path sits below a directory another local user could redirect; a shepherd \
             running as root would refuse to open it"
        );
    }
    Ok(())
}

/// An ancestor of a log path that another local user could use to redirect
/// where that path lands.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LooseAncestor {
    /// The offending component, as it appears in the log path.
    path: PathBuf,
    /// Why it offends: reads as the predicate in `"<path> <reason>"`.
    reason: LooseReason,
}

/// Why an ancestor of a log path counts as loose.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LooseReason {
    /// Owned by a uid that is neither the daemon's own nor root's, so its
    /// owner can replace or redirect it under the daemon (carries that uid).
    /// Also how a symlinked component is caught: the link's own owner is the
    /// user who planted it.
    ForeignOwner(u32),
    /// A directory every local user can create entries in, so anyone can put
    /// a symlink where the next component is about to be resolved.
    WorldWritable,
}

#[cfg(unix)]
impl fmt::Display for LooseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignOwner(uid) => write!(f, "is owned by uid {uid}"),
            Self::WorldWritable => f.write_str("is world-writable"),
        }
    }
}

/// The nearest ancestor of `path` another local user could redirect it
/// through, or `None` when every one is the daemon's to trust.
///
/// Walks the path's own textual components upwards from its parent and stops
/// at the first offender. `symlink_metadata`, never `metadata`: a symlinked
/// component must be read as the link it is, owned by whoever planted it. An
/// ancestor that does not exist, or cannot be stat'd, is skipped, since
/// `open_append` is about to create it at `boot::DIR_MODE` as the daemon.
///
/// One `lstat(2)` per component per log-file open, 7.8 µs for a
/// nine-component path on macOS, so it runs inline rather than paying a
/// `spawn_blocking` hop.
#[cfg(unix)]
pub(super) fn loose_ancestor(path: &Path, daemon_uid: u32) -> Option<LooseAncestor> {
    use std::os::unix::fs::MetadataExt as _;

    path.parent()?
        .ancestors()
        .filter_map(|ancestor| Some((ancestor, std::fs::symlink_metadata(ancestor).ok()?)))
        .find_map(|(ancestor, meta)| {
            let reason = if meta.uid() != daemon_uid && meta.uid() != ROOT_UID {
                LooseReason::ForeignOwner(meta.uid())
            } else if meta.is_dir() && meta.mode() & WORLD_WRITABLE != 0 {
                LooseReason::WorldWritable
            } else {
                return None;
            };
            Some(LooseAncestor {
                path: ancestor.to_path_buf(),
                reason,
            })
        })
}

// Every case here is `#[cfg(unix)]`, as is everything they exercise: the uid
// model `loose_ancestor` reads, the mode bits it tests, and
// `std::os::unix::fs::symlink`.
#[cfg(all(test, unix))]
mod tests {

    use std::io;

    use super::*;
    use crate::testing::capture_logs;

    use super::super::testing::*;

    /// Also the only case pinning the world-writable arm on its own: drop
    /// that arm and this reddens with `None` while the ownership cases below
    /// stay green.
    #[test]
    fn the_nearest_loose_ancestor_is_the_one_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (parent, log) = log_path_under(&dir, 0o777);

        assert_eq!(
            loose_ancestor(&log, me()),
            Some(LooseAncestor {
                path: parent,
                reason: LooseReason::WorldWritable,
            })
        );
    }

    /// The parent is `0700`, so a write-bit-only check waves it through,
    /// while its owner can still replace it under a root shepherd.
    #[test]
    fn an_ancestor_owned_by_another_user_is_loose_however_tight_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let (parent, log) = log_path_under(&dir, 0o700);

        // The predicate is symmetric in the two uids, so an unprivileged
        // runner moves the daemon's rather than the directory's; it cannot
        // chown. A root runner must move the directory's: root is exempt
        // whatever the daemon's uid is.
        let (daemon_uid, owner) = if me() == ROOT_UID {
            std::os::unix::fs::chown(&parent, Some(FOREIGN_UID), None).unwrap();
            (ROOT_UID, FOREIGN_UID)
        } else {
            (me() + 1, me())
        };

        assert_eq!(
            loose_ancestor(&log, daemon_uid),
            Some(LooseAncestor {
                path: parent,
                reason: LooseReason::ForeignOwner(owner),
            })
        );
    }

    /// The link is owned by this user and points at a root-owned, tight
    /// directory, so following it blames the tempdir instead. Only the path
    /// in the answer tells the two apart. This is the case `O_NOFOLLOW`
    /// cannot cover: the redirect is one level up.
    #[test]
    fn a_symlinked_component_is_judged_as_the_link_not_as_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("logs");
        // `/usr` exists and is root-owned `0755` on both tier-1 platforms,
        // an ancestor the walk would wave through if it followed the link.
        std::os::unix::fs::symlink("/usr", &link).unwrap();
        let log = link.join("web-0-out.log");

        let loose = loose_ancestor(&log, me() + 1).expect("a foreign-owned component is loose");
        assert_eq!(
            loose.path,
            link,
            "the link itself must be judged, not what it resolves to: blaming {} means the \
                 walk followed it",
            loose.path.display()
        );
    }

    /// Refusing everywhere would break a developer logging to `/tmp` as
    /// themselves; warning everywhere would leave the root case exiting zero.
    ///
    /// The warn-once half rides along because it is the same call: a count of
    /// two means the dedup set is gone.
    #[test]
    fn a_root_shepherd_refuses_where_an_unprivileged_one_warns_once() {
        let dir = tempfile::tempdir().unwrap();
        let (parent, log) = log_path_under(&dir, 0o777);

        let refused = check_log_ancestry_as(&log, ROOT_UID)
            .expect_err("a root shepherd must not open a log below a loose ancestor");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            refused.to_string().contains(&parent.display().to_string()),
            "the refusal must name the ancestor an operator has to fix: {refused}"
        );

        // Never `me()`: a root test runner would take the arm above.
        // `me() + 1` is non-root by construction and owns nothing here.
        let unprivileged = me() + 1;
        let rendered = capture_logs(|| {
            assert_eq!(check_log_ancestry_as(&log, unprivileged).ok(), Some(()));
            assert_eq!(check_log_ancestry_as(&log, unprivileged).ok(), Some(()));
        });
        assert_eq!(
            rendered.matches("log path sits below").count(),
            1,
            "an unprivileged shepherd warns once per path, not once per open: {rendered}"
        );
    }
}
