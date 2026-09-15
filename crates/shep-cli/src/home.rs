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
use shep_core::paths::{ShepPaths, user_home};

use crate::client::emit_error_locked;
use crate::{cli, commands, exit, output, status, welcome};

/// Prints the one-line shepherd status to stderr, for an invocation clap
/// answers by itself.
///
/// stderr rather than stdout: `shep completions zsh > _shep` writes shell
/// meant to be sourced, which would execute a status line as code.
///
/// Silent when stderr is not a terminal, and silent when `argv` names a
/// `--home`, since the parse that would say which home is the one that just
/// failed.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) async fn print_shepherd_status(argv: &[OsString]) {
    if !std::io::stderr().is_terminal() || argv.iter().any(|a| a == "--home") {
        return;
    }
    let global = GlobalArgs {
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

/// Turns `--home`/`$SHEP_HOME`/the user's home directory into a resolved
/// [`ShepPaths`].
///
/// Bridges clap's already-folded `GlobalArgs::home` back into the closure
/// shape `ShepPaths::resolve` reads the environment through. Which variables
/// name a home directory is [`user_home`]'s question, and differs by
/// platform.
///
/// # Errors
///
/// - [`HomeRefusal::Relative`] if whichever of the two supplied the root
///   named a path without one. Decided here rather than in
///   [`ShepPaths::resolve`], which promises to touch no filesystem: the
///   absolute form of a relative path is a read of this process's own
///   directory.
/// - [`HomeRefusal::Unresolved`] if neither `--home`/`$SHEP_HOME` nor a home
///   directory resolves a root. The home directory is read only as that
///   fallback, so a `--home` invocation still works with none at all.
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
/// creates two directories, reports no error, and leaves the operator with
/// the empty invisible flock this refusal exists to prevent. Single quotes
/// rather than backslashes because a path is one word and reads as one; an
/// embedded `'` closes the quoting around an escaped one and reopens it.
#[cfg(not(windows))]
fn shell_quoted(path: &Path) -> String {
    format!("'{}'", one_line(path).replace('\'', r"'\''"))
}

/// `path` as one argument for [`MKDIR_COMMAND`], for a hint an operator
/// copies straight into a shell.
///
/// Double quotes are the only wrap both shells honour. `cmd.exe` reads a
/// single quote as an ordinary character. A Windows path cannot hold a `"`,
/// so the wrap always closes and nothing inside it needs escaping. Neither
/// shell's variable syntax is quoted by it, so `%TEMP%` or `$env:TEMP` in a
/// path still expands.
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
/// `--format json` and a piped stderr, and this mapping is written regardless,
/// since it is what lets a provisioning script's `shep start server.js` work
/// without `--interpreter`.
///
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
/// shared gate in [`run`] gives every other verb's fresh home.
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

/// The variable behind the default home, named by the refusal for a root
/// that came from there rather than from [`HOME_KNOB`].
///
/// `pub(crate)`: `commands::dev` falls back to the same directory and owes
/// the same spelling when it refuses one.
#[cfg(not(windows))]
pub(crate) const HOME_DIR_VAR: &str = "$HOME";

/// The variable behind the default home, named by the refusal for a root
/// that came from there rather than from [`HOME_KNOB`].
///
/// Names `%USERPROFILE%` for the same reason [`UNRESOLVED_HOME`] does: a
/// Windows session sets no `HOME`, so although [`user_home`] reads that
/// first, `%USERPROFILE%` is the first of the three that answers.
#[cfg(windows)]
pub(crate) const HOME_DIR_VAR: &str = "%USERPROFILE%";

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
mod tests {
    use super::*;
    use crate::style;

    /// fails if the copyable remedy stops being one shell word. A path with
    /// a space rendered `mkdir -p /tmp/my shep`, which creates two
    /// directories and reports no error, leaving exactly the empty
    /// invisible flock this refusal exists to prevent.
    #[cfg(not(windows))]
    #[test]
    fn the_mkdir_hint_survives_a_path_a_shell_would_split() {
        let refusal = HomeRefusal::Missing(PathBuf::from("/tmp/my shep home"));
        let text = refusal.to_string();
        assert!(
            text.contains("mkdir -p '/tmp/my shep home'"),
            "the remedy must name one word: {text}"
        );
        // The line above it names the path as prose, and is not a command.
        assert!(text.contains("no flock at /tmp/my shep home"), "{text}");
    }

    /// fails if the refusal offers to drop a knob the operator may never
    /// have typed. `$SHEP_HOME` reaches this variant through clap's `env`,
    /// so naming the flag alone sends half of them looking for something
    /// that is not on their command line.
    #[test]
    fn the_missing_home_refusal_reads_exactly_this() {
        let text = HomeRefusal::Missing(PathBuf::from("/srv/api")).to_string();
        // Whole message, not a fragment: a substring passes while a line
        // outside it regresses, and all three lines are per-platform.
        let expected = if cfg!(windows) {
            "no flock at /srv/api\n\
             did you mean to drop --home/%SHEP_HOME%? the default is %USERPROFILE%\\.shep\n\
             to set up a flock there deliberately: mkdir \"/srv/api\""
        } else {
            "no flock at /srv/api\n\
             did you mean to drop --home/$SHEP_HOME? the default is ~/.shep\n\
             to set up a flock there deliberately: mkdir -p '/srv/api'"
        };
        assert_eq!(text, expected);
    }

    /// fails if a path can add a line to a refusal. The table emitter keeps
    /// shep's own line breaks, so a `\n` inside an interpolated value would
    /// write a line under `error[usage]:` that reads as shep's own.
    #[test]
    fn a_newline_in_a_missing_home_cannot_forge_a_line() {
        let hostile = PathBuf::from("/tmp/forge-a\nnotice[ok]: your flock is fine");
        let text = HomeRefusal::Missing(hostile).to_string();
        assert_eq!(
            text.lines().count(),
            3,
            "the refusal has three lines of its own: {text:?}"
        );
        assert!(
            !text.lines().any(|line| line.starts_with("notice[")),
            "a line was forged: {text:?}"
        );
        assert!(
            text.starts_with("no flock at /tmp/forge-a notice[ok]: your flock is fine\n"),
            "the newline must become a space, not vanish: {text:?}"
        );
    }

    /// fails if the relative-home refusal takes a line from its path. Same
    /// defect as the missing-home one, two variants over, and the reason to
    /// check it separately is that it interpolates twice.
    #[test]
    fn a_newline_in_a_relative_home_cannot_forge_a_line() {
        let refusal = HomeRefusal::Relative {
            knob: "--home/$SHEP_HOME",
            given: PathBuf::from("forge-b\nerror[internal]: shepherd compromised"),
            absolute: Some(PathBuf::from(
                "/w/forge-b\nerror[internal]: shepherd compromised",
            )),
        };
        let text = refusal.to_string();
        assert_eq!(text.lines().count(), 3, "{text:?}");
        assert!(
            !text.lines().any(|line| line.starts_with("error[")),
            "a line was forged: {text:?}"
        );
    }

    /// fails if the io refusal lets either half add a line. Its `source` is
    /// written by the OS rather than by an operator, and is collapsed for
    /// the same reason: the layout is shep's and the values are not.
    #[test]
    fn neither_half_of_an_io_refusal_can_forge_a_line() {
        let refusal = HomeRefusal::Io {
            path: PathBuf::from("/tmp/forge-c\nnotice[ok]: created"),
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied\nnotice[ok]: retried and it worked",
            ),
        };
        let text = refusal.to_string();
        assert_eq!(text.lines().count(), 1, "{text:?}");
        assert!(!text.contains('\n'), "{text:?}");
    }

    /// fails if an apostrophe in a path breaks out of the quoting and turns
    /// the rest of the hint into shell the operator did not mean to run.
    #[cfg(not(windows))]
    #[test]
    fn an_apostrophe_in_a_path_cannot_escape_the_mkdir_hint() {
        let refusal = HomeRefusal::Missing(PathBuf::from("/tmp/rin's flock"));
        let text = refusal.to_string();
        assert!(
            text.contains(r"mkdir -p '/tmp/rin'\''s flock'"),
            "an embedded quote must close and reopen: {text}"
        );
    }

    /// fails if a Windows operator is handed a remedy from another
    /// platform. `mkdir -p` parses in neither shell here, and a single
    /// quote is an ordinary character to `cmd.exe`, so it would split the
    /// path rather than hold it together.
    #[cfg(windows)]
    #[test]
    fn the_missing_home_remedy_is_one_a_windows_shell_can_run() {
        let spaced = HomeRefusal::Missing(PathBuf::from(r"C:\tmp\my shep home")).to_string();
        assert_eq!(
            spaced,
            "no flock at C:\\tmp\\my shep home\n\
             did you mean to drop --home/%SHEP_HOME%? the default is %USERPROFILE%\\.shep\n\
             to set up a flock there deliberately: mkdir \"C:\\tmp\\my shep home\""
        );

        // A Windows path can hold an apostrophe, and the POSIX escaping
        // would break it apart. Nothing inside a double-quoted wrap needs
        // escaping, because a Windows path cannot hold a `"`.
        let quoted = HomeRefusal::Missing(PathBuf::from(r"C:\tmp\rin's flock")).to_string();
        assert_eq!(
            quoted,
            "no flock at C:\\tmp\\rin's flock\n\
             did you mean to drop --home/%SHEP_HOME%? the default is %USERPROFILE%\\.shep\n\
             to set up a flock there deliberately: mkdir \"C:\\tmp\\rin's flock\""
        );
    }

    /// A [`ShepPaths`] rooted at `root`, so the rule can be exercised without
    /// touching the process-global `$HOME`.
    #[cfg(unix)]
    fn paths_at(root: &std::path::Path) -> ShepPaths {
        let home = root.join(".shep").to_string_lossy().into_owned();
        let env = |key: &str| (key == "SHEP_HOME").then(|| home.clone());
        ShepPaths::resolve(&env, std::path::Path::new("/nonexistent"))
    }

    /// `~/.shep` is a name shep chose, so shep may create it.
    #[cfg(unix)]
    #[test]
    fn a_missing_default_home_is_created_and_reported_as_new() {
        let root = tempfile::tempdir().unwrap();

        let (paths, created) =
            ensure_home_at(paths_at(root.path()), false).expect("a default home is created");
        assert_eq!(paths.home, root.path().join(".shep"));
        assert!(
            created,
            "the first call must report that it created the home"
        );
        assert!(
            paths.home.is_dir(),
            "the home must exist on disk afterwards"
        );

        let (_, created_again) =
            ensure_home_at(paths_at(root.path()), false).expect("second call succeeds");
        assert!(
            !created_again,
            "a home that was already there is not newly created"
        );
    }

    #[cfg(unix)]
    #[test]
    fn creating_startups_default_home_scaffolds_it_like_the_shared_gate() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_at(root.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: style::Presentation::BARE,
            fmt: cli::Format::Table,
        };

        create_default_home(&mut streams, paths.clone()).expect("a default home is created");
        assert!(paths.home.is_dir());
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(written.contains("[interpreters]"), "{written}");

        create_default_home(&mut streams, paths).expect("a home already there is left alone");
    }

    #[cfg(unix)]
    #[test]
    fn an_explicitly_named_missing_home_is_refused_and_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_at(&root.path().join("srv").join("typo"));
        let named = paths.home.clone();

        let refusal = ensure_home_at(paths, true).expect_err("a named missing home is refused");
        assert_eq!(refusal.code(), ExitCode::Usage);
        let message = refusal.to_string();
        assert!(
            message.contains(&named.display().to_string()),
            "the refusal must name the path it refused: {message}"
        );
        assert!(
            message.contains("~/.shep"),
            "the refusal must point at the default as the way out: {message}"
        );
        assert!(
            !named.exists(),
            "a refused path must be left on disk exactly as it was found"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_created_home_is_owner_only_from_the_moment_it_exists() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let (paths, _) = ensure_home_at(paths_at(root.path()), false).unwrap();
        let mode = std::fs::metadata(&paths.home).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "a fresh $SHEP_HOME must be owner-only");
    }

    /// A rooted path on the platform running the test. `/tmp/explicit` has
    /// no drive prefix, so Windows reads it as relative and the gate in
    /// `resolve_paths` refuses it.
    #[cfg(not(windows))]
    const EXPLICIT_HOME: &str = "/tmp/explicit";

    /// A rooted path on the platform running the test.
    #[cfg(windows)]
    const EXPLICIT_HOME: &str = r"C:\tmp\explicit";

    /// Pins `resolve_paths`'s folding of an already-populated
    /// `GlobalArgs::home` only. `$SHEP_HOME` reaches that field through clap,
    /// and is pinned in `cli.rs`.
    #[test]
    fn explicit_home_field_resolves_to_the_expected_shep_paths() {
        let paths = resolve_paths(&global_with_home(Some(EXPLICIT_HOME))).unwrap();
        assert_eq!(paths.home, std::path::Path::new(EXPLICIT_HOME));
        // The control address is a socket file on unix and a named-pipe name
        // on Windows, so `--home` is asserted to reach both derivations.
        #[cfg(unix)]
        assert_eq!(
            paths.socket,
            std::path::Path::new("/tmp/explicit/run/shep.sock")
        );
        #[cfg(windows)]
        assert_eq!(paths.socket, std::path::Path::new(&paths.pipe_name()));
    }

    fn global_with_home(home: Option<&str>) -> cli::GlobalArgs {
        cli::GlobalArgs {
            home: home.map(Into::into),
            format: cli::Format::Table,
            quiet: false,
            style: None,
        }
    }

    /// An absolute path whose bytes are not valid UTF-8, which only unix can
    /// spell. Windows paths are UTF-16, so the same hole there is an unpaired
    /// surrogate and needs its own constructor.
    #[cfg(unix)]
    fn non_utf8_home() -> std::path::PathBuf {
        use std::os::unix::ffi::OsStringExt as _;
        std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"/tmp/shep-\xff-home".to_vec(),
        ))
    }

    /// A home shep cannot carry as text is refused at the door rather than
    /// replaced silently.
    ///
    /// Without this, `to_string_lossy` turns the byte into U+FFFD and every
    /// path in the layout is built from a directory the operator never
    /// named. The refusal has to come before `ShepPaths::resolve`, since that
    /// is where the conversion happens.
    #[cfg(unix)]
    #[test]
    fn a_home_that_is_not_utf8_is_refused_before_any_path_is_derived() {
        let home = non_utf8_home();
        let global = cli::GlobalArgs {
            home: Some(home.clone()),
            format: cli::Format::Table,
            quiet: false,
            style: None,
        };
        let Err(refusal) = resolve_paths(&global) else {
            panic!("a home that is not UTF-8 must not resolve a layout");
        };
        assert!(
            matches!(&refusal, HomeRefusal::NotUtf8 { knob, given }
                if *knob == HOME_KNOB && given == &home),
            "its own refusal, not the relative or unresolved one: {refusal:?}"
        );
        assert_eq!(refusal.code(), ExitCode::Usage);

        let rendered = refusal.to_string();
        assert!(
            rendered.contains(HOME_KNOB),
            "names the spelling to fix: {rendered}"
        );
        assert!(
            rendered.contains("UTF-8"),
            "says what is wrong with it: {rendered}"
        );
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }

    /// The neighbouring gate still answers first for a path that is both
    /// relative and not UTF-8, so one bad home reports one reason.
    #[cfg(unix)]
    #[test]
    fn a_relative_home_is_refused_as_relative_even_when_it_is_also_not_utf8() {
        use std::os::unix::ffi::OsStringExt as _;
        let home =
            std::path::PathBuf::from(std::ffi::OsString::from_vec(b"rel-\xff-home".to_vec()));
        let global = cli::GlobalArgs {
            home: Some(home),
            format: cli::Format::Table,
            quiet: false,
            style: None,
        };
        let Err(refusal) = resolve_paths(&global) else {
            panic!("a relative home must not resolve a layout");
        };
        assert!(
            matches!(refusal, HomeRefusal::Relative { .. }),
            "the rootless reason wins, since it is the one an operator hits first"
        );
    }

    /// The whole point of the gate: a home with no root puts the control
    /// socket at `rel-home/run/shep.sock`, which names one flock from the
    /// directory it was started in and a different, absent one from
    /// anywhere else.
    #[test]
    fn a_relative_home_is_refused_before_any_path_is_derived() {
        let Err(refusal) = resolve_paths(&global_with_home(Some("rel-home"))) else {
            panic!("a relative --home must not resolve a layout");
        };
        assert!(
            matches!(&refusal, HomeRefusal::Relative { knob, given, .. }
                if *knob == HOME_KNOB && given == std::path::Path::new("rel-home")),
            "a rootless home is its own refusal, not the unresolved one"
        );
        assert_eq!(
            refusal.code(),
            ExitCode::Usage,
            "a path shep cannot act on is the operator's to fix"
        );

        let rendered = refusal.to_string();
        assert!(
            rendered.contains("rel-home"),
            "the refusal must quote the path as typed: {rendered}"
        );
        assert!(
            rendered.contains(HOME_KNOB),
            "the refusal must name the spelling to fix: {rendered}"
        );
        let cwd = std::env::current_dir().expect("a current directory");
        assert!(
            rendered.contains(&cwd.join("rel-home").display().to_string()),
            "the remedy must name the absolute form of what was typed: {rendered}"
        );
    }

    /// Windows reads a path with no drive prefix as relative, so `\shep`
    /// carries the same defect as `rel-home` and has to be refused with it.
    #[cfg(windows)]
    #[test]
    fn a_drive_relative_home_is_refused_on_windows() {
        assert!(
            matches!(
                resolve_paths(&global_with_home(Some(r"\shep"))),
                Err(HomeRefusal::Relative { .. })
            ),
            r"`\shep` resolves against whichever drive is current"
        );
    }

    /// The gate is on the resolved root, not on `--home` alone: with no
    /// `--home`, the default home is the home directory plus `.shep`, and a
    /// The other door into the same refusal. `--home` is the one an operator
    /// types, but the home directory the OS hands back is equally capable of
    /// carrying a byte shep cannot render, and it reaches the same
    /// `to_string_lossy`.
    ///
    /// `resolve_paths_in` exists to inject this closure, so the arm is
    /// reachable without mutating the process environment.
    #[cfg(unix)]
    #[test]
    fn a_home_directory_that_is_not_utf8_is_refused_and_names_its_own_variable() {
        use std::os::unix::ffi::OsStringExt as _;
        // Every variable `user_home` reads, so the arm under test is the one
        // that answered rather than a fallback.
        let mangled = |key: &str| {
            matches!(key, "HOME" | "USERPROFILE" | "HOMEDRIVE" | "HOMEPATH")
                .then(|| OsString::from_vec(b"/home/\xff".to_vec()))
        };
        let Err(refusal) = resolve_paths_in(&global_with_home(None), &mangled) else {
            panic!("a home directory that is not UTF-8 must not resolve a layout");
        };
        assert!(
            matches!(&refusal, HomeRefusal::NotUtf8 { knob, .. } if *knob == HOME_DIR_VAR),
            "names the variable that supplied it, not the --home knob: {refusal:?}"
        );
        assert_eq!(refusal.code(), ExitCode::Usage);
        assert!(
            refusal.to_string().contains("UTF-8"),
            "says what is wrong with it"
        );
    }

    /// rootless home directory is the same defect one door over.
    #[test]
    fn a_relative_home_directory_is_refused_and_names_its_own_variable() {
        // Every variable `user_home` reads on either platform, so the arm
        // under test is the one that answered rather than a fallback.
        let rootless = |key: &str| {
            matches!(key, "HOME" | "USERPROFILE" | "HOMEDRIVE" | "HOMEPATH")
                .then(|| OsString::from("ada"))
        };
        let Err(refusal) = resolve_paths_in(&global_with_home(None), &rootless) else {
            panic!("a rootless home directory must not resolve a layout");
        };
        // Pinned by variant, not by rendered text alone: `UNRESOLVED_HOME`
        // also names `$HOME`, so the assertions below pass on a refusal that
        // never noticed the rootless path.
        assert!(
            matches!(&refusal, HomeRefusal::Relative { knob, given, .. }
                if *knob == HOME_DIR_VAR && given == std::path::Path::new("ada")),
            "the refusal must carry the home directory as supplied, not the joined `.shep`"
        );

        let rendered = refusal.to_string();
        assert!(
            rendered.contains(HOME_DIR_VAR),
            "an operator cannot fix `--home` when `--home` is not what said it: {rendered}"
        );
        assert!(
            !rendered.contains(HOME_KNOB),
            "naming a knob the operator did not touch sends them to the wrong fix: {rendered}"
        );
        let cwd = std::env::current_dir().expect("a current directory");
        assert!(
            rendered.contains(&cwd.join("ada").display().to_string()),
            "the remedy must name the absolute form of what was supplied: {rendered}"
        );
    }

    /// The `# Errors` promise that `--home` works "with none at all": no
    /// `$HOME`, no `%USERPROFILE%`, nothing `user_home` reads.
    ///
    /// This is the arm that hands `resolve` an empty `home_dir` as a
    /// placeholder, knowing `SHEP_HOME` answers first and the placeholder
    /// goes unread. Nothing pinned it before, because every other test here
    /// runs against a real environment where `$HOME` is set, so the arm that
    /// prefers the home directory is the one they reach.
    #[test]
    fn an_explicit_home_resolves_with_no_home_directory_at_all() {
        let nothing = |_: &str| None;
        let paths = resolve_paths_in(&global_with_home(Some(EXPLICIT_HOME)), &nothing)
            .expect("--home names a root on its own");
        assert_eq!(
            paths.home,
            std::path::Path::new(EXPLICIT_HOME),
            "the empty placeholder must not reach the resolved home"
        );
        assert!(
            !paths.snapshot.starts_with(".shep"),
            "a path rooted at the empty placeholder would start with `.shep`: {}",
            paths.snapshot.display()
        );
    }

    /// The gate must not refuse the ordinary case it sits in front of.
    #[test]
    fn an_absolute_home_directory_still_resolves_the_default_home() {
        let rooted = |key: &str| (key == "HOME").then(|| OsString::from(EXPLICIT_HOME));
        // Windows reads `HOME` only after `USERPROFILE`, which this closure
        // leaves unset, so one absolute answer serves both platforms.
        let paths = resolve_paths_in(&global_with_home(None), &rooted)
            .expect("an absolute home directory resolves the default home");
        assert_eq!(
            paths.home,
            std::path::Path::new(EXPLICIT_HOME).join(".shep")
        );
    }
}
