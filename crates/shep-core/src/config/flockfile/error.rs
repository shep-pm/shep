//! [`FlockfileError`]: what a parse refuses with, one variant per reason.

use core::fmt;

/// Error type returned from [`Flockfile::parse`](crate::config::flockfile::Flockfile::parse).
///
/// `#[non_exhaustive]`: an out-of-tree consumer could otherwise match
/// this exhaustively and a new variant would break them silently. Growth
/// is anticipated per backend, not per format: `.js` Flockfiles never
/// appear here, since shep-core never executes anything. The node bridge
/// lives in shep-cli, which feeds its output back through
/// [`FlockFormat::Json`](crate::config::flockfile::FlockFormat::Json).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlockfileError {
    /// TOML backend rejected the source (carries its message)
    Toml(String),
    /// YAML backend rejected the source
    Yaml(String),
    /// JSON backend rejected the source
    Json(String),
    /// JSON5 backend rejected the source
    Json5(String),
    /// The document parsed but declared no apps
    NoApps,
    /// The document named one or more keys no field claims.
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

impl fmt::Display for FlockfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(m) => write!(f, "invalid TOML Flockfile: {m}"),
            Self::Yaml(m) => write!(f, "invalid YAML Flockfile: {m}"),
            Self::Json(m) => write!(f, "invalid JSON Flockfile: {m}"),
            Self::Json5(m) => write!(f, "invalid JSON5 Flockfile: {m}"),
            Self::NoApps => f.write_str("Flockfile declares no apps"),
            Self::UnknownKeys { keys } => {
                write!(f, "Flockfile names unrecognized key")?;
                if keys.len() != 1 {
                    f.write_str("s")?;
                }
                write!(f, ": {}", keys.join(", "))
            }
        }
    }
}

impl core::error::Error for FlockfileError {}
