//! `~/` expansion, and the walk over every `AppConfig` field that holds a path.

use core::fmt;

use std::path::Path;

use crate::config::AppConfig;

use super::NormalizeError;

/// Expands a leading `~/` against `home`, and refuses `~user/`.
///
/// `~/` only: `~user/...` needs a passwd lookup whose answer depends on who
/// the daemon runs as, and `$VAR` is never expanded here or anywhere. A
/// value with no leading `~` is returned unchanged. Thin wrapper over
/// [`expand_home_tilde`], attaching the sheep name and field
/// [`NormalizeError`] carries; `shep-cli`'s `shep adopt` calls
/// [`expand_home_tilde`] directly instead.
///
/// # Errors
/// - [`NormalizeError::TildeUser`] if the path names another user's home.
/// - [`NormalizeError::NoHomeForTilde`] if `~/` is used and `home` is `None`.
fn expand_tilde(
    value: &str,
    home: Option<&Path>,
    name: &str,
    field: &'static str,
) -> Result<String, NormalizeError> {
    expand_home_tilde(value, home).map_err(|err| match err {
        TildeError::OtherUser => NormalizeError::TildeUser {
            name: name.to_string(),
            field,
            value: value.to_string(),
        },
        TildeError::NoHome => NormalizeError::NoHomeForTilde {
            name: name.to_string(),
            field,
        },
    })
}

/// Why [`expand_home_tilde`] refused a value, with no per-field context
/// attached, for a caller that has none to give.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TildeError {
    /// The value names another user's home (`~user/...`). Refused rather
    /// than resolved: answering it means a passwd lookup, and under a
    /// systemd unit the answer depends on who the process runs as rather
    /// than on who wrote the value.
    OtherUser,
    /// The value begins `~/` and no home directory could be determined.
    NoHome,
}

impl fmt::Display for TildeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OtherUser => write!(
                f,
                "shep expands only `~/` (your own home); another user's home needs a \
                 passwd lookup whose answer depends on who the process runs as"
            ),
            Self::NoHome => write!(f, "begins with `~/` but no home directory could be found"),
        }
    }
}

impl core::error::Error for TildeError {}

/// Expands a leading `~/` in `value` against `home`
///
/// Accepts `~` alone or `~/...`. `~user/` is refused, since resolving it
/// takes a passwd lookup whose answer depends on who the process runs as.
/// `$VAR` is never expanded. A value with no leading `~` comes back
/// unchanged.
///
/// # Errors
/// - [`TildeError::OtherUser`] if the value names another user's home.
/// - [`TildeError::NoHome`] if `~/` is used and `home` is `None`.
pub fn expand_home_tilde(value: &str, home: Option<&Path>) -> Result<String, TildeError> {
    let Some(rest) = value.strip_prefix('~') else {
        return Ok(value.to_string());
    };
    // `~` alone, or `~/...`. Anything else after the tilde names a user.
    if !(rest.is_empty() || rest.starts_with('/')) {
        return Err(TildeError::OtherUser);
    }
    let Some(home) = home else {
        return Err(TildeError::NoHome);
    };
    // `join` would discard `home` for a rest that still looks absolute, so
    // the separator is trimmed and the two halves are concatenated instead.
    let joined = home.join(rest.trim_start_matches('/'));
    Ok(joined.to_string_lossy().into_owned())
}

/// Every field of an [`AppConfig`] that carries a filesystem path.
///
/// Named once, and walked by [`expand_paths`] and by its own test, so a
/// fifth path field added later fails that test until it is handled.
/// Expanding `~/` in some path fields and not others would be worse than
/// expanding in none: it teaches that tildes work and then fails somewhere
/// the operator has no reason to suspect.
#[cfg(test)]
const PATH_FIELDS: &[&str] = &["script", "cwd", "out_file", "err_file"];

/// Expands `~/` in every path field of `app`, in place.
///
/// # Errors
/// Whatever [`expand_tilde`] refuses, named with the field that carried it.
pub(super) fn expand_paths(app: &mut AppConfig, home: Option<&Path>) -> Result<(), NormalizeError> {
    let name = app.name.clone();
    app.script = expand_tilde(&app.script, home, &name, "script")?;
    for (field, slot) in [
        ("cwd", &mut app.cwd),
        ("out_file", &mut app.out_file),
        ("err_file", &mut app.err_file),
    ] {
        if let Some(value) = slot {
            *slot = Some(expand_tilde(value, home, &name, field)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::normalize::shep_home_fixture;
    use crate::config::normalize::{normalize, normalize_with_home};
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

    /// All four path fields expand `~/`, and expanding some but not others
    /// would be worse than expanding none: it teaches that tildes work and
    /// then fails where the operator has no reason to suspect it.
    #[test]
    fn every_path_field_expands_a_leading_tilde() {
        let home = Path::new("/home/ada");
        let mut app = AppConfig::minimal("web", "~/app/server.js");
        app.cwd = Some("~/app".to_string());
        app.out_file = Some("~/logs/out.log".to_string());
        app.err_file = Some("~/logs/err.log".to_string());

        let resolved = normalize_with_home(app, Some(home), Some(&home.join(".shep")))
            .expect("all four expand");
        let c = resolved.config();
        // Expectations are built with `join` rather than written as literals:
        // the separator is `/` here and `\` on Windows, and hardcoding one
        // turned CI's three Windows legs red when this test first landed.
        let expect = |rest: &str| home.join(rest).to_string_lossy().into_owned();
        assert_eq!(c.script, expect("app/server.js"));
        assert_eq!(c.cwd.as_deref(), Some(expect("app").as_str()));
        assert_eq!(c.out_file.as_deref(), Some(expect("logs/out.log").as_str()));
        assert_eq!(c.err_file.as_deref(), Some(expect("logs/err.log").as_str()));
    }

    /// The anti-drift half. A fifth path field added to `AppConfig` fails
    /// here until `expand_paths` handles it, which is the only thing keeping
    /// the "all four or none" rule true over time.
    #[test]
    fn the_path_field_list_matches_what_expand_paths_walks() {
        let home = Path::new("/home/ada");
        let mut app = AppConfig::minimal("web", "~/s");
        app.cwd = Some("~/c".to_string());
        app.out_file = Some("~/o".to_string());
        app.err_file = Some("~/e".to_string());

        let resolved =
            normalize_with_home(app, Some(home), Some(&home.join(".shep"))).expect("expands");
        let c = resolved.config();
        let expanded = [
            ("script", Some(c.script.as_str())),
            ("cwd", c.cwd.as_deref()),
            ("out_file", c.out_file.as_deref()),
            ("err_file", c.err_file.as_deref()),
        ];
        assert_eq!(
            expanded.len(),
            PATH_FIELDS.len(),
            "PATH_FIELDS and this test must name the same set"
        );
        for (field, value) in expanded {
            assert!(
                PATH_FIELDS.contains(&field),
                "`{field}` is not in PATH_FIELDS"
            );
            assert!(
                value.is_some_and(|v| v.starts_with("/home/ada")),
                "`{field}` was not expanded: {value:?}"
            );
        }
    }

    /// A path with no tilde is untouched, so this is a no-op for every
    /// absolute and relative path anyone already has.
    #[test]
    fn a_path_without_a_tilde_is_left_exactly_as_written() {
        let app = AppConfig::minimal("web", "./server.js");
        let resolved = normalize_with_home(app, Some(Path::new("/home/ada")), shep_home_fixture())
            .expect("no tilde, no change");
        assert_eq!(resolved.config().script, "./server.js");
    }

    /// `~user/` needs a passwd lookup whose answer depends on who the daemon
    /// runs as, so it is refused rather than guessed at.
    #[test]
    fn another_users_home_is_refused_rather_than_resolved() {
        let app = AppConfig::minimal("web", "~deploy/app/server.js");
        let err = normalize_with_home(app, Some(Path::new("/home/ada")), shep_home_fixture())
            .expect_err("~user/ must not resolve");
        assert!(
            matches!(err, NormalizeError::TildeUser { field, .. } if field == "script"),
            "the refusal names the field: {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("~/"),
            "and says what IS supported: {rendered}"
        );
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }

    /// `$VAR` is not expanded, here or anywhere. A config file that expands
    /// variables has to answer whose environment it means.
    #[test]
    fn a_dollar_variable_is_not_expanded() {
        let app = AppConfig::minimal("web", "$HOME/server.js");
        let resolved = normalize_with_home(app, Some(Path::new("/home/ada")), shep_home_fixture())
            .expect("left alone");
        assert_eq!(resolved.config().script, "$HOME/server.js");
    }

    /// `~/` with no home to expand against is an error naming the field
    /// rather than a path containing a literal tilde.
    #[test]
    fn a_tilde_with_no_home_is_an_error_not_a_literal_path() {
        let app = AppConfig::minimal("web", "~/server.js");
        let err = normalize_with_home(app, None, None).expect_err("nothing to expand against");
        assert!(
            matches!(err, NormalizeError::NoHomeForTilde { .. }),
            "{err:?}"
        );
    }

    /// Pins [`expand_home_tilde`]'s own contract, apart from
    /// [`expand_tilde`]'s wrapping into a [`NormalizeError`]: `shep-cli`'s
    /// `shep adopt` calls it directly, with no app name or field to attach.
    #[test]
    fn expand_home_tilde_covers_its_four_documented_cases() {
        let home = Path::new("/home/ada");
        assert_eq!(
            expand_home_tilde("~/bin/dog", Some(home)).unwrap(),
            home.join("bin/dog").to_string_lossy()
        );
        assert_eq!(
            expand_home_tilde("/opt/bin/dog", Some(home)).unwrap(),
            "/opt/bin/dog",
            "a value with no leading ~ is returned unchanged"
        );
        assert_eq!(
            expand_home_tilde("~/bin/dog", None).unwrap_err(),
            TildeError::NoHome
        );
        assert_eq!(
            expand_home_tilde("~deploy/bin/dog", Some(home)).unwrap_err(),
            TildeError::OtherUser
        );
    }

    /// fails if either remedy names a variable in a spelling the reader's
    /// own shell does not use. Both sentences tell an operator what to set,
    /// and `%SHEP_HOME%` is ordinary text to a unix shell exactly as
    /// `$SHEP_HOME` is to `cmd.exe`.
    #[test]
    fn the_two_home_remedies_name_the_variable_this_platform_spells() {
        let tilde = NormalizeError::NoHomeForTilde {
            name: "web".to_string(),
            field: "cwd",
        }
        .to_string();
        let templated = NormalizeError::NoShepHome {
            name: "web".to_string(),
            field: "out_file".to_string(),
        }
        .to_string();

        // Whole messages, spelled out rather than read back from the
        // constants under test: a fragment passes while the prose around
        // it regresses, and a constant agrees with itself.
        let (tilde_expected, templated_expected) = if cfg!(windows) {
            (
                "`web`: cwd begins with `~/` but no home directory could be found. \
                 Set %USERPROFILE%, or write the path out in full.",
                "`web`: out_file carries `{{SHEP_HOME}}` but no shep home could be \
                 found. Set %SHEP_HOME%, or write the path out in full.",
            )
        } else {
            (
                "`web`: cwd begins with `~/` but no home directory could be found. \
                 Set $HOME, or write the path out in full.",
                "`web`: out_file carries `{{SHEP_HOME}}` but no shep home could be \
                 found. Set $SHEP_HOME, or write the path out in full.",
            )
        };
        assert_eq!(tilde, tilde_expected);
        assert_eq!(templated, templated_expected);
    }
}
