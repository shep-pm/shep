use crate::secrets;
use crate::values::UpDuration;
use core::fmt;

/// Error type returned from [`DaemonConfig::load`](crate::config::daemon::DaemonConfig::load).
///
/// `#[non_exhaustive]`: every `[daemon]` key this crate learns to validate
/// brings its own rejection reason, and `deferred.md`'s daemon-config
/// flags layer is a whole set of them at once.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonConfigError {
    /// `shep.toml` is invalid TOML (carries the parser message)
    Toml(String),
    /// A `SHEP_*` env var held an unparseable value (var name, value)
    BadEnvValue(&'static str, String),
    /// A `[daemon]` duration is below the floor that keeps the daemon from
    /// spinning. Carries the key the user actually set: the TOML key or
    /// the environment variable, whichever supplied the winning value.
    BelowMinimum {
        /// `max_cron_sleep` or `SHEP_MAX_CRON_SLEEP`.
        key: &'static str,
        /// The value as the user wrote it.
        value: UpDuration,
        /// The floor it failed.
        min: UpDuration,
    },
    /// `[daemon] environment` is [`crate::secrets::ALL_ENVIRONMENTS`], the
    /// secrets store's every-environment slot, or falls outside the grammar
    /// [`crate::secrets`] keys and environment names share. Carries the
    /// value as written.
    InvalidEnvironment(String),
}

impl core::error::Error for DaemonConfigError {}

impl fmt::Display for DaemonConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(m) => write!(f, "invalid shep.toml: {m}"),
            Self::BadEnvValue(var, v) => write!(f, "invalid value `{v}` for {var}"),
            Self::BelowMinimum { key, value, min } => {
                write!(
                    f,
                    "invalid value `{value}` for {key}: must be at least {min}"
                )
            }
            Self::InvalidEnvironment(value) => write!(
                f,
                "invalid value `{value}` for environment: must be 1-{} bytes of \
                     `[A-Za-z0-9._-]` not starting with `.`, and not `{}` (the secrets \
                     store's every-environment slot)",
                secrets::MAX_KEY_BYTES,
                secrets::ALL_ENVIRONMENTS
            ),
        }
    }
}
