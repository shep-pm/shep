use super::error::DaemonConfigError;
use super::overrides::{DaemonOverrides, parse_daemon_bool};
use super::sections::{
    DaemonSection, LogLevel, MIN_CRON_SLEEP, SecretsSection, StyleSection, WhistleSection,
};
use crate::secrets;
use crate::values::UpDuration;
use core::fmt;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Parsed daemon configuration with raw per-dog sections.
///
/// Dog sections stay untyped here: each dog deserializes its own
/// `[dog.<name>]` table, so dog config schemas live with the dog code.
///
/// `#[non_exhaustive]` guards against a breaking struct literal as this
/// type grows sections, but is not a validation gate: its `pub` fields
/// can still be mutated after [`Self::load`]/[`Self::load_layered`]
/// validate, and shep-core cannot detect that.
#[non_exhaustive]
#[derive(Clone, Default, PartialEq)]
pub struct DaemonConfig {
    /// The `[daemon]` section
    pub daemon: DaemonSection,
    /// The `[whistle]` section
    pub whistle: WhistleSection,
    /// The `[secrets]` section
    pub secrets: SecretsSection,
    /// The `[style]` section
    pub style: StyleSection,
    /// The `[interpreters]` section: a script extension (no leading dot,
    /// `"js"` not `".js"`) mapped to the interpreter that runs it.
    ///
    /// Read by the CLI only, before a request reaches the wire: target
    /// resolution folds a match into an app's own
    /// [`AppConfig::interpreter`](crate::config::AppConfig::interpreter)
    /// only when that field is unset, and `--interpreter` on the command
    /// line outranks both. The daemon itself never reads this field.
    ///
    /// Declared here, like [`StyleSection`], so `RawDaemonConfig`'s
    /// `deny_unknown_fields` does not turn an unrecognized `[interpreters]`
    /// section into a hard parse error on every boot.
    pub interpreters: BTreeMap<String, String>,
    /// Raw `[dog.<name>]` sections keyed by dog name
    pub dog: BTreeMap<String, toml::Table>,
}

/// Redacts `dog`: only the table count is printed.
impl fmt::Debug for DaemonConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonConfig")
            .field("daemon", &self.daemon)
            .field("whistle", &self.whistle)
            .field("secrets", &self.secrets)
            .field("style", &self.style)
            .field("interpreters", &self.interpreters)
            .field("dog", &format_args!("<{} tables>", self.dog.len()))
            .finish()
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub(super) struct RawDaemonConfig {
    daemon: DaemonSection,
    whistle: WhistleSection,
    secrets: SecretsSection,
    style: StyleSection,
    interpreters: BTreeMap<String, String>,
    dog: BTreeMap<String, toml::Table>,
}

impl DaemonConfig {
    /// Builds config from optional file source + environment overrides.
    ///
    /// `file < env`, validated. Equivalent to [`Self::load_layered`] with
    /// an empty [`DaemonOverrides`].
    ///
    /// # Errors
    /// - [`DaemonConfigError::Toml`]: the file source is invalid TOML.
    /// - [`DaemonConfigError::BadEnvValue`]: a `SHEP_*` value is not parseable.
    /// - [`DaemonConfigError::BelowMinimum`]: the effective `max_cron_sleep` is below the floor.
    /// - [`DaemonConfigError::InvalidEnvironment`]: the effective `environment` is `all` or falls outside the secrets store's name grammar.
    pub fn load(
        file_source: Option<&str>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, DaemonConfigError> {
        Self::load_layered(file_source, env, &DaemonOverrides::new())
    }

    /// Builds config from optional file source + environment + CLI-flag
    /// overrides.
    ///
    /// `file < env < flags` (spec §5), validated exactly once, at the end,
    /// so a later layer can rescue a value an earlier one would reject.
    ///
    /// # Errors
    /// - [`DaemonConfigError::Toml`]: the file source is invalid TOML.
    /// - [`DaemonConfigError::BadEnvValue`]: a `SHEP_*` value is not parseable.
    /// - [`DaemonConfigError::BelowMinimum`]: the effective `max_cron_sleep` is below the floor.
    /// - [`DaemonConfigError::InvalidEnvironment`]: the effective `environment` is `all` or falls outside the secrets store's name grammar.
    pub fn load_layered(
        file_source: Option<&str>,
        env: &dyn Fn(&str) -> Option<String>,
        overrides: &DaemonOverrides,
    ) -> Result<Self, DaemonConfigError> {
        let raw: RawDaemonConfig = match file_source {
            Some(src) => toml::from_str(src).map_err(|e| DaemonConfigError::Toml(e.to_string()))?,
            None => RawDaemonConfig::default(),
        };
        let mut cfg = Self {
            daemon: raw.daemon,
            whistle: raw.whistle,
            secrets: raw.secrets,
            style: raw.style,
            interpreters: raw.interpreters,
            dog: raw.dog,
        };
        if let Some(v) = env("SHEP_LOG_JSON") {
            cfg.daemon.log_json = match parse_daemon_bool(&v) {
                Some(value) => value,
                None => return Err(DaemonConfigError::BadEnvValue("SHEP_LOG_JSON", v)),
            };
        }
        if let Some(v) = env("SHEP_LOG_LEVEL") {
            let Some(level) = LogLevel::from_name(&v) else {
                return Err(DaemonConfigError::BadEnvValue("SHEP_LOG_LEVEL", v));
            };
            cfg.daemon.log_level = level;
        }
        if let Some(v) = env("SHEP_SOCKET") {
            cfg.daemon.socket = Some(std::path::PathBuf::from(v));
        }
        // Whichever layer last wrote max_cron_sleep is the key the refusal
        // names, so the operator is pointed at the thing they can edit.
        // Validating per layer instead would stop a good override from
        // rescuing a bad one below it.
        let mut max_cron_sleep_key = "max_cron_sleep";
        if let Some(v) = env("SHEP_MAX_CRON_SLEEP") {
            let parsed = v
                .parse::<UpDuration>()
                .map_err(|_| DaemonConfigError::BadEnvValue("SHEP_MAX_CRON_SLEEP", v))?;
            cfg.daemon.max_cron_sleep = Some(parsed);
            max_cron_sleep_key = "SHEP_MAX_CRON_SLEEP";
        }
        if let Some(value) = overrides.log_json {
            cfg.daemon.log_json = value;
        }
        if let Some(value) = overrides.log_level {
            cfg.daemon.log_level = value;
        }
        if let Some(value) = &overrides.socket {
            cfg.daemon.socket = Some(value.clone());
        }
        if let Some(value) = overrides.max_cron_sleep {
            cfg.daemon.max_cron_sleep = Some(value);
            max_cron_sleep_key = "--max-cron-sleep";
        }
        cfg.validate(max_cron_sleep_key)?;
        Ok(cfg)
    }

    /// Checks every invariant a `DaemonConfig` carries, whatever layers
    /// produced it. One call site, at the bottom of [`Self::load_layered`]: validating
    /// per layer would stop a good `--max-cron-sleep` from rescuing a
    /// broken `shep.toml`.
    ///
    /// `key` is provenance: the spelling the operator actually set, so the
    /// refusal names the thing they can edit. Private; guards construction,
    /// not a later mutation of a `pub` field.
    ///
    /// # Errors
    /// - [`DaemonConfigError::BelowMinimum`]: `max_cron_sleep` is under the floor.
    /// - [`DaemonConfigError::InvalidEnvironment`]: `environment` is `all` or falls outside the secrets store's name grammar.
    fn validate(&self, key: &'static str) -> Result<(), DaemonConfigError> {
        if self.daemon.environment == secrets::ALL_ENVIRONMENTS
            || !secrets::is_name(&self.daemon.environment)
        {
            return Err(DaemonConfigError::InvalidEnvironment(
                self.daemon.environment.clone(),
            ));
        }
        if let Some(value) = self.daemon.max_cron_sleep
            && value < MIN_CRON_SLEEP
        {
            return Err(DaemonConfigError::BelowMinimum {
                key,
                value,
                min: MIN_CRON_SLEEP,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::error::DaemonConfigError;
    use super::super::overrides::DaemonOverrides;
    use super::super::sections::LogLevel;

    use super::super::testing::*;
    use super::*;
    use crate::secrets;
    use crate::values::UpDuration;

    #[test]
    fn missing_max_cron_sleep_leaves_the_field_none() {
        let cfg = DaemonConfig::load(None, &no_env).unwrap();
        assert_eq!(cfg.daemon.max_cron_sleep, None);
    }

    // fails if the field is a bare integer, where "5m" is a TOML error and
    // "5" is five milliseconds
    #[test]
    fn max_cron_sleep_file_value_parses_via_upduration() {
        let cfg = DaemonConfig::load(Some("[daemon]\nmax_cron_sleep = \"5m\""), &no_env).unwrap();
        assert_eq!(
            cfg.daemon.max_cron_sleep,
            Some(UpDuration::from_millis(5 * 60_000))
        );
    }

    // fails if the env read is placed before the file is folded in, or
    // omitted entirely
    #[test]
    fn env_max_cron_sleep_beats_file_value() {
        let env = |k: &str| (k == "SHEP_MAX_CRON_SLEEP").then(|| "90s".to_string());
        let cfg = DaemonConfig::load(Some("[daemon]\nmax_cron_sleep = \"5m\""), &env).unwrap();
        assert_eq!(
            cfg.daemon.max_cron_sleep,
            Some(UpDuration::from_millis(90_000))
        );
    }

    // fails if the env read swallows its parse failure (`.ok()` and drop
    // it, or an `Err` arm that only logs), leaving the file's value
    // silently in force and the typo invisible
    #[test]
    fn bad_env_max_cron_sleep_is_a_typed_error() {
        let env = |k: &str| (k == "SHEP_MAX_CRON_SLEEP").then(|| "banana".to_string());
        assert_eq!(
            DaemonConfig::load(None, &env),
            Err(DaemonConfigError::BadEnvValue(
                "SHEP_MAX_CRON_SLEEP",
                "banana".to_string()
            ))
        );
    }

    // fails if the floor is compared with `>` instead of `>=`, or the check
    // silently clamps instead of rejecting
    #[test]
    fn max_cron_sleep_floor_rejects_below_one_second() {
        let cfg = DaemonConfig::load(Some("[daemon]\nmax_cron_sleep = \"1s\""), &no_env).unwrap();
        assert_eq!(
            cfg.daemon.max_cron_sleep,
            Some(UpDuration::from_millis(1_000))
        );

        assert_eq!(
            DaemonConfig::load(Some("[daemon]\nmax_cron_sleep = \"999\""), &no_env),
            Err(DaemonConfigError::BelowMinimum {
                key: "max_cron_sleep",
                value: UpDuration::from_millis(999),
                min: UpDuration::from_millis(1_000),
            })
        );
    }

    // fails if only the file value is validated and never the override, or
    // if the reported key is the file's even though the environment
    // introduced the fault
    #[test]
    fn env_max_cron_sleep_floor_check_runs_on_the_winner() {
        let env = |k: &str| (k == "SHEP_MAX_CRON_SLEEP").then(|| "0".to_string());
        assert_eq!(
            DaemonConfig::load(Some("[daemon]\nmax_cron_sleep = \"5m\""), &env),
            Err(DaemonConfigError::BelowMinimum {
                key: "SHEP_MAX_CRON_SLEEP",
                value: UpDuration::from_millis(0),
                min: UpDuration::from_millis(1_000),
            })
        );
    }

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = DaemonConfig::load(None, &no_env).unwrap();
        assert!(!cfg.daemon.log_json);
        assert!(cfg.daemon.enabled_dogs.is_empty());
        assert!(cfg.dog.is_empty());
    }

    /// `adopted_dogs` needs `default` (existing files predate it) and
    /// `deny_unknown_fields` (a typo names a binary shep would otherwise
    /// run at the daemon's own trust level).
    #[test]
    fn adopted_dogs_default_empty_and_round_trip_by_name() {
        let bare = DaemonConfig::load(Some("[daemon]\nlog_json = true\n"), &no_env).unwrap();
        assert!(bare.daemon.adopted_dogs.is_empty());

        let src = r#"
    [daemon]
    enabled_dogs = ["metrics", "otel"]

    [daemon.adopted_dogs]
    otel = "/usr/local/bin/shep-otel"
    "#;
        let cfg = DaemonConfig::load(Some(src), &no_env).unwrap();
        assert_eq!(cfg.daemon.enabled_dogs, vec!["metrics", "otel"]);
        assert_eq!(
            cfg.daemon.adopted_dogs.get("otel"),
            Some(&std::path::PathBuf::from("/usr/local/bin/shep-otel"))
        );
        assert!(
            !cfg.daemon.adopted_dogs.contains_key("metrics"),
            "a name with no entry here is a built-in, and that is the whole distinction"
        );
    }

    // fails if the key is unknown, which deny_unknown_fields turns into a
    // startup error, or if it is not defaulted
    #[test]
    fn boot_first_dogs_parses_and_defaults_empty() {
        let config = DaemonConfig::load(
            Some(
                r#"
    [daemon]
    enabled_dogs = ["metrics"]
    boot_first_dogs = ["log-rotate"]
    "#,
            ),
            &no_env,
        )
        .expect("boot_first_dogs is a known key");
        assert_eq!(
            config.daemon.boot_first_dogs,
            vec!["log-rotate".to_string()]
        );

        let bare =
            DaemonConfig::load(Some("[daemon]\n"), &no_env).expect("an empty section parses");
        assert!(bare.daemon.boot_first_dogs.is_empty());
    }

    #[test]
    fn env_overrides_file() {
        let env = |k: &str| (k == "SHEP_LOG_JSON").then(|| "true".to_string());
        let cfg = DaemonConfig::load(Some("[daemon]\nlog_json = false"), &env).unwrap();
        assert!(cfg.daemon.log_json);
    }

    #[test]
    fn the_host_environment_defaults_to_production() {
        let cfg = DaemonConfig::load(None, &|_| None).unwrap();
        assert_eq!(cfg.daemon.environment, "production");
    }

    #[test]
    fn the_host_environment_reads_from_the_file() {
        let cfg =
            DaemonConfig::load(Some("[daemon]\nenvironment = \"staging\"\n"), &|_| None).unwrap();
        assert_eq!(cfg.daemon.environment, "staging");
    }

    #[test]
    fn the_host_environment_cannot_be_all() {
        // `all` is the secrets store's every-environment slot. A host
        // default of `all` would put every sheep with no environment of
        // its own there, bypassing the same refusal `normalize.rs` gives a
        // sheep that names `all` directly.
        let err =
            DaemonConfig::load(Some("[daemon]\nenvironment = \"all\"\n"), &|_| None).unwrap_err();
        // The variant and the value it carries, not the rendered text: the
        // message interpolates `ALL_ENVIRONMENTS` whatever it refused, so its
        // words cannot say which check fired.
        assert_eq!(
            err,
            DaemonConfigError::InvalidEnvironment(secrets::ALL_ENVIRONMENTS.to_string())
        );
    }

    #[test]
    fn a_host_environment_outside_the_grammar_is_refused() {
        for bad in ["", "has space", "has/slash"] {
            let source = format!("[daemon]\nenvironment = \"{bad}\"\n");
            assert!(
                DaemonConfig::load(Some(&source), &|_| None).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    // fails if the env read is placed before the file is folded in, or
    // omitted entirely.
    #[test]
    fn env_log_level_beats_file_value() {
        let env = |k: &str| (k == "SHEP_LOG_LEVEL").then(|| "debug".to_string());
        let cfg = DaemonConfig::load(Some("[daemon]\nlog_level = \"error\""), &env).unwrap();
        assert_eq!(cfg.daemon.log_level, LogLevel::Debug);
    }

    // fails if the env read swallows an unknown name and leaves the
    // default standing, or if the grammar is widened to accept
    // case-insensitive names.
    #[test]
    fn bad_env_log_level_is_a_typed_error() {
        for value in ["verbose", "WARN", ""] {
            let env = |k: &str| (k == "SHEP_LOG_LEVEL").then(|| value.to_string());
            assert_eq!(
                DaemonConfig::load(None, &env),
                Err(DaemonConfigError::BadEnvValue(
                    "SHEP_LOG_LEVEL",
                    value.to_string()
                )),
                "SHEP_LOG_LEVEL={value:?}"
            );
        }
    }

    // fails if a `#[serde(other)]` catch-all swallows a misspelled level
    // into a silent fallback. Pins "unknown variant", not just the
    // misspelled name, since that phrase is the only one exclusive to the
    // level being rejected rather than to some other unknown key.
    #[test]
    fn bad_file_log_level_is_a_toml_error() {
        let err = DaemonConfig::load(Some("[daemon]\nlog_level = \"verbose\""), &no_env)
            .expect_err("a misspelled level must not parse");
        let DaemonConfigError::Toml(message) = err else {
            panic!("a misspelled level is a TOML error, not {err:?}");
        };
        assert!(
            message.contains("unknown variant `verbose`"),
            "the error must reject the level's own name, not some other key: {message:?}"
        );
    }

    #[test]
    fn socket_override_via_file_and_env() {
        let cfg = DaemonConfig::load(Some("[daemon]\nsocket = \"/tmp/a.sock\""), &no_env).unwrap();
        assert_eq!(
            cfg.daemon.socket.as_deref(),
            Some(std::path::Path::new("/tmp/a.sock"))
        );
        let env = |k: &str| (k == "SHEP_SOCKET").then(|| "/tmp/b.sock".to_string());
        let cfg = DaemonConfig::load(Some("[daemon]\nsocket = \"/tmp/a.sock\""), &env).unwrap();
        assert_eq!(
            cfg.daemon.socket.as_deref(),
            Some(std::path::Path::new("/tmp/b.sock"))
        );
    }

    #[test]
    fn bad_toml_is_a_typed_error() {
        assert!(matches!(
            DaemonConfig::load(Some("[daemon"), &no_env),
            Err(DaemonConfigError::Toml(_))
        ));
    }

    // fails if `[whistle]` becomes an unrecognized section: `shep daemon`
    // would exit 4, and an operator who turned control tools on would
    // lose their shepherd on the next boot.
    #[test]
    fn a_whistle_section_parses_and_defaults_to_refusing_control() {
        let cfg = DaemonConfig::load(Some("[whistle]\nallow_control = true\n"), &no_env).unwrap();
        assert!(cfg.whistle.allow_control);

        let absent = DaemonConfig::load(Some("[daemon]\nlog_level = \"info\"\n"), &no_env).unwrap();
        assert!(
            !absent.whistle.allow_control,
            "a file with no [whistle] section leaves control off"
        );

        // A present-but-empty table is the only input that reaches
        // `allow_control`'s own field-level default; an absent `[whistle]`
        // table is filled by the container-level default instead.
        let empty_table = DaemonConfig::load(Some("[whistle]\n"), &no_env).unwrap();
        assert!(
            !empty_table.whistle.allow_control,
            "a [whistle] section with no keys leaves control off"
        );
    }

    // fails if `[secrets]` becomes an unrecognized section, or if the gate
    // stops defaulting shut. A misspelled key is a named error for
    // `[whistle]`'s reason: an operator certain a value was readable and a
    // CLI certain it was not.
    #[test]
    fn a_secrets_section_parses_and_defaults_to_refusing_reads() {
        let cfg = DaemonConfig::load(Some("[secrets]\nallow_read = true\n"), &no_env).unwrap();
        assert!(cfg.secrets.allow_read);

        let absent = DaemonConfig::load(Some("[daemon]\nlog_level = \"info\"\n"), &no_env).unwrap();
        assert!(
            !absent.secrets.allow_read,
            "a file with no [secrets] section leaves reads off"
        );

        let empty_table = DaemonConfig::load(Some("[secrets]\n"), &no_env).unwrap();
        assert!(
            !empty_table.secrets.allow_read,
            "a [secrets] section with no keys leaves reads off"
        );

        let err = DaemonConfig::load(Some("[secrets]\nallow_reads = true\n"), &no_env).unwrap_err();
        let DaemonConfigError::Toml(message) = err else {
            panic!("a misspelled key is a TOML error, got {err:?}")
        };
        assert!(
            message.contains("unknown field `allow_reads`"),
            "the message quotes the key that was not understood: {message}"
        );
    }

    // fails if the section silently accepts a key it does not implement. A
    // `[whistle] allow_contro = true` typo that parsed would leave an
    // operator certain the gate was open and whistle certain it was shut,
    // with nothing anywhere saying otherwise.
    #[test]
    fn a_misspelled_whistle_key_is_a_named_error() {
        let err =
            DaemonConfig::load(Some("[whistle]\nallow_contro = true\n"), &no_env).unwrap_err();
        let DaemonConfigError::Toml(message) = err else {
            panic!("a misspelled key is a TOML error, got {err:?}")
        };
        // The full quoted form, not the bare stem: `"allow_control"` also
        // contains `"allow_contro"`, so a stem-only assertion could pass
        // on a message naming only what serde expected.
        assert!(
            message.contains("unknown field `allow_contro`"),
            "the message quotes the key that was not understood: {message}"
        );
    }

    // Pins that `load` and `load_layered` agree when no flag is set. Does
    // not catch a `bool` standing in for `Option<bool>`, since both sides
    // route through the same code; other tests in this file and cli_e2e
    // pin that instead.
    #[test]
    fn an_absent_flag_leaves_every_lower_layer_alone() {
        let src = "[daemon]\nlog_json = true\nlog_level = \"debug\"\nsocket = \"/tmp/s.sock\"\n";
        let layered =
            DaemonConfig::load_layered(Some(src), &no_env, &DaemonOverrides::new()).unwrap();
        let plain = DaemonConfig::load(Some(src), &no_env).unwrap();
        assert_eq!(layered, plain);
    }

    // fails if `[interpreters]` stops parsing as a plain extension ->
    // interpreter map, or if a value written as a bare word (no quotes
    // needed, since these are ordinary TOML strings) fails to round-trip.
    #[test]
    fn interpreters_parses_as_an_extension_map() {
        let cfg = DaemonConfig::load(
            Some("[interpreters]\njs = \"node\"\npy = \"python3\"\n"),
            &no_env,
        )
        .unwrap();
        assert_eq!(cfg.interpreters.get("js").map(String::as_str), Some("node"));
        assert_eq!(
            cfg.interpreters.get("py").map(String::as_str),
            Some("python3")
        );
        assert_eq!(cfg.interpreters.len(), 2);
    }

    // An empty/absent `[interpreters]` must not fail a `shep.toml` that
    // never mentions the section, which is most of them until an operator
    // (or the first-run scaffold) writes one.
    #[test]
    fn interpreters_defaults_to_empty() {
        assert!(
            DaemonConfig::load(None, &no_env)
                .unwrap()
                .interpreters
                .is_empty()
        );
        assert!(
            DaemonConfig::load(Some("[daemon]\nlog_json = true\n"), &no_env)
                .unwrap()
                .interpreters
                .is_empty()
        );
    }

    // `[interpreters]` values are arbitrary extension keys, not a fixed
    // field set, so `deny_unknown_fields` (which governs struct fields)
    // must not reject an extension this build has never heard of.
    #[test]
    fn an_unrecognised_extension_is_not_an_unknown_field() {
        let cfg = DaemonConfig::load(Some("[interpreters]\nlua = \"lua5.4\"\n"), &no_env).unwrap();
        assert_eq!(
            cfg.interpreters.get("lua").map(String::as_str),
            Some("lua5.4")
        );
    }

    // A value that is not a string (an operator's `js = 5`, say) is still
    // a parse error, shep-core's usual fail-loudly-at-parse-time rule.
    #[test]
    fn a_non_string_interpreter_value_is_a_parse_error() {
        assert!(DaemonConfig::load(Some("[interpreters]\njs = 5\n"), &no_env).is_err());
    }
}
