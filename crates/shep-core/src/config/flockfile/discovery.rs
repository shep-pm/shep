//! Finding a Flockfile on disk: ten names, in the order spec §5 fixes.
//!
//! Case and extension both matter to the order, and `.js` is absent on
//! purpose: shep-core never executes a config.

use std::path::{Path, PathBuf};

/// The filenames [`discover`] looks for, in the order it looks (spec §5)
pub const DISCOVERY_ORDER: &[&str] = &[
    "Flockfile.toml",
    "Flockfile.yaml",
    "Flockfile.yml",
    "Flockfile.json",
    "Flockfile.json5",
    "flockfile.toml",
    "flockfile.yaml",
    "flockfile.yml",
    "flockfile.json",
    "flockfile.json5",
];

/// Finds the Flockfile in a directory (spec §5 ten-name order)
#[must_use]
pub fn discover(dir: &Path) -> Option<PathBuf> {
    DISCOVERY_ORDER
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flockfile::format::FlockFormat;

    #[test]
    fn discover_prefers_toml_then_capitalized() {
        // tempdir gives RAII cleanup instead of a manual remove_dir_all, so
        // a failing assertion above can't leak the directory.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("flockfile.json"), "{}").unwrap();
        std::fs::write(dir.path().join("Flockfile.yaml"), "").unwrap();
        assert_eq!(
            discover(dir.path()),
            Some(dir.path().join("Flockfile.yaml"))
        );
        std::fs::write(dir.path().join("Flockfile.toml"), "").unwrap();
        assert_eq!(
            discover(dir.path()),
            Some(dir.path().join("Flockfile.toml"))
        );
    }

    /// fails if a `.js` name is ever added to the discovery order. Reading
    /// one runs node on it, and discovery is the path with no operator in
    /// the loop, so it must never reach node.
    #[test]
    fn discovery_never_names_a_js_file_and_stays_ten_names() {
        assert_eq!(DISCOVERY_ORDER.len(), 10);
        for name in DISCOVERY_ORDER {
            assert!(
                !name.ends_with(".js"),
                "{name} would let `shep start` execute a repo's JavaScript"
            );
            assert!(FlockFormat::from_path(Path::new(name)).is_some());
        }
    }
}
