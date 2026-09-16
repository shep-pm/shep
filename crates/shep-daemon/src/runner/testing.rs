//! Fixtures and helpers shared by this module's tests.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

/// A uid no fixture in this module creates anything as. Only reached on
/// a root test runner, where everything created is root-owned and root is
/// exempt by construction.
pub(super) const FOREIGN_UID: u32 = 65_432;

/// This process's effective uid: what every fixture directory is owned
/// by, and what the cases move the daemon's uid relative to.
pub(super) fn me() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// A log path two components below `dir`, with its parent created and
/// left at `mode`.
pub(super) fn log_path_under(dir: &tempfile::TempDir, mode: u32) -> (PathBuf, PathBuf) {
    let parent = dir.path().join("logs");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(mode)).unwrap();
    let log = parent.join("web-0-out.log");
    (parent, log)
}
