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

use std::io::Read;

use shep_core::config::DaemonConfig;
use shep_core::paths::ShepPaths;
use shep_core::secrets::{
    self, ALL_ENVIRONMENTS, ProviderCache, Resolution, SecretError, SecretRef, SecretView,
};

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

/// Maps a secret operation error to the corresponding CLI exit code.
///
/// Invalid input errors map to [`ExitCode::Usage`], configuration errors map to
/// [`ExitCode::InvalidConfig`], and I/O or otherwise unrecognized errors map to
/// [`ExitCode::Failure`].
///
/// # Examples
///
/// ```
/// let code = exit_code_for(&SecretError::InvalidKey("bad key".into()));
/// assert_eq!(code, ExitCode::Usage);
/// ```
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

/// Reports a secret-operation error and returns its corresponding exit code.
///
/// The error message is written to the error stream without exposing secret values.
///
/// # Examples
///
/// ```ignore
/// let code = fail(&mut streams, &err);
/// assert_eq!(code, exit_code_for(&err));
/// ```
fn fail(streams: &mut Streams<'_>, err: &SecretError) -> ExitCode {
    let code = exit_code_for(err);
    streams.fail(code, &err.to_string())
}

/// Dispatches `shep secret` to the selected subcommand.
///
/// # Examples
///
/// ```text
/// shep secret list
/// ```
///
/// # Returns
///
/// The exit code produced by the selected subcommand.
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
                    Err((code, message)) => return streams.fail(code, &message),
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

/// Loads the daemon configuration used by the secret command.
///
/// Missing or invalid configuration files produce the default configuration,
/// keeping secret reads disabled and using the default host environment.
///
/// # Examples
///
/// ```no_run
/// # let paths: &ShepPaths = unimplemented!();
/// let config = daemon_config(paths);
/// ```
pub(crate) fn daemon_config(paths: &ShepPaths) -> DaemonConfig
pub(crate) fn daemon_config(paths: &ShepPaths) -> DaemonConfig {
    let text = std::fs::read_to_string(&paths.daemon_config).ok();
    DaemonConfig::load(text.as_deref(), &|_| None).unwrap_or_default()
}

/// Reads a secret value from `reader`, removing at most one trailing newline
/// and its preceding carriage return before decoding it as UTF-8.
///
/// Leading, interior, and other trailing whitespace is preserved. Values
/// exceeding [`secrets::MAX_VALUE_BYTES`] are rejected.
///
/// # Errors
///
/// Returns an exit code and message when reading fails, the value exceeds the
/// size limit, or the value is not valid UTF-8.
///
/// # Examples
///
/// ```
/// use std::io::Cursor;
///
/// let value = resolve_stdin_value(&mut Cursor::new(b"secret\n".to_vec()))
///     .expect("valid secret");
/// assert_eq!(value, "secret");
/// ```
fn resolve_stdin_value(reader: &mut dyn Read) -> Result<String, (ExitCode, String)> {
    let mut bytes = Vec::new();
    reader
        .take(STDIN_READ_CAP as u64)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            (
                ExitCode::Failure,
                format!("could not read the value from stdin: {err}"),
            )
        })?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > secrets::MAX_VALUE_BYTES {
        return Err((
            ExitCode::Usage,
            format!(
                "the value read from stdin is over the {}-byte limit",
                secrets::MAX_VALUE_BYTES
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|_utf8_error| {
        (
            ExitCode::Failure,
            "the value read from stdin is not valid UTF-8".to_string(),
        )
    })
}

/// How much of stdin `--stdin` will pull before refusing.
///
/// Two bytes over [`secrets::MAX_VALUE_BYTES`] so a value at exactly the cap
/// followed by `\r\n` still trims to something storable, and one more so
/// anything longer is over the cap after the trim rather than exactly on it.
const STDIN_READ_CAP: usize = secrets::MAX_VALUE_BYTES + 3;

/// Stores a secret value for the specified key and environment.
///
/// When no environment is provided, the value is stored in the shared
/// [`ALL_ENVIRONMENTS`] slot used as the fallback for every environment.
///
/// # Arguments
///
/// * `key` - The secret key to store.
/// * `environment` - The environment-specific slot, or the shared slot when
///   omitted.
/// * `value` - The secret value to store.
///
/// # Returns
///
/// The command exit code.
///
/// # Examples
///
/// ```text
/// shep secret set API_TOKEN "secret-value" --env production
/// ```
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

/// Reads a secret value from the requested environment and writes it in the selected output format.
///
/// When `environment` is omitted, resolves the value using `host_environment` and the shared
/// environment fallback. Reads are refused when `allow_read` is `false`.
///
/// # Examples
///
/// ```rust,no_run
/// # let mut streams = todo!();
/// # let paths = todo!();
/// # let args = todo!();
/// let exit_code = secret(&mut streams, &paths, &args);
/// assert!(exit_code.success());
/// ```
///
/// # Arguments
///
/// * `key` — The secret key to read.
/// * `environment` — An environment slot to read exactly.
/// * `allow_read` — Whether secret reads are permitted.
/// * `host_environment` — The environment used when no slot is specified.
///
/// # Returns
///
/// The command exit code. `ExitCode::NotFound` indicates that no value exists, while
/// `ExitCode::InvalidConfig` indicates that reads are disabled.
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

/// Resolves an operator-managed secret for a host environment, falling back to the shared environment slot.
///
/// Provider namespaces are not accessible. Unnamespaced keys are validated before the local secret store is read.
///
/// # Errors
///
/// Returns [`SecretError::InvalidKey`] for invalid or namespaced keys, or propagates errors from loading the secret store.
///
/// # Examples
///
/// ```rust,ignore
/// let value = resolve(&paths, "database-url", "production")?;
/// # Ok::<(), SecretError>(())
/// ```
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
        ProviderCache::default(),
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

/// Lists each stored secret key and the environments containing a value, preserving the store's key order.
///
/// Secret values are not included in the output.
///
/// # Examples
///
/// ```text
/// shep secret list
/// ```
///
/// # Returns
///
/// The command's exit status.
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

/// Reports the key and environment of a changed secret slot without exposing its value.
///
/// # Arguments
///
/// * `key` - The secret key whose slot changed.
/// * `environment` - The environment of the changed slot.
///
/// # Returns
///
/// The exit code produced while writing the report.
///
/// # Examples
///
/// ```rust,ignore
/// let exit_code = emit_slot(&mut streams, "API_KEY", "production");
/// assert!(exit_code.success());
/// ```
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

    /// Creates a bare-output stream configuration using the specified format.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut output = Vec::new();
    /// let mut error = Vec::new();
    /// let _streams = streams_with(&mut output, &mut error, Format::Table);
    /// ```
    fn streams_with<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>, fmt: Format) -> Streams<'a> {
        Streams {
            out,
            err,
            style: crate::style::Presentation::BARE,
            fmt,
        }
    }

    /// Creates output streams configured for JSON formatting.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut out = Vec::new();
    /// let mut err = Vec::new();
    /// let _streams = streams(&mut out, &mut err);
    /// ```
    fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
        streams_with(out, err, Format::Json)
    }

    /// Resolves Shepherd paths with `dir` as the `SHEP_HOME` directory.
    ///
    /// # Examples
    ///
    /// ```
    /// let paths = paths_in(std::path::Path::new("/tmp/shep"));
    /// assert_eq!(paths.home, std::path::Path::new("/tmp/shep"));
    /// ```
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

    /// Runs `shep secret get` with `production` as the host environment and captures its results.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let (code, stdout, stderr) = run_get(
    ///     &paths,
    ///     "api-key",
    ///     None,
    ///     true,
    ///     Format::Table,
    /// );
    /// ```
    ///
    /// The returned tuple contains the exit code, standard output, and standard error.
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

    /// Captures the stderr output from a table-formatted secret lookup.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let (code, stderr) = run_get_capturing(paths, "API_KEY", None, true);
    /// assert!(code.success());
    /// assert!(stderr.is_empty());
    /// ```
    fn run_get_capturing(
        paths: &ShepPaths,
        key: &str,
        environment: Option<&str>,
        allow_read: bool,
    ) -> (ExitCode, String) {
        let (code, _, err) = run_get(paths, key, environment, allow_read, Format::Table);
        (code, err)
    }

    /// Captures the exit code and standard output produced by a table-formatted secret lookup.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let (code, output) = run_get_capturing_out(&paths, "DB_PASSWORD", None, true);
    /// assert!(code.success());
    /// assert_eq!(output, "secret-value\n");
    /// ```
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

    /// A reader holding `left` bytes of `x`, counting what was pulled out
    /// of it.
    struct Oversized {
        left: usize,
        pulled: usize,
    }

    impl Read for Oversized {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let taken = buf.len().min(self.left);
            buf[..taken].fill(b'x');
            self.left -= taken;
            self.pulled += taken;
            Ok(taken)
        }
    }

    /// fails if the read is unbounded: an input nothing will accept must be
    /// refused off the front of the stream rather than buffered whole and
    /// then handed to `secrets::set` to reject. The code has to stay
    /// `Usage`, which is what the positional form's `ValueTooLong` exits.
    #[test]
    fn stdin_value_refuses_an_oversized_reader_without_buffering_it() {
        let mut input = Oversized {
            left: secrets::MAX_VALUE_BYTES * 4,
            pulled: 0,
        };

        let (code, message) = resolve_stdin_value(&mut input).unwrap_err();

        assert_eq!(code, ExitCode::Usage);
        assert!(
            message.contains(&secrets::MAX_VALUE_BYTES.to_string()),
            "the refusal names the limit: {message}"
        );
        assert!(
            input.pulled <= secrets::MAX_VALUE_BYTES + 8,
            "only a little past the cap may be read, not the whole stream: {}",
            input.pulled
        );
    }

    /// fails if the bound is drawn so tight that a value at exactly the cap
    /// stops fitting: `echo` appends a newline, so the longest storable
    /// value arrives as `MAX_VALUE_BYTES` plus `\r\n`.
    #[test]
    fn stdin_value_accepts_a_value_at_the_cap_with_a_newline_after_it() {
        let mut at_cap =
            std::io::Cursor::new([vec![b'x'; secrets::MAX_VALUE_BYTES], b"\r\n".to_vec()].concat());
        assert_eq!(
            resolve_stdin_value(&mut at_cap).unwrap().len(),
            secrets::MAX_VALUE_BYTES
        );
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
