//! Turns a CLI target token into `AppConfig`s.
//!
//! [`resolve_target`] walks `start`'s precedence: `-`, an explicit
//! `--flockfile`, a recognised extension, or a bare script path. A `.js`
//! Flockfile is evaluated through node via [`evaluate_js_flockfile`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use shep_core::config::{AppConfig, DeclaredApp, FlockFormat, Flockfile, FlockfileError};

use crate::commands::bounded::{Bounded, run_bounded};
use crate::commands::lifecycle::default_cwd_to_flockfile_dir;
use crate::exit::ExitCode;

/// What [`resolve_target`] can fail with
///
/// Mapped to exit codes by [`target_exit_code`], not by a `From` impl.
/// `start`'s daemon-side failures are `shep_client`'s errors instead.
#[derive(Debug)]
pub enum TargetError {
    /// `target` was `-` and stdin was not valid UTF-8, or the read failed.
    Stdin(std::io::Error),
    /// The extension named a recognised Flockfile format, but the file
    /// could not be read.
    Read {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying IO failure.
        source: std::io::Error,
    },
    /// The source read fine but failed Flockfile validation.
    Flockfile(FlockfileError),
    /// `target` named nothing at any tier of `start`'s precedence: no sheep
    /// by id or name, no fold, no Flockfile, and no path on disk.
    Unresolvable {
        /// The raw target string.
        target: String,
    },
    /// `--flockfile` was given for a path whose extension names no format
    /// this can read.
    UnknownFlockfileFormat {
        /// The path as the operator wrote it.
        path: PathBuf,
    },
    /// A `.js` Flockfile could not be evaluated. `node_missing` separates
    /// "install node" from "your config threw", which exit differently.
    /// No `path` field: every `detail` already names it.
    Js {
        /// What went wrong, already phrased for the operator.
        detail: String,
        /// `true` when node itself was not found on `PATH`.
        node_missing: bool,
    },
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdin(err) => write!(f, "failed to read stdin: {err}"),
            Self::Read { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::Flockfile(err) => write!(f, "{err}"),
            Self::Unresolvable { target } => {
                write!(
                    f,
                    "{target} is not a sheep, a fold, `-`, a recognised Flockfile, or an \
                     existing path"
                )?;
                // Only for a word that could have been an assignment and was
                // not: a name holding a separator was always a path.
                match target.split_once('=') {
                    Some((name, _)) if !name.is_empty() && !name.contains(['/', '\\']) => write!(
                        f,
                        "; `{name}` is not a name an assignment can use, which takes a letter \
                         or `_` and then letters, digits or `_`"
                    ),
                    _ => Ok(()),
                }
            }
            Self::UnknownFlockfileFormat { path } => write!(
                f,
                "--flockfile needs a .toml, .yaml, .yml, .json, .json5 or .js file; {} is none of those",
                path.display()
            ),
            Self::Js { detail, .. } => f.write_str(detail),
        }
    }
}

impl core::error::Error for TargetError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Stdin(err) | Self::Read { source: err, .. } => Some(err),
            Self::Flockfile(err) => Some(err),
            Self::Unresolvable { .. } | Self::UnknownFlockfileFormat { .. } | Self::Js { .. } => {
                None
            }
        }
    }
}

impl From<FlockfileError> for TargetError {
    fn from(source: FlockfileError) -> Self {
        Self::Flockfile(source)
    }
}

/// `start`'s mapping from a resolution failure to the exit code that reports it
pub(crate) fn target_exit_code(err: &TargetError) -> ExitCode {
    match err {
        TargetError::Stdin(_) => ExitCode::Failure,
        TargetError::Read { .. } | TargetError::Unresolvable { .. } => ExitCode::Usage,
        TargetError::Flockfile(_) => ExitCode::InvalidConfig,
        TargetError::UnknownFlockfileFormat { .. } => ExitCode::Usage,
        TargetError::Js {
            node_missing: true, ..
        } => ExitCode::Failure,
        TargetError::Js {
            node_missing: false,
            ..
        } => ExitCode::InvalidConfig,
    }
}

/// How long node gets to hand back a `.js` Flockfile's JSON before shep
/// kills it
///
/// 30s, against ~60ms to require a small module and the couple of seconds a
/// large dependency tree costs on a cold filesystem.
const JS_EVAL_BUDGET: Duration = Duration::from_secs(30);

/// The filename [`evaluate_js_flockfile`] writes [`JS_BRIDGE_SCRIPT`] to
///
/// Plain ASCII with no spaces: it reaches node as a bare relative argument,
/// so nothing needing quoting reaches a command line.
const JS_BRIDGE_FILE: &str = "shep-flockfile-bridge.js";

/// The bridge run by [`evaluate_js_flockfile`], written to a file rather
/// than passed to `node -e`
///
/// A `node` resolving to a `.cmd` shim has `cmd.exe` re-parse a `-e`
/// argument, and this script contains `&&`. A file's contents cross no
/// parser. The path comes from the environment, so a Flockfile path
/// containing `'`, `\` or a newline has no string literal to escape.
const JS_BRIDGE_SCRIPT: &str = "try { \
     process.stdout.write(JSON.stringify(require(process.env.SHEP_FLOCKFILE_PATH))); \
 } catch (err) { \
     process.stderr.write('[bridge saw ' + String(process.env.SHEP_FLOCKFILE_PATH) + '] ' + (err && err.message ? String(err.message) : String(err))); \
     process.exitCode = 1; \
 }";

/// Evaluates a `.js` Flockfile through node and returns its JSON
///
/// `SHEP_FLOCKFILE_PATH` carries the path, absolute: `require("x.js")`
/// without `./` resolves against `node_modules`. `budget` bounds node
/// exiting, not `require` returning. `docs/migration.md` quotes the
/// `node_missing` sentence by hand.
///
/// # Errors
/// - [`TargetError::Read`] if the path could not be canonicalized.
/// - [`TargetError::Js`] with `node_missing` if node is not on `PATH`.
/// - [`TargetError::Js`] if node failed, could not be spawned, ran past
///   `budget`, or left a process holding the output.
pub(crate) fn evaluate_js_flockfile(path: &Path, budget: Duration) -> Result<String, TargetError> {
    let absolute = std::fs::canonicalize(path).map_err(|source| TargetError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    // Bound until this function returns: dropping a `TempDir` deletes the
    // loader node is still reading.
    let scratch = tempfile::Builder::new()
        .prefix("shep-js-bridge")
        .tempdir()
        .map_err(|source| TargetError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    let loader = scratch.path().join(JS_BRIDGE_FILE);
    std::fs::write(&loader, JS_BRIDGE_SCRIPT).map_err(|source| TargetError::Read {
        path: loader.clone(),
        source,
    })?;
    let mut command = std::process::Command::new("node");
    command
        .arg(JS_BRIDGE_FILE)
        .current_dir(scratch.path())
        .env(
            "SHEP_FLOCKFILE_PATH",
            shep_core::paths::strip_verbatim_prefix(&absolute).as_os_str(),
        )
        .stdin(std::process::Stdio::null());
    let output = match run_bounded(&mut command, budget) {
        Ok(Bounded::Exited(output)) => output,
        Ok(Bounded::Killed) => {
            return Err(TargetError::Js {
                detail: format!(
                    "node was still running {} after {}s, so shep killed it; a Flockfile \
                     module has to export its config and let node exit, and one that leaves a \
                     server listening or a timer armed does not",
                    path.display(),
                    budget.as_secs_f32()
                ),
                node_missing: false,
            });
        }
        Ok(Bounded::OutputHeldOpen) => {
            return Err(TargetError::Js {
                detail: format!(
                    "node finished with {} within {}s, but a process it left behind still \
                     holds the output shep was reading, so shep gave up on it; a Flockfile \
                     module must not leave a child of its own on node's stdout or stderr",
                    path.display(),
                    budget.as_secs_f32()
                ),
                node_missing: false,
            });
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(TargetError::Js {
                detail: format!(
                    "reading a .js Flockfile runs it through node, and node was not found on PATH; \
                     install node, or convert {} to a .toml Flockfile",
                    path.display()
                ),
                node_missing: true,
            });
        }
        Err(err) => {
            return Err(TargetError::Js {
                detail: format!("could not run node for {}: {err}", path.display()),
                node_missing: false,
            });
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("node exited non-zero and said nothing");
        return Err(TargetError::Js {
            detail: format!("node could not evaluate {}: {reason}", path.display()),
            node_missing: false,
        });
    }
    String::from_utf8(output.stdout).map_err(|_utf8_error| TargetError::Js {
        detail: format!("node printed non-UTF-8 output for {}", path.display()),
        node_missing: false,
    })
}

/// `word` read as a `NAME=VALUE` assignment, or `None` when the name is one
/// no shell would accept
///
/// The name is a letter or `_` followed by letters, digits or `_`. A value
/// may hold anything, `=` included, since only the first `=` separates the
/// two. `execve` itself validates no name, but one a shell cannot expand is
/// a variable the sheep's own wrapper script could never read.
fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((name, value))
}

/// Splits the leading assignments off `targets`, returning them and what is
/// left
///
/// The shell's own rule: leading words that parse as assignments are the
/// environment, and the first word that does not ends the run. So `A=1
/// ./koji B=2` leaves `B=2` a target, exactly as a shell leaves it an
/// argument. A repeated name takes its last value.
pub(crate) fn split_assignments(targets: &[String]) -> (BTreeMap<String, String>, &[String]) {
    let mut env = BTreeMap::new();
    let mut rest = targets;
    while let Some((word, tail)) = rest.split_first() {
        let Some((name, value)) = assignment(word) else {
            break;
        };
        env.insert(name.to_string(), value.to_string());
        rest = tail;
    }
    (env, rest)
}

/// Resolves `target` into the [`AppConfig`]s `start` should register
///
/// Fixed precedence, do not widen: `-` is Flockfile JSON on `stdin`, then
/// `as_flockfile` or a recognised extension, then any path that exists.
///
/// # Errors
/// - [`TargetError::Stdin`] if `target` is `-` and `stdin` is not UTF-8.
/// - [`TargetError::Read`] if the file could not be read.
/// - [`TargetError::Flockfile`] if the source failed Flockfile validation.
/// - [`TargetError::UnknownFlockfileFormat`] under `as_flockfile`, bad extension.
/// - [`TargetError::Js`] if node failed or ran past [`JS_EVAL_BUDGET`].
/// - [`TargetError::Unresolvable`] if `target` matched none of the above.
pub fn resolve_target(
    target: &str,
    name: Option<&str>,
    stdin: &[u8],
    as_flockfile: bool,
) -> Result<Vec<AppConfig>, TargetError> {
    Ok(resolve_target_declared(target, name, stdin, as_flockfile)?
        .into_iter()
        .map(|declared| declared.config)
        .collect())
}

/// [`resolve_target`], keeping the keys each app's document literally wrote
///
/// A key set answers what a template claims, and only a document makes a
/// claim: the Flockfile tiers go through [`Flockfile::parse_declared`], and
/// a bare script path reports an empty set. A defaulted `cwd` is not
/// declared, nor are `--cwd`, `--fold` and `--interpreter`: they change what
/// a fresh app is registered with and claim nothing about an app the flock
/// already has.
///
/// # Errors
/// Every error [`resolve_target`] returns, for the same inputs.
pub(crate) fn resolve_target_declared(
    target: &str,
    name: Option<&str>,
    stdin: &[u8],
    as_flockfile: bool,
) -> Result<Vec<DeclaredApp>, TargetError> {
    let path = Path::new(target);
    match (target, FlockFormat::from_path(path)) {
        ("-", _) => {
            let source = String::from_utf8(stdin.to_vec()).map_err(|_utf8_error| {
                TargetError::Stdin(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stdin is not UTF-8",
                ))
            })?;
            Ok(Flockfile::parse_declared(&source, FlockFormat::Json)?)
        }
        (_, format) if as_flockfile => match format {
            Some(format) => {
                let source = std::fs::read_to_string(path).map_err(|source| TargetError::Read {
                    path: path.to_path_buf(),
                    source,
                })?;
                let apps = Flockfile::parse_declared(&source, format)?;
                Ok(default_cwd_to_flockfile_dir(apps, path))
            }
            None if path.extension().and_then(|e| e.to_str()) == Some("js") => {
                let json = evaluate_js_flockfile(path, JS_EVAL_BUDGET)?;
                let apps = Flockfile::parse_declared(&json, FlockFormat::Json)?;
                Ok(default_cwd_to_flockfile_dir(apps, path))
            }
            None => Err(TargetError::UnknownFlockfileFormat {
                path: path.to_path_buf(),
            }),
        },
        (_, Some(format)) => {
            let source = std::fs::read_to_string(path).map_err(|source| TargetError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            let apps = Flockfile::parse_declared(&source, format)?;
            Ok(default_cwd_to_flockfile_dir(apps, path))
        }
        // Absolutised against the CLI's cwd, which is where `exists` is
        // answered. The daemon would otherwise resolve it against its own.
        _ if path.exists() => {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(target);
            // Relative only: `canonicalize` resolves symlinks, so an
            // absolute `/var/...` comes back as `/private/var/...` on macOS.
            let script = if path.is_absolute() {
                target.to_string()
            } else {
                std::fs::canonicalize(path)
                    .map(|abs| {
                        shep_core::paths::strip_verbatim_prefix(&abs)
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_else(|_| target.to_string())
            };
            let mut app = AppConfig::minimal(name.unwrap_or(stem), &script);
            // Where the operator ran `shep start`: an unset `cwd` leaves
            // the child inheriting whatever the shepherd was spawned from.
            app.cwd = std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned());
            // Empty key sets: the command line is not a template, so a load
            // has nothing to apply to a sheep the flock already has.
            Ok(vec![DeclaredApp {
                config: app,
                declared: BTreeSet::new(),
                declared_env: BTreeSet::new(),
            }])
        }
        _ => Err(TargetError::Unresolvable {
            target: target.to_string(),
        }),
    }
}

