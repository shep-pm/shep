use std::path::{Path, PathBuf};

/// Anchors an explicit log path at the sheep's `cwd` when it is relative.
///
/// The shepherd opens these files itself, so a bare relative path resolves
/// against the shepherd's directory rather than the sheep's: the same config
/// names one file under the shepherd a handover started from and another
/// under its successor, and `shep bleats` reads a third under the CLI's.
///
/// A sheep with no `cwd` already runs in the shepherd's own directory.
pub(super) fn anchor_log_path(rendered: String, cwd: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(rendered);
    match cwd {
        Some(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    }
}
