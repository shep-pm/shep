//! Which hints an operator turned off, from `$SHEP_HOME/hints.json`.

use std::path::Path;

use serde::Deserialize;

use super::Hint;

/// The file's name under `$SHEP_HOME`.
const FILE: &str = "hints.json";

/// The hints turned off, shaped `{"off": ["logs"]}` on disk.
#[derive(Debug, Default, Deserialize)]
pub(super) struct Off {
    /// Names as written: one this build does not know is never matched.
    #[serde(default)]
    off: Vec<String>,
}

impl Off {
    /// What `home`'s `hints.json` turns off.
    ///
    /// A missing, unreadable or unparsable file turns nothing off: a hint is
    /// never worth failing a listing over.
    pub(super) fn read(home: &Path) -> Self {
        std::fs::read(home.join(FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Whether `hint` is turned off.
    pub(super) fn contains(&self, hint: Hint) -> bool {
        self.off.iter().any(|name| Hint::from_name(name) == Some(hint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_leaves_every_hint_on() {
        let dir = tempfile::tempdir().unwrap();

        let off = Off::read(dir.path());

        assert!(Hint::ALL.iter().all(|hint| !off.contains(*hint)));
    }

    #[test]
    fn an_unparsable_file_leaves_every_hint_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"off": ["logs""#).unwrap();

        let off = Off::read(dir.path());

        assert!(Hint::ALL.iter().all(|hint| !off.contains(*hint)));
    }

    #[test]
    fn a_hint_named_off_is_off_and_an_unknown_name_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"off": ["logs", "nonesuch"]}"#).unwrap();

        let off = Off::read(dir.path());

        assert!(off.contains(Hint::Logs));
        assert!(!off.contains(Hint::Alerts));
    }
}
