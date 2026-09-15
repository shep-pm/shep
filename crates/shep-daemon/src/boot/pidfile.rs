//! The exclusive claim on `$SHEP_HOME`: the pidfile, and the lock held on it
//!
//! [`PidfileLock`] is what makes "one shepherd per home" true. It is taken
//! before the socket bind it serializes, and it is released when the last
//! descriptor on it closes, a crash included, so nothing has to clean up after
//! a daemon that died. [`daemon_liveness`] asks the same question from
//! outside and keeps no claim of its own.

use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use shep_core::paths::ShepPaths;

use super::BootError;

/// The daemon's own pidfile: `$SHEP_HOME/pids/shepd.pid`
#[must_use]
pub fn pidfile(paths: &ShepPaths) -> PathBuf {
    paths.pids.join("shepd.pid")
}

/// Writes the pidfile atomically and durably, through
/// [`shep_core::atomic_file::publish`].
///
/// Fixture seeding only. [`boot`](super::boot) records its pid through
/// `PidfileLock::record` instead: a rename over the locked path swaps in an
/// unlocked inode and disarms the lock for the daemon's life.
///
/// # Errors
/// - [`BootError::Io`] if the pidfile could not be written.
#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn write_pidfile(paths: &ShepPaths, pid: u32) -> Result<(), BootError> {
    use std::io::Write;

    use tempfile::NamedTempFile;

    let path = pidfile(paths);
    let mut tmp = NamedTempFile::new_in(&paths.pids).map_err(|source| BootError::Io {
        path: paths.pids.clone(),
        source,
    })?;
    tmp.write_all(pid.to_string().as_bytes())
        .map_err(|source| BootError::Io {
            path: path.clone(),
            source,
        })?;
    shep_core::atomic_file::publish(tmp, &path).map_err(|source| BootError::Io { path, source })
}

/// Reads the recorded daemon pid, if any
///
/// A missing pidfile reads as `None`, as does one whose contents are not a
/// valid pid. A best-effort hint for [`BootError::AlreadyRunning`], never
/// proof that a daemon is live; the lock is that.
///
/// # Errors
/// - [`BootError::Io`] if the pidfile exists but could not be read.
pub(crate) fn read_pidfile(paths: &ShepPaths) -> Result<Option<u32>, BootError> {
    let path = pidfile(paths);
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(contents.trim().parse::<u32>().ok()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(BootError::Io { path, source }),
    }
}

/// This daemon's exclusive claim on `$SHEP_HOME`: an `flock(2)` held on the
/// pidfile from before [`bind_socket`](super::bind_socket) to the end of [`RunningDaemon::run`](super::RunningDaemon::run).
///
/// Serializes [`bind_socket`](super::bind_socket)'s stale-socket recovery: unserialized, two
/// daemons both see `ConnectionRefused` on a crashed predecessor's leftover
/// and the loser's `remove_file` deletes the winner's fresh listener. A crash
/// needs no cleanup: the kernel releases the lock with the last descriptor on
/// the open file description.
///
/// Windows locks a sibling `shepd.pid.lock`, since `share_mode(0)` would deny
/// the read-only open the loser needs to name the winner. A successor inherits
/// the descriptor already holding the lock; see [`UnixLock`].
#[derive(Debug)]
pub(super) struct PidfileLock {
    #[cfg(unix)]
    flock: UnixLock,
    /// The sibling lock file, held open with every share flag cleared. Held,
    /// never read: [`PidfileLock::record`] writes the pidfile itself.
    #[cfg(windows)]
    _handle: std::fs::File,
}

/// How this process came to hold the pidfile's `flock`.
///
/// An adopted descriptor is never re-locked: `nix` has no constructor for an
/// already-locked file that leaves it locked, and the release-then-relock
/// window is long enough for a second daemon to claim this `$SHEP_HOME`. The
/// lock crosses an `execve` with the descriptor, so there is nothing to redo.
/// Either arm releases the same way, when the last descriptor on the open file
/// description closes.
#[cfg(unix)]
#[derive(Debug)]
enum UnixLock {
    /// Taken here, by this process, with `flock(LOCK_EX | LOCK_NB)`.
    Taken(nix::fcntl::Flock<std::fs::File>),
    /// Inherited across a handover `execve`, still locked, never re-locked.
    Adopted(std::fs::File),
}

#[cfg(unix)]
impl UnixLock {
    /// The locked pidfile, which [`PidfileLock::record`] writes through.
    fn file(&mut self) -> &mut std::fs::File {
        match self {
            Self::Taken(flock) => flock,
            Self::Adopted(file) => file,
        }
    }

    /// The descriptor carrying the lock, for the blob a handover hands on.
    ///
    /// Borrowed, never owned: closing it releases the `flock`.
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        match self {
            Self::Taken(flock) => flock.as_raw_fd(),
            Self::Adopted(file) => file.as_raw_fd(),
        }
    }
}

/// The sibling file the Windows arm locks: the pidfile with `.lock` appended.
///
/// Never renamed, never read, left on disk between boots, as `kv.json.lock`
/// and `barks.jsonl.lock` are.
#[cfg(windows)]
fn pidfile_lock_path(paths: &ShepPaths) -> PathBuf {
    paths.pids.join("shepd.pid.lock")
}

impl PidfileLock {
    /// Opens (creating if necessary) and takes an exclusive, non-blocking
    /// `flock` on `paths`'s pidfile.
    ///
    /// Does not truncate on open: a loser's [`BootError::AlreadyRunning`]
    /// reads the pid the winner recorded through [`Self::record`].
    ///
    /// # Errors
    /// - [`BootError::AlreadyRunning`] if another process holds this lock,
    ///   carrying the pid recorded in the file if any.
    /// - [`BootError::Io`] if the pidfile could not be opened.
    #[cfg(unix)]
    pub(super) fn acquire(paths: &ShepPaths) -> Result<Self, BootError> {
        let path = pidfile(paths);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false) // preserve any pid a previous winner recorded
            .mode(0o600)
            .open(&path)
            .map_err(|source| BootError::Io {
                path: path.clone(),
                source,
            })?;
        match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
            Ok(flock) => Ok(Self {
                flock: UnixLock::Taken(flock),
            }),
            Err((_file, nix::errno::Errno::EWOULDBLOCK)) => Err(BootError::AlreadyRunning {
                pid: read_pidfile(paths)?,
            }),
            Err((_file, errno)) => Err(BootError::Io {
                path,
                source: errno.into(),
            }),
        }
    }

    /// Opens (creating if necessary) the sibling lock file with every share
    /// flag cleared, which no second process can then open at all.
    ///
    /// # Errors
    /// - [`BootError::AlreadyRunning`] if another process holds this lock,
    ///   carrying the pid the winner recorded in the pidfile, which stays
    ///   readable because the lock is on a sibling.
    /// - [`BootError::Io`] if the lock file could not be opened.
    #[cfg(windows)]
    pub(super) fn acquire(paths: &ShepPaths) -> Result<Self, BootError> {
        use std::os::windows::fs::OpenOptionsExt as _;

        /// Another handle already holds share access this open denies.
        const ERROR_SHARING_VIOLATION: i32 = 32;

        let path = pidfile_lock_path(paths);
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
        {
            Ok(handle) => Ok(Self { _handle: handle }),
            // Immediate, never retried: a second daemon is refused now rather
            // than queued behind the first.
            Err(err) if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                Err(BootError::AlreadyRunning {
                    pid: read_pidfile(paths)?,
                })
            }
            Err(source) => Err(BootError::Io { path, source }),
        }
    }

    /// Overwrites the locked pidfile's content with `pid` in place: truncate,
    /// then write at offset 0. Never a temp file plus `rename`, which would
    /// swap in an inode nothing has locked.
    ///
    /// # Errors
    /// - [`BootError::Io`] if the write failed.
    #[cfg(windows)]
    pub(super) fn record(&mut self, paths: &ShepPaths, pid: u32) -> Result<(), BootError> {
        use std::io::Write as _;

        // An ordinary write: this arm's lock is on the sibling `.lock`, so
        // there is no already-locked handle to write through.
        let path = pidfile(paths);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|source| BootError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(pid.to_string().as_bytes())
            .map_err(|source| BootError::Io {
                path: path.clone(),
                source,
            })?;
        file.sync_all()
            .map_err(|source| BootError::Io { path, source })
    }

    /// Holds a pidfile descriptor this image inherited, already locked.
    ///
    /// Nothing here locks, unlocks, truncates or writes: `file` crossed an
    /// `execve` with its `flock` intact, and an `execve` keeps the pid, so the
    /// number the predecessor recorded is this process's own.
    #[cfg(unix)]
    pub(super) fn from_locked(file: std::fs::File) -> Self {
        Self {
            flock: UnixLock::Adopted(file),
        }
    }

    /// The descriptor the lock lives on, for a handover blob to name.
    ///
    /// Borrowed: closing it frees this `$SHEP_HOME` for the next claimant.
    #[cfg(unix)]
    pub(super) fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.flock.as_raw_fd()
    }

    #[cfg(unix)]
    pub(super) fn record(&mut self, paths: &ShepPaths, pid: u32) -> Result<(), BootError> {
        use std::io::{Seek, SeekFrom, Write};

        let path = pidfile(paths);
        let file = self.flock.file();
        file.set_len(0).map_err(|source| BootError::Io {
            path: path.clone(),
            source,
        })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| BootError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(pid.to_string().as_bytes())
            .map_err(|source| BootError::Io {
                path: path.clone(),
                source,
            })?;
        file.sync_all()
            .map_err(|source| BootError::Io { path, source })
    }
}

/// What, if anything, owns this home's pidfile lock
///
/// Proof of life is the lock, not the pidfile's contents: a stale file with a
/// reused pid can fake those, and the kernel drops the lock on process death,
/// `SIGKILL` included. Any lock this takes is released before it returns.
///
/// # Errors
/// - [`BootError::Io`] if the pidfile could not be opened, created or read.
///   A contended lock is not an error; it is [`Shepherd::Running`] or
///   [`Shepherd::Booting`].
pub fn daemon_liveness(paths: &ShepPaths) -> Result<Shepherd, BootError> {
    match PidfileLock::acquire(paths) {
        // Dropped here rather than at the end of the scope: a question-asker
        // holds someone else's home for as short a window as the type allows.
        Ok(lock) => {
            drop(lock);
            Ok(Shepherd::Absent)
        }
        Err(BootError::AlreadyRunning { pid: Some(pid) }) => Ok(Shepherd::Running(pid)),
        Err(BootError::AlreadyRunning { pid: None }) => Ok(Shepherd::Booting),
        // `init_dirs` makes `pids/` on every boot, so a missing one means no
        // daemon has ever run here: an absence, not a failure. Narrow to
        // `NotFound` under `pids/`; a permissions error is a real failure.
        Err(BootError::Io {
            ref path,
            ref source,
        }) if source.kind() == ErrorKind::NotFound && path.starts_with(&paths.pids) => {
            Ok(Shepherd::Absent)
        }
        Err(other) => Err(other),
    }
}

/// What [`daemon_liveness`] found holding a home's pidfile lock.
///
/// Three states, not two: [`boot`](super::boot) takes the lock and records its pid a few
/// statements later, with the socket bind in between, and a caller that read
/// that window as an absence would start a second daemon that then dies unable
/// to take the lock.
///
/// Not `#[non_exhaustive]`, unlike [`BootError`]: the lock is free or held,
/// and a holder has written its pid or has not, so there is no fourth state to
/// add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shepherd {
    /// Nothing holds this home's pidfile lock.
    ///
    /// A stale pidfile naming a long-dead pid reads as this.
    Absent,
    /// A shepherd holds the lock and recorded this pid.
    Running(u32),
    /// A shepherd holds the lock but has not recorded a pid yet.
    ///
    /// It owns the home, so it is not absent, but there is no pid to signal.
    Booting,
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::boot::{init_dirs, paths_in};

    /// Also pins why the lock lives on a sibling `.lock`: a `share_mode(0)`
    /// open of the pidfile itself would make `read_pidfile` fail with a
    /// sharing violation, and `AlreadyRunning` would lose its pid.
    #[test]
    fn a_second_pidfile_lock_is_refused_and_can_still_read_the_winners_pid() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        init_dirs(&paths).unwrap();

        let mut first = PidfileLock::acquire(&paths).expect("the first daemon must win");
        first.record(&paths, 4242).unwrap();

        let refusal = PidfileLock::acquire(&paths).expect_err("a second daemon must be refused");
        let BootError::AlreadyRunning { pid } = refusal else {
            panic!("a contended lock must report AlreadyRunning, got {refusal:?}");
        };
        assert_eq!(
            pid,
            Some(4242),
            "the loser must be able to read the winner's pid off the pidfile"
        );
    }

    /// The crash property: dropping stands in for the process dying, which is
    /// what closes the handle the share-mode reservation is owned by.
    #[test]
    fn dropping_the_lock_releases_it_for_the_next_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        init_dirs(&paths).unwrap();

        let first = PidfileLock::acquire(&paths).unwrap();
        drop(first);

        PidfileLock::acquire(&paths)
            .expect("a released lock must be re-acquirable, or a crash would wedge $SHEP_HOME");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::boot::init_dirs;
    use crate::testing::test_paths;
    use std::time::Duration;

    /// `flock` conflicts between separate open file descriptions even inside
    /// one process, so this process can ask for the lock it holds and be
    /// refused.
    ///
    /// `mem::forget` plus `sys::adopt_handover_fd` stands in for the `execve`:
    /// forgetting skips the `flock(fd, LOCK_UN)` a drop would run, and
    /// adopting the number gives the descriptor one new owner. Duplicating
    /// instead would leave a second descriptor holding the same lock, and both
    /// assertions would hold whatever the adopted arm did.
    #[test]
    fn the_adopted_pidfile_arm_does_not_release_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();

        let mut held = PidfileLock::acquire(&paths).expect("the predecessor must win");
        let fd = std::os::fd::AsRawFd::as_raw_fd(held.flock.file());
        core::mem::forget(held);
        let inherited = crate::sys::adopt_handover_fd(fd)
            .expect("the successor adopts the number the blob named");

        let adopted = PidfileLock::from_locked(inherited);
        let refusal = PidfileLock::acquire(&paths)
            .expect_err("the lock must never be free while a successor holds it");
        assert!(
            matches!(refusal, BootError::AlreadyRunning { .. }),
            "a contended lock must report AlreadyRunning, got {refusal:?}"
        );

        // Retried rather than demanded on the first attempt: a `fork` copies
        // the whole descriptor table, so a child another test in this binary
        // spawns concurrently holds a duplicate of this descriptor, and its
        // `flock` with it, until its own `exec` runs.
        drop(adopted);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let claimed = loop {
            match PidfileLock::acquire(&paths) {
                Ok(claimed) => break claimed,
                Err(error) => assert!(
                    std::time::Instant::now() < deadline,
                    "a successor that exits must leave the home claimable: {error:?}"
                ),
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(claimed);
    }

    #[test]
    fn pidfile_round_trips_and_reports_absence() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        assert_eq!(read_pidfile(&paths).unwrap(), None);
        write_pidfile(&paths, 4242).unwrap();
        assert_eq!(read_pidfile(&paths).unwrap(), Some(4242));
        assert_eq!(pidfile(&paths), paths.pids.join("shepd.pid"));
    }

    #[test]
    fn liveness_reports_none_when_no_daemon_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        assert_eq!(daemon_liveness(&paths).unwrap(), Shepherd::Absent);
    }

    #[test]
    fn liveness_reports_none_for_a_stale_pidfile_nobody_holds() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        std::fs::write(pidfile(&paths), "999999").unwrap();
        // The file names a pid; nothing holds the lock, so nothing is live.
        assert_eq!(daemon_liveness(&paths).unwrap(), Shepherd::Absent);
    }

    #[test]
    fn liveness_reports_the_pid_a_lock_holder_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let mut held = PidfileLock::acquire(&paths).unwrap();
        held.record(&paths, 4242).unwrap();
        assert_eq!(daemon_liveness(&paths).unwrap(), Shepherd::Running(4242));
        drop(held);
        assert_eq!(
            daemon_liveness(&paths).unwrap(),
            Shepherd::Absent,
            "a released lock is not a live daemon, whatever the file still says"
        );
    }

    #[test]
    fn liveness_reports_booting_for_a_holder_that_has_not_recorded_a_pid_yet() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        // `boot` records its pid a few statements after taking the lock; a
        // caller reading that window as an absence starts a second daemon.
        let held = PidfileLock::acquire(&paths).unwrap();
        assert_eq!(daemon_liveness(&paths).unwrap(), Shepherd::Booting);
        drop(held);
        assert_eq!(daemon_liveness(&paths).unwrap(), Shepherd::Absent);
    }
}
