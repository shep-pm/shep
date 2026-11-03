//! The one record `shep get` answers with.

use std::fmt;

use serde::Serialize;

use crate::output::Render;

/// One settings read: what the address named, and the TOML it names.
///
/// `Debug` is redacted (IR-41): `toml` is a sheep's or a dog's own settings
/// text, which routinely carries a token inside an `args` value or a home
/// directory inside a `cwd`, and this is what a `{:?}` on a `--format json`
/// run prints.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SettingsAnswer {
    /// `sheep` or `dog`: which half of the flock the address named.
    pub kind: &'static str,
    /// The name the address named, spelled as the flock spells it.
    pub name: String,
    /// The path inside those settings, spelled as the address spelled it.
    /// Empty for a read of everything the name holds.
    pub path: String,
    /// The answer, as TOML text.
    pub toml: String,
    /// The keys of a dog's table on a sheep that the sheep leaves to the dog's
    /// own section, for a dog that follows the dog settings standard. A key
    /// inside a table the sheep sets part of is its dotted path. Left out
    /// when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inherited: Vec<String>,
}

impl fmt::Debug for SettingsAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SettingsAnswer {{ kind: {:?}, name: {:?}, path: {:?}, toml: <{} bytes> }}",
            self.kind,
            self.name,
            self.path,
            self.toml.len()
        )
    }
}

/// `shep get`'s answer is a TOML document, so `Format::Table` writes it
/// straight to stdout and this impl is reached only by `Format::Json`. It
/// exists for [`SecretValueRow`](super::SecretValueRow)'s reason: `--format
/// json` is one envelope per invocation for every verb but `bleats`, and the
/// envelope is built by [`Render`]. The columns are what those three fields
/// would be at a terminal, not a rendering any operator sees.
impl Render for SettingsAnswer {
    fn headers() -> &'static [&'static str] {
        &["NAME", "KIND", "PATH"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![
            self.name.clone(),
            self.kind.to_string(),
            self.path.clone(),
        ]]
    }

    // The answer itself: a TOML document has no columns, and a cell would
    // flatten the newlines that make it one.
    const JSON_ONLY: &'static [&'static str] = &["toml", "inherited"];

    // NAME is the identity the address named; the two describing it drop
    // first.
    const PRIORITIES: &'static [u8] = &[0, 6, 6];
}

#[cfg(test)]
mod tests {
    use super::super::tests::assert_no_drift;
    use super::*;

    #[test]
    fn settings_answer_matches_its_json_keys() {
        let answer = SettingsAnswer {
            kind: "sheep",
            name: "api".to_string(),
            path: "max_memory".to_string(),
            toml: "max_memory = 1073741824\n".to_string(),
            inherited: vec!["max_memory".to_string()],
        };
        assert_no_drift(&answer, |json| json, &[]);
    }

    /// The redaction this type's own doc claims, pinned as an exact string so
    /// a `derive(Debug)` refactor fails CI rather than shipping the settings
    /// text into a log (IR-41).
    #[test]
    fn settings_answer_debug_prints_a_byte_count_and_never_the_toml() {
        let answer = SettingsAnswer {
            kind: "dog",
            name: "kelpie".to_string(),
            path: "worker".to_string(),
            toml: "[worker]\ntoken = \"hunter2\"\n".to_string(),
            inherited: Vec::new(),
        };
        assert_eq!(
            format!("{answer:?}"),
            "SettingsAnswer { kind: \"dog\", name: \"kelpie\", path: \"worker\", toml: <27 bytes> }"
        );
    }
}
