//! Copies the files of a dog already installed before `shep install` replaces
//! them, so a refusal before the new binary is adopted leaves them as they
//! were.
//!
//! The originals stay where they are until the install replaces them, so a
//! process killed mid-install leaves a dog that still runs. The copies it
//! leaves behind are overwritten by the next install.

use std::path::{Path, PathBuf};

/// The suffix a parked file carries beside its original.
const PARKED: &str = ".kept";

/// Copies of files that one install is about to replace.
///
/// Dropping it puts them back, best effort: a copy that cannot be moved back
/// stays beside its original with the `.kept` suffix. [`Kept::commit`]
/// deletes them instead.
#[derive(Debug)]
pub(super) struct Kept(Vec<(PathBuf, Option<PathBuf>)>);

impl Kept {
    /// Copies each of `files` that exists to its `.kept` sibling.
    ///
    /// # Errors
    /// A file could not be copied. The copies already made are deleted.
    pub(super) fn park(files: &[&Path]) -> std::io::Result<Self> {
        let mut kept = Self(Vec::new());
        for file in files.iter().filter(|file| file.exists()) {
            let mut parked = file.as_os_str().to_owned();
            parked.push(PARKED);
            let parked = PathBuf::from(parked);
            std::fs::copy(file, &parked)?;
            kept.0.push(((*file).to_path_buf(), Some(parked)));
        }
        Ok(kept)
    }

    /// Removes `file` on rollback. The caller ensures it did not exist before this install.
    pub(super) fn track_new(&mut self, file: &Path) {
        self.0.push((file.to_path_buf(), None));
    }

    /// Deletes the copies: the install they were taken for stands.
    pub(super) fn commit(mut self) {
        for (_original, parked) in std::mem::take(&mut self.0) {
            if let Some(parked) = parked {
                let _ = std::fs::remove_file(parked);
            }
        }
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        for (original, parked) in self.0.drain(..) {
            // A rename onto an existing file fails on Windows.
            let _ = std::fs::remove_file(&original);
            if let Some(parked) = parked {
                let _ = std::fs::rename(parked, original);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parking_leaves_the_originals_in_place_and_a_drop_puts_them_back_over_what_replaced_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("receipt.json");
        std::fs::write(&file, "old").unwrap();
        let kept = Kept::park(&[&file, &dir.path().join("absent")]).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        std::fs::write(&file, "new").unwrap();
        drop(kept);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert!(!dir.path().join("receipt.json.kept").exists());
    }

    #[test]
    fn a_committed_park_keeps_the_new_file_and_deletes_the_old() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("receipt.json");
        std::fs::write(&file, "old").unwrap();
        let kept = Kept::park(&[&file]).unwrap();
        std::fs::write(&file, "new").unwrap();
        kept.commit();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        assert!(!dir.path().join("receipt.json.kept").exists());
    }
}
