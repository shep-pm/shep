//! [`FlockFormat`]: which of the four document formats a path names.

use std::path::Path;

/// Input format of a Flockfile
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlockFormat {
    /// `Flockfile.toml`: `[[app]]` tables
    Toml,
    /// `.yaml`/`.yml`
    Yaml,
    /// Strict JSON
    Json,
    /// JSON5 (comments, trailing commas)
    Json5,
}

impl FlockFormat {
    /// Maps a file extension to its format (`None` = unsupported, e.g. `.js`)
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "json" => Some(Self::Json),
            "json5" => Some(Self::Json5),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_from_path() {
        use std::path::Path;
        assert_eq!(
            FlockFormat::from_path(Path::new("Flockfile.toml")),
            Some(FlockFormat::Toml)
        );
        assert_eq!(
            FlockFormat::from_path(Path::new("f.yml")),
            Some(FlockFormat::Yaml)
        );
        assert_eq!(
            FlockFormat::from_path(Path::new("f.json5")),
            Some(FlockFormat::Json5)
        );
        assert_eq!(FlockFormat::from_path(Path::new("f.js")), None);
    }
}
