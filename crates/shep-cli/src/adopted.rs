//! External-subcommand dispatch: running a token clap could not place as an
//! adopted dog's own binary, git and cargo's external-subcommand precedent.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use cli::{Format, GlobalArgs};
use commands::shep_toml::ShepToml;
use exit::ExitCode;

use crate::home::resolve_paths;
use crate::remote::refuse_host;
use crate::{cli, commands, exit};

/// Runs the token clap could not place as an adopted dog: `shep <dogname> [args...]`.
///
/// Adopted dogs only, never a `$PATH` scan. Built-in verbs win: this runs
/// only once clap has failed every subcommand and alias.
///
/// `None` for every case that should reach clap's own unknown-verb error.
pub(crate) fn dispatch_adopted_dog(
    argv: &[OsString],
    err: &clap::Error,
) -> Option<std::process::ExitCode> {
    if err.kind() != clap::error::ErrorKind::InvalidSubcommand {
        return None;
    }
    let name = match err.get(clap::error::ContextKind::InvalidSubcommand) {
        Some(clap::error::ContextValue::String(name)) => name.as_str(),
        _ => return None,
    };
    // clap's error carries the name but not where it sat. Everything after
    // that position is this dog's own argv.
    let index = argv.iter().position(|arg| arg.to_str() == Some(name))?;
    let global = GlobalArgs {
        host: None,
        home: home_before(&argv[1..index]),
        format: Format::Table,
        quiet: false,
        style: None,
    };
    let paths = resolve_paths(&global).ok()?;
    // `_readonly`, not `ShepToml::edit`: `edit` saves even when its closure
    // only reads, so a failed lookup would create `$SHEP_HOME` and write a
    // `shep.toml` on every mistyped verb.
    let path = ShepToml::adopted_dog_path_readonly(&paths.daemon_config, name)
        .ok()
        .flatten()?;
    // An adopted dog is a local binary: running it here for a `--host` the
    // operator aimed elsewhere would be the substitution `--host` refuses.
    if host_before(&argv[1..index]).is_some() {
        return Some(refuse_host(format_before(&argv[1..index])).into());
    }
    let dog_argv = &argv[index + 1..];
    // A dog that declared its subcommands gets shep's own help page for them;
    // every other invocation, and every dog that declared nothing, is passed
    // through to the binary as it always was.
    if let Some(code) = crate::adopted_help::describe_dog(name, &path, &paths.home, dog_argv) {
        return Some(code.into());
    }
    Some(run_adopted_dog(&path, &paths.home, name, dog_argv))
}

/// Scans `prefix`, the argv tokens before the one clap could not place, for
/// `--home` in either `--home value` or `--home=value` form.
///
/// The only global flag [`resolve_paths`] reads. Falls back to `$SHEP_HOME`
/// by hand, since clap's own `env = "SHEP_HOME"` attribute never ran on an
/// argv it could not parse.
pub(crate) fn home_before(prefix: &[OsString]) -> Option<PathBuf> {
    let mut tokens = prefix.iter();
    while let Some(arg) = tokens.next() {
        if let Some(value) = home_equals_value(arg) {
            return Some(value);
        }
        if arg == "--home" {
            return tokens.next().map(PathBuf::from);
        }
    }
    std::env::var_os("SHEP_HOME").map(PathBuf::from)
}

/// Scans `prefix`, the argv tokens before the one clap could not place, for
/// `--host` in either `--host value` or `--host=value` form.
///
/// Only presence matters to the caller, which refuses. A `--host` with no
/// value after it still answers `Some`, with an empty one, and a value that
/// is not UTF-8 is read lossily: both are refused all the same, and neither
/// may be the way a dog slips through. No `$SHEP_HOST` fallback exists.
fn host_before(prefix: &[OsString]) -> Option<String> {
    let mut tokens = prefix.iter();
    while let Some(arg) = tokens.next() {
        if let Some(value) = arg.to_string_lossy().strip_prefix("--host=") {
            return Some(value.to_owned());
        }
        if arg == "--host" {
            return Some(
                tokens
                    .next()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
        }
    }
    None
}

/// Scans `prefix`, the argv tokens before the one clap could not place, for
/// `--format` in either `--format value` or `--format=value` form.
///
/// Only the refusal reads it: clap never finished parsing this argv, so
/// [`cli::GlobalArgs::format`] was never filled in, and a JSON consumer would
/// otherwise get table text. A missing or unknown value answers
/// [`Format::Table`], the flag's own default.
fn format_before(prefix: &[OsString]) -> Format {
    let mut tokens = prefix.iter();
    while let Some(arg) = tokens.next() {
        let value = match arg.to_string_lossy().strip_prefix("--format=") {
            Some(value) => value.to_owned(),
            None if arg == "--format" => match tokens.next() {
                Some(value) => value.to_string_lossy().into_owned(),
                None => break,
            },
            None => continue,
        };
        return <Format as clap::ValueEnum>::from_str(&value, true).unwrap_or(Format::Table);
    }
    Format::Table
}

/// The value in a `--home=value` token, or `None` when `arg` is not one.
///
/// Splits on the platform's own encoding, since `to_str` drops a non-UTF-8
/// value. A dropped `--home=` would fall through to `$SHEP_HOME`.
/// [`resolve_paths`] still refuses the value; this only carries it there.
#[cfg(unix)]
fn home_equals_value(arg: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let rest = arg.as_bytes().strip_prefix(b"--home=")?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(rest)))
}

/// [`home_equals_value`] for Windows, where a path is UTF-16 rather than
/// bytes and the same `to_str` hole is an unpaired surrogate.
#[cfg(windows)]
fn home_equals_value(arg: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    let wide: Vec<u16> = arg.encode_wide().collect();
    let prefix: Vec<u16> = "--home=".encode_utf16().collect();
    let rest = wide.strip_prefix(prefix.as_slice())?;
    Some(PathBuf::from(OsString::from_wide(rest)))
}

/// Runs `path`, an adopted dog's binary: `extra_args` passed through as
/// typed, the two variables every dog is promised (`$SHEP_HOME` to find the
/// shepherd, `$SHEP_DOG_NAME` to name its own `[<name>]` section in
/// `dogs.toml`), stdio inherited.
///
/// `name` is the token the operator typed, so a dog run this way reads the
/// same `dogs.toml` section as the same dog run by the shepherd.
fn run_adopted_dog(
    path: &Path,
    home: &Path,
    name: &str,
    extra_args: &[OsString],
) -> std::process::ExitCode {
    let status = std::process::Command::new(path)
        .args(extra_args)
        .env("SHEP_HOME", home)
        .env(shep_core::dogs::DOG_NAME_VAR, name)
        .status();
    match status {
        Ok(status) => std::process::ExitCode::from(dog_exit_code(status)),
        Err(err) => {
            eprintln!("shep: could not run adopted dog {}: {err}", path.display());
            ExitCode::Failure.into()
        }
    }
}

/// `status`'s own exit code, or `128 + signal` if it died by one, the shell
/// convention `commands::reap::classify` reads a reaped supervisor by.
#[cfg(unix)]
fn dog_exit_code(status: std::process::ExitStatus) -> u8 {
    use std::os::unix::process::ExitStatusExt as _;
    match status.code() {
        Some(code) => code as u8,
        None => (128 + status.signal().unwrap_or(0)) as u8,
    }
}

/// `status`'s own exit code.
///
/// No `128 + signal` arm: every Windows process exit carries a code, so the
/// `unwrap_or` is defensive.
#[cfg(windows)]
fn dog_exit_code(status: std::process::ExitStatus) -> u8 {
    status.code().unwrap_or(1) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    #[cfg(unix)]
    #[test]
    fn home_before_reads_a_separate_value_argument() {
        let prefix = [OsString::from("--home"), OsString::from("/tmp/x")];
        assert_eq!(home_before(&prefix), Some(PathBuf::from("/tmp/x")));
    }

    #[cfg(unix)]
    #[test]
    fn home_before_reads_an_equals_form() {
        let prefix = [OsString::from("--home=/tmp/y")];
        assert_eq!(home_before(&prefix), Some(PathBuf::from("/tmp/y")));
    }

    #[cfg(unix)]
    #[test]
    fn home_before_skips_unrelated_tokens_before_finding_home() {
        let prefix = [
            OsString::from("--format"),
            OsString::from("json"),
            OsString::from("--home"),
            OsString::from("/tmp/z"),
        ];
        assert_eq!(home_before(&prefix), Some(PathBuf::from("/tmp/z")));
    }

    #[test]
    fn host_before_reads_a_separate_value_and_an_equals_form() {
        let spaced = [OsString::from("--host"), OsString::from("amp2")];
        assert_eq!(host_before(&spaced), Some("amp2".to_owned()));

        let equals = [OsString::from("--host=user@amp2")];
        assert_eq!(host_before(&equals), Some("user@amp2".to_owned()));

        let after_others = [
            OsString::from("--format"),
            OsString::from("json"),
            OsString::from("--host"),
            OsString::from("amp2"),
        ];
        assert_eq!(host_before(&after_others), Some("amp2".to_owned()));

        assert_eq!(host_before(&[OsString::from("--home=/tmp/x")]), None);
        assert_eq!(host_before(&[]), None);
        assert_eq!(
            host_before(&[OsString::from("--host")]),
            Some(String::new()),
            "a flag with no value is still a flag that was typed"
        );
    }

    #[test]
    fn format_before_reads_a_separate_value_and_an_equals_form() {
        let spaced = [OsString::from("--format"), OsString::from("json")];
        assert_eq!(format_before(&spaced), Format::Json);

        let equals = [OsString::from("--format=json")];
        assert_eq!(format_before(&equals), Format::Json);

        let after_others = [
            OsString::from("--host"),
            OsString::from("amp2"),
            OsString::from("--format"),
            OsString::from("json"),
        ];
        assert_eq!(format_before(&after_others), Format::Json);

        assert_eq!(format_before(&[]), Format::Table);
        assert_eq!(format_before(&[OsString::from("--format")]), Format::Table);
        assert_eq!(
            format_before(&[OsString::from("--format=xml")]),
            Format::Table
        );
        assert_eq!(
            format_before(&[OsString::from("--host=amp2")]),
            Format::Table
        );
    }

    /// The dog writes a marker file when it runs, so "refused" is read from
    /// the file's absence: a `std::process::ExitCode` cannot be inspected.
    #[cfg(unix)]
    #[test]
    fn an_adopted_dog_behind_host_is_refused_not_run_here() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        std::fs::create_dir_all(&home).unwrap();
        let marker = dir.path().join("ran");
        let script = dir.path().join("mydog.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        let mut mode = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&script, mode).unwrap();
        ShepToml::edit(&home.join("shep.toml"), |cfg| {
            cfg.adopt_dog("mydog", &script, false).unwrap();
        })
        .unwrap();

        for host_args in [vec!["--host", "amp2"], vec!["--host=amp2"]] {
            let argv: Vec<OsString> = ["shep", "--home"]
                .into_iter()
                .map(OsString::from)
                .chain([home.clone().into_os_string()])
                .chain(host_args.into_iter().map(OsString::from))
                .chain([OsString::from("mydog"), OsString::from("koji")])
                .collect();
            let err = Cli::try_parse_from(&argv).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);

            assert!(
                dispatch_adopted_dog(&argv, &err).is_some(),
                "the refusal is an answer, not a fall-through to clap: {argv:?}"
            );
            assert!(!marker.exists(), "the dog ran here: {argv:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn dog_exit_code_reads_a_normal_exit_status() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = std::process::ExitStatus::from_raw(7 << 8);
        assert_eq!(dog_exit_code(status), 7);
    }

    #[cfg(unix)]
    #[test]
    fn dog_exit_code_reads_128_plus_signal_for_a_signalled_status() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = std::process::ExitStatus::from_raw(9); // SIGKILL, no WIFEXITED bit
        assert_eq!(dog_exit_code(status), 128 + 9);
    }

    /// Does not cover the `err.kind()` check in `dispatch_adopted_dog`: a
    /// `MissingRequiredArgument` carries no `ContextKind::InvalidSubcommand`
    /// either, so the context match below it answers `None` regardless.
    #[cfg(unix)]
    #[test]
    fn dispatch_adopted_dog_is_none_for_a_parse_error_that_is_not_invalid_subcommand() {
        let argv: Vec<OsString> = ["shep", "adopt"].into_iter().map(OsString::from).collect();
        let err = Cli::try_parse_from(&argv).unwrap_err();
        assert_ne!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
        assert!(dispatch_adopted_dog(&argv, &err).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_adopted_dog_is_none_for_a_name_shep_toml_has_never_heard_of() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        std::fs::create_dir_all(&home).unwrap();
        let argv: Vec<OsString> = ["shep", "--home"]
            .into_iter()
            .map(OsString::from)
            .chain([home.into_os_string(), OsString::from("nosuchdog")])
            .collect();
        let err = Cli::try_parse_from(&argv).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
        assert!(dispatch_adopted_dog(&argv, &err).is_none());
    }

    /// `home` is never pre-created here, unlike the neighbouring
    /// `dispatch_adopted_dog` tests: that absence is what is asserted.
    #[cfg(unix)]
    #[test]
    fn dispatch_adopted_dog_creates_nothing_for_a_missing_shep_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        assert!(!home.exists(), "test setup must start with no $SHEP_HOME");

        let argv: Vec<OsString> = ["shep", "--home"]
            .into_iter()
            .map(OsString::from)
            .chain([home.clone().into_os_string(), OsString::from("nosuchdog")])
            .collect();
        let err = Cli::try_parse_from(&argv).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);

        assert!(dispatch_adopted_dog(&argv, &err).is_none());
        assert!(
            !home.exists(),
            "a failed dog lookup must never create $SHEP_HOME: {}",
            home.display()
        );
    }

    /// `std::process::ExitCode` cannot be inspected, so this asserts only that
    /// a real spawn-and-wait happened. `cli_e2e.rs` pins the argv and
    /// `SHEP_HOME` contract against the real binary.
    #[cfg(unix)]
    #[test]
    fn dispatch_adopted_dog_finds_a_dog_shep_toml_really_has() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        std::fs::create_dir_all(&home).unwrap();
        let script = dir.path().join("mydog.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        let mut mode = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&script, mode).unwrap();
        ShepToml::edit(&home.join("shep.toml"), |cfg| {
            cfg.adopt_dog("mydog", &script, false).unwrap();
        })
        .unwrap();

        let argv: Vec<OsString> = ["shep", "--home"]
            .into_iter()
            .map(OsString::from)
            .chain([
                home.into_os_string(),
                OsString::from("mydog"),
                OsString::from("koji"),
            ])
            .collect();
        let err = Cli::try_parse_from(&argv).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);

        assert!(
            dispatch_adopted_dog(&argv, &err).is_some(),
            "an adopted dog must dispatch instead of falling through to clap's own error"
        );
    }

    /// The name asserted is the token the operator typed, never the script's
    /// file stem: `mydog.sh` is adopted here as `telemetry`.
    #[cfg(unix)]
    #[test]
    fn a_dog_run_by_name_is_given_the_same_home_and_name_the_shepherd_gives_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        std::fs::create_dir_all(&home).unwrap();
        let seen = dir.path().join("seen");
        let script = dir.path().join("mydog.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n' \"$SHEP_HOME\" \"$SHEP_DOG_NAME\" \"$1\" > {}\n",
                seen.display()
            ),
        )
        .unwrap();
        let mut mode = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&script, mode).unwrap();

        run_adopted_dog(&script, &home, "telemetry", &[OsString::from("koji")]);

        let seen = std::fs::read_to_string(&seen).unwrap();
        assert_eq!(
            seen.lines().collect::<Vec<_>>(),
            vec![
                home.display().to_string().as_str(),
                "telemetry",
                // The name arrives beside the operator's arguments, never in
                // place of them.
                "koji",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn home_before_keeps_a_non_utf8_equals_value() {
        use std::os::unix::ffi::OsStringExt as _;
        let arg = OsString::from_vec(b"--home=/tmp/\xff".to_vec());
        let found = home_before(&[arg]).expect("the value must survive the parse");
        assert_eq!(
            found,
            non_utf8_tmp_home(),
            "the bytes as typed, not a lossy rendering and not a fallback"
        );
    }

    /// The Windows arm of the same hole. A path there is UTF-16, so the
    /// byte that cannot be UTF-8 is instead a high surrogate with no low one
    /// after it, which a filesystem accepts and `to_str` refuses.
    ///
    /// The first assertion is the fixture checking itself. Built wrong, the
    /// value would be ordinary UTF-16, the old `to_str` parse would have
    /// handled it, and the test would pass while exercising nothing.
    #[cfg(windows)]
    #[test]
    fn home_before_keeps_a_lone_surrogate_equals_value() {
        use std::os::windows::ffi::OsStringExt as _;
        let lone = 0xD800_u16;
        let mut typed: Vec<u16> = r"--home=C:\tmp\".encode_utf16().collect();
        typed.push(lone);
        let arg = OsString::from_wide(&typed);
        assert!(
            arg.to_str().is_none(),
            "the fixture must be the case under test, not valid UTF-16"
        );

        let found = home_before(&[arg]).expect("the value must survive the parse");

        let mut want: Vec<u16> = r"C:\tmp\".encode_utf16().collect();
        want.push(lone);
        assert_eq!(
            found,
            PathBuf::from(OsString::from_wide(&want)),
            "the units as typed, not a lossy rendering and not a fallback"
        );
    }

    /// `/tmp/\xff`, the value the test above passes as `--home=`.
    #[cfg(unix)]
    fn non_utf8_tmp_home() -> std::path::PathBuf {
        use std::os::unix::ffi::OsStringExt as _;
        std::path::PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec()))
    }
}
