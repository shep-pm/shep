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

/// What a caller needs out of a log path: the only two opens this crate makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogOpen {
    /// Create the file when missing and append to it: the pump's handle.
    AppendCreating,
    /// Truncate a file that exists and create nothing: `shep flush`.
    TruncateExisting,
}

/// The create mode the standard library defaults to, which the umask narrows.
#[cfg(target_os = "linux")]
const CREATE_MODE: u32 = 0o666;

/// Opens `path` as `open` asks, refusing a symlink the daemon cannot trust
///
/// The one opener of a log file in this crate. [`check_log_ancestry`] runs
/// before it at both call sites and covers ownership and mode. On Linux 5.6
/// and later the parent is resolved through links owned by the daemon's user
/// or root, refusing any other, and the open is `openat2` with
/// `RESOLVE_NO_SYMLINKS` on the result. Elsewhere, and where the kernel or a
/// seccomp filter refuses the syscall, it is `O_NOFOLLOW`: the last component.
///
/// # Errors
///
/// Whatever the open reported, with `ELOOP` relabelled to
/// [`SYMLINK_REFUSED`]: `NotFound` passes through untouched.
pub(crate) async fn open_log_path(open: LogOpen, path: &Path) -> io::Result<tokio::fs::File> {
    #[cfg(target_os = "linux")]
    {
        open_log_path_as(open, path, crate::server::daemon_uid()).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        open_nofollow(open, path).await.map_err(name_the_symlink)
    }
}

/// [`open_log_path`] with the daemon's effective uid supplied, so a link
/// owned by someone else is reachable from a test that cannot chown.
#[cfg(target_os = "linux")]
async fn open_log_path_as(
    open: LogOpen,
    path: &Path,
    daemon_uid: u32,
) -> io::Result<tokio::fs::File> {
    settle(open, path, open_resolving(open, path, daemon_uid).await).await
}

/// The resolving open, on a blocking thread: the walk and the syscall wait on
/// the disk.
#[cfg(target_os = "linux")]
async fn open_resolving(open: LogOpen, path: &Path, daemon_uid: u32) -> io::Result<std::fs::File> {
    use nix::fcntl::OFlag;
    use nix::sys::stat::Mode;

    let (flags, mode) = match open {
        LogOpen::AppendCreating => (
            OFlag::O_WRONLY | OFlag::O_APPEND | OFlag::O_CREAT,
            Mode::from_bits_truncate(CREATE_MODE),
        ),
        // `openat2` rejects a mode without `O_CREAT`.
        LogOpen::TruncateExisting => (OFlag::O_WRONLY | OFlag::O_TRUNC, Mode::empty()),
    };
    let flags = flags | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let resolved = super::trusted_links::resolve_parent(&path, daemon_uid)?;
        crate::sys::open_resolving_no_symlinks(&resolved, flags, mode)
    })
    .await
    .map_err(io::Error::other)?
}

/// Turns what the resolving open reported into the handle, falling back to
/// [`open_nofollow`] when the syscall is not available here.
///
/// `ENOSYS` is a kernel below 5.6; `EPERM` is a seccomp filter that blocks it.
/// Split from [`open_log_path`] so a test can report either without one.
#[cfg(target_os = "linux")]
async fn settle(
    open: LogOpen,
    path: &Path,
    reported: io::Result<std::fs::File>,
) -> io::Result<tokio::fs::File> {
    match reported {
        Ok(file) => Ok(tokio::fs::File::from_std(file)),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(nix::libc::ENOSYS | nix::libc::EPERM)
            ) =>
        {
            open_nofollow(open, path).await.map_err(name_the_symlink)
        }
        Err(error) => Err(name_the_symlink(error)),
    }
}

/// The check-then-`O_NOFOLLOW` open: the only one off Linux, and Linux's
/// fallback.
async fn open_nofollow(open: LogOpen, path: &Path) -> io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    match open {
        LogOpen::AppendCreating => options.create(true).append(true),
        LogOpen::TruncateExisting => options.write(true).truncate(true),
    };
    #[cfg(unix)]
    options.custom_flags(nix::libc::O_NOFOLLOW);
    options.open(path).await
}

/// Relabels the `ELOOP` an `O_NOFOLLOW` open answers with, leaving every
/// other error exactly as the OS reported it.
///
/// One errno covers both supported platforms: POSIX specifies `ELOOP` and
/// Darwin's `open(2)` matches. The kind is carried over; only the message
/// changes.
#[cfg(unix)]
fn name_the_symlink(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(nix::libc::ELOOP) {
        io::Error::new(error.kind(), SYMLINK_REFUSED)
    } else {
        error
    }
}

/// The non-unix arm of [`name_the_symlink`]: no `O_NOFOLLOW`, so no refusal
/// to relabel.
#[cfg(not(unix))]
fn name_the_symlink(error: io::Error) -> io::Error {
    error
}

/// Refuses a log path another local user could redirect, under root only
///
/// [`open_log_path`]'s other half, run before it (and before any `mkdir`)
/// at both call sites. A loose ancestry escalates only under a privileged
/// daemon, so root refuses and everyone else is warned once per path.
/// [`loose_ancestor`] defines loose. The window between the check and the
/// open is closed by [`open_log_path`] on Linux 5.6 and later, and open
/// elsewhere (`docs/specs/deferred.md`).
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

/// Whether a link or directory owned by `owner` is one the daemon can trust
/// not to redirect it: its own user's, or root's.
#[cfg(unix)]
pub(super) fn is_trusted_owner(owner: u32, daemon_uid: u32) -> bool {
    owner == daemon_uid || owner == ROOT_UID
}

/// The permission bit that lets every local user create entries in a
/// directory. Narrower than `boot`'s socket-directory check (`0o022`, group
/// or world): a group-writable log directory names accounts an operator
/// chose, while this bit names everyone.
#[cfg(unix)]
const WORLD_WRITABLE: u32 = 0o002;

/// Log paths whose loose ancestry has already been reported, so an
/// unprivileged shepherd says it once rather than on every open.
///
/// Keyed by the log path, not by the offending ancestor: that is what an
/// operator asking which of their apps this is about is asking. Bounded by
/// the number of distinct log paths in the flock.
#[cfg(unix)]
static WARNED_LOOSE_LOG_PATHS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<PathBuf>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// [`check_log_ancestry`] with the daemon's effective uid supplied, so the
/// privileged arm is reachable from a test that is not running as root.
///
/// # Errors
///
/// [`check_log_ancestry`]'s.
#[cfg(unix)]
fn check_log_ancestry_as(path: &Path, daemon_uid: u32) -> io::Result<()> {
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
struct LooseAncestor {
    /// The offending component, as it appears in the log path.
    path: PathBuf,
    /// Why it offends: reads as the predicate in `"<path> <reason>"`.
    reason: LooseReason,
}

/// Why an ancestor of a log path counts as loose.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LooseReason {
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
/// ancestor that does not exist, or cannot be stat'd, is skipped.
#[cfg(unix)]
fn loose_ancestor(path: &Path, daemon_uid: u32) -> Option<LooseAncestor> {
    use std::os::unix::fs::MetadataExt as _;

    path.parent()?
        .ancestors()
        .filter_map(|ancestor| Some((ancestor, std::fs::symlink_metadata(ancestor).ok()?)))
        .find_map(|(ancestor, meta)| {
            let reason = if !is_trusted_owner(meta.uid(), daemon_uid) {
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

    /// The link is foreign to the daemon and points at a root-owned, tight
    /// directory, so a walk that followed it would wave it through and blame
    /// a higher ancestor instead: the tempdir, or `/tmp` under root, where
    /// the root-owned tempdir is exempt. Only the path in the answer tells
    /// the two apart. This is the case `O_NOFOLLOW`
    /// cannot cover: the redirect is one level up.
    #[test]
    fn a_symlinked_component_is_judged_as_the_link_not_as_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("logs");
        // `/usr` exists and is root-owned `0755` on both tier-1 platforms,
        // an ancestor the walk would wave through if it followed the link.
        std::os::unix::fs::symlink("/usr", &link).unwrap();
        let log = link.join("web-0-out.log");

        // As in the ownership case above: a root runner owns the link, and
        // root is exempt whatever the daemon's uid is, so it hands the link
        // away instead of moving the daemon's uid.
        let daemon_uid = if me() == ROOT_UID {
            use std::os::unix::fs::MetadataExt as _;

            std::os::unix::fs::lchown(&link, Some(FOREIGN_UID), None).unwrap();
            assert_eq!(
                std::fs::symlink_metadata(&link).unwrap().uid(),
                FOREIGN_UID,
                "lchown must hand over the link itself, not what it points at"
            );
            ROOT_UID
        } else {
            me() + 1
        };

        let loose = loose_ancestor(&log, daemon_uid).expect("a foreign-owned component is loose");
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

    /// A symlinked parent the resolving open refuses and the `O_NOFOLLOW`
    /// open follows, so the file landing below the link proves which open ran.
    #[cfg(target_os = "linux")]
    async fn open_below_a_symlinked_parent_after(errno: i32) -> (io::Result<()>, bool) {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("logs");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let reported = Err(io::Error::from_raw_os_error(errno));
        let opened = settle(LogOpen::AppendCreating, &link.join("web-out.log"), reported)
            .await
            .map(drop);
        (opened, real.join("web-out.log").exists())
    }

    /// Fails if `ENOSYS` is not the fallback's trigger: a kernel below 5.6
    /// would then refuse every log open outright.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_enosys_from_the_resolving_open_falls_back_to_the_nofollow_open() {
        let (opened, created) = open_below_a_symlinked_parent_after(nix::libc::ENOSYS).await;

        opened.expect("the fallback open must succeed where the resolving one is unavailable");
        assert!(
            created,
            "the fallback is the O_NOFOLLOW open, which follows the parent"
        );
    }

    /// Fails if `EPERM` is not the fallback's trigger: a seccomp filter that
    /// blocks `openat2` would then refuse every log open outright.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_eperm_from_the_resolving_open_falls_back_to_the_nofollow_open() {
        let (opened, created) = open_below_a_symlinked_parent_after(nix::libc::EPERM).await;

        opened.expect("the fallback open must succeed where the resolving one is blocked");
        assert!(
            created,
            "the fallback is the O_NOFOLLOW open, which follows the parent"
        );
    }

    /// Fails if the fallback widens to every errno: an `ELOOP` is the
    /// resolving open working, and must reach the operator as the symlink
    /// refusal rather than be retried by an open that follows the parent.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn any_other_errno_from_the_resolving_open_is_not_retried() {
        let (opened, created) = open_below_a_symlinked_parent_after(nix::libc::ELOOP).await;

        let error = opened.expect_err("an ELOOP must be reported, not retried");
        assert_eq!(error.to_string(), SYMLINK_REFUSED);
        assert!(!created, "no file may land below the link");
    }

    /// Fails if the walk's refusal is lost: a link owned by neither the
    /// daemon nor root can be retargeted by its owner, so the open must stop
    /// at it and leave the directory it points at without a file.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_symlinked_parent_owned_by_another_user_is_refused_on_linux() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("logs");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // As in the ownership cases above: a root runner hands the link away.
        let daemon_uid = if me() == ROOT_UID {
            std::os::unix::fs::lchown(&link, Some(FOREIGN_UID), None).unwrap();
            ROOT_UID
        } else {
            me() + 1
        };

        let error = open_log_path_as(
            LogOpen::AppendCreating,
            &link.join("web-out.log"),
            daemon_uid,
        )
        .await
        .expect_err("a link owned by another user must not be followed");

        let rendered = error.to_string();
        assert!(rendered.starts_with(SYMLINK_REFUSED), "{rendered}");
        assert!(rendered.contains(&link.display().to_string()), "{rendered}");
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
    }
}
