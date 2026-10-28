use std::path::{Path, PathBuf};

/// Anchors an explicit log path at the sheep's `cwd` when it is relative.
///
/// The shepherd opens these files itself. A bare relative path resolves
/// against its own directory, not the sheep's. The same config then names
/// one file under a handover's predecessor and another under its successor.
/// `shep bleats` reads a third under the CLI's.
pub(super) fn anchor_log_path(rendered: String, cwd: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(rendered);
    match cwd {
        Some(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    }
}
