use super::dog_table::{DogTable, deserialize_dogs};
use super::env_value::deserialize_env;
use super::probe::ProbeConfig;
use std::collections::BTreeMap;
// use schemars::generate
use crate::config::LevelRule;
use crate::values::{MemSize, UpDuration};
use serde::{Deserialize, Serialize};

/// Per-app configuration: one sheep's entry in a Flockfile.
///
/// Field names are the Flockfile contract, and sheep-native: pm2
/// spellings are rejected, the importer translates them. Deserializing
/// this type directly tolerates an unknown field, since it rides the
/// wire. A Flockfile typo fails at [`Flockfile::parse`](crate::config::Flockfile::parse).
///
/// # Example
/// ```
/// use shep_core::config::AppConfig;
///
/// let app: AppConfig = toml::from_str("name = \"web\"\nscript = \"./srv\"").unwrap();
/// assert!(app.autorestart); // spec default
/// ```
// wire format: changing field names/defaults is a breaking change
// No `deny_unknown_fields` here: this type rides the wire, where an
// unknown field means a newer peer. The denial lives in `Flockfile::parse`.
// `schemars(deny_unknown_fields)` below shapes a schema only, never a parse.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
#[serde(default)]
pub struct AppConfig {
    /// Unique sheep name (required)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "my-first-sheep",
        "group": "process",
        "blurb": "A convenient and unique name for shep to display",
        "accepts": ["letters, digits, and most punctuation"],
        "refuses": ["a path separator or a colon", "a bare . or .."]
    })))]
    pub name: String,
    /// Executable or script path (required)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "./index.js",
        "group": "process",
        "blurb": "The script that shep should use to launch your app",
        "accepts": ["an absolute or relative path, expanded from cwd",
                    "~ expands, $VARS do not",
                    "a script shep cannot resolve yet: warned"],
        "neighbours": [{"field": "cwd",         "note": "resolved against this cwd"},
                       {"field": "interpreter", "note": "picks what runs this script"}]
    })))]
    pub script: String,
    /// Arguments passed to the script
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "inputs",
        "blurb": "Arguments passed to the script, as a list",
        "accepts": ["a list of strings, one argument each",
                    "{{instance}}, {{name}}, {{SHEP_HOME}} and {{secret:key}} expand"],
        "refuses": ["an unclosed {{ token", "a token shep does not define"]
    })))]
    pub args: Vec<String>,
    /// Working directory (default: daemon's cwd at spawn registration)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "/srv/app",
        "group": "process",
        "blurb": "Where the process runs. Without it, the daemon's own directory",
        "accepts": ["an absolute or relative path, expanded from cwd",
                    "~ expands, $VARS do not",
                    "a directory that does not exist yet: warned"],
        "neighbours": [{"field": "script",        "note": "resolved against this cwd"},
                       {"field": "out_file",      "note": "relative paths follow it too"},
                       {"field": "watch_options", "note": "globs are rooted here"}]
    })))]
    pub cwd: Option<String>,
    /// Interpreter override (`"none"` = run script directly)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "none",
        "group": "process",
        "blurb": "What runs the script. Set it to none to exec the file directly",
        "accepts": ["a command name found on PATH", "none, to exec the script directly"]
    })))]
    pub interpreter: Option<String>,
    /// A crabpot settings file. The sheep runs inside crabpot's sandbox.
    ///
    /// The shepherd spawns `crabpot -s <file> -- <command>`, finding
    /// `crabpot` on the sheep's `PATH`. A relative file is read from `cwd`.
    /// Not supported on Windows.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "sandbox.json",
        "group": "process",
        "blurb": "A crabpot settings file. The sheep runs inside crabpot's sandbox",
        "accepts": ["an absolute or relative path, a relative one read from cwd"],
        "refuses": ["an empty value", "any value on Windows"],
        "neighbours": [{"field": "cwd", "note": "a relative file is read from it"}]
    })))]
    pub sandbox: Option<String>,
    /// Environment for the sheep (merged over the daemon's filtered env).
    ///
    /// A value may arrive as a string, a bare boolean, or a bare whole
    /// number. It leaves as a string either way, so the wire always carries
    /// strings. A float is refused: `1.10` would reach the process as `1.1`.
    /// Quote a YAML value whose text must survive: `yes` becomes `"true"`
    /// and `0x1F` becomes `"31"`.
    #[serde(deserialize_with = "deserialize_env")]
    #[cfg_attr(feature = "schema", schemars(
        extend(
            "init" = {
                "example": "{ NODE_ENV = 'production' }",
                "group": "inputs",
                "blurb": "Environment variables for this app, layered over the daemon's own",
                "accepts": ["a table of KEY = value pairs",
                            "a bare true or 8080, which arrives as text",
                            "{{instance}}, {{name}}, {{SHEP_HOME}} and {{secret:key}} expand in a value"],
                "refuses": ["a float, since 1.10 would arrive as 1.1",
                            "SHEP_INSTANCE, SHEP_NAME, or SHEP_ENVIRONMENT, which shep sets itself",
                            "an unclosed {{ token",
                            "a token shep does not define"],
                "neighbours": [{"field": "environment", "note": "which environment {{secret:...}} reads from"}]
            },
            "additionalProperties" = {
                "anyOf": [{ "type": "string" }, { "type": "boolean" }, { "type": "integer" }]
            }
        )
    ))]
    pub env: BTreeMap<String, String>,
    /// Which environment this sheep resolves `{{secret:...}}` in.
    ///
    /// Absent falls back to `[daemon] environment` in `shep.toml`, which
    /// itself defaults to `production`. Never `all`: that is the store's
    /// every-environment slot, and a sheep claiming it would read that slot
    /// twice and never one of its own.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "staging",
        "group": "inputs",
        "blurb": "Which environment this app resolves secrets in",
        "accepts": ["a name from the secrets store"],
        "refuses": ["all, the store's every-environment slot",
                    "a name outside letters, digits, dot, underscore, or dash"],
        "neighbours": [{"field": "env", "note": "sets which environment its {{secret:...}} reads from"}]
    })))]
    pub environment: Option<String>,
    /// Instance count ("cluster" = N fork instances; spec §4)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "process",
        "blurb": "How many copies of this app to run"
    })))]
    pub instances: u32,
    /// Restart on unexpected exit
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "restart",
        "blurb": "Restarts the process automatically when it exits unexpectedly"
    })))]
    pub autorestart: bool,
    /// Start when the daemon starts / on `shep muster`
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "restart",
        "blurb": "Start this app when the daemon starts, and on shep muster"
    })))]
    pub autostart: bool,
    /// Exit codes treated as clean stop (no restart)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "restart",
        "blurb": "Exit codes that mean a clean stop, so shep will not restart",
        "accepts": ["a list of exit codes"]
    })))]
    pub stop_exit_codes: Vec<i32>,
    /// Uptime below this marks an exit as unstable
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "restart",
        "blurb": "An exit sooner than this counts as unstable"
    })))]
    pub min_uptime: UpDuration,
    /// Consecutive unstable exits before `errored`
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "restart",
        "blurb": "How many unstable exits in a row before shep gives up"
    })))]
    pub max_restarts: u32,
    /// Fixed delay before every restart (alternative to backoff)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "3s",
        "group": "restart",
        "blurb": "A fixed wait before every restart, instead of growing backoff"
    })))]
    pub restart_delay: Option<UpDuration>,
    /// Initial backoff delay; grows ×1.5 capped at 15s (spec §4)
    ///
    /// Defaults to 100ms, not unset. An unstable exit with no delay burns
    /// the whole `max_restarts` budget in a second. A fixed `restart_delay`
    /// takes precedence over this field on every exit. So `"0"` here
    /// disables the backoff only while `restart_delay` is unset.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "5s",
        "group": "restart",
        "blurb": "Starting delay between restarts, growing each time it fails again"
    })))]
    pub exp_backoff_restart_delay: Option<UpDuration>,
    /// Stop signal, one of `SIGTERM`/`SIGINT`/`SIGQUIT`/`SIGUSR2` (the `SIG`
    /// prefix and the case are both optional). Unset means `SIGTERM`.
    ///
    /// A `String` rather than a [`KillSignal`](crate::config::KillSignal) so
    /// the Flockfile schema and this struct's wire form stay plain text;
    /// `normalize` is what refuses a name outside that set, the same split
    /// `cron_restart` and the watch globs already use.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "SIGTERM",
        "group": "shutdown",
        "blurb": "Which signal shep sends first when stopping this app",
        "suggest": ["SIGTERM", "SIGINT", "SIGQUIT", "SIGUSR2"],
        "accepts": ["SIGTERM, SIGINT, SIGQUIT, or SIGUSR2",
                    "the SIG prefix and case are both optional"],
        "refuses": ["a signal outside that list"]
    })))]
    pub kill_signal: Option<String>,
    /// Grace period between stop signal and SIGKILL
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "shutdown",
        "blurb": "How long shep waits after the stop signal before SIGKILL"
    })))]
    pub kill_timeout: UpDuration,
    /// Send `{"kind":"shutdown"}` on the shepherd channel instead of a signal
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "shutdown",
        "blurb": "Ask the app to stop over the channel instead of signalling it"
    })))]
    pub shutdown_with_message: bool,
    /// Readiness fallback window when no ready signal/probe configured
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "readiness",
        "blurb": "How long to wait for readiness when nothing else reports it"
    })))]
    pub listen_timeout: UpDuration,
    /// Drain window for the old instance during reload
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "shutdown",
        "blurb": "How long the old instance gets to drain during a reload"
    })))]
    pub graceful_timeout: UpDuration,
    /// How long a triggered action gets to answer on the shepherd channel
    /// before its row becomes `ActionOutcome::TimedOut`.
    ///
    /// Defaults to 3s, under the 5s an RPC caller gets when it sends no
    /// deadline of its own. Past that budget a caller gives up with
    /// `DeadlineExceeded` before the daemon's `TimedOut` row reaches it. A
    /// slow action can ask for longer through `Client::request_with_deadline`.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "shutdown",
        "blurb": "How long a triggered action has to answer before shep gives up"
    })))]
    pub action_timeout: UpDuration,
    /// Memory ceiling — polling enforcer restarts above this
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "512M",
        "group": "restart",
        "blurb": "Restart the app if it climbs above this much memory"
    })))]
    pub max_memory: Option<MemSize>,
    /// Watch files and restart on change
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "watch",
        "blurb": "Restart when a file changes"
    })))]
    pub watch: bool,
    /// Watch ignore globs (defaults added daemon-side: dot-entries, node_modules)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "watch",
        "blurb": "Paths watch should skip, on top of dotfiles and node_modules",
        "accepts": ["a list of glob patterns"],
        "refuses": ["a pattern globset cannot compile"],
        "neighbours": [{"field": "cwd",           "note": "globs are rooted here"},
                       {"field": "watch_options", "note": "removed from what this matches"}]
    })))]
    pub ignore_watch: Vec<String>,
    /// Watch debounce window (default 500ms, applied daemon-side)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "500",
        "group": "watch",
        "blurb": "How long to wait after a change before restarting"
    })))]
    pub watch_delay: Option<UpDuration>,
    /// Cron pattern for scheduled restarts (croner dialect)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "* * * * *",
        "group": "cron",
        "blurb": "Restart on a schedule, written as a cron pattern",
        "suggest": ["*/5 * * * *", "0 * * * *", "0 0 * * *", "0 0 * * 0"],
        "accepts": ["five fields: minute hour day month weekday",
                    "a nickname like @daily or @hourly"],
        "refuses": ["a field outside its valid range",
                    "a sixth seconds field, or L, W, # or ?",
                    "a pattern croner cannot parse"],
        "neighbours": [{"field": "cron_timezone", "note": "sets which zone this pattern reads in"}]
    })))]
    pub cron_restart: Option<String>,
    /// Fold (group) this sheep belongs to
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "backend",
        "group": "process",
        "blurb": "A fold to group this app with others, for commands that take one"
    })))]
    pub fold: Option<String>,
    /// Sheep or dogs that must be up before this one starts
    ///
    /// Names, never `name:slot`. A dependency on a multi-instance app waits
    /// for every instance.
    ///
    /// Read once when a batch is ordered: at a boot, a muster, or a staged
    /// start. An edit reaches the next such operation, not the running child.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "[\"db\", \"cache\"]",
        "group": "process",
        "blurb": "Other sheep or dogs that must be up before this one starts",
        "accepts": ["a list of sheep or dog names"],
        "refuses": ["this sheep's own name", "a name:slot instance reference"]
    })))]
    pub depends_on: Vec<String>,
    /// Run as this user (unix)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "www-data",
        "group": "process",
        "blurb": "Run as this user, on unix",
        "accepts": ["a unix user name"],
        "refuses": ["a name with no passwd entry",
                    "another user, unless the shepherd runs as root"],
        "neighbours": [{"field": "group", "note": "resolved together at spawn"}]
    })))]
    pub user: Option<String>,
    /// Run as this group (unix)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "www-data",
        "group": "process",
        "blurb": "Run as this group, on unix",
        "accepts": ["a unix group name"],
        "refuses": ["a name with no group entry",
                    "another group, unless the shepherd runs as root"],
        "neighbours": [{"field": "user", "note": "resolved together at spawn"}]
    })))]
    pub group: Option<String>,
    /// Stdout log file (default: `{{SHEP_HOME}}/logs/{{name}}-{{instance}}-out.log`; `merge_logs` collapses to `{{name}}-out.log`)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "{{SHEP_HOME}}/logs/{{name}}-{{instance}}-out.log",
        "group": "logging",
        "blurb": "Where stdout goes. Written out, the default is {{SHEP_HOME}}/logs/{{name}}-{{instance}}-out.log",
        "accepts": ["a path, relative paths follow cwd",
                    "{{instance}}, {{name}} and {{SHEP_HOME}} expand",
                    "a parent that does not exist yet: warned"],
        "refuses": ["a {{secret:...}} token",
                    "one path for every instance, without merge_logs"],
        "neighbours": [{"field": "err_file",   "note": "shares the same collision rule"},
                       {"field": "merge_logs", "note": "shares one file, unless this path spells {{instance}} out"}]
    })))]
    pub out_file: Option<String>,
    /// Stderr log file (default: `{{SHEP_HOME}}/logs/{{name}}-{{instance}}-err.log`; `merge_logs` collapses to `{{name}}-err.log`)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "{{SHEP_HOME}}/logs/{{name}}-{{instance}}-err.log",
        "group": "logging",
        "blurb": "Where stderr goes. Written out, the default is {{SHEP_HOME}}/logs/{{name}}-{{instance}}-err.log",
        "accepts": ["a path, relative paths follow cwd",
                    "{{instance}}, {{name}} and {{SHEP_HOME}} expand",
                    "a parent that does not exist yet: warned"],
        "refuses": ["a {{secret:...}} token",
                    "one path for every instance, without merge_logs"],
        "neighbours": [{"field": "out_file",   "note": "shares the same collision rule"},
                       {"field": "merge_logs", "note": "shares one file, unless this path spells {{instance}} out"}]
    })))]
    pub err_file: Option<String>,
    /// Merge instance logs into one file pair
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "logging",
        "blurb": "Put every instance's output in one pair of files"
    })))]
    pub merge_logs: bool,
    /// Start each line in `out_file` and `err_file` with the time it was
    /// written (default: `true`)
    ///
    /// Only the files: `shep bleats --follow` and a dog subscribed to
    /// `log.*` see the sheep's line verbatim either way.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "logging",
        "blurb": "Start every log line with the time it was written. On unless set false",
        "accepts": ["true or false"],
        "neighbours": [{"field": "out_file", "note": "the file whose lines carry the stamp"},
                       {"field": "err_file", "note": "likewise"}]
    })))]
    pub log_timestamps: bool,
    /// How this app's own lines announce their level, for a client that
    /// filters by one.
    ///
    /// Ordered: rules are tried as written and the first match wins.
    /// Declaring any replaces the reader's built-in guess rather than
    /// adding to it. Nothing here hides a line: an unclassified line
    /// survives every level filter.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "[{ pattern = '\\[ERROR\\]', level = 'error' }]",
        "group": "logging",
        "blurb": "What this app's own log levels look like, for filtering",
        "accepts": ["a list of { pattern, level } rules, tried in order",
                    "a regex matched against the whole line, (?i) folds case",
                    "a level of trace, debug, info, warn, or error"],
        "refuses": ["an empty pattern, which would claim every line",
                    "a pattern regex cannot compile"],
        "neighbours": [{"field": "out_file", "note": "the lines these rules read"}]
    })))]
    pub level_rules: Vec<LevelRule>,
    /// One table per dog, keyed by dog name, for this sheep alone.
    ///
    /// shep stores this and hands it to the dog it names, and never reads or
    /// validates it. A dog name need not be one shep already knows: an
    /// operator configures a dog before installing it. A `dogs` key that is
    /// not a table is refused, naming the key.
    #[serde(deserialize_with = "deserialize_dogs")]
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": { "jobs": { "concurrency": 2 } },
        "group": "dogs",
        "blurb": "Settings for one dog on this sheep alone, opaque to shep",
        "accepts": ["a table of dog name to a table of settings",
                    "a dog name shep does not know about yet"],
        "refuses": ["a value that is not a table", "an empty dog name"]
    })))]
    pub dogs: BTreeMap<String, DogTable>,
    /// Open the shepherd channel on fd 3 for this app on its own, without
    /// needing `wait_ready` or `shutdown_with_message` to imply it.
    ///
    /// Defaults to `false`: a socketpair plus two pump tasks per sheep is
    /// real cost weighed against spec §14.11's single-digit-MB idle-RSS
    /// goal, so a channel is opened only when something asks for one.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "inputs",
        "blurb": "Opens fd 3 so the app can talk to shep directly"
    })))]
    pub channel: bool,
    /// Open a pipe on this sheep's stdin, so `shep whisper` can write to it.
    ///
    /// Defaults to `false`. A null fd 0 is how many programs decide they are
    /// non-interactive. A pipe moves them to the other branch without being
    /// asked, and costs a descriptor and a pump task per sheep. Nothing
    /// implies this field: `wait_ready` and `shutdown_with_message` imply
    /// `channel`, not this. The app sees EOF only when the sheep is leaving.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "inputs",
        "blurb": "Keeps stdin open so shep whisper can write to the process"
    })))]
    pub stdin: bool,
    /// Expect `{"kind":"ready"}` on the shepherd channel
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "readiness",
        "blurb": "Wait for the app to say it is ready on the channel"
    })))]
    pub wait_ready: bool,
    /// Whether the app itself sets `SO_REUSEPORT` before it binds. shep binds
    /// nothing, so it cannot set the option for the app.
    ///
    /// Read by one thing: which reload the daemon runs. Set, and the
    /// replacement is spawned alongside the instance it replaces. An app that
    /// claims the option but does not set it takes `EADDRINUSE`. Unset with a
    /// `readiness_probe`, reload is serial: a probe cannot say which instance answered.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "process",
        "blurb": "The app sets SO_REUSEPORT itself, so reload may overlap the two instances"
    })))]
    pub reuse_port: bool,
    /// Readiness probe — gates reload's AwaitReady (spec §7)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": { "kind": "http", "target": "http://127.0.0.1:8080/ready" },
        "group": "readiness",
        "blurb": "A health check shep waits on before it treats a reload as finished",
        "accepts": ["kind: http, tcp, or exec",
                    "a target matching the kind: a url, host:port, or command"],
        "refuses": ["a failure_threshold of 0", "an interval below its own floor"],
        "neighbours": [{"field": "liveness_probe", "note": "uses a different interval floor"},
                       {"field": "reuse_port",     "note": "governs whether reload overlaps it"}]
    })))]
    pub readiness_probe: Option<ProbeConfig>,
    /// Liveness probe — failures feed the restart policy (spec §7)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": { "kind": "http", "target": "http://127.0.0.1:8080/healthz" },
        "group": "readiness",
        "blurb": "A health check that triggers a restart when it keeps failing",
        "accepts": ["kind: http, tcp, or exec",
                    "a target matching the kind: a url, host:port, or command"],
        "refuses": ["a failure_threshold of 0", "an interval below its own floor"],
        "neighbours": [{"field": "readiness_probe", "note": "uses a different interval floor"}]
    })))]
    pub liveness_probe: Option<ProbeConfig>,
    /// Watch include globs (empty = watch cwd)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "watch",
        "blurb": "Which paths to watch. Empty means the working directory",
        "accepts": ["a list of glob patterns", "empty watches the whole cwd"],
        "refuses": ["a pattern globset cannot compile"],
        "neighbours": [{"field": "cwd",          "note": "globs are rooted here"},
                       {"field": "watch",        "note": "has no effect unless watch is on"},
                       {"field": "ignore_watch", "note": "skipped even when matched here"}]
    })))]
    pub watch_options: Vec<String>,
    /// Timezone for `cron_restart` (IANA name)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "US/Eastern",
        "group": "cron",
        "blurb": "Which timezone cron_restart is read in, as an IANA name",
        "accepts": ["an IANA zone name, like US/Eastern"],
        "refuses": ["a name outside the IANA database"],
        "neighbours": [{"field": "cron_restart", "note": "the pattern this zone is read against"}]
    })))]
    pub cron_timezone: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::probe::ProbeKind;

    // use schemars::generate

    use super::*;
    use crate::values::{MemSize, UpDuration};

    use super::super::testing::*;

    #[test]
    fn minimal_config_gets_spec_defaults() {
        let app = AppConfig::minimal("web", "./server");
        assert_eq!(app.name, "web");
        assert_eq!(app.script, "./server");
        assert!(app.autorestart);
        assert!(app.autostart);
        assert_eq!(app.instances, 1);
        assert_eq!(app.min_uptime, UpDuration::from_millis(1000));
        assert_eq!(app.max_restarts, 16);
        assert_eq!(app.kill_timeout, UpDuration::from_millis(1600));
        assert_eq!(app.listen_timeout, UpDuration::from_millis(3000));
        assert_eq!(app.graceful_timeout, UpDuration::from_millis(8000));
        assert_eq!(app.action_timeout, UpDuration::from_millis(3000));
        assert!(app.max_memory.is_none());
        assert!(app.fold.is_none());
        assert!(!app.channel);
    }

    #[test]
    fn unstable_restarts_are_throttled_by_default() {
        let app = AppConfig::minimal("web", "./srv");
        assert_eq!(
            app.exp_backoff_restart_delay,
            Some(UpDuration::from_millis(100))
        );
    }

    #[test]
    fn stdin_is_not_piped_unless_the_app_asks() {
        let app = AppConfig::minimal("web", "./srv");
        assert!(!app.stdin);
        let parsed: AppConfig = toml::from_str("name = \"web\"\nscript = \"./srv\"").unwrap();
        assert!(!parsed.stdin);
    }

    #[test]
    fn the_flockfile_key_is_stdin() {
        let parsed: AppConfig =
            toml::from_str("name = \"web\"\nscript = \"./srv\"\nstdin = true").unwrap();
        assert!(parsed.stdin);
    }

    #[test]
    fn environment_defaults_to_absent_and_parses_from_a_flockfile() {
        assert_eq!(AppConfig::default().environment, None);
        let app: AppConfig =
            toml::from_str("name = \"web\"\nscript = \"./srv\"\nenvironment = \"staging\"")
                .unwrap();
        assert_eq!(app.environment.as_deref(), Some("staging"));
    }

    #[test]
    fn toml_round_trip_with_newtypes() {
        let toml_src = r#"
    name = "worker"
    script = "python3"
    args = ["job.py", "--fast"]
    max_memory = "512M"
    min_uptime = "5s"
    fold = "backend"
    env = { RUST_LOG = "info" }
    "#;
        let app: AppConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(app.max_memory, Some("512M".parse::<MemSize>().unwrap()));
        assert_eq!(app.min_uptime, UpDuration::from_millis(5000));
        assert_eq!(app.fold.as_deref(), Some("backend"));
        assert_eq!(app.env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert_eq!(app.args, vec!["job.py", "--fast"]);
    }

    /// Raw TOML scalars in `env` read as their string form. `true` becomes
    /// `"true"`, `8080` becomes `"8080"`, and quoted strings pass through.
    /// A single test here covers the deserialization path; the
    /// `flockfile` test that reads the same document through all four
    /// formats covers the format dispatch.
    #[test]
    fn env_coerces_raw_scalars_to_their_string_form() {
        let src = r#"
    name = "web"
    script = "./srv"
    env = { SOME_BOOL = true, PORT = 8080, NEG = -1, STR = "plain" }
    "#;
        let app: AppConfig = toml::from_str(src).unwrap();
        assert_eq!(app.env["SOME_BOOL"], "true");
        assert_eq!(app.env["PORT"], "8080");
        assert_eq!(app.env["NEG"], "-1");
        assert_eq!(app.env["STR"], "plain");
    }

    /// A protocol-8 peer still serializes `increment_var`, which this build
    /// no longer has. It must deserialize and be dropped rather than error,
    /// since `MIN_SUPPORTED` is 8.
    ///
    /// Fails if `deny_unknown_fields` comes back to this struct. The denial
    /// lives in `Flockfile::parse` so a newer daemon can hand an older client
    /// a config it does not fully understand.
    #[test]
    fn a_protocol_8_payload_carrying_increment_var_still_deserializes() {
        let src = r#"{ "name":"web","script":"./srv","increment_var":null }"#;
        let app = serde_json::from_str::<AppConfig>(src)
            .expect("a version-8 payload must not be refused for a field this build dropped");
        assert_eq!(app.name, "web");
        assert_eq!(app.script, "./srv");

        let populated = r#"{ "name":"web","script":"./srv","increment_var":"WORKER_ID" }"#;
        let app = serde_json::from_str::<AppConfig>(populated)
            .expect("a populated one is ignored too, not refused");
        assert_eq!(app.name, "web");
    }

    /// A float is refused rather than coerced. `f64` carries no trailing zero
    /// and no written precision, so accepting one hands the process a value
    /// the operator did not write. Quoting is the way to keep the text.
    ///
    /// Asserted through JSON: a TOML error echoes the offending source line,
    /// which would put the value in the message whatever serde said.
    #[test]
    fn env_refuses_a_float_because_its_written_form_would_not_survive() {
        let src = r#"{ "name":"web","script":"./srv","env":{ "RATIO": 1.10 } }"#;
        let err = serde_json::from_str::<AppConfig>(src)
            .expect_err("a float env value must be refused, not rounded")
            .to_string();
        assert!(
            err.contains("quote it"),
            "the error must name the fix, got: {err}"
        );
        assert!(!err.contains("1.1"), "the error leaked the value: {err}");

        let quoted = src.replace("1.10", r#""1.10""#);
        let app: AppConfig = serde_json::from_str(&quoted).unwrap();
        assert_eq!(app.env["RATIO"], "1.10");

        assert!(
            toml::from_str::<AppConfig>(
                "name = \"web\"\nscript = \"./srv\"\nenv = { RATIO = 1.10 }\n"
            )
            .is_err(),
            "TOML must refuse a float too"
        );
    }

    /// A whole number larger than `i64::MAX` is valid JSON and must load.
    /// TOML cannot reach this test: its spec bounds integers to signed 64-bit,
    /// so only JSON exercises this path. The value stringifies to its full
    /// positive form, not a negative wrap. Pinned to the exact string.
    #[test]
    fn env_reads_a_number_beyond_i64_max_without_wrapping() {
        let beyond_i64 = u64::MAX; // 18446744073709551615
        let src = format!(r#"{{ "name":"web","script":"./srv","env":{{ "BIG": {beyond_i64} }} }}"#);
        let app = serde_json::from_str::<AppConfig>(&src)
            .expect("a u64 beyond i64::MAX is valid JSON and must load");
        assert_eq!(app.env["BIG"], "18446744073709551615");
    }

    /// Serialization is the inverse of the coercion: whatever form a value
    /// arrived as, the wire form is a string. A `true` that deserialized
    /// into `"true"` must serialize to the JSON string `"true"`, not the
    /// boolean `true`.
    #[test]
    fn env_serialization_is_always_string_regardless_of_input_form() {
        let src = r#"
    name = "web"
    script = "./srv"
    env = { SOME_BOOL = true, PORT = 8080, STR = "hello" }
    "#;
        let app: AppConfig = toml::from_str(src).unwrap();
        let wire = serde_json::to_value(&app).unwrap();
        for (key, expected) in [("SOME_BOOL", "true"), ("PORT", "8080"), ("STR", "hello")] {
            assert_eq!(
                wire["env"].get(key).and_then(serde_json::Value::as_str),
                Some(expected),
                "wire form of {key} must be a string, not a scalar"
            );
        }
    }

    /// A value that is neither a string, boolean, nor number is refused, not
    /// guessed at. An array or object under `env` is a structural mistake —
    /// an operator meant a table or a list, and guessing a serialization is
    /// how a wrong value hides for months.
    #[test]
    fn env_refuses_structural_values() {
        for (label, inner) in [("array", r#"["a", "b"]"#), ("object", r#"{"k": "v"}"#)] {
            let src = format!(r#"{{ "name":"web","script":"./srv","env":{{"X":{inner}}}}}"#);
            assert!(
                serde_json::from_str::<AppConfig>(&src).is_err(),
                "a {label} env value must be refused"
            );
        }
    }

    /// The wire path is the opposite of a Flockfile's: an unknown field
    /// means a newer peer, and ignoring it is what stops a new Flockfile
    /// field breaking an older client that reads a config off the wire.
    /// `deny_unknown_fields` used to live here; the same typo is now
    /// refused only at `Flockfile::parse`, where the input really is a
    /// hand-written file.
    #[test]
    fn an_unknown_field_on_the_wire_is_ignored_rather_than_refused() {
        let config: AppConfig =
            serde_json::from_str(r#"{"name":"web","script":"./srv","invented_next_year":true}"#)
                .expect("the wire path tolerates what it does not know");
        assert_eq!(config.name, "web");
    }

    #[test]
    fn probe_config_parses_with_defaults() {
        let src = r#"
    name = "api"
    script = "./api"

    [readiness_probe]
    kind = "http"
    target = "http://127.0.0.1:8080/healthz"
    "#;
        let app: AppConfig = toml::from_str(src).unwrap();
        let probe = app.readiness_probe.unwrap();
        assert_eq!(probe.kind, ProbeKind::Http);
        assert_eq!(probe.target, "http://127.0.0.1:8080/healthz");
        assert_eq!(probe.interval, UpDuration::from_millis(10_000));
        assert_eq!(probe.timeout, UpDuration::from_millis(5_000));
        assert_eq!(probe.failure_threshold, 3);
        assert!(app.liveness_probe.is_none());
    }

    #[test]
    fn debug_redacts_env_values() {
        // Exact string pinned so a lazy derive(Debug) refactor fails here.
        let mut app = AppConfig::minimal("web", "./srv");
        app.env
            .insert("DATABASE_URL".to_string(), "postgres://secret".to_string());
        app.env.insert("RUST_LOG".to_string(), "info".to_string());
        assert_eq!(
            format!("{app:?}"),
            "AppConfig { name: \"web\", script: \"./srv\", env: <2 vars>, .. }"
        );
    }

    /// A dog's table can hold a credential the way env can; `AppConfig`'s
    /// manual `Debug` omits `dogs` entirely via `finish_non_exhaustive`, so
    /// a value inside one must never appear.
    #[test]
    fn debug_never_prints_a_dogs_table_value() {
        let mut app = AppConfig::minimal("web", "./srv");
        let mut table = serde_json::Map::new();
        table.insert(
            "token".to_string(),
            serde_json::Value::String("super-secret-token".to_string()),
        );
        app.dogs
            .insert("jobs".to_string(), crate::config::DogTable::from(table));
        assert!(!format!("{app:?}").contains("super-secret-token"));
    }

    /// The `AppConfig` property table of the Flockfile schema, owned because
    /// the schema value is a local a borrow could not outlive.
    fn app_props() -> serde_json::Map<String, serde_json::Value> {
        crate::config::flockfile_schema_json()
            .to_value()
            .pointer("/$defs/AppConfig/properties")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .expect("app config properties must exist")
    }

    /// The path fields are the ones an operator gets wrong, and the ones the
    /// type table cannot describe. Each states what it takes.
    #[test]
    fn the_path_fields_state_what_they_accept() {
        let props = app_props();
        for name in ["cwd", "script", "out_file", "err_file"] {
            let accepts = props[name]["init"]["accepts"].as_array();
            assert!(
                accepts.is_some_and(|forms| !forms.is_empty()),
                "{name} carries no accepted forms"
            );
        }
    }

    /// Every entry carries both halves, so nothing renders half a line.
    #[test]
    fn every_neighbour_entry_carries_a_field_and_a_note() {
        let props = app_props();
        for (name, prop) in props {
            let Some(entries) = prop["init"]["neighbours"].as_array() else {
                continue;
            };
            for entry in entries {
                assert!(
                    entry["field"].is_string(),
                    "{name} has a neighbour with no field"
                );
                assert!(
                    entry["note"].is_string(),
                    "{name} has a neighbour with no note"
                );
            }
        }
    }

    /// fails if a field claims a refusal `normalize` does not make. The
    /// static tables behind the same panel are parser-backed in
    /// `shep-cli`'s `lookout::validation`; a per-field clause is only
    /// backed here.
    #[test]
    fn every_per_field_refusal_is_one_normalize_really_makes() {
        for claim in refusal_claims() {
            let Proof::Refused { value, matches } = claim.proof else {
                continue;
            };
            let field = claim.field;
            let refusal = claim.refusal;
            match crate::config::normalize(*value) {
                Ok(_) => panic!("{field} says it refuses {refusal}, and normalize accepted it"),
                Err(err) => assert!(
                    matches(&err),
                    "{field}'s \"{refusal}\" was refused as {err:?}, which is not the variant \
                         the table names"
                ),
            }
        }
    }

    /// fails if a value the panel offers with one keypress is one
    /// `normalize` turns around. A suggestion is a literal fill, so it is
    /// the one claim a config can be checked against directly.
    #[test]
    fn every_suggested_value_is_one_normalize_accepts() {
        let props = app_props();
        for (field, prop) in props {
            let Some(values) = prop["init"]["suggest"].as_array() else {
                continue;
            };
            for value in values {
                let value = value.as_str().expect("a suggestion is a string");
                let app = sheep(|a| match field.as_str() {
                    "cron_restart" => a.cron_restart = Some(value.to_owned()),
                    "kill_signal" => a.kill_signal = Some(value.to_owned()),
                    other => panic!("{other} suggests values that this test cannot place"),
                });
                assert!(
                    crate::config::normalize(app).is_ok(),
                    "{field} suggests `{value}`, which normalize refuses"
                );
            }
        }
    }

    /// fails if the schema and the table have drifted apart in either
    /// direction: a new clause with no proof, or a proof for a clause no
    /// field writes any more.
    #[test]
    fn the_refusal_table_and_the_schema_name_the_same_clauses() {
        let props = app_props();
        let in_schema: Vec<(&str, &str)> = props
            .iter()
            .filter_map(|(field, prop)| Some((field.as_str(), prop["init"]["refuses"].as_array()?)))
            .flat_map(|(field, clauses)| {
                clauses.iter().map(move |clause| {
                    (
                        field,
                        clause.as_str().expect("a refusal clause is a string"),
                    )
                })
            })
            .collect();

        let claims = refusal_claims();
        let proved: Vec<(&str, &str)> = claims.iter().map(|c| (c.field, c.refusal)).collect();

        let unproved: Vec<_> = in_schema
            .iter()
            .filter(|clause| !proved.contains(clause))
            .collect();
        let stale: Vec<_> = proved
            .iter()
            .filter(|clause| !in_schema.contains(clause))
            .collect();
        assert!(
            unproved.is_empty() && stale.is_empty(),
            "a refusal and its proof live together, in refusal_claims in this file.\n  \
                 claimed by a field, proved nowhere: {unproved:?}\n  \
                 proved here, claimed by no field: {stale:?}"
        );
    }
}
