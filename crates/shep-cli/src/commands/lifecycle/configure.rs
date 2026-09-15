//! Merges a load's declared config into the app the flock already has.
//!
//! The declared-key merge, interpreter resolution, cwd defaulting, reset
//! depth and the per-app refusal line that [`start::load`](super::start::load)
//! applies before it respawns anything.

use std::collections::BTreeMap;
use std::path::Path;

use clap::ValueEnum as _;
use shep_client::{Client, START_DEADLINE};
use shep_core::config::{DeclaredApp, ResetDepth};
use shep_core::protocol::{Request, Response, SheepApplied};

use crate::cli::{ResetMode, StartArgs};
use crate::commands::lifecycle::Load;
use crate::exit::ExitCode;
use crate::output::Streams;

/// Whether any row in a `Request::Restart` reply came back `errored`
///
/// The reply has no per-id error slot, so a failed respawn arrives inside
/// an `Ok`.
pub(crate) fn any_restart_failed(procs: &[shep_core::protocol::ProcessInfo]) -> bool {
    procs
        .iter()
        .any(|info| info.status == shep_core::status::ProcStatus::Errored)
}

/// Gives every app that set no `cwd` the Flockfile's own directory
///
/// Fills the field without adding `cwd` to the declared key set: the
/// document did not ask for this directory, so a load must not establish it
/// on a sheep the flock already has and overwrite a `--cwd` set since.
///
/// Absolute, via `canonicalize`, because the daemon resolves a relative cwd
/// against its own. A path that cannot be canonicalised is a silent no-op,
/// and an app that sets its own `cwd` keeps it.
pub(crate) fn default_cwd_to_flockfile_dir(
    apps: Vec<DeclaredApp>,
    flockfile: &Path,
) -> Vec<DeclaredApp> {
    let Some(dir) = std::fs::canonicalize(flockfile)
        .ok()
        .and_then(|abs| abs.parent().map(Path::to_path_buf))
        .map(|dir| shep_core::paths::strip_verbatim_prefix(&dir).into_owned())
    else {
        return apps;
    };
    let dir = dir.to_string_lossy().into_owned();
    apps.into_iter()
        .map(|mut app| {
            if app.config.cwd.is_none() {
                app.config.cwd = Some(dir.clone());
            }
            app
        })
        .collect()
}

/// Merges the Flockfile's declaration into each app the flock already has,
/// and tells the operator what that did
///
/// `Request::Start` on a name the flock already has adds instances rather
/// than reconciling config, so this is the request that applies an edited
/// file to a running app.
///
/// Additive: a Flockfile arrives from the app's own repository, so a load
/// appends what nobody has established and leaves alone what an operator
/// set since. `--reset` widens that. Only apps whose document declared
/// something are sent, so `shep start ./thing` applies nothing. Field names
/// only, never values, since `env` carries secrets.
pub(crate) async fn apply_declared(
    client: &Client,
    streams: &mut Streams<'_>,
    declared: &[DeclaredApp],
    reset: ResetDepth,
    mode: Load,
) -> ExitCode {
    let apps: Vec<DeclaredApp> = declared
        .iter()
        .filter(|app| !app.declared.is_empty())
        .cloned()
        .collect();
    if apps.is_empty() {
        return ExitCode::Success;
    }
    let report = match client
        .request_with_deadline(Request::ApplyConfig { apps, reset }, Some(START_DEADLINE))
        .await
    {
        Ok(Response::Applied(report)) => report,
        // `Response` is `#[non_exhaustive]`, so an answer this client does
        // not recognise is a daemon-side fault rather than a bad file.
        Ok(_other) => {
            let message = "the daemon answered the config load with a response this client does \
                 not understand; the Flockfile's edits are not in effect";
            return streams.fail(ExitCode::Internal, message);
        }
        // The code the class of failure already has: an unreachable daemon
        // is `DaemonUnreachable`, an expired deadline `DeadlineExceeded`, a
        // daemon-side refusal whatever its `RpcErrorCode` maps to.
        Err(err) => {
            let message = format!(
                "the Flockfile's edits could not be applied, so they are not in effect: {err}"
            );
            return streams.fail(ExitCode::from(&err), &message);
        }
    };
    let mut failure: Option<ExitCode> = None;
    for sheep in report {
        // An app with nothing to say prints nothing: a deploy re-runs the
        // same unchanged Flockfile every time.
        let Some(message) = applied_line(&sheep) else {
            continue;
        };
        if sheep.refused.is_some() {
            // Config the operator declared did not land, so exiting 0 would
            // tell `shep start F.toml && deploy` to carry on. Every app is
            // reported before the code is returned.
            failure = failure.or(Some(streams.fail(REFUSED_EXIT, &message)));
        } else {
            streams.aside(mode.verb(), &message);
        }
    }
    failure.unwrap_or(ExitCode::Success)
}

/// What a per-app refusal inside an otherwise successful load exits with
///
/// One code for every refusal: [`SheepApplied::refused`] carries a sentence
/// and nothing machine-readable, so the class cannot be recovered without
/// matching on the daemon's prose.
const REFUSED_EXIT: ExitCode = ExitCode::InvalidConfig;

/// The first of two codes to have failed, `Success` when neither did
///
/// `start` works in order, so a later failure overwriting an earlier one
/// would leave the operator reading the symptom instead of the cause.
pub(crate) fn first_failure(earlier: ExitCode, later: ExitCode) -> ExitCode {
    if earlier == ExitCode::Success {
        later
    } else {
        earlier
    }
}

/// One line for what a load did to one app, or `None` when it did nothing
///
/// A pending field always travels with the verb that promotes it. The
/// clause is a gerund, `waiting on`, which agrees with the singular and
/// plural subjects `join(", ")` can produce.
pub(crate) fn applied_line(sheep: &SheepApplied) -> Option<String> {
    let name = &sheep.name;
    let mut parts = Vec::new();
    if !sheep.applied.is_empty() {
        parts.push(format!("applied {}", sheep.applied.join(", ")));
    }
    if !sheep.pending.is_empty() {
        parts.push(format!(
            "{} waiting on the next spawn (`shep reload {name}` promotes them)",
            sheep.pending.join(", ")
        ));
    }
    if let Some(refused) = &sheep.refused {
        parts.push(refused.clone());
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("{name}: {}", parts.join("; ")))
}

/// `shep.toml`'s `[interpreters]` entry for `script`'s own extension, if it
/// has one and the map names it
///
/// `Path::extension` reads a dotfile like `.bashrc` as extensionless, so an
/// entry keyed `""` can never match here.
pub(crate) fn mapped_interpreter(
    script: &str,
    interpreters: &BTreeMap<String, String>,
) -> Option<String> {
    let extension = Path::new(script).extension()?.to_str()?;
    interpreters.get(extension).cloned()
}

/// Folds `shep.toml`'s `[interpreters]` mapping and `--interpreter` onto
/// `apps`
///
/// Precedence: `shep.toml`, then a Flockfile's own `interpreter` field,
/// then the flag. Filling only `None` slots from `interpreters` is what
/// makes an app's own value outrank the map, the literal `"none"` included.
/// `flag` then overwrites every app.
pub(crate) fn apply_interpreters(
    apps: &mut [DeclaredApp],
    interpreters: &BTreeMap<String, String>,
    flag: Option<&str>,
) {
    if !interpreters.is_empty() {
        for app in apps.iter_mut() {
            if app.config.interpreter.is_none()
                && let Some(mapped) = mapped_interpreter(&app.config.script, interpreters)
            {
                app.config.interpreter = Some(mapped);
            }
        }
    }
    if let Some(interpreter) = flag {
        for app in apps.iter_mut() {
            app.config.interpreter = Some(interpreter.to_string());
        }
    }
}

/// The `--reset=<mode>` the operator typed, `None` when they typed nothing
///
/// Carries the mode so the two arms that refuse a reset quote back what was
/// written. A bare `--reset` is a usage error on its own.
pub(crate) fn reset_flag(args: &StartArgs) -> Option<String> {
    args.reset.map(|mode| {
        let name = mode
            .to_possible_value()
            .expect("every ResetMode variant has a possible value")
            .get_name()
            .to_string();
        format!("--reset={name}")
    })
}

pub(crate) fn reset_depth(args: &StartArgs) -> ResetDepth {
    args.reset.map_or(ResetDepth::None, ResetMode::to_depth)
}
