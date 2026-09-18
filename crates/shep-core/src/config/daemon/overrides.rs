use super::sections::LogLevel;
use crate::values::UpDuration;
use std::path::PathBuf;

/// The CLI-flag layer of `file < env < flags` (spec §5).
///
/// Every field is `Option`: `None` means the flag was absent and the
/// layer below wins. Nothing here validates; [`DaemonConfig::load_layered`](crate::config::daemon::DaemonConfig::load_layered)
/// runs the single validation pass once, after all three layers.
///
/// `#[non_exhaustive]`: this type grows a field whenever the hidden
/// `daemon` subcommand grows a flag. Build one with [`Self::new`] and the
/// chained setters.
///
/// `Debug` is derived, not redacted: four values, none a secret.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonOverrides {
    /// `--log-json`
    pub log_json: Option<bool>,
    /// `--log-level`
    pub log_level: Option<LogLevel>,
    /// `--socket`
    pub socket: Option<PathBuf>,
    /// `--max-cron-sleep`
    pub max_cron_sleep: Option<UpDuration>,
}

/// The boolean grammar of `shep.toml` and the `SHEP_*` environment: `1`,
/// `0`, `true`, `false`, and nothing else.
///
/// One function so the file/env layer and the `--log-json` flag cannot
/// drift. clap's own `BoolishValueParser` additionally accepts
/// `yes`/`no`/`y`/`n`/`on`/`off`; using it would widen the grammar on the
/// flag side only.
///
/// Not a general boolean parser: exporting it only under this name keeps
/// exactly one answer to what counts as true in shep's daemon config.
#[must_use]
pub fn parse_daemon_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

impl DaemonOverrides {
    /// An empty layer: every flag absent.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the `--log-json` override.
    #[must_use]
    pub fn log_json(mut self, value: Option<bool>) -> Self {
        self.log_json = value;
        self
    }

    /// Sets the `--log-level` override.
    #[must_use]
    pub fn log_level(mut self, value: Option<LogLevel>) -> Self {
        self.log_level = value;
        self
    }

    /// Sets the `--socket` override.
    #[must_use]
    pub fn socket(mut self, value: Option<PathBuf>) -> Self {
        self.socket = value;
        self
    }

    /// Sets the `--max-cron-sleep` override.
    #[must_use]
    pub fn max_cron_sleep(mut self, value: Option<UpDuration>) -> Self {
        self.max_cron_sleep = value;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::DaemonConfig;
    use super::super::error::DaemonConfigError;
    use super::super::sections::{LogLevel, MIN_CRON_SLEEP};

    use super::super::testing::*;
    use super::*;
    use crate::values::UpDuration;

    #[test]
    fn below_minimum_display_is_exact() {
        let err = DaemonConfigError::BelowMinimum {
            key: "max_cron_sleep",
            value: UpDuration::from_millis(999),
            min: UpDuration::from_millis(1_000),
        };
        assert_eq!(
            err.to_string(),
            "invalid value `999` for max_cron_sleep: must be at least 1s"
        );
    }

    #[test]
    fn file_sets_values_and_keeps_dog_sections_raw() {
        let src = r#"
    [daemon]
    log_json = true
    enabled_dogs = ["metrics"]

    [dog.metrics]
    port = 9615
    "#;
        let cfg = DaemonConfig::load(Some(src), &no_env).unwrap();
        assert!(cfg.daemon.log_json);
        assert_eq!(cfg.daemon.enabled_dogs, vec!["metrics"]);
        assert_eq!(cfg.dog["metrics"]["port"].as_integer(), Some(9615));
    }

    // fails if validation moves back into a per-layer position: a later
    // layer must be able to rescue a value an earlier one would reject.
    #[test]
    fn a_flag_rescues_a_below_floor_file_value() {
        let cfg = DaemonConfig::load_layered(
            Some("[daemon]\nmax_cron_sleep = \"500\"\n"),
            &no_env,
            &DaemonOverrides::new().max_cron_sleep(Some(UpDuration::from_millis(300_000))),
        )
        .unwrap();
        assert_eq!(
            cfg.daemon.max_cron_sleep,
            Some(UpDuration::from_millis(300_000))
        );
    }

    // fails if a below-floor FLAG is accepted, or if the refusal names the
    // TOML key the operator did not set.
    #[test]
    fn a_below_floor_flag_is_refused_naming_the_flag() {
        let err = DaemonConfig::load_layered(
            None,
            &no_env,
            &DaemonOverrides::new().max_cron_sleep(Some(UpDuration::from_millis(500))),
        )
        .unwrap_err();
        assert_eq!(
            err,
            DaemonConfigError::BelowMinimum {
                key: "--max-cron-sleep",
                value: UpDuration::from_millis(500),
                min: MIN_CRON_SLEEP,
            }
        );
        assert!(err.to_string().contains("--max-cron-sleep"), "got: {err}");
    }

    // fails if a flag stops beating the env layer.
    #[test]
    fn a_flag_beats_the_environment() {
        let env = |k: &str| (k == "SHEP_LOG_LEVEL").then(|| "trace".to_string());
        let cfg = DaemonConfig::load_layered(
            Some("[daemon]\nlog_level = \"error\"\n"),
            &env,
            &DaemonOverrides::new().log_level(Some(LogLevel::Info)),
        )
        .unwrap();
        assert_eq!(cfg.daemon.log_level, LogLevel::Info);
    }

    #[test]
    fn the_bool_grammar_is_exactly_four_spellings() {
        assert_eq!(parse_daemon_bool("1"), Some(true));
        assert_eq!(parse_daemon_bool("0"), Some(false));
        assert_eq!(parse_daemon_bool("true"), Some(true));
        assert_eq!(parse_daemon_bool("false"), Some(false));
        for wider in ["yes", "no", "on", "off", "TRUE", "y"] {
            assert_eq!(
                parse_daemon_bool(wider),
                None,
                "{wider} must not be a boolean here"
            );
        }
    }
}
