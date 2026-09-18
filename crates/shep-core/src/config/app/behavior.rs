use super::schema::AppConfig;
use core::fmt;
use std::collections::BTreeMap;
// use schemars::generate
use crate::values::UpDuration;

/// Redacts `env`: only its length is printed.
impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("name", &self.name)
            .field("script", &self.script)
            .field("env", &format_args!("<{} vars>", self.env.len()))
            .finish_non_exhaustive()
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            script: String::new(),
            args: Vec::new(),
            cwd: None,
            interpreter: None,
            env: BTreeMap::new(),
            environment: None,
            instances: 1,
            autorestart: true,
            autostart: true,
            stop_exit_codes: Vec::new(),
            min_uptime: UpDuration::from_millis(1000),
            max_restarts: 16,
            restart_delay: None,
            // Not None: see the field's doc comment. An unstable exit with
            // no restart policy configured must not restart instantly.
            exp_backoff_restart_delay: Some(UpDuration::from_millis(100)),
            kill_signal: None,
            kill_timeout: UpDuration::from_millis(1600),
            shutdown_with_message: false,
            listen_timeout: UpDuration::from_millis(3000),
            graceful_timeout: UpDuration::from_millis(8000),
            action_timeout: UpDuration::from_millis(3000),
            max_memory: None,
            watch: false,
            ignore_watch: Vec::new(),
            watch_delay: None,
            cron_restart: None,
            fold: None,
            depends_on: Vec::new(),
            user: None,
            group: None,
            out_file: None,
            err_file: None,
            merge_logs: false,
            level_rules: Vec::new(),
            channel: false,
            stdin: false,
            wait_ready: false,
            reuse_port: false,
            readiness_probe: None,
            liveness_probe: None,
            watch_options: Vec::new(),
            cron_timezone: None,
        }
    }
}

impl AppConfig {
    /// A minimal config with spec defaults, the programmatic entry point.
    #[must_use]
    pub fn minimal(name: &str, script: &str) -> Self {
        Self {
            name: name.to_string(),
            script: script.to_string(),
            ..Self::default()
        }
    }

    /// The names of the fields whose values differ between `self` and
    /// `other`, in field-name order.
    ///
    /// Names only, never values. The one caller sends this list across the
    /// wire to be printed at an operator, and [`AppConfig::env`] carries
    /// secrets, so a differing `env` reports `"env"` and stops there.
    ///
    /// Compare configs that have both been through
    /// [`normalize`](fn@crate::config::normalize). Two configs differing only
    /// in what normalization would have filled in are not a difference an
    /// operator can act on, and reporting them would make the caller noisy
    /// about nothing.
    ///
    /// # Example
    ///
    /// ```
    /// use shep_core::config::AppConfig;
    ///
    /// let stored = AppConfig::minimal("web", "./srv");
    /// let mut edited = stored.clone();
    /// edited.cwd = Some("/srv".to_string());
    ///
    /// assert_eq!(stored.drifted_fields(&edited), vec!["cwd".to_string()]);
    /// assert!(stored.drifted_fields(&stored).is_empty());
    /// ```
    #[must_use]
    pub fn drifted_fields(&self, other: &Self) -> Vec<String> {
        if self == other {
            return Vec::new();
        }
        // Serde-compared, not field by field: a new field needs no edit here.
        // Sorted since `serde_json::Map` is a `BTreeMap` only while
        // `preserve_order` is off crate-wide. An empty result means no
        // drift, or none could be computed.
        let (Ok(serde_json::Value::Object(mine)), Ok(serde_json::Value::Object(theirs))) =
            (serde_json::to_value(self), serde_json::to_value(other))
        else {
            return Vec::new();
        };
        let mut fields: Vec<String> = mine
            .iter()
            .filter(|(key, value)| theirs.get(key.as_str()) != Some(value))
            .map(|(key, _)| key.clone())
            .collect();
        fields.sort_unstable();
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::super::schema::AppConfig;

    // use schemars::generate

    #[test]
    fn an_unedited_config_has_drifted_in_no_field() {
        let app = AppConfig::minimal("web", "./srv");

        assert!(app.drifted_fields(&app.clone()).is_empty());
    }

    #[test]
    fn drift_names_every_edited_field_and_no_other() {
        // Two fields, not one, so a comparator that stopped at the first
        // difference fails here.
        let stored = AppConfig::minimal("proto-api", "./proto-enum-api");
        let mut edited = stored.clone();
        edited.cwd = Some("/srv/pogo-proto-api".to_string());
        edited.args = vec!["-config".to_string(), "config.toml".to_string()];

        assert_eq!(
            stored.drifted_fields(&edited),
            vec!["args".to_string(), "cwd".to_string()]
        );
    }

    #[test]
    fn drift_reports_env_by_name_and_never_by_value() {
        let stored = AppConfig::minimal("web", "./srv");
        let mut edited = stored.clone();
        edited
            .env
            .insert("DATABASE_URL".to_string(), "postgres://hunter2".to_string());

        let fields = edited.drifted_fields(&stored);

        assert_eq!(fields, vec!["env".to_string()]);
        // Names go to an operator; a value never should.
        assert!(!fields.concat().contains("hunter2"));
    }

    #[test]
    fn drift_is_symmetric() {
        let stored = AppConfig::minimal("web", "./srv");
        let mut edited = stored.clone();
        edited.instances = 4;

        assert_eq!(
            stored.drifted_fields(&edited),
            edited.drifted_fields(&stored)
        );
        assert_eq!(
            stored.drifted_fields(&edited),
            vec!["instances".to_string()]
        );
    }
}
