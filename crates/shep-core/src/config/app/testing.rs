//! Fixtures and helpers shared by this module's tests.

use super::probe::{
    ProbeConfig, ProbeKind, default_failure_threshold, default_probe_interval,
    default_probe_timeout,
};
use super::schema::AppConfig;
// use schemars::generate
use crate::config::LevelRule;
use crate::config::{LineLevel, NormalizeError};
use crate::values::UpDuration;

/// Where a per-field `refuses` clause is enforced.
///
/// A panel naming a refusal nothing makes is the defect this table
/// exists to stop, so a claim is either exercised against `normalize`
/// or carries the reason it cannot be.
pub(super) enum Proof {
    /// `normalize` refuses this config, with an error this predicate
    /// accepts.
    Refused {
        /// A sheep whose only fault is the one the claim names.
        /// Boxed so this arm does not set the size of every row.
        value: Box<AppConfig>,
        /// The variant the refusal must arrive as.
        matches: fn(&NormalizeError) -> bool,
    },
    /// Enforced somewhere `normalize` cannot reach, named here so the
    /// gap stays a decision rather than an oversight.
    Elsewhere(&'static str),
}

/// One clause of one field's `refuses` list, with its proof.
pub(super) struct RefusalClaim {
    /// The Flockfile field carrying the clause.
    pub(super) field: &'static str,
    /// The clause, character for character as the schema writes it.
    pub(super) refusal: &'static str,
    /// What makes it true.
    pub(super) proof: Proof,
}

/// A claim `normalize` proves.
pub(super) fn refused(
    field: &'static str,
    refusal: &'static str,
    value: AppConfig,
    matches: fn(&NormalizeError) -> bool,
) -> RefusalClaim {
    RefusalClaim {
        field,
        refusal,
        proof: Proof::Refused {
            value: Box::new(value),
            matches,
        },
    }
}

/// A claim enforced past `normalize`, with `where` naming the enforcer.
pub(super) fn elsewhere(
    field: &'static str,
    refusal: &'static str,
    place: &'static str,
) -> RefusalClaim {
    RefusalClaim {
        field,
        refusal,
        proof: Proof::Elsewhere(place),
    }
}

/// A minimal sheep with one thing changed, so a row carries only the
/// value its own claim is about.
pub(super) fn sheep(edit: impl FnOnce(&mut AppConfig)) -> AppConfig {
    let mut app = AppConfig::minimal("web", "./srv");
    edit(&mut app);
    app
}

/// A probe of `kind` with everything else at its default, read from the
/// same functions serde fills a missing field from.
pub(super) fn probe(kind: ProbeKind, target: &str) -> ProbeConfig {
    ProbeConfig {
        kind,
        target: target.to_owned(),
        interval: default_probe_interval(),
        timeout: default_probe_timeout(),
        failure_threshold: default_failure_threshold(),
    }
}

/// Every `refuses` clause in the schema, paired with what enforces it.
///
/// Ordered by field, then as the field writes them. A clause naming
/// several spellings gets a row for each, since one of them passing
/// says nothing about the rest.
///
/// Refusals only. Most fields have nothing validating them, so proving
/// an `accepts` clause by handing `normalize` a value it never inspects
/// would pass whatever the clause said.
pub(super) fn refusal_claims() -> Vec<RefusalClaim> {
    vec![
        refused(
            "args",
            "an unclosed {{ token",
            sheep(|a| a.args = vec!["{{name".to_owned()]),
            |e| matches!(e, NormalizeError::BadTemplate { .. }),
        ),
        refused(
            "args",
            "a token shep does not define",
            sheep(|a| a.args = vec!["{{slot}}".to_owned()]),
            |e| matches!(e, NormalizeError::BadTemplate { .. }),
        ),
        refused(
            "cron_restart",
            "a field outside its valid range",
            sheep(|a| a.cron_restart = Some("99 * * * *".to_owned())),
            |e| matches!(e, NormalizeError::InvalidCron { .. }),
        ),
        refused(
            "cron_restart",
            "a sixth seconds field, or L, W, # or ?",
            sheep(|a| a.cron_restart = Some("0 0 * * * *".to_owned())),
            |e| matches!(e, NormalizeError::InvalidCron { .. }),
        ),
        refused(
            "cron_restart",
            "a sixth seconds field, or L, W, # or ?",
            sheep(|a| a.cron_restart = Some("0 0 L * *".to_owned())),
            |e| matches!(e, NormalizeError::InvalidCron { .. }),
        ),
        refused(
            "cron_restart",
            "a sixth seconds field, or L, W, # or ?",
            sheep(|a| a.cron_restart = Some("0 0 15W * *".to_owned())),
            |e| matches!(e, NormalizeError::InvalidCron { .. }),
        ),
        refused(
            "cron_restart",
            "a sixth seconds field, or L, W, # or ?",
            sheep(|a| a.cron_restart = Some("0 0 * * 1#2".to_owned())),
            |e| matches!(e, NormalizeError::InvalidCron { .. }),
        ),
        refused(
            "cron_restart",
            "a sixth seconds field, or L, W, # or ?",
            sheep(|a| a.cron_restart = Some("0 0 ? * *".to_owned())),
            |e| matches!(e, NormalizeError::InvalidCron { .. }),
        ),
        refused(
            "cron_restart",
            "a pattern croner cannot parse",
            sheep(|a| a.cron_restart = Some("every tuesday".to_owned())),
            |e| matches!(e, NormalizeError::InvalidCron { .. }),
        ),
        refused(
            "cron_timezone",
            "a name outside the IANA database",
            sheep(|a| a.cron_timezone = Some("Mars/Olympus".to_owned())),
            |e| matches!(e, NormalizeError::InvalidTimezone { .. }),
        ),
        refused(
            "depends_on",
            "this sheep's own name",
            sheep(|a| a.depends_on = vec!["web".to_owned()]),
            |e| matches!(e, NormalizeError::SelfDependency(_)),
        ),
        refused(
            "depends_on",
            "a name:slot instance reference",
            sheep(|a| a.depends_on = vec!["api:0".to_owned()]),
            |e| matches!(e, NormalizeError::InstanceDependency { .. }),
        ),
        elsewhere(
            "env",
            "a float, since 1.10 would arrive as 1.1",
            "EnvValue's Deserialize, before normalize sees the table",
        ),
        refused(
            "env",
            "a token shep does not define",
            sheep(|a| {
                a.env.insert("WORKER".to_owned(), "{{slot}}".to_owned());
            }),
            |e| matches!(e, NormalizeError::BadTemplate { .. }),
        ),
        refused(
            "env",
            "SHEP_INSTANCE, SHEP_NAME, or SHEP_ENVIRONMENT, which shep sets itself",
            sheep(|a| {
                a.env.insert("SHEP_INSTANCE".to_owned(), "0".to_owned());
            }),
            |e| matches!(e, NormalizeError::ReservedEnvVar { .. }),
        ),
        refused(
            "env",
            "SHEP_INSTANCE, SHEP_NAME, or SHEP_ENVIRONMENT, which shep sets itself",
            sheep(|a| {
                a.env.insert("SHEP_NAME".to_owned(), "web".to_owned());
            }),
            |e| matches!(e, NormalizeError::ReservedEnvVar { .. }),
        ),
        refused(
            "env",
            "SHEP_INSTANCE, SHEP_NAME, or SHEP_ENVIRONMENT, which shep sets itself",
            sheep(|a| {
                a.env
                    .insert("SHEP_ENVIRONMENT".to_owned(), "staging".to_owned());
            }),
            |e| matches!(e, NormalizeError::ReservedEnvVar { .. }),
        ),
        refused(
            "env",
            "an unclosed {{ token",
            sheep(|a| {
                a.env.insert("GREETING".to_owned(), "{{name".to_owned());
            }),
            |e| matches!(e, NormalizeError::BadTemplate { .. }),
        ),
        refused(
            "environment",
            "all, the store's every-environment slot",
            sheep(|a| a.environment = Some(crate::secrets::ALL_ENVIRONMENTS.to_owned())),
            |e| matches!(e, NormalizeError::InvalidEnvironment { .. }),
        ),
        refused(
            "environment",
            "a name outside letters, digits, dot, underscore, or dash",
            sheep(|a| a.environment = Some("staging!".to_owned())),
            |e| matches!(e, NormalizeError::InvalidEnvironment { .. }),
        ),
        refused(
            "err_file",
            "a {{secret:...}} token",
            sheep(|a| a.err_file = Some("/var/log/{{secret:tenant}}.log".to_owned())),
            |e| matches!(e, NormalizeError::SecretInLogPath { .. }),
        ),
        refused(
            "err_file",
            "one path for every instance, without merge_logs",
            sheep(|a| {
                a.instances = 2;
                a.err_file = Some("/var/log/web-err.log".to_owned());
            }),
            |e| matches!(e, NormalizeError::SharedLogPath { .. }),
        ),
        elsewhere(
            "group",
            "a name with no group entry",
            "shep-daemon's privilege::resolve, at spawn",
        ),
        elsewhere(
            "group",
            "another group, unless the shepherd runs as root",
            "shep-daemon's privilege::resolve, at spawn",
        ),
        refused(
            "ignore_watch",
            "a pattern globset cannot compile",
            sheep(|a| a.ignore_watch = vec!["[".to_owned()]),
            |e| matches!(e, NormalizeError::InvalidWatchGlob { .. }),
        ),
        refused(
            "kill_signal",
            "a signal outside that list",
            sheep(|a| a.kill_signal = Some("SIGKILL".to_owned())),
            |e| matches!(e, NormalizeError::InvalidKillSignal { .. }),
        ),
        refused(
            "level_rules",
            "an empty pattern, which would claim every line",
            sheep(|a| {
                a.level_rules = vec![LevelRule {
                    pattern: String::new(),
                    level: LineLevel::Error,
                }];
            }),
            |e| matches!(e, NormalizeError::InvalidLevelRule { .. }),
        ),
        refused(
            "level_rules",
            "a pattern regex cannot compile",
            sheep(|a| {
                a.level_rules = vec![LevelRule {
                    pattern: "[unterminated".to_owned(),
                    level: LineLevel::Error,
                }];
            }),
            |e| matches!(e, NormalizeError::InvalidLevelRule { .. }),
        ),
        refused(
            "liveness_probe",
            "a failure_threshold of 0",
            sheep(|a| {
                let mut p = probe(ProbeKind::Tcp, "127.0.0.1:8080");
                p.failure_threshold = 0;
                a.liveness_probe = Some(p);
            }),
            |e| matches!(e, NormalizeError::ZeroFailureThreshold { .. }),
        ),
        refused(
            "liveness_probe",
            "an interval below its own floor",
            sheep(|a| {
                let mut p = probe(ProbeKind::Tcp, "127.0.0.1:8080");
                p.interval = UpDuration::from_millis(500);
                a.liveness_probe = Some(p);
            }),
            |e| matches!(e, NormalizeError::IntervalBelowMinimum { .. }),
        ),
        refused(
            "name",
            "a path separator or a colon",
            sheep(|a| a.name = "web/api".to_owned()),
            |e| matches!(e, NormalizeError::InvalidName(_)),
        ),
        refused(
            "name",
            "a path separator or a colon",
            sheep(|a| a.name = r"web\api".to_owned()),
            |e| matches!(e, NormalizeError::InvalidName(_)),
        ),
        refused(
            "name",
            "a path separator or a colon",
            sheep(|a| a.name = "web:0".to_owned()),
            |e| matches!(e, NormalizeError::InvalidName(_)),
        ),
        refused(
            "name",
            "a bare . or ..",
            sheep(|a| a.name = ".".to_owned()),
            |e| matches!(e, NormalizeError::InvalidName(_)),
        ),
        refused(
            "name",
            "a bare . or ..",
            sheep(|a| a.name = "..".to_owned()),
            |e| matches!(e, NormalizeError::InvalidName(_)),
        ),
        refused(
            "out_file",
            "a {{secret:...}} token",
            sheep(|a| a.out_file = Some("/var/log/{{secret:tenant}}.log".to_owned())),
            |e| matches!(e, NormalizeError::SecretInLogPath { .. }),
        ),
        refused(
            "out_file",
            "one path for every instance, without merge_logs",
            sheep(|a| {
                a.instances = 2;
                a.out_file = Some("/var/log/web-out.log".to_owned());
            }),
            |e| matches!(e, NormalizeError::SharedLogPath { .. }),
        ),
        refused(
            "readiness_probe",
            "a failure_threshold of 0",
            sheep(|a| {
                let mut p = probe(ProbeKind::Tcp, "127.0.0.1:8080");
                p.failure_threshold = 0;
                a.readiness_probe = Some(p);
            }),
            |e| matches!(e, NormalizeError::ZeroFailureThreshold { .. }),
        ),
        refused(
            "readiness_probe",
            "an interval below its own floor",
            sheep(|a| {
                let mut p = probe(ProbeKind::Tcp, "127.0.0.1:8080");
                p.interval = UpDuration::from_millis(0);
                a.readiness_probe = Some(p);
            }),
            |e| matches!(e, NormalizeError::IntervalBelowMinimum { .. }),
        ),
        elsewhere(
            "user",
            "a name with no passwd entry",
            "shep-daemon's privilege::resolve, at spawn",
        ),
        elsewhere(
            "user",
            "another user, unless the shepherd runs as root",
            "shep-daemon's privilege::resolve, at spawn",
        ),
        refused(
            "watch_options",
            "a pattern globset cannot compile",
            sheep(|a| a.watch_options = vec!["[".to_owned()]),
            |e| matches!(e, NormalizeError::InvalidWatchGlob { .. }),
        ),
    ]
}
