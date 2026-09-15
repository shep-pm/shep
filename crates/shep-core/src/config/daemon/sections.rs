use crate::values::UpDuration;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The `[daemon]` section
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DaemonSection {
    /// Emit the daemon's own logs as JSON lines
    pub log_json: bool,
    /// Lowest severity of the daemon's own records that reaches its log
    pub log_level: LogLevel,
    /// The environment every sheep resolves in unless it sets its own.
    ///
    /// A shepherd supervising real processes on a host is production unless
    /// somebody says otherwise.
    pub environment: String,
    /// Control-socket path override (default: `$SHEP_HOME/run/shep.sock`)
    pub socket: Option<std::path::PathBuf>,
    /// Dogs to autostart with the daemon (`shep enable` writes this)
    pub enabled_dogs: Vec<String>,
    /// Where an adopted dog's binary lives, keyed by dog name
    /// (`shep adopt` writes this; `shep rehome` removes it).
    ///
    /// A name in [`Self::enabled_dogs`] with no entry here is a built-in
    /// dog, an argv branch of the shep binary itself. Not recorded inside
    /// `[dog.<name>]`: that table is the dog's own opaque configuration,
    /// and a shep-owned key inside it would collide with a third-party
    /// dog's schema.
    pub adopted_dogs: BTreeMap<String, PathBuf>,
    /// Dogs that run before every sheep, rather than after the flock.
    ///
    /// The default position for a dog is a final stage, for the reason
    /// `boot.rs` gives: a metrics dog must not answer for a flock that is not
    /// up yet. A log-rotation dog is the opposite case, since it has to be
    /// running before a sheep starts writing. shep cannot tell which is
    /// which, because an adopted dog is a third-party binary, so the
    /// operator says.
    ///
    /// Here rather than in `dogs.toml` for the reason [`Self::adopted_dogs`]
    /// gives: that file's `[<name>]` table is the dog's own opaque
    /// configuration and a shep-owned key inside it would collide with a
    /// third-party dog's schema.
    ///
    /// A name absent from [`Self::enabled_dogs`] is inert here.
    pub boot_first_dogs: Vec<String>,
    /// Longest a cron worker sleeps before re-deriving its next occurrence.
    ///
    /// Shorter recovers faster from a suspended laptop or an NTP step and
    /// costs proportionally more wakeups per cron-configured sheep; longer
    /// is cheaper and drifts further. Unset means the daemon's own default.
    /// There is no upper bound: a very long value only degrades to sleeping
    /// straight through to the occurrence, which still fires.
    pub max_cron_sleep: Option<UpDuration>,
}

/// Not derived: [`DaemonSection::environment`] defaults to `"production"`,
/// which `String`'s own `Default` cannot express.
impl Default for DaemonSection {
    fn default() -> Self {
        Self {
            log_json: false,
            log_level: LogLevel::default(),
            environment: "production".to_string(),
            socket: None,
            enabled_dogs: Vec::new(),
            adopted_dogs: BTreeMap::new(),
            boot_first_dogs: Vec::new(),
            max_cron_sleep: None,
        }
    }
}

/// How much of the daemon's own diagnostics reaches its log.
///
/// Written as one of the names below in `[daemon] log_level` or in
/// `SHEP_LOG_LEVEL`, lowercase and nothing else, the same closed grammar
/// `log_json` accepts, so a typo is a startup error naming the value
/// rather than a level silently reverting to the default.
///
/// The default is [`LogLevel::Warn`]. The daemon's records are dominated
/// by warn-and-continue arms, each the only account of a decision the
/// operator cannot otherwise see. [`LogLevel::Debug`] adds per-decision
/// detail firing per dropped restart and per child metric sample, a
/// firehose on a busy flock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Nothing at all: the daemon writes no records of its own.
    Off,
    /// Only faults the daemon could not work around.
    Error,
    /// Faults the daemon worked around, and what working around them cost.
    #[default]
    Warn,
    /// Lifecycle milestones: the daemon came up, the daemon is going down.
    Info,
    /// Per-decision detail: every restart weighed, every metric sampled.
    Debug,
    /// Everything the daemon can say about itself.
    Trace,
}

/// Floor on `[daemon] max_cron_sleep`.
///
/// Zero makes every sleep return immediately, spinning the loop while
/// still firing correctly, which is what makes it hard to attribute. One
/// second is a floor no legitimate configuration wants to be under: a
/// five-field cron pattern cannot name anything finer than a minute.
pub(super) const MIN_CRON_SLEEP: UpDuration = UpDuration::from_millis(1_000);

/// The `[whistle]` section.
///
/// One key, a gate rather than a tuning knob: `shep whistle`'s four
/// control tools exist only when this is `true`; its five read-only
/// tools exist regardless.
///
/// Lives only in `shep.toml`, no flag or env var, since config is
/// auditable where a flag is not. The shepherd itself never reads this
/// key; `shep whistle` reads the file directly. Declared here anyway
/// because `RawDaemonConfig` denies unknown fields, so an undeclared
/// `[whistle]` section would refuse the whole file to boot. `Debug` is
/// derived, not redacted: one boolean, nothing to leak.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WhistleSection {
    /// Whether `shep whistle` offers its control tools. Default `false`.
    pub allow_control: bool,
}

/// The `[secrets]` section: whether the CLI will print a stored value back.
///
/// One key, a gate rather than a tuning knob, for [`WhistleSection`]'s
/// reason and read the same way: `shep secret get` reads this file itself,
/// the shepherd never reads this key, and it is declared here so an
/// undeclared `[secrets]` section is not a refused boot.
///
/// `Debug` is derived rather than redacted: one boolean, no secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecretsSection {
    /// Whether `shep secret get` prints a value. Default `false`.
    pub allow_read: bool,
}

/// The `[style]` section: how much the CLI dresses up its output.
///
/// Read by the CLI only. The daemon has no opinion about how anyone likes
/// their tables, and parses this solely so an unknown key is not an error.
///
/// `Debug` is derived rather than redacted: one optional string, no
/// secret, nothing a `{:?}` could leak.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StyleSection {
    /// `full`, `plain` or `bare`. Absent means the CLI decides.
    pub level: Option<String>,
}

impl LogLevel {
    /// The one spelling this level is written as, in the file and in the
    /// environment alike
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// The level `name` spells, or `None` when it spells no level.
    ///
    /// The inverse of [`LogLevel::as_str`], and exact: an uppercase or
    /// mixed-case name is not a level here, because `SHEP_LOG_JSON` accepts
    /// no `TRUE` either.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "off" => Some(Self::Off),
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::DaemonConfig;

    use super::super::testing::*;
    use super::*;

    #[test]
    fn an_unset_log_level_is_warn() {
        assert_eq!(
            DaemonConfig::load(None, &no_env).unwrap().daemon.log_level,
            LogLevel::Warn
        );
    }

    // `as_str`, `from_name` and serde's `rename_all` are three separate
    // spellings of the same mapping; nothing else keeps them in agreement.
    // fails if any one drifts from the other two.
    #[test]
    fn every_log_level_name_means_the_same_thing_in_the_file_and_the_environment() {
        let levels = [
            LogLevel::Off,
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ];
        for level in levels {
            let name = level.as_str();
            assert_eq!(LogLevel::from_name(name), Some(level), "from_name({name})");

            let file = format!("[daemon]\nlog_level = \"{name}\"");
            let cfg = DaemonConfig::load(Some(&file), &no_env).unwrap();
            assert_eq!(cfg.daemon.log_level, level, "[daemon] log_level = {name:?}");

            let env = |k: &str| (k == "SHEP_LOG_LEVEL").then(|| name.to_string());
            let cfg = DaemonConfig::load(None, &env).unwrap();
            assert_eq!(cfg.daemon.log_level, level, "SHEP_LOG_LEVEL={name}");
        }
    }

    #[test]
    fn debug_redacts_dog_values() {
        // Dog tables carry things like webhook URLs; a lazy derive(Debug)
        // would land them in daemon logs. Exact string pinned so that
        // regression fails here instead of leaking a secret.
        let cfg = DaemonConfig::load(Some("[dog.metrics]\nport = 9615"), &no_env).unwrap();
        assert_eq!(
            format!("{cfg:?}"),
            "DaemonConfig { daemon: DaemonSection { log_json: false, log_level: Warn, environment: \"production\", socket: None, enabled_dogs: [], adopted_dogs: {}, boot_first_dogs: [], max_cron_sleep: None }, whistle: WhistleSection { allow_control: false }, secrets: SecretsSection { allow_read: false }, style: StyleSection { level: None }, interpreters: {}, dog: <1 tables> }"
        );
    }
}
