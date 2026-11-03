//! [`NormalizeError`], the one refusal every step of this module speaks.

use core::fmt;

use crate::config::KillSignal;
use crate::paths::{HOME_DIR_VAR, SHEP_HOME_VAR};
use crate::secrets;
use crate::values::UpDuration;

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{normalize, normalize_all, normalize_with_home};
#[cfg(doc)]
use crate::config::ProbeTarget;

/// Error type returned from [`normalize`] and [`normalize_all`]
///
/// `#[non_exhaustive]`: every config surface this crate learns to validate
/// brings its own rejection reasons with it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    /// `name` is empty
    MissingName,
    /// `name` contains `/`, `\` or `:`, or is `.`/`..`. A path separator
    /// would escape the shep home, since the name becomes a filesystem path
    /// stem; a colon is the `name:slot` separator, and is also illegal in a
    /// Windows filename, which a sheep name becomes part of. Carries the
    /// name.
    InvalidName(String),
    /// `environment` is [`crate::secrets::ALL_ENVIRONMENTS`], the secrets
    /// store's every-environment slot, or falls outside the grammar
    /// [`crate::secrets`] keys and environment names share. Carries the
    /// sheep name and the value as written.
    InvalidEnvironment {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// The value as the user wrote it.
        value: String,
    },
    /// An app's `env` sets a variable shep injects itself. Carries the sheep
    /// name and the variable, so the error names the entry to edit.
    ReservedEnvVar {
        /// The sheep name
        name: String,
        /// The variable the app tried to set
        var: &'static str,
    },
    /// `script` is empty
    MissingScript,
    /// `instances` is zero
    ZeroInstances,
    /// `cron_restart` is not valid in croner's dialect. Carries the pattern
    /// and the rejection reason.
    InvalidCron {
        /// The pattern as the user wrote it
        pattern: String,
        /// Why it was rejected
        reason: String,
    },
    /// `cron_timezone` is not a name in the IANA time-zone database
    InvalidTimezone {
        /// The value as the user wrote it
        name: String,
    },
    /// Two apps in one flock share this name
    DuplicateName(String),
    /// Two or more apps in one document depend on each other. Carries the
    /// cycle as a path, so the refusal names it rather than only reporting
    /// that one exists.
    DependencyCycle(Vec<String>),
    /// A `readiness_probe` or `liveness_probe` target is malformed. Carries
    /// which probe and the rendered reason.
    InvalidProbe {
        /// `"readiness_probe"` or `"liveness_probe"`, so the error names the
        /// line the user has to edit.
        probe: &'static str,
        /// [`ProbeTarget::parse`]'s rendered rejection reason.
        reason: String,
    },
    /// A `readiness_probe` or `liveness_probe` has `failure_threshold == 0`.
    ZeroFailureThreshold {
        /// `"readiness_probe"` or `"liveness_probe"`, so the error names the
        /// line the user has to edit.
        probe: &'static str,
    },
    /// A `readiness_probe` or `liveness_probe` has an `interval` under the
    /// floor its own loop in the daemon honours. At `0` that would spin the
    /// loop as fast as the runtime allows; a `liveness_probe` under a full
    /// second would instead be silently polled at that second.
    IntervalBelowMinimum {
        /// `"readiness_probe"` or `"liveness_probe"`, so the error names the
        /// line the user has to edit.
        probe: &'static str,
        /// The value as the user wrote it.
        value: UpDuration,
        /// The floor it failed.
        min: UpDuration,
    },
    /// `max_memory` is `0`, a ceiling every live process is already over, so
    /// the enforcer would restart the sheep on every poll forever. Carries
    /// the app name.
    ZeroMaxMemory {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// `memory_enforce = "kernel"` with no `max_memory`, so there is no value
    /// to write into `memory.max`. Carries the app name.
    MemoryEnforceNeedsMaxMemory {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// `memory_enforce = "kernel"` on a platform with no cgroups: anything but
    /// Linux. Carries the app name.
    MemoryEnforceKernelUnsupported {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// `max_cpu_cores` is below 0.01, negative or not a finite number: a quota
    /// `cpu.max` cannot hold, since the kernel refuses under 1000 microseconds
    /// per period, and zero would stall the sheep outright. Carries the app
    /// name.
    MaxCpuCoresOutOfRange {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// `max_cpu_cores` on a platform with no cgroups: anything but Linux.
    /// Carries the app name.
    MaxCpuCoresUnsupported {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// `action_timeout` is above `normalize`'s own ceiling: a wait no
    /// RPC caller could ever be given enough deadline to outlast, since the
    /// daemon clamps every deadline a caller can ask for. Carries the app
    /// name, the value as written, and the ceiling it failed. The ceiling
    /// itself is accepted.
    ActionTimeoutTooLong {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// The value as the user wrote it.
        value: UpDuration,
        /// The ceiling it failed.
        max: UpDuration,
    },
    /// `kill_signal` names a signal the daemon's stop ladder cannot send.
    /// Carries the app name and the value as written.
    InvalidKillSignal {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// The value as the user wrote it.
        value: String,
    },
    /// `watch` is enabled but the app sets no `cwd`, so there is no
    /// directory to watch. Carries the app name.
    WatchWithoutCwd {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// A path begins `~user/`, naming another user's home.
    ///
    /// Refused rather than resolved: answering it means a passwd lookup, and
    /// under a systemd unit the answer is not obviously the one anyone meant.
    /// `~/` is supported; this is not.
    TildeUser {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// Which field carried it.
        field: &'static str,
        /// The path as written.
        value: String,
    },
    /// A path begins `~/` and no home directory could be determined.
    NoHomeForTilde {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// Which field carried it.
        field: &'static str,
    },
    /// A value carries `{{SHEP_HOME}}` and no shep home could be determined.
    ///
    /// The CLI refuses this before normalising, when no `--home` or
    /// environment variable resolves a root. It guards a library caller
    /// of [`normalize_with_home`] supplying its own directories.
    NoShepHome {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// Which field carried it. Owned rather than `&'static str`, since
        /// an `env` key and an `args` index are both spelled at runtime.
        field: String,
    },
    /// `watch_delay` is `0`, which would spin the debouncer's own OS thread.
    /// Carries the app name.
    ZeroWatchDelay {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// A `watch_options` or `ignore_watch` pattern is one globset will not
    /// compile, so the watch it describes could never be armed.
    InvalidWatchGlob {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// `"watch_options"` or `"ignore_watch"`, so the error names which
        /// of the two lists to edit.
        field: &'static str,
        /// The pattern as the user wrote it.
        pattern: String,
        /// globset's own rendered reason.
        reason: String,
    },
    /// A `level_rules` entry shep cannot use: an empty pattern, which would
    /// claim every line, or one the regex engine refuses. Carries the sheep
    /// name and the rejection rendered.
    InvalidLevelRule {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// [`crate::config::LevelRuleError`]'s own rendering, so this variant
        /// does not restate a grammar it does not own.
        reason: String,
    },
    /// A value carries a `{{...}}` that is not a template token, or a `{{`
    /// it never closes. Carries the sheep name, which field held it, and the
    /// rejection rendered.
    BadTemplate {
        /// The sheep name
        name: String,
        /// Which field, for example `env.WORKER` or `args[1]`
        field: String,
        /// The template grammar's own error, rendered, so this
        /// variant does not have to restate the grammar's own copy
        reason: String,
    },
    /// An explicit `out_file` or `err_file` carries a `{{secret:...}}`.
    ///
    /// A resolved log path is not contained the way a resolved `env` value
    /// is: it reaches `ProcessInfo`, every bus event carrying one,
    /// `shep flock`, `shep describe`, `shep lookout`, and a filename on disk
    /// under `$SHEP_HOME/logs`. `{{instance}}` and `{{name}}` still render
    /// there; only a secret is refused.
    SecretInLogPath {
        /// The sheep name
        name: String,
        /// `out_file` or `err_file`
        field: &'static str,
    },
    /// An explicit log path renders to the same string for two different
    /// slots, the app runs more than one instance, and `merge_logs` is off,
    /// so every instance would write to one file without having asked to.
    /// A path with no `{{instance}}` is the ordinary case; a `{{name}}`-only
    /// path and an escaped `{{{{instance}}}}` collide for the same reason.
    SharedLogPath {
        /// The sheep name
        name: String,
        /// `out_file` or `err_file`
        field: &'static str,
    },
    /// An app names itself in `depends_on`. Carries the sheep name. A
    /// one-node cycle, caught here rather than in the graph because it is
    /// visible in a single `AppConfig`.
    SelfDependency(String),
    /// A `depends_on` entry names one instance rather than an app. Carries
    /// the sheep and the offending target, so the refusal can name both.
    InstanceDependency {
        /// The sheep whose list holds the entry
        sheep: String,
        /// The entry as written
        target: String,
    },
    /// A `dogs` key is the empty string. Carries the sheep name, so the
    /// error names which Flockfile entry to edit.
    EmptyDogName {
        /// The sheep name
        name: String,
    },
    /// `sandbox` is set, to any value. The sandbox dog replaced the key, and
    /// ignoring it would run the sheep unsandboxed. Carries the sheep name.
    SandboxKeyRemoved {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// A `dogs.sandbox` table is set on Windows, where crabpot's
    /// sandbox cannot start from session 0. Carries the sheep name.
    SandboxOnWindows {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// A `listen` name is empty or holds a character outside ASCII letters,
    /// digits, `_` and `-`, which `LISTEN_FDNAMES` cannot carry.
    InvalidListenName {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// The `listen` name as written.
        listener: String,
    },
    /// A `listen` address is not a `host:port` socket address with a literal
    /// IP, like `0.0.0.0:8080` or `[::1]:8080`.
    InvalidListenAddress {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// The `listen` name the address belongs to.
        listener: String,
        /// The address as written.
        address: String,
    },
    /// Two `listen` names hold the same address, so the second bind would
    /// fail.
    DuplicateListenAddress {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
        /// The first `listen` name holding the address, in name order.
        first: String,
        /// The later `listen` name holding it.
        second: String,
        /// The address as the second wrote it.
        address: String,
    },
    /// `listen` is set together with a `dogs.sandbox` table, and
    /// nothing yet shows crabpot passes the descriptors through.
    ListenWithSandbox {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
    /// `listen` is set on Windows, which has no descriptor numbers to pass
    /// sockets by.
    ListenOnWindows {
        /// The sheep name, so the error names which Flockfile entry to edit.
        name: String,
    },
}

impl fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingName => f.write_str("app config is missing a name"),
            Self::InvalidName(n) => {
                write!(
                    f,
                    "sheep name `{n}` may not contain a path separator or a colon, or be `.` or `..`; use `-` in place of a colon"
                )
            }
            Self::InvalidEnvironment { name, value } => write!(
                f,
                "sheep `{name}` has environment = `{value}`: must be 1-128 bytes of \
                 `[A-Za-z0-9._-]` not starting with `.`, and not `{}` (the secrets \
                 store's every-environment slot)",
                secrets::ALL_ENVIRONMENTS
            ),
            Self::ReservedEnvVar { name, var } => {
                // The two identity variables have a template to borrow
                // instead; the environment is not a template token, so its
                // advice points at the field that selects one (#284).
                let advice = match *var {
                    "SHEP_INSTANCE" => "or `{{instance}}` in your own variable",
                    "SHEP_NAME" => "or `{{name}}` in your own variable",
                    _ => "or set `environment` on the app",
                };
                write!(
                    f,
                    "sheep `{name}` sets `{var}` in env, but shep injects it: use a different name, {advice}"
                )
            }
            Self::MissingScript => f.write_str("app config is missing a script"),
            Self::ZeroInstances => f.write_str("instances must be at least 1"),
            Self::InvalidCron { pattern, reason } => {
                write!(f, "invalid cron_restart pattern `{pattern}`: {reason}")
            }
            Self::InvalidTimezone { name } => {
                write!(f, "`{name}` is not a recognized IANA timezone")
            }
            Self::DuplicateName(n) => write!(f, "duplicate sheep name `{n}`"),
            Self::DependencyCycle(cycle) => write!(
                f,
                "dependency cycle: {}",
                crate::config::graph::render_cycle(cycle)
            ),
            Self::InvalidProbe { probe, reason } => write!(f, "{probe}: {reason}"),
            Self::ZeroFailureThreshold { probe } => {
                write!(f, "{probe}.failure_threshold must be at least 1")
            }
            Self::IntervalBelowMinimum { probe, value, min } => {
                write!(f, "{probe}.interval is `{value}`: must be at least {min}")
            }
            Self::ZeroMaxMemory { name } => {
                write!(
                    f,
                    "sheep `{name}` has max_memory = 0, a limit nothing can stay under"
                )
            }
            Self::MemoryEnforceNeedsMaxMemory { name } => {
                write!(
                    f,
                    "sheep `{name}` has memory_enforce = \"kernel\" but no max_memory to enforce"
                )
            }
            Self::MemoryEnforceKernelUnsupported { name } => {
                write!(
                    f,
                    "sheep `{name}` has memory_enforce = \"kernel\", which needs Linux cgroups; \
                     use \"poll\" on this platform"
                )
            }
            Self::MaxCpuCoresOutOfRange { name } => {
                write!(
                    f,
                    "sheep `{name}` has a max_cpu_cores below 0.01 cores, or not a number"
                )
            }
            Self::MaxCpuCoresUnsupported { name } => {
                write!(
                    f,
                    "sheep `{name}` has max_cpu_cores, which needs Linux cgroups; \
                     remove it on this platform"
                )
            }
            Self::ActionTimeoutTooLong { name, value, max } => {
                write!(
                    f,
                    "sheep `{name}` has action_timeout = {value}: must be at most {max}, \
                     the longest wait any caller's deadline could ever cover"
                )
            }
            Self::InvalidKillSignal { name, value } => {
                write!(
                    f,
                    "`{name}`: kill_signal `{value}` is not one shep can send (accepted: {})",
                    KillSignal::ACCEPTED.join(", ")
                )
            }
            Self::TildeUser { name, field, value } => write!(
                f,
                "`{name}`: {field} is `{value}`, and shep expands only `~/` (your own home). \
                 Another user's home needs a passwd lookup whose answer depends on who the \
                 daemon runs as, so write the path out in full instead."
            ),
            Self::NoHomeForTilde { name, field } => write!(
                f,
                "`{name}`: {field} begins with `~/` but no home directory could be found. \
                 Set {HOME_DIR_VAR}, or write the path out in full."
            ),
            Self::NoShepHome { name, field } => write!(
                f,
                "`{name}`: {field} carries `{{{{SHEP_HOME}}}}` but no shep home could be \
                 found. Set {SHEP_HOME_VAR}, or write the path out in full."
            ),
            Self::WatchWithoutCwd { name } => {
                write!(f, "sheep `{name}` has watch = true but no cwd to watch")
            }
            Self::ZeroWatchDelay { name } => {
                write!(
                    f,
                    "sheep `{name}` has watch_delay = 0: must be greater than 0"
                )
            }
            Self::InvalidWatchGlob {
                name,
                field,
                pattern,
                reason,
            } => write!(
                f,
                "sheep `{name}` has an invalid {field} pattern `{pattern}`: {reason}"
            ),
            Self::InvalidLevelRule { name, reason } => {
                write!(f, "sheep `{name}` has an invalid level rule: {reason}")
            }
            Self::BadTemplate {
                name,
                field,
                reason,
            } => write!(f, "sheep `{name}`, {field}: {reason}"),
            Self::SecretInLogPath { name, field } => write!(
                f,
                "sheep `{name}` puts a `{{{{secret:...}}}}` in `{field}`: a log path may not hold a secret, since it becomes a filename on disk and is reported by `shep flock` and `shep describe`"
            ),
            Self::SharedLogPath { name, field } => write!(
                f,
                "sheep `{name}` runs several instances and sets `{field}` to one path: put `{{{{instance}}}}` in it, or set `merge_logs = true` to share it on purpose"
            ),
            Self::SelfDependency(n) => {
                write!(f, "`{n}` names itself in depends_on")
            }
            Self::InstanceDependency { sheep, target } => {
                let app = target.split(':').next().unwrap_or(target);
                write!(
                    f,
                    "`{sheep}` depends on `{target}`, which names one instance. \
                     Depend on `{app}` instead: a dependency waits for every \
                     instance of an app"
                )
            }
            Self::EmptyDogName { name } => {
                write!(f, "sheep `{name}` has a dogs entry with an empty name")
            }
            Self::SandboxKeyRemoved { name } => write!(
                f,
                "sheep `{name}` sets sandbox, which the sandbox dog replaced: remove the key and configure the dog in an [app.dogs.sandbox] table"
            ),
            Self::SandboxOnWindows { name } => write!(
                f,
                "sheep `{name}` sets a dogs.sandbox table, which Windows does not support: crabpot's Windows sandbox cannot start from session 0, where the shepherd runs"
            ),
            Self::InvalidListenName { name, listener } => write!(
                f,
                "sheep `{name}` has a listen name `{listener}` that is not made of ASCII letters, digits, `_` and `-`"
            ),
            Self::InvalidListenAddress {
                name,
                listener,
                address,
            } => write!(
                f,
                "sheep `{name}` has listen `{listener}` = \"{address}\", which is not an IP address and port like \"0.0.0.0:8080\" or \"[::1]:8080\""
            ),
            Self::DuplicateListenAddress {
                name,
                first,
                second,
                address,
            } => write!(
                f,
                "sheep `{name}` has listen `{first}` and `{second}` on the same address \"{address}\""
            ),
            Self::ListenWithSandbox { name } => write!(
                f,
                "sheep `{name}` sets listen and a dogs.sandbox table together, which is not supported: nothing yet shows crabpot passes the shepherd's sockets through to the sandboxed command"
            ),
            Self::ListenOnWindows { name } => write!(
                f,
                "sheep `{name}` sets listen, which Windows does not support: the sockets are passed to the sheep as file descriptors"
            ),
        }
    }
}

impl core::error::Error for NormalizeError {}
