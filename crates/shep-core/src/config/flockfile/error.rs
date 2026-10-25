//! [`FlockfileError`]: what a parse refuses with, one variant per reason.

use super::format::FlockFormat;
use core::fmt;

/// Error type returned from [`Flockfile::parse`](crate::config::flockfile::Flockfile::parse).
///
/// `#[non_exhaustive]`: an out-of-tree consumer could otherwise match
/// this exhaustively and a new variant would break them silently. Growth
/// is anticipated per backend, not per format: `.js` Flockfiles never
/// appear here, since shep-core never executes anything. The node bridge
/// lives in shep-cli, which feeds its output back through
/// [`FlockFormat::Json`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlockfileError {
    /// The backend for `format` rejected the source
    Parse {
        /// The format whose backend refused the source.
        format: FlockFormat,
        /// The backend's own message.
        message: String,
    },
    /// The document parsed but declared no apps
    NoApps,
    /// An `app` entry named one or more keys no field claims.
    ///
    /// Only a key inside an `app` entry lands here. A top-level typo is
    /// refused earlier, by `RawFlockfile`'s own `deny_unknown_fields`,
    /// which the backend's derived `Deserialize` enforces directly rather
    /// than through the `serde_ignored` callback this variant collects
    /// from: it arrives as [`FlockfileError::Parse`] naming the key.
    ///
    /// A Flockfile is hand-written, unlike the same [`AppConfig`](crate::config::AppConfig)/
    /// [`ProbeConfig`](crate::config::ProbeConfig) shape riding the wire,
    /// where an unknown field means a newer peer rather than a typo.
    /// `keys` names every offending key (dotted path for a nested one, e.g.
    /// `app.0.readiness_probe.<the misspelled key>`) so one refusal lists
    /// every typo instead of one refusal per run.
    UnknownKeys {
        /// Every key the document named that no field claimed.
        keys: Vec<String>,
    },
}

impl FlockfileError {
    /// The variant for `format`'s backend refusing the source, carrying
    /// `message`.
    #[must_use]
    pub(super) fn backend(format: FlockFormat, message: impl fmt::Display) -> Self {
        Self::Parse {
            format,
            message: message.to_string(),
        }
    }
}

impl fmt::Display for FlockfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoApps => f.write_str("Flockfile declares no apps"),
            Self::UnknownKeys { keys } => {
                write!(f, "Flockfile names unrecognized key")?;
                if keys.len() != 1 {
                    f.write_str("s")?;
                }
                write!(f, ": {}", keys.join(", "))
            }
            Self::Parse { format, message } => {
                let label = match format {
                    FlockFormat::Toml => "TOML",
                    FlockFormat::Yaml => "YAML",
                    FlockFormat::Json => "JSON",
                    FlockFormat::Json5 => "JSON5",
                };
                write!(f, "invalid {label} Flockfile: {message}")
            }
        }
    }
}

impl core::error::Error for FlockfileError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each backend refusal names its own format, in the one sentence shape
    /// every format shares. The strings are what an operator reads in
    /// `shep start` output, so they are pinned whole.
    #[test]
    fn each_backend_variant_displays_its_format_and_message() {
        let cases = [
            (FlockFormat::Toml, "invalid TOML Flockfile: x"),
            (FlockFormat::Yaml, "invalid YAML Flockfile: x"),
            (FlockFormat::Json, "invalid JSON Flockfile: x"),
            (FlockFormat::Json5, "invalid JSON5 Flockfile: x"),
        ];
        for (format, rendered) in cases {
            let err = FlockfileError::Parse {
                format,
                message: "x".to_string(),
            };
            assert_eq!(
                FlockfileError::backend(format, "x"),
                err,
                "{format:?}: backend built the wrong variant"
            );
            assert_eq!(err.to_string(), rendered);
        }
    }
}
