//! Parks the files of a dog already installed while `shep install` replaces
//! them, so a refusal before the new binary is adopted leaves them as they
//! were.

use std::path::{Path, PathBuf};

/// The suffix a parked file carries beside its original.
const PARKED: &str = ".kept";

/// Files moved aside for the length of one install.
///
/// Dropping it puts them back, best effort: a file that cannot be moved
/// back stays beside its original with the `.kept` suffix. [`Kept::commit`]
/// deletes them instead.
#[derive(Debug)]
pub(super) struct Kept(Vec<(PathBuf, PathBuf)>);

impl Kept {
    /// Moves each of `files` that exists to its `.kept` sibling.
    ///
    /// # Errors
    /// A file could not be moved. The ones already moved are put back.
    pub(super) fn park(files: &[&Path]) -> std::io::Result<Self> {
        let mut kept = Self(Vec::new());
        for file in files.iter().filter(|file| file.exists()) {
            let mut parked = file.as_os_str().to_owned();
            parked.push(PARKED);
            let parked = PathBuf::from(parked);
            // A rename onto an existing file fails on Windows.
            let _ = std::fs::remove_file(&parked);
            std::fs::rename(file, &parked)?;
            kept.0.push(((*file).to_path_buf(), parked));
        }
        Ok(kept)
    }

    /// Deletes the parked files: the install they were parked for stands.
    pub(super) fn commit(mut self) {
        for (_original, parked) in std::mem::take(&mut self.0) {
            let _ = std::fs::remove_file(parked);
        }
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        for (original, parked) in self.0.drain(..) {
            let _ = std::fs::remove_file(&original);
            let _ = std::fs::rename(parked, original);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_park_puts_the_files_back_over_what_replaced_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("receipt.json");
        std::fs::write(&file, "old").unwrap();
        let kept = Kept::park(&[&file, &dir.path().join("absent")]).unwrap();
        assert!(!file.exists());
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
