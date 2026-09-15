//! The `start` and `add` verb entry points.
//!
//! The per-target load pipeline: resolve a target, register what the flock
//! does not have, merge declared config into what it does, then respawn.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;

use shep_client::{Client, START_DEADLINE};
use shep_core::config::{AppConfig, DeclaredApp, ResetDepth};
use shep_core::protocol::{EnvValue, ProcessInfo, Request, Response, SelectorSpec};

use crate::cli::StartArgs;
use crate::commands::selector::parse_selector;
use crate::commands::lifecycle::configure::{
    apply_declared, apply_interpreters, first_failure, reset_depth, reset_flag,
};
use crate::commands::lifecycle::resolve::{TargetError, resolve_target_declared, split_assignments};
use crate::commands::lifecycle::respawn::{already_registered, resume_all};
use crate::commands::lifecycle::selectors::{
    fail_target, flock_matches, flock_now, is_reachable_as_a_name, render_outcome, request_each,
    selector_miss,
};
use crate::commands::lifecycle::staged_start_deadline;
use crate::exit::ExitCode;
use crate::output::{FlockRows, Streams};

/// Which of the two verbs that read a Flockfile is running
///
/// They share targets, resolution, merge and refusals; the difference is
/// whether anything is spawned at the end. One code path, so a document
/// cannot register differently depending on which verb read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Load {
    /// `shep start`: register what the flock does not have, and bring up what
    /// it does.
    Start,
    /// `shep add`: register what the flock does not have, and stop there.
    Add,
}

impl Load {
    /// The verb's own name, for a notice's code and for the `--format json`
    /// envelope's `command`.
    pub(crate) fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Add => "add",
        }
    }
}

/// Registers what `args` resolves to and brings all of it up.
pub async fn start(
    client: &Client,
    streams: &mut Streams<'_>,
    args: &StartArgs,
    discovered: Option<&Path>,
    interpreters: &BTreeMap<String, String>,
) -> ExitCode {
    load(client, streams, args, discovered, interpreters, Load::Start).await
}

/// Registers what `args` resolves to and starts none of it
///
/// The app lands registered and stopped, so a template shipping `env = {
/// DB_HOST = "" }` can be filled in before it spawns. The order is
/// register, fill in, start.
///
/// An app the flock already has is merged into and left as it is, running
/// or not, so re-running this after editing a template cannot stop a
/// service.
pub async fn add(
    client: &Client,
    streams: &mut Streams<'_>,
    args: &StartArgs,
    discovered: Option<&Path>,
    interpreters: &BTreeMap<String, String>,
) -> ExitCode {
    load(client, streams, args, discovered, interpreters, Load::Add).await
}

/// The body [`start`] and [`add`] share; see [`Load`] for what they do not
async fn load(
    client: &Client,
    streams: &mut Streams<'_>,
    args: &StartArgs,
    discovered: Option<&Path>,
    interpreters: &BTreeMap<String, String>,
    mode: Load,
) -> ExitCode {
    let (assignments, targets) = split_assignments(&args.targets);
    // An environment belongs to one sheep for the reason a name does, and
    // the shell has no analogue to borrow: one assignment prefix there
    // introduces one command.
    if !assignments.is_empty() && targets.len() > 1 {
        let message = "an assignment takes one target: an environment belongs to one sheep";
        return streams.fail(ExitCode::Usage, message);
    }
    // A word holding whitespace is one a shell never hands over: the
    // operator quoted the assignment and its target together, which is the
    // one spelling that would need shep to split and requote for itself.
    if !assignments.is_empty() && targets.is_empty() {
        let quoted = args
            .targets
            .iter()
            .find(|word| word.split_whitespace().count() > 1);
        let message = match quoted {
            Some(word) => format!(
                "an assignment needs a target; `{word}` is one word, so drop the quotes and \
                 let the shell split it"
            ),
            None => {
                "an assignment needs a target: a script path, or a sheep the flock has".to_string()
            }
        };
        return streams.fail(ExitCode::Usage, &message);
    }
    // `--name` renames the sheep a target becomes, and a name is unique to
    // one sheep, so it cannot mean anything across several targets.
    if args.name.is_some() && targets.len() > 1 {
        let message = "--name takes one target: a name belongs to one sheep";
        return streams.fail(ExitCode::Usage, message);
    }
    if targets.is_empty() {
        let mut started = Vec::new();
        let code = load_one(
            client,
            streams,
            args,
            None,
            discovered,
            interpreters,
            &mut started,
            mode,
            &assignments,
        )
        .await;
        // Printed whenever the verb succeeded, not only when it touched a
        // row. A failing verb leaves stdout empty, crate-wide.
        if code == ExitCode::Success {
            let wrote = render_outcome(client, streams, mode.verb(), FlockRows(started)).await;
            if wrote != ExitCode::Success {
                return wrote;
            }
        }
        return code;
    }
    // In turn, not atomically: if the second target fails the first is
    // already up. The exit code is the first failure.
    let mut failure: Option<ExitCode> = None;
    let mut started = Vec::new();
    for target in targets {
        let code = load_one(
            client,
            streams,
            args,
            Some(target),
            discovered,
            interpreters,
            &mut started,
            mode,
            &assignments,
        )
        .await;
        if code != ExitCode::Success {
            failure = failure.or(Some(code));
        }
    }
    // One table for the whole invocation, keyed on the outcome alone and
    // never on `started` being non-empty: every row is attempted, so a
    // partly-failed fold ends non-empty, and a table there would sit beside
    // an error envelope under `--format json`.
    if failure.is_none() {
        let wrote = render_outcome(client, streams, mode.verb(), FlockRows(started)).await;
        if wrote != ExitCode::Success {
            return wrote;
        }
    }
    failure.unwrap_or(ExitCode::Success)
}

/// Sets each assignment on `name` as an operator override
///
/// One request per key, since [`Request::SetSheepEnv`] carries one. The
/// first failure stops the run and comes back unphrased: a value the sheep
/// is already running with is a notice, and one it has never seen is a
/// refusal, and only the caller knows which it has.
///
/// # Errors
/// The first key the shepherd did not record, and why.
async fn set_assignments(
    client: &Client,
    name: &str,
    assignments: &BTreeMap<String, String>,
) -> Result<(), String> {
    for (key, value) in assignments {
        let body = Request::SetSheepEnv {
            name: name.to_string(),
            key: key.clone(),
            value: Some(EnvValue::from(value.clone())),
        };
        match client.request(body).await {
            Ok(Response::SheepEnvSet { .. }) => {}
            Ok(_unrecognised) => {
                return Err(format!(
                    "{name}: the shepherd answered {key} with something this client does not \
                     understand"
                ));
            }
            Err(err) => return Err(format!("{name}: {key} was not recorded ({err})")),
        }
    }
    Ok(())
}

/// One target's worth of [`load`]
///
/// `interpreters` is read once by the caller rather than per target.
#[allow(clippy::too_many_arguments)]
async fn load_one(
    client: &Client,
    streams: &mut Streams<'_>,
    args: &StartArgs,
    target: Option<&str>,
    discovered: Option<&Path>,
    interpreters: &BTreeMap<String, String>,
    started: &mut Vec<shep_core::protocol::ProcessInfo>,
    mode: Load,
    assignments: &BTreeMap<String, String>,
) -> ExitCode {
    // Everything from here to `resolve_target` is the precedence in
    // `StartArgs::targets`' own help: a sheep by id or name, then a fold,
    // then a Flockfile, then a path. Each tier claims the token only if it
    // resolves there. `-` and `--flockfile` skip the flock entirely.
    let mut listing: Option<Vec<ProcessInfo>> = None;
    let mut missed: Option<String> = None;
    if let Some(token) = target
        && token != "-"
        && !args.flockfile
    {
        // Parsed client-side, as every selector-taking verb does, so a
        // malformed one is a local usage error rather than a round trip.
        let selector = match parse_selector(streams, token) {
            Ok(selector) => selector,
            Err(code) => return code,
        };
        if is_reachable_as_a_name(&selector) {
            let flock = flock_now(client).await;
            let matched = flock_matches(&selector, &flock);
            if !matched.is_empty() {
                // Returns rather than falling through: a token that resolved
                // to a sheep reads no file, so `shep start web` cannot apply
                // whatever Flockfile sits in the operator's directory. A
                // reset flag is refused: there is no template to reset to.
                // Instances of one name are one sheep; two names are two, and
                // an assignment names neither of them.
                if let Some(flag) = reset_flag(args) {
                    let message = format!(
                        "{flag} needs a Flockfile to reset to; {token} names a sheep, not a file"
                    );
                    return streams.fail(ExitCode::Usage, &message);
                }
                // Instances of one name are one sheep; two names are two, and
                // an assignment names neither of them. Recorded before the
                // resume below, which is the spawn that promotes it.
                if !assignments.is_empty() {
                    let names: BTreeSet<&str> =
                        matched.iter().map(|info| info.name.as_str()).collect();
                    if names.len() > 1 {
                        let message = format!(
                            "an assignment takes a script path or one sheep; {token} names several"
                        );
                        return streams.fail(ExitCode::Usage, &message);
                    }
                    for name in &names {
                        if let Err(reason) = set_assignments(client, name, assignments).await {
                            return streams.fail(ExitCode::Failure, &reason);
                        }
                    }
                }
                return match mode {
                    Load::Start => {
                        resume_all(client, streams, Some(token), &matched, started).await
                    }
                    // The sheep is registered, which is all `add` was asked
                    // for. Said anyway, so a printed table is explained.
                    Load::Add => already_registered(streams, &matched),
                };
            }
            // Held for the failure path below rather than reported here: the
            // token may still name a Flockfile or a path.
            missed = selector_miss(token, &selector, &flock);
            listing = Some(flock);
        }
    }

    let discovered = discovered.map(|p| p.to_string_lossy().into_owned());
    let target: &str = match (target, discovered.as_deref()) {
        (Some(target), _) => target,
        (None, Some(found)) => found,
        (None, None) => {
            let message = "no target and no Flockfile in this directory";
            return streams.fail(ExitCode::Usage, message);
        }
    };

    let stdin = if target == "-" {
        let mut buf = Vec::new();
        if let Err(source) = std::io::stdin().lock().read_to_end(&mut buf) {
            return fail_target(streams, &TargetError::Stdin(source));
        }
        buf
    } else {
        Vec::new()
    };

    let mut apps =
        match resolve_target_declared(target, args.name.as_deref(), &stdin, args.flockfile) {
            Ok(apps) => apps,
            // The token named nothing anywhere. One written unmistakably as
            // a selector is reported as one, at exit 3, matching every other
            // verb's answer for a selector that matched nothing.
            Err(TargetError::Unresolvable { .. }) if missed.is_some() => {
                let message = missed.unwrap_or_default();
                return streams.fail(ExitCode::NotFound, &message);
            }
            Err(err) => return fail_target(streams, &err),
        };

    // The other target that supplies no template: a bare script path gets
    // an empty declared set, so a reset would exit 0 having reset nothing.
    // Tested on the declared set, not the token's shape, since that is what
    // a reset acts on. Every Flockfile app declares at least `script`.
    if let Some(flag) = reset_flag(args)
        && apps.iter().all(|app| app.declared.is_empty())
    {
        let message =
            format!("{flag} needs a Flockfile to reset to; {target} is a script, not a Flockfile");
        return streams.fail(ExitCode::Usage, &message);
    }

    // A file may declare several apps and an assignment names none of them,
    // so nothing says which one the operator meant. Tested on the declared
    // set for the reason the reset above is: it is what a file supplies.
    if !assignments.is_empty() && apps.iter().any(|app| !app.declared.is_empty()) {
        let message = format!(
            "an assignment takes a script path or one sheep; {target} is a Flockfile, which \
             may declare several"
        );
        return streams.fail(ExitCode::Usage, &message);
    }

    if let Some(fold) = &args.fold {
        for app in &mut apps {
            app.config.fold = Some(fold.clone());
        }
    }
    // After the per-app defaults above, so an explicit flag wins over both
    // the script form's default and a Flockfile's own value.
    if let Some(cwd) = &args.cwd {
        for app in &mut apps {
            app.config.cwd = Some(cwd.clone());
        }
    }
    // After the file's own values, since an assignment is something an
    // operator typed for this one sheep.
    for app in &mut apps {
        for (key, value) in assignments {
            app.config.env.insert(key.clone(), value.clone());
        }
    }
    apply_interpreters(&mut apps, interpreters, args.interpreter.as_deref());
    // The bare-name rule again, now the target is a set of named apps: a
    // name the flock already has is that sheep. Without it `shep start
    // ./thing` spawns a second copy of a one-instance app.
    let flock = match listing {
        Some(flock) => flock,
        None => flock_now(client).await,
    };
    // Every row the name has, not the first: respawns go out per row, so one
    // row standing in for a clustered app leaves the other instances down.
    let mut resumed: Vec<(DeclaredApp, Vec<ProcessInfo>)> = Vec::new();
    let mut fresh = Vec::new();
    for app in apps {
        let rows: Vec<ProcessInfo> = flock
            .iter()
            .filter(|info| info.name == app.config.name)
            .cloned()
            .collect();
        if rows.is_empty() {
            fresh.push(app);
        } else {
            resumed.push((app, rows));
        }
    }
    // Before the resumes below, since a `NeedsRespawn` field parks for the
    // next spawn and that is the resume immediately below. The code is
    // carried rather than returned: a refused field must not stop the flock
    // coming back up. The merge runs under `add` too, only the resume does not.
    let declared: Vec<DeclaredApp> = resumed.iter().map(|(app, _)| app.clone()).collect();
    let applied = apply_declared(client, streams, &declared, reset_depth(args), mode).await;
    if !resumed.is_empty() && mode == Load::Start {
        // `None`, not the operator's token: this arm reached the flock
        // through a Flockfile or a path, so there is no selector to quote.
        let existing: Vec<ProcessInfo> = resumed
            .iter()
            .flat_map(|(_, rows)| rows.iter().cloned())
            .collect();
        let code = resume_all(client, streams, None, &existing, started).await;
        if code != ExitCode::Success {
            return first_failure(applied, code);
        }
    }
    if fresh.is_empty() {
        return applied;
    }
    let apps: Vec<AppConfig> = fresh.iter().map(|app| app.config.clone()).collect();

    let (procs, failure) = request_each(
        client,
        streams, // Both requests carry the apps, so this "selector" is a
        // placeholder the body closure ignores.
        &[SelectorSpec::All],
        Some(match mode {
            // `add` registers and starts nothing, so it runs no stages and
            // needs no more than the batch budget. `START_DEADLINE`: a
            // single-threaded actor behind a batch of cold spawns outruns the
            // client's default 5s on its own.
            Load::Add => START_DEADLINE,
            Load::Start => staged_start_deadline(&apps),
        }),
        |_| match mode {
            Load::Start => Request::Start { apps: apps.clone() },
            Load::Add => Request::Add { apps: apps.clone() },
        },
        |response| match response {
            Response::Started(procs) | Response::Added(procs) => Some(procs),
            _ => None,
        },
    )
    .await;
    // Establishes what a fresh app's file declared: `Start` and `Add` write
    // nothing to the override store. `ResetDepth::None` whatever flag was
    // passed, since a reset would drop the record this call writes. Only
    // apps the daemon registered: a failed spawn is not in `procs`.
    let registered: BTreeSet<&str> = procs.iter().map(|info| info.name.as_str()).collect();
    let established: Vec<DeclaredApp> = fresh
        .into_iter()
        .filter(|app| registered.contains(app.config.name.as_str()))
        .collect();
    let recorded = apply_declared(client, streams, &established, ResetDepth::None, mode).await;
    // `Start` and `Add` write nothing to the override store, so a value only
    // the config carries is one the next Flockfile load replaces. The sheep
    // is up with it either way, which is what makes this a notice.
    for name in &registered {
        if let Err(reason) = set_assignments(client, name, assignments).await {
            let message = format!(
                "{reason}; it is set for this spawn, but a Flockfile load could replace it"
            );
            streams.aside(mode.verb(), &message);
        }
    }
    started.extend(procs);
    first_failure(
        applied,
        first_failure(failure.unwrap_or(ExitCode::Success), recorded),
    )
}

