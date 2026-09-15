//! `$SHEP_HOME`'s directory layout, owner-only at creation
//!
//! [`init_dirs`] is the one place the layout comes into existence, and it runs
//! on every boot rather than only the first: a directory that already exists
//! looser is forced back to [`DIR_MODE`]. That is the `0700` guarantee
//! `crate::server::RpcServer`'s doc names as boot's.

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

use shep_core::paths::ShepPaths;

use super::BootError;

/// Mode for every directory shep creates (spec §10: no other user, at all)
pub const DIR_MODE: u32 = 0o700;

/// Creates `dir` and any missing parents at [`DIR_MODE`] via
/// [`DirBuilderExt::mode`], closing the TOCTOU a `create_dir_all` plus a
/// separate `chmod` leaves open: the umask-derived mode in between is wide
/// enough to race a symlink onto the socket path underneath.
///
/// Does not touch a directory that already exists; that is [`init_dirs`]'s
/// `set_permissions` pass.
#[cfg(windows)]
fn create_dir_at_dir_mode(dir: &Path) -> std::io::Result<()> {
    // No mode to set: `DIR_MODE` is a POSIX word, Windows access control is an
    // inherited ACL. The control pipe's ACL guards the socket instead, so
    // `flock.json`'s `env` stays readable to another account with access to
    // the profile; the operator docs name that gap.
    std::fs::DirBuilder::new().recursive(true).create(dir)
}

#[cfg(unix)]
fn create_dir_at_dir_mode(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .mode(DIR_MODE)
        .recursive(true)
        .create(dir)
}

/// Creates `$SHEP_HOME` and its subdirectories, tightening loose modes
///
/// Idempotent: a restart onto an existing layout forces every directory back
/// to [`DIR_MODE`].
///
/// # Errors
/// - [`BootError::Io`] if a directory could not be created or chmod'ed.
pub(crate) fn init_dirs(paths: &ShepPaths) -> Result<(), BootError> {
    for dir in [&paths.home, &paths.logs, &paths.pids, &paths.run] {
        create_dir_at_dir_mode(dir).map_err(|source| BootError::Io {
            path: dir.clone(),
            source,
        })?;
        // Unix only: Windows has no scalar mode to force a directory back to.
        #[cfg(unix)]
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE)).map_err(
            |source| BootError::Io {
                path: dir.clone(),
                source,
            },
        )?;
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::boot::paths_in;

    /// No mode to assert on this platform, only that the directories every
    /// later step writes into exist.
    #[test]
    fn init_dirs_creates_the_whole_layout() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        init_dirs(&paths).unwrap();
        for expected in [&paths.home, &paths.logs, &paths.pids, &paths.run] {
            assert!(expected.is_dir(), "{} was not created", expected.display());
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testing::test_paths;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn init_dirs_creates_the_whole_layout_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        for path in [&paths.home, &paths.logs, &paths.pids, &paths.run] {
            assert!(path.is_dir(), "{} was not created", path.display());
            assert_eq!(mode_of(path), DIR_MODE, "{}", path.display());
        }
        init_dirs(&paths).unwrap(); // idempotent: a restart must not fail here
    }

    #[test]
    fn a_fresh_dir_lands_at_dir_mode_with_no_separate_chmod() {
        // No `set_permissions` here, so this passes only if `DirBuilder`'s
        // `.mode(DIR_MODE)` lands the mode at creation. `init_dirs`' own tests
        // observe the mode after its chmod pass and cannot see that window.
        let dir = tempfile::tempdir().unwrap();
        let never_existed = dir.path().join("nested").join("run");
        create_dir_at_dir_mode(&never_existed).unwrap();
        assert_eq!(
            mode_of(&never_existed),
            DIR_MODE,
            "a freshly created dir must be DIR_MODE at creation, not after a later chmod"
        );
    }

    #[test]
    fn init_dirs_tightens_a_world_readable_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::set_permissions(&paths.run, std::fs::Permissions::from_mode(0o755)).unwrap();
        init_dirs(&paths).unwrap();
        assert_eq!(
            mode_of(&paths.run),
            DIR_MODE,
            "a loose run dir must be tightened, not accepted"
        );
    }
}
