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
