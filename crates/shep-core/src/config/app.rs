//! Per-app configuration schema: one sheep's Flockfile entry.

use core::fmt;

use std::collections::BTreeMap;

// use schemars::generate
use serde::{Deserialize, Deserializer, Serialize};

use crate::config::LevelRule;
use crate::values::{MemSize, UpDuration};

/// How a health probe checks a sheep
// wire format: changing these strings is a breaking change
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// HTTP GET must return 2xx
    Http,
    /// TCP connect must succeed
    Tcp,
    /// Command must exit 0
    Exec,
}

/// Readiness/liveness probe configuration (spec §7)
// wire format: changing field names/defaults is a breaking change
// `deny_unknown_fields` used to live here. This type rides the wire inside
// `AppConfig` (itself carried by `Request::Start`, `Request::Add`, and
// `Response::SheepConfig`), where an unknown field means a newer peer, not
// a typo — denying it here would make a newer daemon's reply break an
// older client. The denial moved to `Flockfile::parse`, where the input
// really is a hand-written file. Do not restore the serde attribute here.
//
// The schema-only sibling attribute below is not the same thing and stays:
// `schemars(deny_unknown_fields)` only shapes the generated
// `additionalProperties: false`, which an editor uses to flag a Flockfile
// typo before a parse ever runs. It never reaches `#[derive(Deserialize)]`
// (schemars mirrors it into a synthesized attribute its own macro expansion
// reads, not the real one), so the wire still tolerates an unknown field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct ProbeConfig {
    /// Probe mechanism
    pub kind: ProbeKind,
    /// URL (http), `host:port` (tcp), or command line (exec)
    pub target: String,
    /// Time between probes (default 10s)
    #[serde(default = "default_probe_interval")]
    pub interval: UpDuration,
    /// Per-probe timeout (default 5s)
    #[serde(default = "default_probe_timeout")]
    pub timeout: UpDuration,
    /// Consecutive failures before the probe reports unhealthy (default 3)
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
}

fn default_probe_interval() -> UpDuration {
    UpDuration::from_millis(10_000)
}
fn default_probe_timeout() -> UpDuration {
    UpDuration::from_millis(5_000)
}
fn default_failure_threshold() -> u32 {
    3
}

/// Per-app configuration — one sheep's entry in a Flockfile
///
/// Field names are the Flockfile contract (sheep-native; pm2 spellings are
/// rejected — the importer translates them). Deserializing this type
/// directly tolerates an unknown field, since it also rides the wire; a
/// Flockfile typo instead fails loudly at [`Flockfile::parse`](crate::config::Flockfile::parse),
/// the input that really is hand-written.
///
/// # Example
/// ```
/// use shep_core::config::AppConfig;
///
/// let app: AppConfig = toml::from_str("name = \"web\"\nscript = \"./srv\"").unwrap();
/// assert!(app.autorestart); // spec default
/// ```
// wire format: changing field names/defaults is a breaking change
//
// `deny_unknown_fields` used to sit beside `default` here. This type rides
// the wire inside `Request::Start`, `Request::Add`, and
// `Response::SheepConfig` — the last of which is a newer daemon handing an
// older client a config it does not fully understand, which is exactly the
// case an unknown field means "a newer peer", not a typo. The denial moved
// to `Flockfile::parse`, where the input really is a hand-written file. Do
// not restore the serde attribute here.
//
// The schema-only sibling attribute below is not the same thing and stays:
// `schemars(deny_unknown_fields)` only shapes the generated
// `additionalProperties: false`, which an editor uses to flag a Flockfile
// typo before a parse ever runs. It never reaches `#[derive(Deserialize)]`
// (schemars mirrors it into a synthesized attribute its own macro expansion
// reads, not the real one), so the wire still tolerates an unknown field.
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
                    "~ expands, $VARS do not"],
        "neighbours": [{"field": "cwd",         "note": "resolved against this cwd"},
                       {"field": "interpreter", "note": "picks what runs this script"}]
    })))]
    pub script: String,
    /// Arguments passed to the script
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "inputs",
        "blurb": "Arguments passed to the script, as a list",
        "accepts": ["a list of strings, one argument each",
                    "{{instance}}, {{name}}, and {{secret:key}} expand"],
        "refuses": ["an unclosed {{ token", "a token shep does not define"]
    })))]
    pub args: Vec<String>,
    /// Working directory (default: daemon's cwd at spawn registration)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "/srv/app",
        "group": "process",
        "blurb": "Where the process runs. Without it, the daemon's own directory",
        "accepts": ["an absolute or relative path, expanded from cwd",
                    "~ expands, $VARS do not"],
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
    /// Environment for the sheep (merged over the daemon's filtered env).
    ///
    /// A value may arrive as a string, a bare boolean, or a bare whole
    /// number (`SOME_BOOL = true`, `PORT = 8080`), and leaves as a string
    /// either way, so this field stays `BTreeMap<String, String>` and the
    /// wire always carries strings. A float is refused: `1.10` would reach
    /// the process as `1.1`. YAML resolves an unquoted `yes` to `"true"`
    /// and `0x1F` to `"31"`, so quote a value whose text must survive.
    #[serde(deserialize_with = "deserialize_env")]
    #[cfg_attr(feature = "schema", schemars(
        extend(
            "init" = {
                "example": "{ NODE_ENV = 'production' }",
                "group": "inputs",
                "blurb": "Environment variables for this app, layered over the daemon's own",
                "accepts": ["a table of KEY = value pairs",
                            "a bare true or 8080, which arrives as text",
                            "{{instance}}, {{name}}, and {{secret:key}} expand in a value"],
                "refuses": ["a float, since 1.10 would arrive as 1.1",
                            "SHEP_INSTANCE, SHEP_NAME, or SHEP_ENVIRONMENT, which shep sets itself",
                            "an unclosed {{ token"],
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
    /// Defaults to 100ms, not unset. An unstable exit (sooner than
    /// `min_uptime`) with neither this nor `restart_delay` configured would
    /// otherwise restart with no delay at all, so an app that can never
    /// start (a missing dependency, a bad config) would burn its whole
    /// `max_restarts` budget inside a second, logging the same failure
    /// dozens of times.
    ///
    /// All of the above assumes `restart_delay` is unset. A fixed
    /// `restart_delay` takes precedence over this field on every exit,
    /// stable or not, so a stable exit restarts immediately only while
    /// `restart_delay` stays unset, and setting this field to `"0"`
    /// disables the backoff without producing an immediate restart if a
    /// nonzero `restart_delay` is also configured.
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
    /// Defaults to 3s — comfortably under the 5s an RPC caller gets when it
    /// sends no deadline of its own (`shep-client`'s `DEFAULT_DEADLINE`,
    /// mirrored daemon-side as `rpc`'s `DEFAULT_DEADLINE_MS`). The margin
    /// matters more than the number: push this past that budget and a caller
    /// using the plain default gives up with `DeadlineExceeded` before the
    /// daemon's own honest `TimedOut` row ever reaches it. A legitimately
    /// slow action (a cache flush, say) can still ask for longer, but its
    /// caller has to ask for a longer deadline in step —
    /// `Client::request_with_deadline`, the way `shep logs -f` already asks
    /// for `LOG_PLANE_DEADLINE` rather than the client's default. `normalize`
    /// refuses a value no caller could ever satisfy, however long a deadline
    /// it asks for; a value merely above the *default* budget is a caller's
    /// choice to widen its own deadline, not a config error this crate can
    /// see.
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
        "accepts": ["a five field cron pattern, croner's dialect"],
        "refuses": ["a field outside its valid range", "a pattern croner cannot parse"],
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
    /// Names, never `name:slot`: a dependency on one instance of a
    /// load-balanced app is not a claim about availability. A dependency on
    /// a multi-instance app waits for every instance.
    ///
    /// Read once when a batch is ordered, at a boot, a muster, or a staged
    /// start, so an edit reaches the next such operation rather than the
    /// running child.
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
        "refuses": ["a name with no passwd entry"],
        "neighbours": [{"field": "group", "note": "resolved together at spawn"}]
    })))]
    pub user: Option<String>,
    /// Run as this group (unix)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "www-data",
        "group": "process",
        "blurb": "Run as this group, on unix",
        "accepts": ["a unix group name"],
        "refuses": ["a name with no group entry"],
        "neighbours": [{"field": "user", "note": "resolved together at spawn"}]
    })))]
    pub group: Option<String>,
    /// Stdout log file (default: `$SHEP_HOME/logs/<name>-<instance>-out.log`; `merge_logs` collapses to `<name>-out.log`)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "/var/log/my-first-sheep/out.log",
        "group": "logging",
        "blurb": "Where stdout goes. Defaults to a file under $SHEP_HOME/logs",
        "accepts": ["a path, relative paths follow cwd",
                    "{{instance}} and {{name}} expand"],
        "refuses": ["a {{secret:...}} token",
                    "one path for every instance, without merge_logs"],
        "neighbours": [{"field": "err_file",   "note": "shares the same collision rule"},
                       {"field": "merge_logs", "note": "lets instances share one file on purpose"}]
    })))]
    pub out_file: Option<String>,
    /// Stderr log file (default: `$SHEP_HOME/logs/<name>-<instance>-err.log`; `merge_logs` collapses to `<name>-err.log`)
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "/var/log/my-first-sheep/err.log",
        "group": "logging",
        "blurb": "Where stderr goes. Defaults to a file under $SHEP_HOME/logs",
        "accepts": ["a path, relative paths follow cwd",
                    "{{instance}} and {{name}} expand"],
        "refuses": ["a {{secret:...}} token",
                    "one path for every instance, without merge_logs"],
        "neighbours": [{"field": "out_file",   "note": "shares the same collision rule"},
                       {"field": "merge_logs", "note": "lets instances share one file on purpose"}]
    })))]
    pub err_file: Option<String>,
    /// Merge instance logs into one file pair
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "group": "logging",
        "blurb": "Put every instance's output in one pair of files"
    })))]
    pub merge_logs: bool,
    /// How this app's own lines announce their level, for a client that
    /// filters by one.
    ///
    /// Ordered: rules are tried as written and the first match wins.
    /// Declaring any replaces the reader's built-in guess for this app
    /// rather than adding to it, so a line matching no rule announces no
    /// level. Nothing here hides a line: an unclassified line survives
    /// every level filter.
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
    /// Defaults to `false`, and the default is the decision rather than a
    /// convenience. Without it a sheep gets `/dev/null` on fd 0, which is what
    /// every sheep has had until now, and three things argue for keeping it
    /// that way unless an app asks otherwise:
    ///
    /// - Flipping it for the whole flock is a behaviour change to processes
    ///   nobody asked to change.
    /// - **Programs detect stdin.** A closed or null fd 0 is how a great many
    ///   programs decide they are non-interactive — no prompt, no pager, no
    ///   readline, no colour. Handing them a pipe silently moves them to the
    ///   other branch.
    /// - It costs a descriptor and a pump task per sheep for the whole life of
    ///   the process, against spec §14.11's single-digit-MB idle-RSS goal — the
    ///   same budget [`Self::channel`]'s own default is protecting.
    ///
    /// Unlike `channel`, nothing implies this: `wait_ready` and
    /// `shutdown_with_message` both need fd 3 and so turn `channel` on for you,
    /// while nothing in shep needs a sheep's stdin except an operator typing
    /// `shep whisper`. A sheep without it answers a `no_stdin` row and names
    /// this field.
    ///
    /// The pipe's write end lives as long as the sheep does, so the app sees
    /// EOF on stdin when the process is on its way out, never before.
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
    /// Asserts that the app itself sets `SO_REUSEPORT` before it binds —
    /// shep binds nothing, so it cannot set the option on the app's behalf.
    /// The child process owns the mechanism (Node ≥22's `reusePort`, Go's
    /// `net.ListenConfig.Control`, nginx's `reuseport`); shep's contribution
    /// is permission for the old and new instance to overlap during reload,
    /// not the socket option itself.
    ///
    /// That permission is what the field buys, and it is read by exactly one
    /// thing: which reload the daemon runs for the app.
    ///
    /// - **Unset**, and the app has a `readiness_probe`: reload is SERIAL.
    ///   The instance being replaced is drained first and its replacement is
    ///   spawned into the empty slot, so the app is down for the length of
    ///   the drain. That is the cost of an honest answer — while both
    ///   instances are up, a probe against an address cannot say which of
    ///   them answered, and shep would take the outgoing instance's reply as
    ///   proof the incoming one is ready.
    /// - **Set**: reload OVERLAPS. The replacement is spawned alongside the
    ///   instance it replaces and takes over without a gap — if the app really does set
    ///   `SO_REUSEPORT`. If it does not, the replacement takes `EADDRINUSE`
    ///   and the reload fails, which is the failure this field exists to keep
    ///   opt-in.
    ///
    /// An app with no `readiness_probe` overlaps either way: with nothing
    /// probing an address, there is no answer for the wrong instance to give.
    /// So does one using `wait_ready`, because the shepherd channel a
    /// replacement reports on is its own — the instance being replaced has no
    /// way to answer it. Both of those need `SO_REUSEPORT` as much as a
    /// `reuse_port` app does if they bind an address, since they are overlapped
    /// too; what this field changes is which apps get overlapped, not what an
    /// overlap costs.
    ///
    /// Setting this on an app that does NOT set the socket option is the one
    /// way to get it wrong, and shep cannot check it: the option is set
    /// inside the child, after the fork, on a socket shep never sees.
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
    /// Removed. Set your own variable to `{{instance}}` in `env` instead.
    ///
    /// Kept only so `normalize` can reject it with that instruction: a
    /// `deny_unknown_fields` serde error would name no replacement. Remove
    /// in 0.2.
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub increment_var: Option<String>,
}

/// One value an `env` table may carry: a string, or a bare boolean or whole
/// number an operator wrote without quoting.
///
/// Exists only to read a Flockfile, where the document is hand-written and a
/// bare value is a plausible shortcut. It never rides the wire: [`AppConfig`]
/// is serialized through its own impls, which see only `String`.
///
/// Debug does not leak an env value. A derived one would print the contents,
/// and a `{:?}` on a config mid-parse is how a secret reaches a log.
enum EnvValue {
    /// A quoted value, kept verbatim
    Str(String),
    /// A bare `true` or `false`
    Bool(bool),
    /// A whole number, signed or unsigned
    Int(i128),
}

impl fmt::Debug for EnvValue {
    /// Prints only the shape of the value, never its contents.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(_) => f.write_str("<str>"),
            Self::Bool(_) => f.write_str("<bool>"),
            Self::Int(_) => f.write_str("<int>"),
        }
    }
}

impl EnvValue {
    /// Renders the value as the string a process receives. Consuming: a borrow
    /// would force the `Str` arm to clone.
    #[must_use]
    fn into_string(self) -> String {
        match self {
            Self::Str(s) => s,
            Self::Bool(b) => b.to_string(),
            Self::Int(n) => n.to_string(),
        }
    }
}

impl<'de> serde::de::Deserialize<'de> for EnvValue {
    /// Reads one `env` value in whatever raw form it arrives.
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct EnvValueVisitor;

        impl serde::de::Visitor<'_> for EnvValueVisitor {
            type Value = EnvValue;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a string, boolean, or whole number")
            }

            /// A quoted value, kept verbatim.
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<EnvValue, E> {
                Ok(EnvValue::Str(v.to_string()))
            }

            /// A quoted value from a non-borrowed source.
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<EnvValue, E> {
                Ok(EnvValue::Str(v))
            }

            /// A bare `true` or `false`.
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<EnvValue, E> {
                Ok(EnvValue::Bool(v))
            }

            /// A whole signed number.
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<EnvValue, E> {
                // i128 holds the full i64 range losslessly.
                Ok(EnvValue::Int(i128::from(v)))
            }

            /// A whole unsigned number. Only JSON can produce one beyond
            /// `i64::MAX`; TOML's own spec bounds integers to signed 64-bit,
            /// so its `visit_u64` input is always within `i64::MAX` and the
            /// wider type is invisible from a TOML Flockfile. i128 holds
            /// whatever we receive losslessly, so no value is refused.
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<EnvValue, E> {
                Ok(EnvValue::Int(i128::from(v)))
            }

            /// A float, refused. `f64` carries no trailing zero and no
            /// written precision, so `1.10` would reach the process as
            /// `1.1`. The value is left out of the message: an `env` value
            /// never reaches a log.
            fn visit_f64<E: serde::de::Error>(self, _v: f64) -> Result<EnvValue, E> {
                Err(E::custom(
                    "a float env value loses its written form, quote it",
                ))
            }
        }

        de.deserialize_any(EnvValueVisitor)
    }
}

/// Reads an `env` table, rendering each [`EnvValue`] as the string a process
/// receives. The `deserialize_with` on [`AppConfig::env`].
fn deserialize_env<'de, D: Deserializer<'de>>(de: D) -> Result<BTreeMap<String, String>, D::Error> {
    Ok(BTreeMap::<String, EnvValue>::deserialize(de)?
        .into_iter()
        .map(|(k, v)| (k, v.into_string()))
        .collect())
}

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
            increment_var: None,
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
    use super::*;
    use crate::values::{MemSize, UpDuration};

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

    /// `EnvValue::Debug` prints only the kind, never the value — the exact
    /// string is pinned so a derived `Debug` (which prints the contents) fails
    /// here. This is the unit half of the redaction guarantee.
    #[test]
    fn env_value_debug_never_prints_the_value() {
        let cases = [
            (EnvValue::Str("postgres://secret".to_string()), "<str>"),
            (EnvValue::Bool(true), "<bool>"),
            (EnvValue::Int(9_223_372_036_854_775_807), "<int>"),
        ];
        for (value, expected) in cases {
            assert_eq!(format!("{value:?}"), expected);
        }
    }

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

    /// The path fields are the ones an operator gets wrong, and the ones the
    /// type table cannot describe. Each states what it takes.
    #[test]
    fn the_path_fields_state_what_they_accept() {
        let schema = crate::config::flockfile_schema_json().to_value();
        let props = schema
            .pointer("/$defs/AppConfig/properties")
            .and_then(serde_json::Value::as_object)
            .expect("app config properties must exist");
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
        let schema = crate::config::flockfile_schema_json().to_value();
        let props = schema
            .pointer("/$defs/AppConfig/properties")
            .and_then(serde_json::Value::as_object)
            .expect("app config properties must exist");
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
}
