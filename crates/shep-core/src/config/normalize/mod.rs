//! Validation and normalization: `AppConfig` -> `ResolvedApp`
//!
//! `ResolvedApp` is a proof token: constructing one is only possible through
//! [`normalize`], so daemon code can require it and skip re-validation.

use std::path::Path;

use std::collections::BTreeSet;

use globset::Glob;

use crate::config::{
    AppConfig, CronParseError, CronSchedule, KillSignal, LevelMatcher, ProbeConfig, ProbeTarget,
};
use crate::secrets;
use crate::values::UpDuration;

mod error;
mod tilde;

use tilde::expand_paths;

pub use error::NormalizeError;
pub use tilde::{TildeError, expand_home_tilde};

/// Shortest `interval` a `liveness_probe` may name.
///
/// The daemon's liveness loop floors whatever it is handed at this same
/// value, so a smaller number would be silently honoured as this one with
/// no warning.
///
/// One second is a floor no legitimate configuration wants to be under: for
/// [`ProbeKind::Exec`](crate::config::ProbeKind::Exec) a shorter interval is
/// that many process spawns per second, per sheep, for as long as it runs.
const MIN_LIVENESS_INTERVAL: UpDuration = UpDuration::from_millis(1_000);

/// Shortest `interval` a `readiness_probe` may name.
///
/// A whole second lower than [`MIN_LIVENESS_INTERVAL`]: the readiness wait
/// honours its `interval` exactly as written, bounded by `listen_timeout`,
/// so only zero needs rejecting. A fast app polling every 20ms to leave
/// `starting` sooner must not lose that.
const MIN_READINESS_INTERVAL: UpDuration = UpDuration::from_millis(1);

/// Longest `action_timeout` an app may name.
///
/// The daemon clamps every RPC deadline to `MAX_DEADLINE_MS` (60s, in
/// shep-daemon's `rpc` module), so a value at or above that line could
/// never be honoured by any caller. Set 2s under the clamp so the daemon
/// still has room to build the `TimedOut` row and send it back after the
/// wait gives up.
const MAX_ACTION_TIMEOUT: UpDuration = UpDuration::from_millis(58_000);

/// A validated app config: only obtainable via [`normalize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedApp {
    config: AppConfig,
}

impl ResolvedApp {
    /// Borrow the validated configuration
    #[must_use]
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Unwrap the validated configuration (consumes the proof token)
    #[must_use]
    pub fn into_config(self) -> AppConfig {
        self.config
    }
}

/// Validates one app config
///
/// # Errors
///
/// - [`NormalizeError::MissingName`]: `name` is empty.
/// - [`NormalizeError::InvalidName`]: `name` contains a path separator or a colon, or is `.`/`..`.
/// - [`NormalizeError::InvalidEnvironment`]: `environment` is `all`, the secrets store's every-environment slot, or falls outside the store's name grammar.
/// - [`NormalizeError::ReservedEnvVar`]: `env` sets `SHEP_INSTANCE`, `SHEP_NAME` or `SHEP_ENVIRONMENT`, which shep injects itself.
/// - [`NormalizeError::MissingScript`]: `script` is empty.
/// - [`NormalizeError::ZeroInstances`]: `instances == 0`.
/// - [`NormalizeError::InvalidCron`]: `cron_restart` is not valid in croner's dialect.
/// - [`NormalizeError::InvalidTimezone`]: `cron_timezone` is not a name in the IANA time-zone database.
/// - [`NormalizeError::InvalidProbe`]: a `readiness_probe`/`liveness_probe` target [`ProbeTarget::parse`] rejects.
/// - [`NormalizeError::ZeroFailureThreshold`]: a probe's `failure_threshold` is explicitly `0`.
/// - [`NormalizeError::IntervalBelowMinimum`]: a probe's `interval` is under the floor its own loop honours.
/// - [`NormalizeError::ZeroMaxMemory`]: `max_memory` is `0`.
/// - [`NormalizeError::ActionTimeoutTooLong`]: `action_timeout` is at or above the ceiling no RPC caller could wait past.
/// - [`NormalizeError::InvalidKillSignal`]: `kill_signal` names a signal the daemon's stop ladder cannot send.
/// - [`NormalizeError::WatchWithoutCwd`]: `watch` is `true` with no `cwd` set.
/// - [`NormalizeError::ZeroWatchDelay`]: `watch_delay` is `0`.
/// - [`NormalizeError::InvalidWatchGlob`]: a `watch_options` or `ignore_watch` pattern globset will not compile.
/// - [`NormalizeError::InvalidLevelRule`]: a `level_rules` entry has an empty pattern, one regex will not compile, or the list is longer than the ceiling on how many rules an app may declare.
/// - [`NormalizeError::BadTemplate`]: an `env`/`args`/log-path value carries an undefined or unclosed `{{...}}` token.
/// - [`NormalizeError::SecretInLogPath`]: `out_file` or `err_file` carries a `{{secret:...}}`.
/// - [`NormalizeError::SharedLogPath`]: `out_file` or `err_file` renders to the same path for two instances.
/// - [`NormalizeError::TildeUser`]: a path field names another user's `~user` home.
/// - [`NormalizeError::NoHomeForTilde`]: a path field expands `~/` but no home directory could be found.
/// - [`NormalizeError::NoShepHome`]: a templated value carries `{{SHEP_HOME}}` but no shep home could be found.
/// - [`NormalizeError::SelfDependency`]: an app names itself in `depends_on`.
/// - [`NormalizeError::InstanceDependency`]: a `depends_on` entry is written `name:slot`.
pub fn normalize(app: AppConfig) -> Result<ResolvedApp, NormalizeError> {
    let home = std::env::home_dir();
    let shep_home = crate::paths::shep_home(&|key| std::env::var(key).ok(), home.as_deref());
    normalize_with_home(app, home.as_deref(), shep_home.as_deref())
}

/// [`normalize`], with both home directories supplied rather than read.
///
/// Parameters so the `~/` expansion above is testable without mutating the
/// process environment, which is racy under a parallel `cargo test`. This is
/// also the seam that matters for correctness rather than only for tests:
/// the daemon may run as a different user than the CLI, so `~` has to be
/// resolved where the config is normalised, not where it is executed.
///
/// `shep_home` arrives the same way and for the same reason: it is the
/// directory `{{SHEP_HOME}}` expands to, and reading it inside the renderer
/// would resolve it wherever the value happened to be rendered.
///
/// # Errors
/// The same set [`normalize`] documents.
pub fn normalize_with_home(
    mut app: AppConfig,
    home: Option<&Path>,
    shep_home: Option<&Path>,
) -> Result<ResolvedApp, NormalizeError> {
    if app.name.is_empty() {
        return Err(NormalizeError::MissingName);
    }
    if app.name.contains(['/', '\\', ':']) || app.name == "." || app.name == ".." {
        return Err(NormalizeError::InvalidName(app.name));
    }
    if let Some(environment) = &app.environment
        && (environment == secrets::ALL_ENVIRONMENTS || !secrets::is_name(environment))
    {
        return Err(NormalizeError::InvalidEnvironment {
            name: app.name.clone(),
            value: environment.clone(),
        });
    }
    for var in ["SHEP_INSTANCE", "SHEP_NAME", "SHEP_ENVIRONMENT"] {
        if app.env.contains_key(var) {
            return Err(NormalizeError::ReservedEnvVar {
                name: app.name.clone(),
                var,
            });
        }
    }
    for (key, value) in &app.env {
        validate_template(&app.name, &format!("env.{key}"), value, shep_home)?;
    }
    for (index, value) in app.args.iter().enumerate() {
        validate_template(&app.name, &format!("args[{index}]"), value, shep_home)?;
    }
    for (field, value) in [("out_file", &app.out_file), ("err_file", &app.err_file)] {
        if let Some(value) = value {
            validate_template(&app.name, field, value, shep_home)?;
            if crate::config::template::holds_secret(value) {
                return Err(NormalizeError::SecretInLogPath {
                    name: app.name.clone(),
                    field,
                });
            }
        }
    }
    if app.script.is_empty() {
        return Err(NormalizeError::MissingScript);
    }
    // After the emptiness checks, so a missing script is reported as missing
    // rather than as a path problem, and before every check below that reads
    // a path.
    expand_paths(&mut app, home)?;
    if app.instances == 0 {
        return Err(NormalizeError::ZeroInstances);
    }
    // After the template validation above, so a malformed `out_file`/`err_file`
    // is reported as a bad template rather than as a shared path.
    if app.instances > 1 && !app.merge_logs {
        for (field, path) in [("out_file", &app.out_file), ("err_file", &app.err_file)] {
            // Rendered rather than searched for a substring: an escaped
            // `{{{{instance}}}}` contains the token's spelling but renders to
            // one literal path for every instance, which is exactly the
            // collision this refuses. Two slots that render alike collide.
            if let Some(path) = path
                && crate::config::template::render_positional(path, &app.name, 0, shep_home)
                    == crate::config::template::render_positional(path, &app.name, 1, shep_home)
            {
                return Err(NormalizeError::SharedLogPath {
                    name: app.name.clone(),
                    field,
                });
            }
        }
    }
    if let Some(pattern) = &app.cron_restart {
        CronSchedule::parse(pattern, app.cron_timezone.as_deref()).map_err(|e| match e {
            CronParseError::Pattern { pattern, reason } => {
                NormalizeError::InvalidCron { pattern, reason }
            }
            CronParseError::Timezone { name } => NormalizeError::InvalidTimezone { name },
        })?;
    } else if let Some(tz_name) = &app.cron_timezone {
        // A Flockfile can carry `cron_timezone` with no `cron_restart` to
        // pair it with, still a typo the user wants to hear about.
        crate::config::cron::parse_timezone_name(tz_name).ok_or_else(|| {
            NormalizeError::InvalidTimezone {
                name: tz_name.clone(),
            }
        })?;
    }
    validate_probe(
        app.readiness_probe.as_ref(),
        "readiness_probe",
        MIN_READINESS_INTERVAL,
    )?;
    validate_probe(
        app.liveness_probe.as_ref(),
        "liveness_probe",
        MIN_LIVENESS_INTERVAL,
    )?;
    if app.max_memory.is_some_and(|limit| limit.bytes() == 0) {
        // A ceiling every live process is already over: the enforcer would
        // report a breach on every reading, and the automatic restart that
        // follows resets the restart budget rather than spending it, so
        // `max_restarts` can never end the loop.
        return Err(NormalizeError::ZeroMaxMemory { name: app.name });
    }
    if let Some(name) = &app.kill_signal
        && KillSignal::parse(name).is_none()
    {
        // Rejected rather than clamped: a typo silently falling back to
        // SIGTERM would cost every stop and reload for the life of the
        // process, with no evidence but a detached daemon's log.
        return Err(NormalizeError::InvalidKillSignal {
            name: app.name,
            value: name.clone(),
        });
    }
    if app.action_timeout > MAX_ACTION_TIMEOUT {
        // Rejected rather than clamped: every value above the ceiling is
        // equally unreachable by any caller, so there is no honest lowered
        // value to silently fall back to.
        return Err(NormalizeError::ActionTimeoutTooLong {
            name: app.name,
            value: app.action_timeout,
            max: MAX_ACTION_TIMEOUT,
        });
    }
    if app.watch && app.cwd.is_none() {
        // `watch` asked for a feature the daemon has no directory to arm:
        // there is no cwd in the Flockfile, and defaulting to the daemon's
        // own cwd risks watching the whole filesystem under a systemd unit
        // with no `WorkingDirectory=`.
        return Err(NormalizeError::WatchWithoutCwd { name: app.name });
    }
    if app.watch_delay == Some(UpDuration::from_millis(0)) {
        // notify's debouncer derives its own poll tick as `watch_delay / 4`
        // on a dedicated OS thread, so a zero turns that thread into
        // `loop { sleep(0); lock(); }`, a CPU-spinning busy loop.
        return Err(NormalizeError::ZeroWatchDelay { name: app.name });
    }
    // Both lists are checked whether or not `watch` is on, so a typo'd
    // pattern is named now rather than the day `watch` flips on and nothing
    // happens.
    validate_watch_globs(&app.name, "watch_options", &app.watch_options)?;
    validate_watch_globs(&app.name, "ignore_watch", &app.ignore_watch)?;
    // Compiled to reject, and the result discarded: whichever client
    // classifies this sheep's lines compiles its own. A pattern refused
    // here can never reach one, which is the point of refusing it at config
    // time rather than at render time, where nobody would read the message.
    LevelMatcher::compile(&app.level_rules).map_err(|err| NormalizeError::InvalidLevelRule {
        name: app.name.clone(),
        reason: err.to_string(),
    })?;
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::with_capacity(app.depends_on.len());
    for target in &app.depends_on {
        if target == &app.name {
            return Err(NormalizeError::SelfDependency(app.name.clone()));
        }
        if target.contains(':') {
            return Err(NormalizeError::InstanceDependency {
                sheep: app.name.clone(),
                target: target.clone(),
            });
        }
        if seen.insert(target.clone()) {
            deduped.push(target.clone());
        }
    }
    app.depends_on = deduped;
    Ok(ResolvedApp { config: app })
}

/// Validates one template value, naming `field` in any rejection so the user
/// knows which entry to edit.
///
/// The `{{SHEP_HOME}}` check lives here rather than beside the log paths
/// because all four templated field kinds run through this one function, and
/// a value nothing can expand is as broken in an `env` entry as in a path.
///
/// # Errors
/// - [`NormalizeError::BadTemplate`] if `value` carries a `{{...}}` this
///   grammar does not define, or a `{{` this value never closes. Both of
///   [`crate::config::template::validate`]'s own rejections map here, so the
///   two are told apart by the rendered `reason` the variant carries rather
///   than by the variant.
/// - [`NormalizeError::NoShepHome`] if `value` carries a `{{SHEP_HOME}}` and
///   `shep_home` is `None`.
fn validate_template(
    name: &str,
    field: &str,
    value: &str,
    shep_home: Option<&Path>,
) -> Result<(), NormalizeError> {
    crate::config::template::validate(value).map_err(|reason| NormalizeError::BadTemplate {
        name: name.to_string(),
        field: field.to_string(),
        reason: reason.to_string(),
    })?;
    if shep_home.is_none() && crate::config::template::holds_shep_home(value) {
        return Err(NormalizeError::NoShepHome {
            name: name.to_string(),
            field: field.to_string(),
        });
    }
    Ok(())
}

/// Validates one of an app's two watch glob lists, rejecting any pattern
/// globset will not compile. `field` is the Flockfile field name
/// (`"watch_options"` or `"ignore_watch"`), carried into any error so the
/// user knows which list to edit. The compiled globs are discarded: this
/// function's job is rejection, and the daemon builds its own watch filter
/// when it arms the watch.
fn validate_watch_globs(
    name: &str,
    field: &'static str,
    patterns: &[String],
) -> Result<(), NormalizeError> {
    for pattern in patterns {
        Glob::new(pattern).map_err(|err| NormalizeError::InvalidWatchGlob {
            name: name.to_string(),
            field,
            pattern: pattern.clone(),
            reason: err.to_string(),
        })?;
    }
    Ok(())
}

/// Validates one probe's target, `failure_threshold` and `interval`, if the
/// probe is configured. `probe` is the Flockfile field name
/// (`"readiness_probe"` or `"liveness_probe"`), carried into any error so
/// the user knows which field to edit; `min_interval` is the floor that
/// probe's own loop in the daemon honours. Its own parsed [`ProbeTarget`] is
/// discarded: the daemon re-parses when it arms the probe.
fn validate_probe(
    probe: Option<&ProbeConfig>,
    name: &'static str,
    min_interval: UpDuration,
) -> Result<(), NormalizeError> {
    let Some(probe) = probe else {
        return Ok(());
    };
    ProbeTarget::parse(probe).map_err(|reason| NormalizeError::InvalidProbe {
        probe: name,
        reason: reason.to_string(),
    })?;
    if probe.failure_threshold == 0 {
        // Unhealthy before the first probe ever runs would make the liveness
        // loop restart the sheep immediately and forever.
        return Err(NormalizeError::ZeroFailureThreshold { probe: name });
    }
    if probe.interval < min_interval {
        // A zero interval would spin either probe loop as fast as
        // `ProbeKind::Exec` can spawn processes. A small but nonzero
        // liveness interval is refused too: `spawn_liveness_task` rounds
        // it up silently, leaving nothing to report the discrepancy.
        return Err(NormalizeError::IntervalBelowMinimum {
            probe: name,
            value: probe.interval,
            min: min_interval,
        });
    }
    Ok(())
}

/// Validates a whole flock, rejecting duplicate sheep names
///
/// # Errors
///
/// Everything [`normalize`] returns, plus
/// [`NormalizeError::DuplicateName`]: two apps share a `name`.
/// [`NormalizeError::DependencyCycle`]: two apps in the document depend on each other.
pub fn normalize_all(apps: Vec<AppConfig>) -> Result<Vec<ResolvedApp>, NormalizeError> {
    let mut seen = BTreeSet::new();
    let resolved: Vec<ResolvedApp> = apps
        .into_iter()
        .map(|app| {
            if !seen.insert(app.name.clone()) {
                return Err(NormalizeError::DuplicateName(app.name));
            }
            normalize(app)
        })
        .collect::<Result<_, _>>()?;
    // Document-local only: a name this document does not hold is left to the
    // daemon, which is the only place that knows the whole flock.
    let nodes: Vec<crate::config::graph::BootNode> = resolved
        .iter()
        .map(|app| crate::config::graph::BootNode {
            name: app.config().name.clone(),
            depends_on: app.config().depends_on.clone(),
            kind: crate::config::graph::NodeKind::Sheep,
        })
        .collect();
    if let Some(cycle) = crate::config::graph::plan(&nodes).cycles.into_iter().next() {
        return Err(NormalizeError::DependencyCycle(cycle));
    }
    Ok(resolved)
}

/// A shep home for the cases that never mention one, so a fixture
/// growing a `{{SHEP_HOME}}` later reads as the token rather than as a
/// refusal.
#[cfg(test)]
fn shep_home_fixture() -> Option<&'static Path> {
    Some(Path::new("/home/ada/.shep"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LevelRule, LevelRuleError, LineLevel};
    use crate::paths::SHEP_HOME_VAR;

    /// A resolved secret in a log path becomes a filename on disk and is
    /// reported by `shep flock` and `shep describe`, so it is refused at
    /// config time rather than rendered. Nothing pinned this before, in
    /// either field.
    #[test]
    fn a_secret_in_either_log_path_is_refused() {
        for (field, set) in [
            (
                "out_file",
                (|app: &mut AppConfig| {
                    app.out_file = Some("/var/log/{{secret:LOG_KEY}}.log".to_string());
                }) as fn(&mut AppConfig),
            ),
            ("err_file", |app| {
                app.err_file = Some("/var/log/{{secret:vercel/LOG_KEY}}.log".to_string());
            }),
        ] {
            let mut app = AppConfig::minimal("web", "/srv/server.js");
            set(&mut app);
            let err = normalize_with_home(app, Some(Path::new("/home/ada")), shep_home_fixture())
                .expect_err("a log path may not hold a secret");
            assert!(
                matches!(&err, NormalizeError::SecretInLogPath { field: got, .. } if *got == field),
                "{field}: {err:?}"
            );
            let rendered = err.to_string();
            assert!(rendered.contains(field), "names the field: {rendered}");
            assert!(
                !rendered.contains("LOG_KEY"),
                "and never echoes the key back: {rendered}"
            );
        }
    }

    /// The token is refused where nothing could expand it, rather than
    /// reaching a child as a filename with braces in it. Every templated
    /// field kind, since one shared validator sees all four.
    #[test]
    fn shep_home_with_no_shep_home_is_refused_in_every_templated_field() {
        for (field, mutate) in [
            (
                "out_file",
                (|app: &mut AppConfig| {
                    app.out_file = Some("{{SHEP_HOME}}/logs/out.log".to_string());
                }) as fn(&mut AppConfig),
            ),
            ("err_file", |app| {
                app.err_file = Some("{{SHEP_HOME}}/logs/err.log".to_string());
            }),
            ("env.DATA_DIR", |app| {
                app.env
                    .insert("DATA_DIR".to_string(), "{{SHEP_HOME}}/data".to_string());
            }),
            ("args[0]", |app| {
                app.args = vec!["--state={{SHEP_HOME}}/state".to_string()];
            }),
        ] {
            let mut app = AppConfig::minimal("web", "/srv/server.js");
            mutate(&mut app);
            let err = normalize_with_home(app, Some(Path::new("/home/ada")), None)
                .expect_err("nothing to expand it against");
            assert!(
                matches!(&err, NormalizeError::NoShepHome { field: got, .. } if got == field),
                "{field}: {err:?}"
            );
            let rendered = err.to_string();
            // The constant, not the literal: which spelling is right here
            // is platform-dependent, and has its own test.
            assert!(
                rendered.contains(SHEP_HOME_VAR),
                "names the fix: {rendered}"
            );
            assert!(
                !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
                "no em or en dash in copy a user reads: {rendered}"
            );
        }
    }

    #[test]
    fn shep_home_is_accepted_once_there_is_one() {
        let mut app = AppConfig::minimal("web", "/srv/server.js");
        app.out_file = Some("{{SHEP_HOME}}/logs/{{name}}-{{instance}}-out.log".to_string());
        app.env
            .insert("DATA_DIR".to_string(), "{{SHEP_HOME}}/data".to_string());
        app.args = vec!["--state={{SHEP_HOME}}/state".to_string()];
        let resolved = normalize_with_home(app, Some(Path::new("/home/ada")), shep_home_fixture())
            .expect("every field accepts it");
        // Stored as written: the token expands at spawn, per instance, not
        // here. Unlike `~`, which `expand_paths` resolves in place.
        assert_eq!(
            resolved.config().out_file.as_deref(),
            Some("{{SHEP_HOME}}/logs/{{name}}-{{instance}}-out.log")
        );
    }

    /// The collision check reads the rendered path, and `{{SHEP_HOME}}`
    /// renders alike for every slot, so a path carrying only that one still
    /// collides. Same rule `{{name}}` alone already falls under.
    #[test]
    fn a_shep_home_log_path_still_needs_an_instance_token() {
        let mut app = AppConfig::minimal("web", "/srv/server.js");
        app.instances = 2;
        app.out_file = Some("{{SHEP_HOME}}/logs/{{name}}-out.log".to_string());
        let err = normalize_with_home(app, Some(Path::new("/home/ada")), shep_home_fixture())
            .expect_err("both slots render one path");
        assert!(matches!(
            err,
            NormalizeError::SharedLogPath {
                field: "out_file",
                ..
            }
        ));

        let mut app = AppConfig::minimal("web", "/srv/server.js");
        app.instances = 2;
        app.out_file = Some("{{SHEP_HOME}}/logs/{{name}}-{{instance}}-out.log".to_string());
        assert!(
            normalize_with_home(app, Some(Path::new("/home/ada")), shep_home_fixture()).is_ok(),
            "the slot tells them apart"
        );
    }

    /// `reuse_port` loads because reload's overlap mode is chosen from it:
    /// refusing it would deny an operator the only way to ask for an
    /// overlapping reload.
    #[test]
    fn reuse_port_loads_now_that_reload_reads_it() {
        let mut app = AppConfig::minimal("web", "./server");
        app.reuse_port = true;

        let resolved = normalize(app).expect("reuse_port is implemented");
        assert!(resolved.config().reuse_port);
    }

    /// The default is off, so every Flockfile that does not mention it keeps
    /// loading.
    #[test]
    fn an_app_that_never_mentions_reuse_port_still_normalizes() {
        let resolved = normalize(AppConfig::minimal("web", "./server"))
            .expect("the common case must be untouched");
        assert!(!resolved.config().reuse_port);
    }
    use crate::config::AppConfig;

    #[test]
    fn a_colon_in_a_name_is_refused_because_it_is_the_instance_separator() {
        let err = normalize(AppConfig::minimal("web:2", "./srv")).unwrap_err();
        assert_eq!(err, NormalizeError::InvalidName("web:2".to_string()));

        let rendered = err.to_string();
        assert!(rendered.contains(':'), "says which character: {rendered}");
        // Spec D3 requires the error to name the character and suggest `-`.
        assert!(
            rendered.contains("`-`"),
            "suggests the stand-in: {rendered}"
        );
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );

        assert!(normalize(AppConfig::minimal("web-2", "./srv")).is_ok());
    }

    #[test]
    fn the_reserved_env_vars_are_refused_rather_than_overwritten() {
        for var in ["SHEP_INSTANCE", "SHEP_NAME", "SHEP_ENVIRONMENT"] {
            let mut app = AppConfig::minimal("web", "./srv");
            app.env.insert(var.to_string(), "mine".to_string());
            let err = normalize(app).unwrap_err();
            let rendered = err.to_string();
            assert!(rendered.contains(var), "names the variable: {rendered}");
            assert!(
                !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
                "no em or en dash in copy a user reads: {rendered}"
            );
        }
    }

    #[test]
    fn valid_minimal_config_normalizes() {
        let resolved = normalize(AppConfig::minimal("web", "./srv")).unwrap();
        assert_eq!(resolved.config().name, "web");
    }

    #[test]
    fn names_that_reach_the_filesystem_are_rejected() {
        // A name becomes a log/pid file stem via Path::join; a slash-prefixed
        // or dotdot name would escape $SHEP_HOME. Reject at the config boundary.
        for bad in ["/etc/passwd", "..", ".", "a/b", "a\\b"] {
            assert_eq!(
                normalize(AppConfig::minimal(bad, "./srv")).unwrap_err(),
                NormalizeError::InvalidName(bad.to_string())
            );
        }
        assert!(normalize(AppConfig::minimal("web-1", "./srv")).is_ok());
    }

    #[test]
    fn missing_name_and_script_are_distinct_errors() {
        assert_eq!(
            normalize(AppConfig::minimal("", "./srv")).unwrap_err(),
            NormalizeError::MissingName
        );
        assert_eq!(
            normalize(AppConfig::minimal("web", "")).unwrap_err(),
            NormalizeError::MissingScript
        );
    }

    #[test]
    fn zero_instances_rejected() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.instances = 0;
        assert_eq!(normalize(app).unwrap_err(), NormalizeError::ZeroInstances);
    }

    #[test]
    fn bad_cron_pattern_rejected_with_pattern_and_reason_carried_through() {
        // fails if the reason is not carried through from croner.
        let mut app = AppConfig::minimal("web", "./srv");
        app.cron_restart = Some("not a cron".to_string());
        match normalize(app).unwrap_err() {
            NormalizeError::InvalidCron { pattern, reason } => {
                assert_eq!(pattern, "not a cron");
                assert!(!reason.is_empty());
            }
            other => panic!("expected InvalidCron, got {other:?}"),
        }
    }

    #[test]
    fn five_tokens_of_garbage_cron_pattern_rejected() {
        // fails if the validator only counts whitespace-separated tokens
        // instead of checking each field's range: five numeric-looking
        // tokens, all out of range.
        let mut app = AppConfig::minimal("web", "./srv");
        app.cron_restart = Some("99 99 99 99 99".to_string());
        match normalize(app).unwrap_err() {
            NormalizeError::InvalidCron { pattern, .. } => {
                assert_eq!(pattern, "99 99 99 99 99");
            }
            other => panic!("expected InvalidCron, got {other:?}"),
        }
    }

    #[test]
    fn bad_cron_timezone_rejected_alongside_a_valid_cron_restart() {
        // fails if the `cron_restart` branch maps CronParseError::Timezone to
        // anything but NormalizeError::InvalidTimezone. CronSchedule::parse
        // resolves the zone before the pattern, so a valid pattern paired
        // with a bad zone is the only input that reaches that arm.
        let mut app = AppConfig::minimal("web", "./srv");
        app.cron_restart = Some("0 3 * * *".to_string());
        app.cron_timezone = Some("Mars/Olympus".to_string());
        match normalize(app).unwrap_err() {
            NormalizeError::InvalidTimezone { name } => assert_eq!(name, "Mars/Olympus"),
            other => panic!("expected InvalidTimezone, got {other:?}"),
        }
    }

    #[test]
    fn cron_timezone_validated_even_without_cron_restart() {
        // fails if timezone validation is skipped when there's no pattern to
        // pair it with: a Flockfile with only a bad `cron_timezone` is a
        // typo the user wants to hear about.
        let mut app = AppConfig::minimal("web", "./srv");
        app.cron_timezone = Some("Mars/Olympus".to_string());
        match normalize(app).unwrap_err() {
            NormalizeError::InvalidTimezone { name } => assert_eq!(name, "Mars/Olympus"),
            other => panic!("expected InvalidTimezone, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_names_rejected_across_a_flock() {
        let apps = vec![
            AppConfig::minimal("web", "./a"),
            AppConfig::minimal("web", "./b"),
        ];
        assert_eq!(
            normalize_all(apps).unwrap_err(),
            NormalizeError::DuplicateName("web".to_string())
        );
    }

    #[test]
    fn a_cycle_inside_one_document_is_refused_and_named() {
        // fails if normalize_all accepts a cycle, or reports it without a path
        let mut a = AppConfig::minimal("a", "./a");
        a.depends_on = vec!["b".to_string()];
        let mut b = AppConfig::minimal("b", "./b");
        b.depends_on = vec!["a".to_string()];
        match normalize_all(vec![a, b]) {
            Err(NormalizeError::DependencyCycle(cycle)) => {
                let rendered = NormalizeError::DependencyCycle(cycle).to_string();
                assert!(rendered.contains("a"), "{rendered}");
                assert!(rendered.contains("b"), "{rendered}");
                assert!(
                    rendered.contains(" -> "),
                    "the path must be shown: {rendered}"
                );
            }
            other => panic!("expected DependencyCycle, got {other:?}"),
        }
    }

    #[test]
    fn a_dependency_on_an_app_outside_the_document_is_accepted() {
        // fails if the document-local check refuses a cross-repository
        // dependency, which would make depends_on unusable across Flockfiles
        let mut api = AppConfig::minimal("api", "./api");
        api.depends_on = vec!["db-in-another-repo".to_string()];
        normalize_all(vec![api]).expect("an unresolved name is not a document error");
    }

    fn probe_config(target: &str) -> crate::config::ProbeConfig {
        crate::config::ProbeConfig {
            kind: crate::config::ProbeKind::Http,
            target: target.to_string(),
            interval: crate::values::UpDuration::from_millis(10_000),
            timeout: crate::values::UpDuration::from_millis(5_000),
            failure_threshold: 3,
        }
    }

    #[test]
    fn malformed_readiness_probe_target_rejected_naming_the_field() {
        // fails if validate_probe is never called for readiness_probe, or if
        // it drops which of the two probe fields the rejection came from
        let mut app = AppConfig::minimal("web", "./srv");
        app.readiness_probe = Some(probe_config("not-a-url"));
        match normalize(app).unwrap_err() {
            NormalizeError::InvalidProbe { probe, reason } => {
                assert_eq!(probe, "readiness_probe");
                assert!(!reason.is_empty());
            }
            other => panic!("expected InvalidProbe, got {other:?}"),
        }
    }

    #[test]
    fn malformed_liveness_probe_target_rejected_naming_the_field() {
        // fails if only readiness_probe is ever validated, leaving a bad
        // liveness_probe target to surface later at the daemon's first poll
        let mut app = AppConfig::minimal("web", "./srv");
        app.liveness_probe = Some(probe_config("not-a-url"));
        match normalize(app).unwrap_err() {
            NormalizeError::InvalidProbe { probe, .. } => assert_eq!(probe, "liveness_probe"),
            other => panic!("expected InvalidProbe, got {other:?}"),
        }
    }

    #[test]
    fn valid_probe_targets_accepted() {
        // fails if validate_probe rejects a well-formed target outright
        let mut app = AppConfig::minimal("web", "./srv");
        app.readiness_probe = Some(probe_config("http://127.0.0.1:8080/healthz"));
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn zero_failure_threshold_rejected() {
        // fails if failure_threshold is never inspected: a threshold of 0
        // means "unhealthy before the first probe ever runs"
        let mut app = AppConfig::minimal("web", "./srv");
        let mut probe = probe_config("http://127.0.0.1:8080/healthz");
        probe.failure_threshold = 0;
        app.readiness_probe = Some(probe);
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::ZeroFailureThreshold {
                probe: "readiness_probe"
            }
        );
        // fails if the message regresses to a bare variant name with no
        // explanation.
        assert!(err.to_string().contains("at least 1"), "{err}");
    }

    #[test]
    fn zero_interval_rejected() {
        // fails if interval is never inspected: a zero interval would spin
        // the readiness wait as fast as the runtime allows for the whole
        // `listen_timeout` (`await_ready` does not floor it)
        let mut app = AppConfig::minimal("web", "./srv");
        let mut probe = probe_config("http://127.0.0.1:8080/healthz");
        probe.interval = UpDuration::from_millis(0);
        app.readiness_probe = Some(probe);
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::IntervalBelowMinimum {
                probe: "readiness_probe",
                value: UpDuration::from_millis(0),
                min: MIN_READINESS_INTERVAL,
            }
        );
        // fails if the message regresses to a bare variant name with no
        // explanation.
        assert!(err.to_string().contains("must be at least"), "{err}");
    }

    #[test]
    fn a_liveness_interval_under_the_floor_is_rejected_rather_than_clamped() {
        // fails if the liveness check is `interval == 0` rather than a
        // floor: a 500ms interval survives equality and is then silently
        // rounded up to a full second by `MIN_PROBE_INTERVAL`. Also fails
        // if the rejection drops the value the user wrote.
        let mut app = AppConfig::minimal("web", "./srv");
        let mut probe = probe_config("http://127.0.0.1:8080/healthz");
        probe.interval = UpDuration::from_millis(500);
        app.liveness_probe = Some(probe);
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::IntervalBelowMinimum {
                probe: "liveness_probe",
                value: UpDuration::from_millis(500),
                min: MIN_LIVENESS_INTERVAL,
            }
        );
        assert!(err.to_string().contains("500"), "{err}");
    }

    #[test]
    fn a_liveness_interval_exactly_at_the_floor_is_accepted() {
        // fails if the comparison is `<=` rather than `<`: the floor is a
        // value the liveness loop honours exactly, so naming it must not be
        // an error.
        let mut app = AppConfig::minimal("web", "./srv");
        let mut probe = probe_config("http://127.0.0.1:8080/healthz");
        probe.interval = MIN_LIVENESS_INTERVAL;
        app.liveness_probe = Some(probe);
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn a_sub_second_readiness_interval_is_accepted() {
        // fails if both probes are validated against the liveness floor: a
        // readiness wait is bounded by `listen_timeout` and honours its
        // `interval` exactly as written, so a fast app polling every 50ms
        // to leave `starting` sooner must not be refused.
        let mut app = AppConfig::minimal("web", "./srv");
        let mut probe = probe_config("http://127.0.0.1:8080/healthz");
        probe.interval = UpDuration::from_millis(50);
        app.readiness_probe = Some(probe);
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn zero_max_memory_rejected() {
        // fails if `max_memory` is never inspected: zero is a ceiling every
        // live process is already over, so the enforcer breaches on every
        // reading and the automatic restart that follows resets the
        // restart budget instead of spending it.
        let mut app = AppConfig::minimal("web", "./srv");
        app.max_memory = Some(crate::values::MemSize::from_bytes(0));
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::ZeroMaxMemory {
                name: "web".to_string()
            }
        );
        // fails if the message regresses to a bare variant name with no
        // explanation.
        assert!(err.to_string().contains("max_memory"), "{err}");
    }

    #[test]
    fn a_nonzero_max_memory_is_accepted() {
        // fails if the check fires on `max_memory` being set at all rather
        // than on its being zero: that would refuse every app that
        // configures a limit, which is the whole feature
        let mut app = AppConfig::minimal("web", "./srv");
        app.max_memory = Some("512M".parse().unwrap());
        assert!(normalize(app).is_ok());
    }

    /// fails if a `kill_signal` shep cannot send is accepted here: that puts
    /// SIGTERM on the wire for the life of the process with nothing but one
    /// daemon log line to say so.
    #[test]
    fn a_kill_signal_shep_cannot_send_is_refused_by_name() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.kill_signal = Some("SIGUSR1".to_string());

        let err = normalize(app).unwrap_err();

        assert_eq!(
            err,
            NormalizeError::InvalidKillSignal {
                name: "web".to_string(),
                value: "SIGUSR1".to_string(),
            }
        );
        // The message has to name the accepted set, because the operator's next
        // move is picking a different word and there is nowhere else to look.
        let rendered = err.to_string();
        assert!(rendered.contains("SIGUSR1"), "{rendered}");
        assert!(rendered.contains("SIGTERM"), "{rendered}");
        assert!(rendered.contains("SIGUSR2"), "{rendered}");
    }

    /// fails if the four supported names, their bare forms, or a lowercase
    /// spelling stop being accepted. This is the compatibility half: every
    /// spelling `stop_signal` accepted before this task must still normalize.
    #[test]
    fn every_spelling_the_daemon_already_accepted_still_normalizes() {
        for name in [
            "SIGTERM", "TERM", "sigterm", "term", "SIGINT", "INT", "SIGQUIT", "QUIT", "SIGUSR2",
            "USR2", "sigusr2",
        ] {
            let mut app = AppConfig::minimal("web", "./srv");
            app.kill_signal = Some(name.to_string());
            assert!(
                normalize(app).is_ok(),
                "`{name}` was accepted before this task and must still be"
            );
        }
    }

    /// fails if an unset `kill_signal` is refused: the overwhelmingly common
    /// case, and the one a validation pass is most likely to break by
    /// treating `None` as an empty string.
    #[test]
    fn an_unset_kill_signal_is_not_a_config_error() {
        let app = AppConfig::minimal("web", "./srv");
        assert!(app.kill_signal.is_none());
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn a_sheep_cannot_claim_the_all_environment() {
        // `all` is the store's every-environment slot. A sheep resolving in
        // it would read that slot twice and never its own.
        let mut app = AppConfig::minimal("web", "./srv");
        app.environment = Some("all".to_string());
        let err = normalize(app).unwrap_err();
        assert!(err.to_string().contains("all"), "{err}");
    }

    #[test]
    fn an_environment_outside_the_grammar_is_refused() {
        for bad in ["", "has space", "has/slash"] {
            let mut app = AppConfig::minimal("web", "./srv");
            app.environment = Some(bad.to_string());
            assert!(normalize(app).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn action_timeout_past_the_ceiling_is_rejected() {
        // fails if `action_timeout` is never inspected. One millisecond over
        // the ceiling is deliberate: a test at a round number like 60s could
        // pass by coincidence if the check used the wrong constant entirely
        // (`MAX_DEADLINE_MS` itself, say, instead of the margin under it).
        let mut app = AppConfig::minimal("web", "./srv");
        app.action_timeout = UpDuration::from_millis(MAX_ACTION_TIMEOUT.as_millis() + 1);
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::ActionTimeoutTooLong {
                name: "web".to_string(),
                value: UpDuration::from_millis(MAX_ACTION_TIMEOUT.as_millis() + 1),
                max: MAX_ACTION_TIMEOUT,
            }
        );
        // fails if the message regresses to a bare variant name with no
        // explanation.
        assert!(err.to_string().contains("action_timeout"), "{err}");
    }

    #[test]
    fn action_timeout_at_the_ceiling_is_accepted() {
        // fails if the comparison is `>=` rather than `>`: the ceiling
        // itself still leaves the daemon its full margin under the hard
        // clamp, so it is not one of the values nothing could ever satisfy.
        let mut app = AppConfig::minimal("web", "./srv");
        app.action_timeout = MAX_ACTION_TIMEOUT;
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn the_default_action_timeout_is_accepted() {
        // fails if `AppConfig::default()`'s own value ever drifts past the
        // ceiling normalize enforces: the one combination that must never
        // reject the config nobody customized.
        assert!(normalize(AppConfig::minimal("web", "./srv")).is_ok());
    }

    #[test]
    fn zero_watch_delay_rejected() {
        // fails if `watch_delay` is never inspected. notify's debouncer
        // derives its poll tick as `watch_delay / 4` and sleeps it on its own
        // OS thread, so zero is `loop { sleep(0); lock(); }`, a CPU-spinning
        // busy loop.
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch = true;
        app.cwd = Some("/srv/web".to_string());
        app.watch_delay = Some(UpDuration::from_millis(0));
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::ZeroWatchDelay {
                name: "web".to_string()
            }
        );
        // fails if the message regresses to a bare variant name with no
        // explanation.
        assert!(err.to_string().contains("watch_delay"), "{err}");
    }

    #[test]
    fn a_zero_watch_delay_is_rejected_with_watch_off() {
        // fails if the check is nested inside the `watch` block: an app
        // carrying `watch_delay = "0"` with `watch = false` would normalize
        // clean, and the spin would arrive the day someone flips `watch =
        // true`
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch_delay = Some(UpDuration::from_millis(0));
        assert!(matches!(
            normalize(app).unwrap_err(),
            NormalizeError::ZeroWatchDelay { .. }
        ));
    }

    #[test]
    fn a_nonzero_watch_delay_is_accepted() {
        // fails if the check fires on `watch_delay` being set at all rather
        // than on its being zero: that would refuse every app that tunes
        // its own debounce
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch = true;
        app.cwd = Some("/srv/web".to_string());
        app.watch_delay = Some(UpDuration::from_millis(1));
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn default_failure_threshold_from_toml_accepted() {
        // fails if the check fires on the ordinary default instead of only
        // an explicit 0. Deserializes a Flockfile snippet that omits
        // `failure_threshold`, exercising the real serde default rather
        // than `probe_config`'s hardcoded `3`.
        let src = r#"
name = "web"
script = "./srv"

[readiness_probe]
kind = "http"
target = "http://127.0.0.1:8080/healthz"
"#;
        let app: AppConfig = toml::from_str(src).unwrap();
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn watch_true_without_cwd_rejected_naming_the_app() {
        // fails if a validator never looks at `watch`, or looks at it but
        // carries no app name, leaving the user unable to tell which
        // Flockfile entry to edit
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch = true;
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::WatchWithoutCwd {
                name: "web".to_string()
            }
        );
        // fails if the message regresses to a bare variant name with no
        // explanation.
        assert!(err.to_string().contains("no cwd to watch"), "{err}");
    }

    #[test]
    fn watch_true_with_cwd_accepted() {
        // fails if the check fires on `watch` alone, ignoring that a cwd was
        // actually provided
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch = true;
        app.cwd = Some("/srv/web".to_string());
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn level_rules_survive_normalization_in_the_order_they_were_written() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.level_rules = vec![
            LevelRule {
                pattern: r"\[ERROR\]".to_string(),
                level: LineLevel::Error,
            },
            LevelRule {
                pattern: r"\[WARN\]".to_string(),
                level: LineLevel::Warn,
            },
        ];
        let resolved = normalize(app.clone()).unwrap();
        assert_eq!(resolved.config().level_rules, app.level_rules);
    }

    /// The whole reason patterns are compiled here: a Flockfile that reaches
    /// the shepherd with a pattern nobody can compile would fail at draw
    /// time, in a client, where the message reaches nobody.
    #[test]
    fn a_level_rule_pattern_that_will_not_compile_is_rejected() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.level_rules = vec![LevelRule {
            pattern: "[unclosed".to_string(),
            level: LineLevel::Error,
        }];
        let err = normalize(app).unwrap_err();
        let rendered = err.to_string();
        for expected in ["web", "[unclosed"] {
            assert!(
                rendered.contains(expected),
                "{expected} missing: {rendered}"
            );
        }
    }

    /// An empty pattern matches every line, so the rule would claim the feed
    /// and leave every rule after it dead.
    #[test]
    fn an_empty_level_rule_pattern_is_rejected() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.level_rules = vec![LevelRule {
            pattern: String::new(),
            level: LineLevel::Warn,
        }];
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::InvalidLevelRule {
                name: "web".to_string(),
                reason: LevelRuleError::EmptyPattern {
                    level: LineLevel::Warn
                }
                .to_string(),
            }
        );
    }

    #[test]
    fn a_watch_options_glob_that_will_not_compile_is_rejected() {
        // fails if `watch_options` patterns are never compiled at config
        // time. Also fails if the rejection blames the whole list instead
        // of the one bad pattern: the valid `src/**` comes first, so
        // naming it, or the patterns joined together, is wrong.
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch = true;
        app.cwd = Some("/srv/web".to_string());
        app.watch_options = vec!["src/**".to_string(), "[".to_string()];
        let err = normalize(app).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::InvalidWatchGlob {
                name: "web".to_string(),
                field: "watch_options",
                pattern: "[".to_string(),
                reason: Glob::new("[").unwrap_err().to_string(),
            }
        );
        // fails if the message drops the app name, the list or the pattern:
        // the three things that name the Flockfile line to edit.
        let rendered = err.to_string();
        for expected in ["web", "watch_options", "`[`"] {
            assert!(
                rendered.contains(expected),
                "{expected} missing: {rendered}"
            );
        }
    }

    #[test]
    fn an_ignore_watch_glob_that_will_not_compile_is_rejected() {
        // fails if only `watch_options` is ever compiled, leaving a mistyped
        // `ignore_watch` to cost the app its watch at arm time instead
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch = true;
        app.cwd = Some("/srv/web".to_string());
        app.ignore_watch = vec!["[".to_string()];
        match normalize(app).unwrap_err() {
            NormalizeError::InvalidWatchGlob { field, pattern, .. } => {
                assert_eq!(field, "ignore_watch");
                assert_eq!(pattern, "[");
            }
            other => panic!("expected InvalidWatchGlob, got {other:?}"),
        }
    }

    #[test]
    fn a_glob_that_will_not_compile_is_rejected_with_watch_off() {
        // fails if glob validation is nested inside the `watch` check: an app
        // carrying a mistyped glob with `watch = false` would then normalize
        // clean, and the typo would surface only the day someone flips
        // `watch = true`
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch_options = vec!["[".to_string()];
        assert!(matches!(
            normalize(app).unwrap_err(),
            NormalizeError::InvalidWatchGlob { .. }
        ));
    }

    #[test]
    fn well_formed_watch_globs_are_accepted() {
        // fails if the check rejects patterns globset compiles happily:
        // recursive `**`, a character class, a negated class and a brace
        // alternation. Also fails if it is wired to a parser that is not
        // globset's, since these are a syntax error to a regex engine.
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch = true;
        app.cwd = Some("/srv/web".to_string());
        app.watch_options = vec!["src/**/*.rs".to_string(), "*.[ch]".to_string()];
        app.ignore_watch = vec!["target/**".to_string(), "**/[!.]*.{tmp,swp}".to_string()];
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn a_typo_in_an_env_template_is_refused_and_names_the_field() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.env
            .insert("WORKER".to_string(), "w-{{instnace}}".to_string());
        let err = normalize(app).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("instnace"), "names the typo: {rendered}");
        assert!(rendered.contains("WORKER"), "and the field: {rendered}");
    }

    #[test]
    fn a_typo_in_an_arg_template_is_refused_too() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.args = vec!["--port".to_string(), "91{{slot}}".to_string()];
        let err = normalize(app).unwrap_err();
        assert!(err.to_string().contains("slot"), "{err}");
    }

    #[test]
    fn an_explicit_log_path_shared_by_every_instance_is_refused() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.instances = 3;
        app.out_file = Some("/var/log/web.log".to_string());
        let err = normalize(app).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("out_file"), "names the field: {rendered}");
        assert!(
            rendered.contains("{{instance}}") && rendered.contains("merge_logs"),
            "and both ways out: {rendered}"
        );
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }

    #[test]
    fn the_three_ways_out_of_the_shared_log_refusal_all_work() {
        // A slot in the path.
        let mut templated = AppConfig::minimal("web", "./srv");
        templated.instances = 3;
        templated.out_file = Some("/var/log/web-{{instance}}.log".to_string());
        assert!(normalize(templated).is_ok());

        // Asking for the merge on purpose.
        let mut merged = AppConfig::minimal("web", "./srv");
        merged.instances = 3;
        merged.out_file = Some("/var/log/web.log".to_string());
        merged.merge_logs = true;
        assert!(normalize(merged).is_ok());

        // One instance cannot collide with itself.
        let mut single = AppConfig::minimal("web", "./srv");
        single.out_file = Some("/var/log/web.log".to_string());
        assert!(normalize(single).is_ok());
    }

    #[test]
    fn an_escaped_template_in_a_log_path_does_not_satisfy_the_refusal() {
        // `{{{{instance}}}}` spells the token but renders to one literal path
        // for every instance, so a substring check would wave it through.
        let mut app = AppConfig::minimal("web", "./srv");
        app.instances = 3;
        app.out_file = Some("/var/log/web-{{{{instance}}}}.log".to_string());
        assert!(normalize(app).is_err());
    }

    #[test]
    fn a_name_only_template_does_not_resolve_the_collision() {
        // `{{name}}` is the same for every instance, so a path carrying only it
        // still puts every instance on one file. Presence of a token is not the
        // test; rendering differently is.
        let mut app = AppConfig::minimal("web", "./srv");
        app.instances = 3;
        app.out_file = Some("/var/log/{{name}}.log".to_string());
        assert!(normalize(app).is_err());
    }

    /// A resolved `env` value reaches the child and nothing else. A
    /// resolved log path reaches `ProcessInfo`, every bus event carrying
    /// one, `shep flock`, `shep describe`, `shep lookout` and a filename
    /// under `$SHEP_HOME/logs`, which is four more places than the design
    /// says a plaintext secret ever lives.
    #[test]
    fn a_secret_in_a_log_path_is_refused() {
        for field in ["out_file", "err_file"] {
            let mut app = AppConfig::minimal("web", "./srv");
            let path = Some("/var/log/{{secret:TOKEN}}.log".to_string());
            if field == "out_file" {
                app.out_file = path;
            } else {
                app.err_file = path;
            }
            match normalize(app).unwrap_err() {
                NormalizeError::SecretInLogPath { name, field: got } => {
                    assert_eq!(name, "web");
                    assert_eq!(got, field);
                }
                other => panic!("expected SecretInLogPath for {field}, got {other:?}"),
            }
        }
    }

    /// The refusal is the `secret:` prefix alone: a multi-instance app
    /// needs `{{instance}}` in its log paths, and `{{name}}` is how the
    /// other two path fields are written.
    #[test]
    fn the_positional_tokens_still_render_in_a_log_path() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.instances = 2;
        app.out_file = Some("/var/log/{{name}}-{{instance}}.out".to_string());
        app.err_file = Some("/var/log/{{name}}-{{instance}}.err".to_string());
        assert!(normalize(app).is_ok());
    }

    /// A namespaced reference is the same refusal: `SecretRef::parse` reads
    /// both shapes, and only one of them being refused would be a hole
    /// nobody could guess at.
    #[test]
    fn a_namespaced_secret_in_a_log_path_is_refused_too() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.err_file = Some("/var/log/{{secret:vercel/TOKEN}}.log".to_string());
        assert!(matches!(
            normalize(app).unwrap_err(),
            NormalizeError::SecretInLogPath { .. }
        ));
    }

    #[test]
    fn a_bad_template_in_a_log_path_is_reported_as_bad_template_not_shared_path() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.instances = 3;
        app.out_file = Some("/var/log/web-{{instnace}}.log".to_string());
        match normalize(app).unwrap_err() {
            NormalizeError::BadTemplate { field, reason, .. } => {
                assert_eq!(field, "out_file");
                assert!(reason.contains("instnace"), "{reason}");
            }
            other => panic!("expected BadTemplate, got {other:?}"),
        }
    }

    #[test]
    fn watch_options_without_watch_or_cwd_accepted() {
        // fails if the check is keyed on `watch_options` being non-empty
        // rather than on `watch` being true: that would reject a Flockfile
        // that never asked to be watched
        let mut app = AppConfig::minimal("web", "./srv");
        app.watch_options = vec!["src/**".to_string()];
        assert!(normalize(app).is_ok());
    }

    #[test]
    fn a_sheep_that_depends_on_itself_is_refused_by_name() {
        // fails if the self-edge check is missing, or if it reports a bare
        // cycle rather than naming the sheep
        let mut app = AppConfig::minimal("api", "./api");
        app.depends_on = vec!["api".to_string()];
        match normalize(app) {
            Err(NormalizeError::SelfDependency(name)) => assert_eq!(name, "api"),
            other => panic!("expected SelfDependency, got {other:?}"),
        }
    }

    #[test]
    fn a_dependency_on_one_instance_is_refused_naming_the_app_form() {
        // fails if `name:slot` is accepted as a dependency target
        let mut app = AppConfig::minimal("api", "./api");
        app.depends_on = vec!["db:2".to_string()];
        match normalize(app) {
            Err(NormalizeError::InstanceDependency { sheep, target }) => {
                assert_eq!(sheep, "api");
                assert_eq!(target, "db:2");
            }
            other => panic!("expected InstanceDependency, got {other:?}"),
        }
        let rendered = NormalizeError::InstanceDependency {
            sheep: "api".to_string(),
            target: "db:2".to_string(),
        }
        .to_string();
        assert!(
            rendered.contains("`db`"),
            "the refusal must name the app-level form: {rendered}"
        );
    }

    #[test]
    fn duplicate_dependencies_dedupe_rather_than_refusing() {
        // fails if a repeated name is an error, or if it survives into the
        // normalized config twice
        let mut app = AppConfig::minimal("api", "./api");
        app.depends_on = vec!["db".to_string(), "db".to_string()];
        let resolved = normalize(app).expect("a repeated name is not an error");
        assert_eq!(resolved.config().depends_on, vec!["db".to_string()]);
    }

    #[test]
    fn an_ordinary_dependency_list_survives_normalize() {
        // fails if the field is dropped or reordered
        let mut app = AppConfig::minimal("api", "./api");
        app.depends_on = vec!["db".to_string(), "cache".to_string()];
        let resolved = normalize(app).expect("an ordinary list normalizes");
        assert_eq!(
            resolved.config().depends_on,
            vec!["db".to_string(), "cache".to_string()]
        );
    }
}
