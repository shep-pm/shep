use super::DogGrantsSection;
use crate::values::UpDuration;
use core::fmt;
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The `[daemon]` section
///
/// Each field carries `x-shep-apply` when the schema is generated: what an
/// edit to it costs, as one of the `APPLY_*` names in [`crate::config`]'s
/// schema module. The settings screen reads it so a field added later says
/// what it needs without the screen learning the field.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct DaemonSection {
    /// Emit the daemon's own logs as JSON lines
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub log_json: bool,
    /// Lowest severity of the daemon's own records that reaches its log
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub log_level: LogLevel,
    /// The environment every sheep resolves in unless it sets its own.
    ///
    /// A shepherd supervising real processes on a host is production unless
    /// somebody says otherwise.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub environment: String,
    /// Control-socket path override (default: `$SHEP_HOME/run/shep.sock`)
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "restart")))]
    pub socket: Option<PathBuf>,
    /// Dogs to autostart with the daemon (`shep enable` writes this)
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub enabled_dogs: Vec<String>,
    /// Where an adopted dog's binary lives, and the environment the operator
    /// sets for it, keyed by dog name (`shep adopt` writes this; `shep
    /// delete` removes it).
    ///
    /// A name in [`Self::enabled_dogs`] with no entry here is a built-in dog,
    /// an argv branch of the shep binary itself. Not recorded inside
    /// `[dog.<name>]`: that table is the dog's own opaque configuration,
    /// which the dog asks for over the socket. This is the operator's half.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub adopted_dogs: BTreeMap<String, AdoptedDog>,
    /// Adopted dogs whose `--version` answer asked for the shepherd channel,
    /// so they are spawned with `channel` and `shutdown_with_message` set.
    ///
    /// `shep adopt` writes this from the dog's own answer, never from an
    /// operator's flag, and `shep rehome` removes it. Here rather than in
    /// `dogs.toml` for the reason [`Self::adopted_dogs`] gives. A name
    /// absent from [`Self::adopted_dogs`] is inert here.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub channel_dogs: Vec<String>,
    /// What the operator granted each dog, keyed by dog name.
    ///
    /// A dog with no entry is undeclared and keeps full access; the shepherd
    /// checks every request on a dog's own connection against its entry.
    /// Here rather than in `dogs.toml` for the reason [`Self::adopted_dogs`]
    /// gives. A name absent from [`Self::enabled_dogs`] is inert here.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub dog_grants: BTreeMap<String, DogGrantsSection>,
    /// Dogs that run before every sheep, rather than after the flock.
    ///
    /// The default position for a dog is a final stage: a metrics dog must
    /// not answer for a flock that is not up yet. A log-rotation dog is the
    /// opposite case. shep cannot tell which is which, because an adopted dog
    /// is a third-party binary, so the operator says. A name absent from
    /// [`Self::enabled_dogs`] is inert here.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
    pub boot_first_dogs: Vec<String>,
    /// Longest a cron worker sleeps before re-deriving its next occurrence.
    ///
    /// Shorter recovers faster from a suspended laptop or an NTP step and
    /// costs proportionally more wakeups per cron-configured sheep; longer
    /// is cheaper and drifts further. Unset means the daemon's own default.
    /// There is no upper bound: a very long value only degrades to sleeping
    /// straight through to the occurrence, which still fires.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "reload")))]
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
            channel_dogs: Vec::new(),
            dog_grants: BTreeMap::new(),
            boot_first_dogs: Vec::new(),
            max_cron_sleep: None,
        }
    }
}

/// One entry of `[daemon.adopted_dogs]`: an adopted dog's binary, and the
/// environment the operator sets for it.
///
/// Two shapes on disk. A bare path string is what every `shep.toml` wrote
/// before `env` existed and what `shep adopt` still writes; it reads as an
/// entry with an empty `env`. A table spells both halves out:
/// `path = "..."` with an optional `env` table beside it.
///
/// `Debug` is manual and redacted (IR-41): `env` holds the operator's own
/// values, `{{secret:...}}` references and literals alike, and a
/// `DaemonConfig` is printed when a boot refuses its file.
#[derive(Clone, PartialEq, Eq)]
pub struct AdoptedDog {
    /// Where the dog's binary lives, exactly as the operator wrote it.
    pub path: PathBuf,
    /// Environment the shepherd adds to the dog's, as written.
    ///
    /// A value may hold a `{{secret:...}}`, resolved at spawn the way a
    /// sheep's `env` is. `SHEP_HOME` and `SHEP_DOG_NAME` are the shepherd's
    /// own and an entry here cannot replace them.
    pub env: BTreeMap<String, String>,
}

impl fmt::Debug for AdoptedDog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AdoptedDog {{ path: {:?}, env: <{} keys> }}",
            self.path,
            self.env.len()
        )
    }
}

/// The two shapes `[daemon.adopted_dogs]` writes one entry as.
struct AdoptedDogShape;

impl<'de> Visitor<'de> for AdoptedDogShape {
    type Value = AdoptedDog;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a binary path, or a table with `path` and an optional `env`")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(AdoptedDog {
            path: PathBuf::from(v),
            env: BTreeMap::new(),
        })
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        self.visit_str(&v)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut path: Option<PathBuf> = None;
        let mut env: Option<BTreeMap<String, String>> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "path" => {
                    if path.is_some() {
                        return Err(de::Error::duplicate_field("path"));
                    }
                    path = Some(PathBuf::from(map.next_value::<String>()?));
                }
                "env" => {
                    if env.is_some() {
                        return Err(de::Error::duplicate_field("env"));
                    }
                    env = Some(map.next_value()?);
                }
                other => return Err(de::Error::unknown_field(other, &["path", "env"])),
            }
        }
        Ok(AdoptedDog {
            path: path.ok_or_else(|| de::Error::missing_field("path"))?,
            env: env.unwrap_or_default(),
        })
    }
}

impl<'de> Deserialize<'de> for AdoptedDog {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(AdoptedDogShape)
    }
}

/// The schema of the two shapes, which serde's derive cannot express for a
/// union the deserializer reads by hand.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for AdoptedDog {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AdoptedDog".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "anyOf": [
                { "type": "string" },
                {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "env": {
                            "type": "object",
                            "additionalProperties": { "type": "string" }
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

/// How much of the daemon's own diagnostics reaches its log.
///
/// Written as one of the names below in `[daemon] log_level` or in
/// `SHEP_LOG_LEVEL`, lowercase and nothing else, so a typo is a startup
/// error naming the value rather than a level silently reverting. The
/// default is [`LogLevel::Warn`]: the daemon's records are dominated by
/// warn-and-continue arms. [`LogLevel::Debug`] is a firehose on a busy flock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
// Serialize only so the schema can print `warn` as the field's default.
#[cfg_attr(feature = "schema", derive(serde::Serialize, schemars::JsonSchema))]
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
/// control tools exist only when this is `true`; its five read-only tools
/// exist regardless. Lives only in `shep.toml`, with no flag or env var,
/// since config is auditable where a flag is not. Declared here because
/// `RawDaemonConfig` denies unknown fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct WhistleSection {
    /// Whether `shep whistle` offers its control tools. Default `false`.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "whistle")))]
    pub allow_control: bool,
}

/// The `[secrets]` section: whether the CLI will print a stored value back.
///
/// One key, a gate rather than a tuning knob, for [`WhistleSection`]'s
/// reason and read the same way: `shep secret get` reads this file
/// itself, the shepherd never reads this key, and it is declared here so
/// an undeclared `[secrets]` section is not a refused boot.
/// `Debug` is derived rather than redacted: one boolean, no secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct SecretsSection {
    /// Whether `shep secret get` prints a value. Default `false`.
    #[cfg_attr(feature = "schema", schemars(extend("x-shep-apply" = "next-command")))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct StyleSection {
    /// `full`, `plain` or `bare`. Absent means the CLI decides.
    #[cfg_attr(feature = "schema", schemars(
        extend("x-shep-apply" = "next-command", "enum" = ["full", "plain", "bare"])
    ))]
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

    /// IR-41. `env` is the operator's own values, and a `DaemonConfig` is
    /// printed when a boot refuses its file. Exact string, so a
    /// `#[derive(Debug)]` on [`AdoptedDog`] fails here.
    #[test]
    fn an_adopted_dogs_debug_prints_its_path_and_no_env_value() {
        let cfg = DaemonConfig::load(
            Some("[daemon.adopted_dogs.otel]\npath = \"/bin/otel\"\nenv = { KEY = \"hunter2\" }\n"),
            &no_env,
        )
        .unwrap();
        assert_eq!(
            format!("{:?}", cfg.daemon.adopted_dogs["otel"]),
            "AdoptedDog { path: \"/bin/otel\", env: <1 keys> }"
        );
        assert!(
            !format!("{cfg:?}").contains("hunter2"),
            "{:?}",
            cfg.daemon.adopted_dogs
        );
    }

    #[test]
    fn debug_redacts_dog_values() {
        // Dog tables carry things like webhook URLs; a lazy derive(Debug)
        // would land them in daemon logs. Exact string pinned so that
        // regression fails here instead of leaking a secret.
        let cfg = DaemonConfig::load(Some("[dog.metrics]\nport = 9615"), &no_env).unwrap();
        assert_eq!(
            format!("{cfg:?}"),
            "DaemonConfig { daemon: DaemonSection { log_json: false, log_level: Warn, environment: \"production\", socket: None, enabled_dogs: [], adopted_dogs: {}, channel_dogs: [], dog_grants: {}, boot_first_dogs: [], max_cron_sleep: None }, whistle: WhistleSection { allow_control: false }, secrets: SecretsSection { allow_read: false }, style: StyleSection { level: None }, interpreters: {}, hosts: [], dog: <1 tables> }"
        );
    }
}
