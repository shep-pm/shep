//! `shep secret`: the CLI half of [`shep_core::secrets`].
//!
//! No [`Client`](shep_client::Client) anywhere in this module, for
//! [`crate::commands::kv`]'s reason: filling the store before the first
//! `shep start` is the ordinary first run, so it has to work with no
//! shepherd running.
//!
//! Reading a value back is gated on `[secrets] allow_read` in `shep.toml`.
//! Nothing else here is: `list` names keys and the environments each has a
//! value for, and `set`/`unset` report the slot they touched, so a value
//! reaches stdout down exactly one path.

use std::collections::BTreeMap;

use shep_core::config::DaemonConfig;
use shep_core::paths::ShepPaths;
use shep_core::secrets::{self, ALL_ENVIRONMENTS, Resolution, SecretError, SecretRef, SecretView};

use crate::cli::{SecretArgs, SecretCommand};
use crate::exit::ExitCode;
use crate::output::{SecretKeyRow, SecretKeyRows, SecretSlotRow, Streams, emit, write_outcome};

/// The one sentence that tells an operator how to open the read gate.
///
/// Names the file and the line to write, for
/// [`crate::whistle::gate::Control::how_to_open`]'s reason: an operator
/// told only that reading is off will guess, and the likeliest guess is a
/// flag that does not exist.
const HOW_TO_ALLOW_READ: &str = "printing a stored secret back is off; add `[secrets]` with \
     `allow_read = true` to $SHEP_HOME/shep.toml";

/// The exit code each [`SecretError`] maps to.
///
/// `InvalidKey`/`InvalidEnvironment`/`ValueTooLong` are `Usage`: the
/// operator typed it. `FutureVersion`/`Decode` are `InvalidConfig`: the file
/// on disk is the problem. `SecretError` is `#[non_exhaustive]`, so a future
/// variant falls through to [`ExitCode::Failure`].
fn exit_code_for(err: &SecretError) -> ExitCode {
    match err {
        SecretError::InvalidKey(_)
        | SecretError::InvalidEnvironment(_)
        | SecretError::ValueTooLong { .. } => ExitCode::Usage,
        SecretError::FutureVersion(_) | SecretError::Decode(_) => ExitCode::InvalidConfig,
        // `SecretError::Io` and any future variant both land here.
        _ => ExitCode::Failure,
    }
}

/// Renders `err` to `streams.err` and returns the code [`exit_code_for`]
/// maps it to.
///
/// No variant of [`SecretError`] carries a value, so this cannot print one.
fn fail(streams: &mut Streams<'_>, err: &SecretError) -> ExitCode {
    let code = exit_code_for(err);
    streams.fail(code, &err.to_string())
}

/// `shep secret`, dispatched to one of its four subcommands.
pub fn secret(streams: &mut Streams<'_>, paths: &ShepPaths, args: &SecretArgs) -> ExitCode {
    match &args.command {
        SecretCommand::Set { key, value, env } => set(streams, paths, key, env.as_deref(), value),
        SecretCommand::Get { key, env } => {
            let config = daemon_config(paths);
            get(
                streams,
                paths,
                key,
                env.as_deref(),
                config.secrets.allow_read,
                &config.daemon.environment,
            )
        }
        SecretCommand::Unset { key, env } => unset(streams, paths, key, env.as_deref()),
        SecretCommand::List => list(streams, paths),
    }
}

/// `shep.toml` as this verb reads it, or the defaults when there is no file
/// or it will not parse.
///
/// A broken file leaves the gate shut and the host environment at its
/// default, matching [`crate::whistle::gate::resolve_control`]: a config
/// nobody can parse is the worst moment for a gate to disappear. None of
/// `DaemonConfig`'s environment overrides touch either field, so the
/// closure is always `&|_| None`.
fn daemon_config(paths: &ShepPaths) -> DaemonConfig {
    let text = std::fs::read_to_string(&paths.daemon_config).ok();
    DaemonConfig::load(text.as_deref(), &|_| None).unwrap_or_default()
}

/// `shep secret set <key> <value> [--env <environment>]`.
///
/// No `--env` means [`ALL_ENVIRONMENTS`], the slot every environment falls
/// back to.
fn set(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    key: &str,
    environment: Option<&str>,
    value: &str,
) -> ExitCode {
    let environment = environment.unwrap_or(ALL_ENVIRONMENTS);
    match secrets::set(&paths.secrets, key, environment, value) {
        Ok(()) => emit_slot(streams, key, environment),
        Err(err) => fail(streams, &err),
    }
}

/// `shep secret get <key> [--env <environment>]`.
///
/// Writes the value and a newline to `streams.out` and nothing else, in
/// both formats, for [`crate::commands::schema`]'s reason: the whole point
/// is `DB_PASSWORD=$(shep secret get DB_PASSWORD)`, and an envelope would
/// make a one-value read need `jq`.
///
/// `--env` reads that slot exactly. Without it the value resolves the way a
/// spawn in `host_environment` would, so an operator checking a value sees
/// what the sheep would see.
///
/// Exits [`ExitCode::NotFound`] for a key with no value, writing nothing to
/// `streams.out`, so `shep secret get k || echo default` works in a script.
/// The gate is read before the store is, so a refusal cannot say whether
/// the key exists.
fn get(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    key: &str,
    environment: Option<&str>,
    allow_read: bool,
    host_environment: &str,
) -> ExitCode {
    if !allow_read {
        return streams.fail(ExitCode::InvalidConfig, HOW_TO_ALLOW_READ);
    }

    let found = match environment {
        Some(environment) => secrets::get(&paths.secrets, key, environment),
        None => resolve(paths, key, host_environment),
    };
    match found {
        Ok(Some(value)) => write_outcome(writeln!(streams.out, "{value}")),
        Ok(None) => {
            let message = match environment {
                Some(environment) => format!("`{key}` has no value for `{environment}`"),
                None => {
                    format!("`{key}` has no value for `{host_environment}` or `{ALL_ENVIRONMENTS}`")
                }
            };
            streams.fail(ExitCode::NotFound, &message)
        }
        Err(err) => fail(streams, &err),
    }
}

/// The value a sheep running in `host_environment` would resolve `key` to:
/// that environment's own slot, then [`ALL_ENVIRONMENTS`].
///
/// The operator's own store alone. A provider dog's namespace is not
/// reachable from here: those values live in the shepherd's memory, are not
/// an operator's to edit, and reading one through this verb would say they
/// are.
///
/// # Errors
/// [`SecretError::InvalidKey`] for a key outside the grammar, which
/// includes any `namespace/key` reference, plus `Io`, `Decode` and
/// `FutureVersion` exactly as [`secrets::all`] returns them.
fn resolve(
    paths: &ShepPaths,
    key: &str,
    host_environment: &str,
) -> Result<Option<String>, SecretError> {
    if !secrets::is_name(key) {
        return Err(SecretError::InvalidKey(key.to_string()));
    }
    let view = SecretView::new(
        host_environment.to_string(),
        secrets::all(&paths.secrets)?,
        BTreeMap::new(),
    );
    Ok(
        match view.resolve(&SecretRef {
            namespace: None,
            key,
        }) {
            Resolution::Found(value) => Some(value.to_string()),
            // A reference naming no namespace cannot miss one.
            Resolution::MissingKey | Resolution::MissingNamespace => None,
        },
    )
}

/// `shep secret unset <key> [--env <environment>]`.
///
/// Exits [`ExitCode::NotFound`] for a slot that held nothing, rather than
/// exiting 0 on a no-op an operator would read as success.
fn unset(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    key: &str,
    environment: Option<&str>,
) -> ExitCode {
    let environment = environment.unwrap_or(ALL_ENVIRONMENTS);
    match secrets::unset(&paths.secrets, key, environment) {
        Ok(true) => emit_slot(streams, key, environment),
        Ok(false) => {
            let message = format!("`{key}` has no value for `{environment}`");
            streams.fail(ExitCode::NotFound, &message)
        }
        Err(err) => fail(streams, &err),
    }
}

/// `shep secret list`: every key and the environments it has a value for,
/// in [`secrets::all`]'s `BTreeMap` order.
fn list(streams: &mut Streams<'_>, paths: &ShepPaths) -> ExitCode {
    match secrets::all(&paths.secrets) {
        Ok(entries) => {
            let rows = SecretKeyRows(
                entries
                    .into_iter()
                    .map(|(key, by_environment)| SecretKeyRow {
                        key,
                        environments: by_environment.into_keys().collect(),
                    })
                    .collect(),
            );
            write_outcome(emit(
                &mut *streams.out,
                streams.fmt,
                "secret",
                rows,
                streams.style,
            ))
        }
        Err(err) => fail(streams, &err),
    }
}

/// The one report `set` and `unset` share: which slot changed, never what
/// is in it.
fn emit_slot(streams: &mut Streams<'_>, key: &str, environment: &str) -> ExitCode {
    let row = SecretSlotRow {
        key: key.to_string(),
        environment: environment.to_string(),
    };
    write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        "secret",
        row,
        streams.style,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use shep_core::paths::ShepPaths;
    use shep_core::secrets::ALL_ENVIRONMENTS;

    use crate::cli::Format;
    use crate::exit::ExitCode;
    use crate::output::Streams;

    fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
        Streams {
            out,
            err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Json,
        }
    }

    /// `$SHEP_HOME` is `dir` itself, so `paths.secrets`' parent exists:
    /// `secrets::set` stages via `tempfile_in`, which creates no parent.
    fn paths_in(dir: &Path) -> ShepPaths {
        let home = dir.display().to_string();
        ShepPaths::resolve(&move |key| (key == "SHEP_HOME").then(|| home.clone()), dir)
    }

    /// `shep secret set`, its refusal's stderr as the `Err`.
    fn run_set(
        paths: &ShepPaths,
        key: &str,
        environment: Option<&str>,
        value: &str,
    ) -> Result<(), String> {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = set(
            &mut streams(&mut out, &mut err),
            paths,
            key,
            environment,
            value,
        );
        match code {
            ExitCode::Success => Ok(()),
            _ => Err(String::from_utf8(err).unwrap()),
        }
    }

    /// `shep secret list`'s stdout, its refusal's stderr as the `Err`.
    fn render_list(paths: &ShepPaths) -> Result<String, String> {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = list(&mut streams(&mut out, &mut err), paths);
        match code {
            ExitCode::Success => Ok(String::from_utf8(out).unwrap()),
            _ => Err(String::from_utf8(err).unwrap()),
        }
    }

    /// `shep secret get` against a store whose host environment is
    /// `production`: the code, then stdout, then stderr.
    fn run_get(
        paths: &ShepPaths,
        key: &str,
        environment: Option<&str>,
        allow_read: bool,
    ) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = get(
            &mut streams(&mut out, &mut err),
            paths,
            key,
            environment,
            allow_read,
            "production",
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    /// [`run_get`]'s stderr half.
    fn run_get_capturing(
        paths: &ShepPaths,
        key: &str,
        environment: Option<&str>,
        allow_read: bool,
    ) -> (ExitCode, String) {
        let (code, _, err) = run_get(paths, key, environment, allow_read);
        (code, err)
    }

    /// [`run_get`]'s stdout half.
    fn run_get_capturing_out(
        paths: &ShepPaths,
        key: &str,
        environment: Option<&str>,
        allow_read: bool,
    ) -> (ExitCode, String) {
        let (code, out, _) = run_get(paths, key, environment, allow_read);
        (code, out)
    }

    #[test]
    fn set_then_list_names_the_key_and_its_environments_but_no_value() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "DB_PASSWORD", None, "hunter2").unwrap();
        run_set(&paths, "DB_PASSWORD", Some("staging"), "staging-pw").unwrap();

        let listed = render_list(&paths).unwrap();
        assert!(listed.contains("DB_PASSWORD"));
        assert!(listed.contains("all"));
        assert!(listed.contains("staging"));
        assert!(!listed.contains("hunter2"), "a list never prints a value");
        assert!(!listed.contains("staging-pw"));
    }

    #[test]
    fn set_with_no_env_writes_the_all_slot() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();
        assert_eq!(
            shep_core::secrets::get(&paths.secrets, "K", ALL_ENVIRONMENTS)
                .unwrap()
                .as_deref(),
            Some("v")
        );
    }

    #[test]
    fn get_is_refused_unless_shep_toml_turns_it_on() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();

        let (code, err) = run_get_capturing(&paths, "K", None, /* allow_read */ false);
        assert_eq!(code, ExitCode::InvalidConfig);
        assert!(err.contains("allow_read"), "{err}");
        assert!(err.contains("[secrets]"), "{err}");
        assert!(
            !err.contains('v') || !err.contains("value is"),
            "no value in the refusal"
        );
    }

    #[test]
    fn get_prints_the_value_once_it_is_turned_on() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();
        let (code, out) = run_get_capturing_out(&paths, "K", None, /* allow_read */ true);
        assert_eq!(code, ExitCode::Success);
        assert_eq!(out.trim(), "v");
    }

    #[test]
    fn get_on_a_missing_key_exits_not_found_and_prints_nothing() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        let (code, out) = run_get_capturing_out(&paths, "ABSENT", None, true);
        assert_eq!(
            code,
            ExitCode::NotFound,
            "so `shep secret get k || default` works"
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_bad_key_exits_usage_and_a_future_store_exits_invalid_config() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        assert_eq!(
            exit_code_for(&SecretError::InvalidKey("x y".into())),
            ExitCode::Usage
        );
        assert_eq!(
            exit_code_for(&SecretError::FutureVersion(9)),
            ExitCode::InvalidConfig
        );
        let _ = paths;
    }

    #[test]
    fn the_store_works_with_no_shepherd_running() {
        // The whole reason this verb touches the file directly. If this test
        // ever needs a daemon, the design has been broken.
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();
        assert!(paths.secrets.exists());
    }
}
