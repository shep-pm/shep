//! `$SHEP_HOME` lifecycle: resolution from `--home`/`$SHEP_HOME`/the user's
//! home directory, refusal reporting, and the first-run scaffold a fresh
//! home gets.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use cli::{Format, GlobalArgs};
use commands::shep_toml::ShepToml;
use exit::ExitCode;
use output::Streams;
use shep_core::paths::{HOME_DIR_VAR, ShepPaths, user_home};

use crate::client::emit_error_locked;
use crate::{cli, commands, exit, output, status, welcome};

/// Prints the one-line shepherd status to stderr, for an invocation clap
/// answers by itself.
///
/// stderr rather than stdout: `shep completions zsh > _shep` writes shell
/// meant to be sourced, which would execute a status line as code. Silent
/// when stderr is not a terminal or `argv` names a `--home` or `--host`.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) async fn print_shepherd_status(argv: &[OsString]) {
    if !std::io::stderr().is_terminal() || names_another_shepherd(argv) {
        return;
    }
    let global = GlobalArgs {
        host: None,
        home: None,
        format: Format::Table,
        quiet: false,
        // Plain prose to stderr, not a rendered table: nothing on this path
        // reads `style`.
        style: None,
    };
    let Ok(paths) = resolve_paths(&global) else {
        return;
    };
    let status = status::ShepherdStatus::probe(&paths).await;
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{}", status::one_line(&status));
}

/// Whether `argv` points at a shepherd other than the default one: `--home`
/// or `--host`, spelled `--flag value` or `--flag=value`.
///
/// Only the flag's presence matters, so a value clap would have refused still
/// counts.
fn names_another_shepherd(argv: &[OsString]) -> bool {
    argv.iter().any(|arg| {
        let arg = arg.to_string_lossy();
        ["--home", "--host"].iter().any(|flag| {
            arg.strip_prefix(flag)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
        })
    })
}

/// Turns `--home`/`$SHEP_HOME`/the user's home directory into a resolved
/// [`ShepPaths`].
///
/// Bridges clap's already-folded `GlobalArgs::home` back into the closure
/// shape `ShepPaths::resolve` reads the environment through. Which variables
/// name a home directory is [`user_home`]'s question, and differs by platform.
///
/// # Errors
/// - [`HomeRefusal::Relative`] if whichever of the two supplied the root
///   named a path without one. Decided here rather than in
///   [`ShepPaths::resolve`], which promises to touch no filesystem.
/// - [`HomeRefusal::Unresolved`] if neither `--home`/`$SHEP_HOME` nor a home
///   directory resolves a root.
pub(crate) fn resolve_paths(global: &GlobalArgs) -> Result<ShepPaths, HomeRefusal> {
    resolve_paths_in(global, &|key| std::env::var_os(key))
}

/// [`resolve_paths`] with the environment injected, so the home-directory
/// arm can be pinned without mutating this process's own.
///
/// # Errors
///
/// [`resolve_paths`]'s, unchanged.
fn resolve_paths_in(
    global: &GlobalArgs,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<ShepPaths, HomeRefusal> {
    if let Some(named) = global.home.as_ref() {
        require_absolute(HOME_KNOB, named)?;
        require_utf8(HOME_KNOB, named)?;
    }
    let env = |key: &str| match key {
        "SHEP_HOME" => global
            .home
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        other => var(other).map(|value| value.to_string_lossy().into_owned()),
    };
    let home_dir = match (user_home(var), env("SHEP_HOME")) {
        (Some(dir), _) => dir,
        (None, Some(_)) => PathBuf::new(),
        (None, None) => return Err(HomeRefusal::Unresolved),
    };
    // Only when the home directory is what `resolve` will join `.shep` onto:
    // an absolute `--home` above has already won, and the empty placeholder
    // this arm can carry goes unread in that case.
    if global.home.is_none() {
        require_absolute(HOME_DIR_VAR, &home_dir)?;
        require_utf8(HOME_DIR_VAR, &home_dir)?;
    }
    Ok(ShepPaths::resolve(&env, &home_dir))
}

/// Refuses `candidate` when its bytes are not valid UTF-8, naming `knob` as
/// the spelling to fix.
///
/// A path is bytes on unix and UTF-16 on Windows, and neither promises valid
/// UTF-8. Shep's own surfaces are all text, so the conversion happens
/// somewhere regardless; this makes it happen once, loudly, at the only door
/// an operator can name a home through.
///
/// # Errors
///
/// [`HomeRefusal::NotUtf8`], carrying `candidate` as typed.
fn require_utf8(knob: &'static str, candidate: &Path) -> Result<(), HomeRefusal> {
    if candidate.to_str().is_some() {
        return Ok(());
    }
    Err(HomeRefusal::NotUtf8 {
        knob,
        given: candidate.to_path_buf(),
    })
}

/// Refuses `candidate` when it has no root, naming `knob` as the spelling to
/// fix.
///
/// Every path in a [`ShepPaths`] is `$SHEP_HOME` plus a fixed tail, the
/// control socket included, so one rootless home is a whole flock that
/// answers from one directory and reports nothing from the next.
///
/// # Errors
///
/// [`HomeRefusal::Relative`], carrying `candidate` as typed and its absolute
/// form when this process's own directory could be read.
fn require_absolute(knob: &'static str, candidate: &Path) -> Result<(), HomeRefusal> {
    if candidate.is_absolute() {
        return Ok(());
    }
    Err(HomeRefusal::Relative {
        knob,
        absolute: absolute_form(candidate),
        given: candidate.to_path_buf(),
    })
}

/// Reports a refusal that stopped a verb before it had a `$SHEP_HOME`, and
/// hands back the status to exit with.
///
/// Four call sites end this way, two reaching it through [`resolve_paths`]
/// and two through [`ensure_home`]. A refusal that printed differently
/// depending on which one caught it would be a bug rather than a variation,
/// and the two furthest apart sit 900 lines from each other.
pub(crate) fn report_home_refusal(fmt: Format, refusal: &HomeRefusal) -> ExitCode {
    let code = refusal.code();
    emit_error_locked(fmt, code, &refusal.to_string());
    code
}

/// What a rootless `candidate` would have meant from here, for a refusal's
/// remedy line. `None` when this process's directory could not be read.
///
/// Shared with `commands::dev`, so the two refusals suggest the same path
/// for the same typed one.
pub(crate) fn absolute_form(candidate: &Path) -> Option<PathBuf> {
    std::env::current_dir().ok().map(|cwd| cwd.join(candidate))
}

/// Why [`resolve_paths`] or [`ensure_home`] would not hand back a layout.
///
/// A type rather than a bare [`ExitCode`]: three of the four carry the path
/// they are about, and an operator cannot act on a refusal that omits it.
#[derive(Debug)]
pub(crate) enum HomeRefusal {
    /// None of `--home`, `$SHEP_HOME` or the user's home directory
    /// resolved a root directory.
    Unresolved,
    /// Something named a path with no root, so every path derived from it
    /// would resolve against whatever directory the process happens to be
    /// in.
    Relative {
        /// The spelling an operator has to fix: `--home`/`$SHEP_HOME`, or
        /// the home-directory variable when that is what supplied the root.
        knob: &'static str,
        /// The path as it was spelled.
        given: PathBuf,
        /// The same path joined onto this process's directory, for the
        /// remedy line. `None` when that directory could not be read.
        absolute: Option<PathBuf>,
    },
    /// Something named a path whose bytes are not valid UTF-8.
    ///
    /// Refused rather than carried, because the path does not stay a path:
    /// it reaches the `{{SHEP_HOME}}` template, the Windows pipe name and
    /// every log path on the wire as a `String`, and each of those
    /// conversions is lossy. Shep would then read and write a directory
    /// whose name is not the one the operator typed, and say nothing.
    NotUtf8 {
        /// The spelling an operator has to fix, as in [`Self::Relative`].
        knob: &'static str,
        /// The path as it was spelled, rendered lossily for the message.
        /// Nothing but the message reads it.
        given: PathBuf,
    },
    /// `--home`/`$SHEP_HOME` named a directory that is not there. Never
    /// created: a named path is not a path shep may invent.
    Missing(PathBuf),
    /// The default home did not exist and could not be created.
    Io {
        /// The directory whose creation failed.
        path: PathBuf,
        /// What the filesystem said.
        source: std::io::Error,
    },
}

impl core::fmt::Display for HomeRefusal {
    /// The operator-facing message, remedy included.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unresolved => f.write_str(UNRESOLVED_HOME),
            Self::Relative {
                knob,
                given,
                absolute,
            } => write_relative_refusal(f, knob, given, absolute.as_deref()),
            Self::NotUtf8 { knob, given } => write!(
                f,
                "{knob} must be valid UTF-8, and {given} is not\n  \
                 shep carries the home path into the `{{{{SHEP_HOME}}}}` template, the log \
                 paths it reports, and the control socket's own name, all of which are text, \
                 so a byte that is not UTF-8 would be replaced and shep would use a \
                 directory you did not name",
                given = given.display(),
            ),
            Self::Missing(path) => write!(
                f,
                "no flock at {path}\n\
                 did you mean to drop {HOME_KNOB}? the default is {DEFAULT_HOME_SPELLING}\n\
                 to set up a flock there deliberately: {MKDIR_COMMAND} {quoted}",
                path = one_line(path),
                quoted = shell_quoted(path),
            ),
            Self::Io { path, source } => write!(
                f,
                "could not create {path}: {source}",
                path = one_line(path),
                source = crate::terminal_safe::sanitise(&source.to_string()).0,
            ),
        }
    }
}

/// `path` as one word a POSIX shell will not split, for a hint an operator
/// copies straight into one.
///
/// Unquoted, `--home "/tmp/my shep"` rendered `mkdir -p /tmp/my shep`, which
/// creates two directories and reports no error. Single quotes rather than
/// backslashes because a path is one word and reads as one.
#[cfg(not(windows))]
fn shell_quoted(path: &Path) -> String {
    format!("'{}'", one_line(path).replace('\'', r"'\''"))
}

/// `path` as one argument for [`MKDIR_COMMAND`], for a hint an operator
/// copies straight into a shell.
///
/// Double quotes are the only wrap both shells honour: `cmd.exe` reads a
/// single quote as an ordinary character. A Windows path cannot hold a `"`.
#[cfg(windows)]
fn shell_quoted(path: &Path) -> String {
    format!("\"{}\"", one_line(path))
}

/// `path` as a single line, for composing into prose whose line breaks a
/// table keeps.
///
/// A `--home` carrying a `\n` otherwise writes a line of its own choosing
/// under `error[usage]:`, which a reader takes for shep's. The layout is
/// shep's and stays multi-line; every value placed into it is collapsed
/// here, the way `refuse_version_skew` collapses `daemon_version`.
fn one_line(path: &Path) -> String {
    crate::terminal_safe::sanitise(&path.display().to_string()).0
}

impl core::error::Error for HomeRefusal {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Unresolved | Self::Relative { .. } | Self::NotUtf8 { .. } | Self::Missing(_) => {
                None
            }
            Self::Io { source, .. } => Some(source),
        }
    }
}

impl HomeRefusal {
    /// The status the command ends with.
    ///
    /// `Internal` rather than `Usage` for the io case: the operator asked for
    /// something reasonable and shep failed at it.
    pub(crate) fn code(&self) -> ExitCode {
        match self {
            Self::Unresolved | Self::Relative { .. } | Self::NotUtf8 { .. } | Self::Missing(_) => {
                ExitCode::Usage
            }
            Self::Io { .. } => ExitCode::Internal,
        }
    }
}

/// Resolves `$SHEP_HOME` and makes sure the directory is there, reporting
/// whether this call is what created it.
///
/// # Errors
///
/// Every variant of [`HomeRefusal`]; see [`ensure_home_at`].
pub(crate) fn ensure_home(global: &GlobalArgs) -> Result<(ShepPaths, bool), HomeRefusal> {
    let paths = resolve_paths(global)?;
    ensure_home_at(paths, global.home.is_some())
}

/// [`ensure_home`] with the environment already resolved away. `explicit` is
/// whether the operator named this home, by `--home` or `$SHEP_HOME`.
///
/// A default home is created, a named one is not: `~/.shep` is a name shep
/// chose, while `/srv/api` was typed, and creating a typo leaves a second,
/// empty, invisible flock. Only the root; `logs/`, `pids/` and `run/` stay
/// `shep_daemon::boot::init_dirs`' job.
///
/// # Errors
///
/// - [`HomeRefusal::Missing`] if `explicit` and the directory is not there.
/// - [`HomeRefusal::Io`] if the directory could not be created.
fn ensure_home_at(paths: ShepPaths, explicit: bool) -> Result<(ShepPaths, bool), HomeRefusal> {
    if paths.home.is_dir() {
        return Ok((paths, false));
    }
    if explicit {
        return Err(HomeRefusal::Missing(paths.home));
    }

    // `.mode(DIR_MODE)` at creation, never create-then-chmod: that leaves the
    // directory at the ambient umask long enough for another user to open a
    // handle that survives the chmod.
    let built = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(shep_daemon::boot::DIR_MODE)
                .create(&paths.home)
        }
        #[cfg(not(unix))]
        {
            std::fs::DirBuilder::new()
                .recursive(true)
                .create(&paths.home)
        }
    };

    match built {
        Ok(()) => Ok((paths, true)),
        Err(source) => Err(HomeRefusal::Io {
            path: paths.home,
            source,
        }),
    }
}

/// Writes `shep.toml`'s starter `[interpreters]` mapping the moment
/// `$SHEP_HOME` is first created.
///
/// Not folded into [`welcome::on_first_run`]: the banner is suppressed under
/// `--format json` and a piped stderr, and this mapping is written regardless.
/// Best-effort: a failure reports to stderr and continues.
pub(crate) fn scaffold_first_run_interpreters(paths: &ShepPaths) {
    if let Err(err) = ShepToml::edit(&paths.daemon_config, ShepToml::write_starter_interpreters) {
        let mut err_stream = std::io::stderr();
        let _ = writeln!(
            err_stream,
            "could not write a starter interpreter mapping to {}: {err}",
            paths.daemon_config.display()
        );
    }
}

/// Creates `paths.home` if it is not there, with the first-run scaffold the
/// shared gate in [`crate::dispatch::run`] gives every other verb's fresh
/// home.
///
/// `startup` is the caller, for its own default home: the target user's
/// `<passwd home>/.shep`, which [`ensure_home`] cannot resolve since it
/// reads this process's environment. A named home never reaches this;
/// `run` sends one through the shared gate, which refuses a missing one.
///
/// # Errors
///
/// [`HomeRefusal::Io`] when the directory could not be created.
#[cfg(unix)]
pub(crate) fn create_default_home(
    streams: &mut Streams<'_>,
    paths: ShepPaths,
) -> Result<(), HomeRefusal> {
    let (paths, home_is_new) = ensure_home_at(paths, false)?;
    if home_is_new {
        scaffold_first_run_interpreters(&paths);
        welcome::on_first_run(streams, &paths.home, std::io::stderr().is_terminal());
    }
    Ok(())
}

/// What [`HomeRefusal::Unresolved`] reports.
#[cfg(not(windows))]
const UNRESOLVED_HOME: &str = "none of --home, $SHEP_HOME, or $HOME resolves a root directory";

/// What [`HomeRefusal::Unresolved`] reports.
///
/// Names `%USERPROFILE%`, not `$HOME`: a Windows session sets no `HOME`, so
/// naming it sends an operator looking for a variable that was never going
/// to be there.
#[cfg(windows)]
const UNRESOLVED_HOME: &str =
    "none of --home, %SHEP_HOME%, or %USERPROFILE% resolves a root directory";

/// How an operator on this platform names the home directly, for a refusal
/// that has to say what to fix.
#[cfg(not(windows))]
pub(crate) const HOME_KNOB: &str = "--home/$SHEP_HOME";

/// How an operator on this platform names the home directly, for a refusal
/// that has to say what to fix.
#[cfg(windows)]
pub(crate) const HOME_KNOB: &str = "--home/%SHEP_HOME%";

/// How an operator on this platform spells the default home, for the
/// refusal that offers dropping `--home`.
#[cfg(not(windows))]
const DEFAULT_HOME_SPELLING: &str = "~/.shep";

/// Names `%USERPROFILE%` for the same reason [`UNRESOLVED_HOME`] does, and
/// spells the separator the way this platform prints one. `cmd.exe` expands
/// no `~`.
#[cfg(windows)]
const DEFAULT_HOME_SPELLING: &str = r"%USERPROFILE%\.shep";

/// The command that creates a directory and every parent it needs, for a
/// remedy an operator copies into a shell.
#[cfg(not(windows))]
const MKDIR_COMMAND: &str = "mkdir -p";

/// `-p` is neither shell's flag. `cmd.exe` and PowerShell both create the
/// intermediate directories from `mkdir` alone.
#[cfg(windows)]
const MKDIR_COMMAND: &str = "mkdir";

/// The one refusal shep gives for a home with no root, whichever knob named
/// it: `knob` is the spelling to fix, `absolute` the same path against this
/// process's directory when that could be read.
///
/// Shared with `commands::dev`, which gates `$SHEP_DEV_HOME` on the same
/// rule and owes an operator the same sentence.
pub(crate) fn write_relative_refusal(
    f: &mut core::fmt::Formatter<'_>,
    knob: &str,
    given: &Path,
    absolute: Option<&Path>,
) -> core::fmt::Result {
    write!(
        f,
        "{knob} must be an absolute path, not {given}\n\
         a relative home is read against whatever directory shep runs in, so the flock it \
         names is reachable from that one directory and nowhere else",
        given = one_line(given),
    )?;
    match absolute {
        Some(absolute) => write!(f, "\ndid you mean: {}", one_line(absolute)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
