use std::path::Path;

use globset::Glob;

use crate::config::{ProbeConfig, ProbeTarget};
use crate::values::UpDuration;

use super::NormalizeError;

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
pub(super) fn validate_template(
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
pub(super) fn validate_watch_globs(
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
pub(super) fn validate_probe(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::config::normalize::normalize;
    use crate::config::normalize::resolved_app::{MIN_LIVENESS_INTERVAL, MIN_READINESS_INTERVAL};

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
}
