//! One exclusive advisory lock, keyed on the file it guards.
//!
//! Every store under `$SHEP_HOME` that publishes a new value by `rename`
//! holds this across its read-modify-write. They are `kv.json`,
//! `overrides.json`, `secrets.json`, `barks.jsonl`, `shep.toml` and
//! `dogs.toml`. `flock(2)` on unix, an exclusive `share_mode(0)` open on
//! Windows. Separate from [`crate::atomic_file`], which is the replace.
//!
//! This keeps two of those replaces apart, and is held across reads the
//! replace never sees.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// An exclusive advisory lock over one file, held for as long as the value
/// lives and released when it drops, including on an early `?` and by the
/// kernel if the process dies holding it.
///
/// The lock is on a sibling `<name>.lock`, never on the guarded file: a
/// store publishes by `rename`, which replaces the inode. Two are held at
/// once only over `shep.toml` then `dogs.toml`.
#[derive(Debug)]
pub struct FileLock {
    /// `flock(2)` is released by this handle's `Drop`. Named with a
    /// leading underscore because it is held, never read.
    #[cfg(unix)]
    _flock: nix::fcntl::Flock<std::fs::File>,
    /// The lock file, opened with `share_mode(0)` so no other handle,
    /// same-process or not, can open it while this one is live. Released
    /// by `Drop`, the same role `_flock` plays on unix, and named with a
    /// leading underscore for the same reason.
    #[cfg(windows)]
    _handle: std::fs::File,
}

impl FileLock {
    /// Blocks until this process holds `path`'s lock exclusively.
    ///
    /// # Errors
    /// The lock file could not be created beside `path`, or `flock` failed
    /// for a reason other than contention (contention blocks rather than
    /// failing).
    #[cfg(unix)]
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;

        use nix::fcntl::{Flock, FlockArg};

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(crate::atomic_file::OWNER_ONLY_FILE_MODE)
            .open(lock_path(path))?;

        // `LockExclusive` blocks; the non-blocking variant would need a
        // retry loop and a deadline, and a writer that waits its turn
        // behind another is exactly the behaviour wanted here.
        Flock::lock(file, FlockArg::LockExclusive)
            .map(|flock| Self { _flock: flock })
            .map_err(|(_file, errno)| std::io::Error::from(errno))
    }

    /// Blocks until this process holds `path`'s lock exclusively.
    ///
    /// `share_mode(0)` denies every other open, in this process or another,
    /// giving the same exclusivity as unix `flock` through a different
    /// door. It gives no blocking wait, though: a contended open fails at
    /// once with `ERROR_SHARING_VIOLATION`, so this polls on a short sleep
    /// until it succeeds.
    ///
    /// # Errors
    /// The lock file could not be created beside `path`, or the open failed
    /// for a reason other than sharing contention (contention retries
    /// rather than failing).
    #[cfg(windows)]
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt as _;

        /// Windows' `ERROR_SHARING_VIOLATION`: another handle already holds
        /// share access this open's `share_mode(0)` denies. Hardcoded
        /// rather than pulled from `windows-sys`, since this crate has no
        /// other Windows-only dependency.
        const ERROR_SHARING_VIOLATION: i32 = 32;

        /// How long a contended retry sleeps before trying again. Short
        /// enough that a lock held for one write's duration (a handful of
        /// small file operations) costs this loop only a few iterations,
        /// long enough not to spin the CPU while it waits.
        const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);

        let lock_path = lock_path(path);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(&lock_path)
            {
                Ok(handle) => return Ok(Self { _handle: handle }),
                Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl FileLock {
    /// [`Self::acquire`], giving up after `wait` instead of blocking for as
    /// long as the holder lives. `Ok(None)` is the holder still holding it.
    ///
    /// Polls: `flock(2)` has no timed wait, so the non-blocking form is
    /// retried on a short sleep until `wait` has passed. One attempt is made
    /// even for a zero `wait`.
    ///
    /// # Errors
    /// As [`Self::acquire`]; contention is the `None`, never an error.
    pub fn acquire_within(path: &Path, wait: Duration) -> std::io::Result<Option<Self>> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            if let Some(lock) = Self::try_acquire(path)? {
                return Ok(Some(lock));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// One non-blocking attempt at `path`'s lock; `None` when it is held.
    #[cfg(unix)]
    fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::unix::fs::OpenOptionsExt as _;

        use nix::errno::Errno;
        use nix::fcntl::{Flock, FlockArg};

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(crate::atomic_file::OWNER_ONLY_FILE_MODE)
            .open(lock_path(path))?;
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(flock) => Ok(Some(Self { _flock: flock })),
            Err((_file, Errno::EWOULDBLOCK)) => Ok(None),
            Err((_file, errno)) => Err(std::io::Error::from(errno)),
        }
    }

    /// One attempt at `path`'s lock; `None` when it is held.
    #[cfg(windows)]
    fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::windows::fs::OpenOptionsExt as _;

        /// As in [`Self::acquire`].
        const ERROR_SHARING_VIOLATION: i32 = 32;

        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(lock_path(path))
        {
            Ok(handle) => Ok(Some(Self { _handle: handle })),
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// How long [`FileLock::acquire_within`] sleeps between attempts.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The lock file that guards `path`: its own name with `.lock` appended, so
/// it sits in `$SHEP_HOME` next to the file it guards and inherits that
/// directory's `0700`.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".lock");
    crate::atomic_file::parent_of(path).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounded_acquire_of_a_held_lock_gives_up_after_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("otel");
        let held = FileLock::acquire(&path).unwrap();

        let started = std::time::Instant::now();
        let second = FileLock::acquire_within(&path, Duration::from_millis(100)).unwrap();

        assert!(second.is_none(), "the lock was held");
        assert!(started.elapsed() >= Duration::from_millis(100));
        drop(held);
        assert!(
            FileLock::acquire_within(&path, Duration::ZERO)
                .unwrap()
                .is_some(),
            "free once released"
        );
    }

    #[test]
    fn the_lock_file_is_a_sibling_of_the_file_it_guards() {
        let path = Path::new("/var/lib/shep/kv.json");
        assert_eq!(lock_path(path), Path::new("/var/lib/shep/kv.json.lock"));
    }

    #[test]
    fn a_second_acquire_on_the_same_path_blocks_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        let first = FileLock::acquire(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            let _second = FileLock::acquire(&path).unwrap();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "must block"
        );
        drop(first);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("must proceed once released");
        t.join().unwrap();
    }

    #[test]
    fn two_different_paths_do_not_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let _kv = FileLock::acquire(&dir.path().join("kv.json")).unwrap();
        let _secrets = FileLock::acquire(&dir.path().join("secrets.json")).unwrap();
    }
}
