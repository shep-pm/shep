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
//! reaches stdout down exactly one path, in `get`, whichever format is
//! asked for.

use std::collections::BTreeMap;
use std::io::Read;

use shep_core::config::DaemonConfig;
use shep_core::paths::ShepPaths;
use shep_core::secrets::{self, ALL_ENVIRONMENTS, Resolution, SecretError, SecretRef, SecretView};

use crate::cli::{Format, SecretArgs, SecretCommand};
use crate::exit::ExitCode;
use crate::output::{
    SecretKeyRow, SecretKeyRows, SecretSlotRow, SecretValueRow, Streams, emit, write_outcome,
};

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
        SecretCommand::Set {
            key,
            value,
            env,
            stdin,
        } => {
            let value = if *stdin {
                match resolve_stdin_value(&mut std::io::stdin().lock()) {
                    Ok(value) => value,
                    Err(message) => return streams.fail(ExitCode::Failure, &message),
                }
            } else {
                // clap's `required_unless_present = "stdin"` guarantees this.
                value
                    .clone()
                    .expect("clap requires a value unless --stdin is set")
            };
            set(streams, paths, key, env.as_deref(), &value)
        }
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

/// `--stdin`'s value: `reader`'s bytes, with at most one trailing `\n`
/// stripped, and one `\r` immediately before it if present, then decoded as
/// UTF-8.
///
/// Only that one newline is trimmed. Leading and interior whitespace can be
/// part of a credential, so `echo "$PW" | shep secret set KEY --stdin` and
/// `printf %s "$PW" | shep secret set KEY --stdin` both store exactly what
/// was piped, and nothing wider is touched.
///
/// `reader` rather than reading `std::io::stdin()` directly keeps this
/// testable without touching the test process's real stdin; [`secret`]
/// passes the real one.
///
/// # Errors
/// The read failed, or the trimmed bytes are not valid UTF-8.
fn resolve_stdin_value(reader: &mut dyn Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|err| format!("could not read the value from stdin: {err}"))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    String::from_utf8(bytes)
        .map_err(|_utf8_error| "the value read from stdin is not valid UTF-8".to_string())
}

/// `shep secret set <key> (<value> | --stdin) [--env <environment>]`.
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
/// `Format::Table` writes the value and a newline to `streams.out` and
/// nothing else: the whole point is
/// `DB_PASSWORD=$(shep secret get DB_PASSWORD)`, and a table with a KEY
/// column would give the substitution something to strip. `Format::Json`
/// wraps the same value in the standard envelope instead, through
/// [`SecretValueRow`], the same shape [`crate::commands::kv`]'s own
/// single-key `get` wraps a value in: `--format json` has to answer the
/// contract every command but `bleats` does
/// (`web/src/pages/docs/json-output.astro`), and a bare credential like
/// `hunter2` is not even valid JSON.
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
        Ok(Some(value)) => match streams.fmt {
            Format::Table => write_outcome(writeln!(streams.out, "{value}")),
            Format::Json => {
                let row = SecretValueRow {
                    key: key.to_string(),
                    value,
                };
                write_outcome(emit(
                    &mut *streams.out,
                    streams.fmt,
                    "secret",
                    row,
                    streams.style,
                ))
            }
        },
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

    use crate::cli::{Cli, Format};
    use crate::exit::ExitCode;
    use crate::output::Streams;

    fn streams_with<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>, fmt: Format) -> Streams<'a> {
        Streams {
            out,
            err,
            style: crate::style::Presentation::BARE,
            fmt,
        }
    }

    fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
        streams_with(out, err, Format::Json)
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
        fmt: Format,
    ) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = get(
            &mut streams_with(&mut out, &mut err, fmt),
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

    /// [`run_get`]'s stderr half, under [`Format::Table`]: every caller
    /// below is after the refusal text, not the envelope shape.
    fn run_get_capturing(
        paths: &ShepPaths,
        key: &str,
        environment: Option<&str>,
        allow_read: bool,
    ) -> (ExitCode, String) {
        let (code, _, err) = run_get(paths, key, environment, allow_read, Format::Table);
        (code, err)
    }

    /// [`run_get`]'s stdout half, under [`Format::Table`]: the bare-value
    /// contract `shep secret get k || echo default` and
    /// `DB_PASSWORD=$(shep secret get DB_PASSWORD)` depend on.
    /// [`get_under_json_wraps_the_value_in_the_standard_envelope`] covers
    /// the other format.
    fn run_get_capturing_out(
        paths: &ShepPaths,
        key: &str,
        environment: Option<&str>,
        allow_read: bool,
    ) -> (ExitCode, String) {
        let (code, out, _) = run_get(paths, key, environment, allow_read, Format::Table);
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

    /// fails if `--format json` goes back to the bare value: a bare
    /// `hunter2` is not JSON at all, so `shep secret get K --format json |
    /// jq .` would fail outright, and it would be an undocumented second
    /// exception to the envelope contract beside `bleats`.
    #[test]
    fn get_under_json_wraps_the_value_in_the_standard_envelope() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "hunter2").unwrap();

        let (code, out, _) = run_get(&paths, "K", None, true, Format::Json);
        assert_eq!(code, ExitCode::Success);
        let envelope: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(envelope["command"], "secret");
        assert_eq!(envelope["data"]["key"], "K");
        assert_eq!(envelope["data"]["value"], "hunter2");
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

    /// The door an operator uses, not [`exit_code_for`] on its own: a bad
    /// key is refused before the file is opened, so a typo cannot leave a
    /// store behind.
    #[test]
    fn a_bad_key_is_refused_through_set_and_creates_no_store() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = set(
            &mut streams(&mut out, &mut err),
            &paths,
            "not valid",
            None,
            "v",
        );
        assert_eq!(code, ExitCode::Usage);
        assert!(
            !paths.secrets.exists(),
            "the store must not be created on a refused key"
        );
    }

    /// fails if `--env` starts falling back to `all`. An operator asking
    /// what `staging` holds is asking about that slot, and answering with
    /// production's shared value would report a slot that is empty as
    /// filled.
    #[test]
    fn get_with_an_env_reads_that_slot_exactly() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "shared").unwrap();

        let (code, out) = run_get_capturing_out(&paths, "K", Some("staging"), true);
        assert_eq!(code, ExitCode::NotFound);
        assert!(out.is_empty(), "{out}");

        run_set(&paths, "K", Some("staging"), "staged").unwrap();
        let (code, out) = run_get_capturing_out(&paths, "K", Some("staging"), true);
        assert_eq!(code, ExitCode::Success);
        assert_eq!(out.trim(), "staged");
    }

    /// fails if either report grows the value it just wrote or removed.
    /// `shep secret set` runs in terminals and in CI logs, and a value
    /// echoed there outlives the command.
    #[test]
    fn set_and_unset_report_the_slot_and_never_the_value() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        let mut out = Vec::new();
        let mut err = Vec::new();

        let code = set(
            &mut streams(&mut out, &mut err),
            &paths,
            "K",
            Some("staging"),
            "hunter2",
        );
        assert_eq!(code, ExitCode::Success);
        let written = String::from_utf8(std::mem::take(&mut out)).unwrap();
        assert!(written.contains("K"), "{written}");
        assert!(written.contains("staging"), "{written}");
        assert!(!written.contains("hunter2"), "{written}");

        let code = unset(
            &mut streams(&mut out, &mut err),
            &paths,
            "K",
            Some("staging"),
        );
        assert_eq!(code, ExitCode::Success);
        let removed = String::from_utf8(out).unwrap();
        assert!(removed.contains("staging"), "{removed}");
        assert!(!removed.contains("hunter2"), "{removed}");
    }

    /// fails if an unset that removed nothing starts exiting 0, which an
    /// operator would read as a value having been there.
    #[test]
    fn unset_on_an_empty_slot_exits_not_found() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = unset(&mut streams(&mut out, &mut err), &paths, "ABSENT", None);
        assert_eq!(code, ExitCode::NotFound);
        assert!(out.is_empty(), "{out:?}");
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

    /// Covers `printf %s "$PW" | shep secret set KEY --stdin`: no trailing
    /// newline at all, so nothing is trimmed.
    #[test]
    fn stdin_value_reads_bytes_with_no_trailing_newline() {
        let mut input = std::io::Cursor::new(b"hunter2".to_vec());
        assert_eq!(resolve_stdin_value(&mut input).unwrap(), "hunter2");
    }

    /// Covers `echo "$PW" | shep secret set KEY --stdin` on both a unix
    /// pipe (`\n`) and a Windows one (`\r\n`): exactly one trailing newline
    /// comes off, so both store the same value.
    #[test]
    fn stdin_value_strips_exactly_one_trailing_newline_and_a_preceding_cr() {
        let mut lf = std::io::Cursor::new(b"hunter2\n".to_vec());
        assert_eq!(resolve_stdin_value(&mut lf).unwrap(), "hunter2");

        let mut crlf = std::io::Cursor::new(b"hunter2\r\n".to_vec());
        assert_eq!(resolve_stdin_value(&mut crlf).unwrap(), "hunter2");
    }

    /// fails if trimming widens past that one newline: leading and interior
    /// whitespace, and a `\r` anywhere but immediately before the final
    /// `\n`, can be part of the credential and must survive.
    #[test]
    fn stdin_value_trims_nothing_else() {
        let mut input = std::io::Cursor::new(b" hunter2 \r more\n".to_vec());
        assert_eq!(resolve_stdin_value(&mut input).unwrap(), " hunter2 \r more");
    }

    /// fails if `set K --stdin` stops parsing with no positional value: the
    /// whole point of the flag is a value with no argument at all.
    #[test]
    fn set_stdin_alone_parses_with_no_positional_value() {
        use clap::Parser;

        let cli = Cli::try_parse_from(["shep", "secret", "set", "K", "--stdin"]).unwrap();
        let crate::cli::Commands::Secret(args) = cli.command else {
            panic!("expected Commands::Secret");
        };
        let SecretCommand::Set { value, stdin, .. } = args.command else {
            panic!("expected SecretCommand::Set");
        };
        assert_eq!(value, None);
        assert!(stdin);
    }

    /// A positional value and `--stdin` disagree about where the value
    /// comes from; clap refuses before either is ever read, naming both.
    #[test]
    fn set_refuses_a_positional_value_and_stdin_together() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "secret", "set", "K", "v", "--stdin"]).is_err());
    }

    /// Neither a positional value nor `--stdin` leaves nothing to store.
    #[test]
    fn set_requires_a_positional_value_or_stdin() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "secret", "set", "K"]).is_err());
        assert!(Cli::try_parse_from(["shep", "secret", "set", "K", "v"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "secret", "set", "K", "--stdin"]).is_ok());
    }
}
