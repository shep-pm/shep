use super::dog_table::{DogTable, deserialize_dogs};
use super::env_value::deserialize_env;
use super::probe::ProbeConfig;
use std::collections::BTreeMap;
// use schemars::generate
use crate::config::{CpuCores, LevelRule, MemoryEnforce};
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
    /// Removed. The sandbox dog replaced it, so any value is refused.
    ///
    /// Configure the dog in an `[app.dogs.sandbox]` table. The field stays
    /// deserialize-only so a peer that still sends the key is told, not
    /// ignored: `Flockfile::parse` and `normalize` both refuse it. It is never
    /// serialized and the schema does not list it.
    #[serde(skip_serializing)]
    #[cfg_attr(feature = "schema", schemars(skip))]
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
    /// Who enforces `max_memory`: `"poll"` (shep samples) or `"kernel"` (cgroup `memory.max`)
    ///
    /// `"kernel"` needs `max_memory`, Linux and a delegated cgroup v2 subtree. An
    /// OOM kill counts against `max_restarts`, unlike a polled restart.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "kernel",
        "group": "restart",
        "blurb": "Let the kernel cap memory at the moment of allocation instead of shep sampling it",
        "accepts": ["poll, the default", "kernel, on Linux with cgroup v2 delegated"],
        "refuses": ["kernel without max_memory", "kernel on macOS or Windows"],
        "neighbours": [{"field": "max_memory", "note": "the limit it enforces"}]
    })))]
    pub memory_enforce: MemoryEnforce,
    /// CPU quota in cores: `1.5` caps the sheep and its lambs at one and a half cores
    ///
    /// A quota, not affinity, like `docker run --cpus`: the tree is throttled,
    /// never killed. Needs Linux and a delegated cgroup v2 subtree.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": 1.5,
        "group": "restart",
        "blurb": "Throttle the app and its children to this many cores' worth of CPU time",
        "accepts": ["a number of cores of 0.01 or more, such as 0.5 or 2.5"],
        "refuses": ["below 0.01, zero or a negative number", "any value on macOS or Windows"],
        "neighbours": [{"field": "max_memory", "note": "the memory ceiling beside it"}]
    })))]
    pub max_cpu_cores: Option<CpuCores>,
    /// How many tasks, processes and threads, the sheep and its lambs may run at once
    ///
    /// Written to the cgroup's `pids.max`, like `docker run --pids-limit`. A
    /// thread counts as a task, so leave headroom above the healthy count. A
    /// fork or thread past it fails with `EAGAIN` and nothing is killed, so a
    /// fork bomb stops there. Needs Linux and a delegated cgroup v2 subtree.
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 4_194_304)))]
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": 256,
        "group": "restart",
        "blurb": "Cap how many tasks, processes and threads, the app and its children can run",
        "accepts": ["a whole number from 1 to 4194304"],
        "refuses": ["zero or a number above 4194304", "any value on macOS or Windows"],
        "neighbours": [{"field": "max_memory", "note": "the memory ceiling beside it"}]
    })))]
    pub max_tasks: Option<u32>,
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
        "refuses": ["a value that is not a table", "an empty dog name",
                    "a sandbox table on Windows"]
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
    /// Run this sheep on a pseudo-terminal instead of pipes.
    ///
    /// Defaults to `false`. For a program that behaves differently, or refuses
    /// to run, without a terminal: a shell, a REPL, a curses tool. Not
    /// supported on Windows. Set together with `stdin`, this field wins.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "inputs",
        "blurb": "Runs the process on a pseudo-terminal, for a program that needs a terminal",
        "refuses": ["pty = true on Windows"]
    })))]
    pub pty: bool,
    /// Expect `{"kind":"ready"}` on the shepherd channel
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "readiness",
        "blurb": "Wait for the app to say it is ready on the channel"
    })))]
    pub wait_ready: bool,
    /// Whether the app itself sets `SO_REUSEPORT` before it binds. shep cannot
    /// set the option for a socket the app binds; `listen` is how shep binds one.
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
    /// TCP addresses the shepherd binds for this sheep, by name.
    ///
    /// Bound once, when the first instance starts, and passed to every instance
    /// the systemd way: fd 3 upward in name order, with `LISTEN_FDS`,
    /// `LISTEN_FDNAMES` and `LISTEN_PID`. The shepherd channel moves to the fd
    /// after the last socket, named by `SHEP_CHANNEL_FD`. A reload overlaps on
    /// the same sockets. Not supported on Windows, or together with the sandbox dog.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": { "http": "0.0.0.0:8080" },
        "group": "process",
        "blurb": "TCP addresses shep binds and hands to every instance as LISTEN_FDS sockets",
        "accepts": ["a table of name to ip:port, such as 0.0.0.0:8080",
                    "a name of ASCII letters, digits, _ and -"],
        "refuses": ["an address that does not parse", "the same address twice",
                    "a name outside the accepted set", "any value on Windows",
                    "a value together with the sandbox dog"],
        "neighbours": [{"field": "reuse_port", "note": "needed for overlap only when listen is unset"},
                       {"field": "channel",    "note": "moves to the fd after the last socket"}]
    })))]
    pub listen: BTreeMap<String, String>,
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
mod tests;
