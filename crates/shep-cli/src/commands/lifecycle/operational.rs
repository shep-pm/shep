//! The selector-driven operational verbs.
//!
//! `stop`, `restart`, `reload`, `delete` and `stock`, plus the refusal
//! rendering they share for a staged walk over a fold.

use std::collections::BTreeSet;
use std::time::Duration;

use shep_client::{Client, RELOAD_DEADLINE, START_DEADLINE};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec, SheepRefusal};

use crate::cli::{Format, SelectorArgs, StockArgs};
use crate::commands::dogs;
use crate::commands::lifecycle::selectors::{parse_selectors, render_outcome, request_each};
use crate::commands::rpc::request_payload;
use crate::exit::ExitCode;
use crate::output::{DeletedIds, FlockRows, Streams, emit_partial, write_outcome};

/// Stops the sheep matching `args.selector`.
pub async fn stop(client: &Client, streams: &mut Streams<'_>, args: &SelectorArgs) -> ExitCode {
    let selectors = match parse_selectors(streams, &args.selectors) {
        Ok(selectors) => selectors,
        Err(code) => return code,
    };
    let (procs, failure) = request_each(
        client,
        streams,
        &selectors,
        None,
        |selector| Request::Stop { selector },
        |response| match response {
            Response::Stopped(procs) => Some(procs),
            _ => None,
        },
    )
    .await;
    // Printed whenever the verb did its work; see `load`.
    if !procs.is_empty() || failure.is_none() {
        let wrote = render_outcome(client, streams, "stop", FlockRows(procs)).await;
        if wrote != ExitCode::Success {
            return wrote;
        }
    }
    failure.unwrap_or(ExitCode::Success)
}

/// Restarts the sheep matching `args.selector`
///
/// `paths` is here so a restart that names a dog is warned about before the
/// request goes out. See [`dogs::warn_of_a_dog_a_restart_would_break`].
///
/// Sent with [`START_DEADLINE`]: the daemon holds each stage for its
/// members' `listen_timeout`, so a client on the default abandons a restart.
pub async fn restart(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: Option<&ShepPaths>,
    args: &SelectorArgs,
) -> ExitCode {
    restart_within(client, streams, paths, args, dogs::VERSION_BUDGET).await
}

/// [`restart`], against a caller-chosen budget for the dog probe below
///
/// A timed-out probe answers unknown and unknown is silent, so at the
/// production budget a busy machine turns a test's subject into thin air.
pub async fn restart_within(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: Option<&ShepPaths>,
    args: &SelectorArgs,
    probe: Duration,
) -> ExitCode {
    let selectors = match parse_selectors(streams, &args.selectors) {
        Ok(selectors) => selectors,
        Err(code) => return code,
    };
    // Before `request_each`: a warning that arrives after the restart is a
    // description of a state the operator is already in.
    if let Some(paths) = paths {
        dogs::warn_of_a_dog_a_restart_would_break(streams, paths, &selectors, probe);
        warn_of_a_dog_a_restart_would_not_change(client, streams, paths, &selectors).await;
    }
    let mut refused: Vec<SheepRefusal> = Vec::new();
    let (procs, failure) = request_each(
        client,
        streams,
        &selectors,
        Some(START_DEADLINE),
        |selector| Request::Restart { selector },
        |response| match response {
            Response::Restarted {
                accepted,
                refused: rows,
            } => {
                refused.extend(rows);
                Some(accepted)
            }
            _ => None,
        },
    )
    .await;
    // Named before `procs` is moved into the table below. The `Restart`
    // reply has no per-id error slot, so a failed respawn reaches here as an
    // ordinary `errored` row inside an `Ok`, and not as a refusal: the sheep
    // was reached, it is the child that could not exec.
    let failed: Vec<String> = procs
        .iter()
        .filter(|info| info.status == shep_core::status::ProcStatus::Errored)
        .map(|info| info.name.clone())
        .collect();

    // Stdout stays empty on a failure, as every verb's failure path does.
    // The cost is that a `restart all` where one of ten fails lists none of
    // the nine that came back.
    let carried = (!procs.is_empty() || failure.is_none()) && failed.is_empty();
    if carried {
        let wrote = render_partial(client, streams, "restart", procs, &refused).await;
        if wrote != ExitCode::Success {
            return wrote;
        }
    }

    if !failed.is_empty() {
        let names = failed.join(", ");
        let mut message = format!(
            "{names} did not come back up; see `shep bleats {}` or its log files for why",
            failed[0]
        );
        // The refused apps have nowhere else to go from here. Stdout stayed
        // empty, so the `--format json` envelope that would have carried
        // them was never printed, and a second error object is what
        // `cli.rs`' one-object-per-invocation rule forbids.
        if let Some(refusal) = refused_line("restart", &refused) {
            message.push_str("; ");
            message.push_str(&refusal);
        }
        return streams.fail(ExitCode::SpawnFailed, &message);
    }

    finish_walk(streams, "restart", &refused, carried, failure)
}

/// Warns, before `restart` sends anything, about an adopted dog whose entry
/// in `shep.toml` differs from the one it was spawned with.
///
/// A restart respawns a dog from the spec it already holds, and the shepherd
/// never reads `shep.toml`, so the changed `path` or `env` does not apply.
/// `shep disable` then `shep enable` does. A warning, never a refusal: the
/// restart still happens.
///
/// Only a `Name` selector is checked, as for the probe above.
async fn warn_of_a_dog_a_restart_would_not_change(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    selectors: &[SelectorSpec],
) {
    let named: BTreeSet<String> = selectors
        .iter()
        .filter_map(|selector| match selector {
            SelectorSpec::Name(name) => Some(name.clone()),
            _ => None,
        })
        .collect();
    if named.is_empty() {
        return;
    }
    for kept in dogs::kept_entries(client, paths, Some(&named)).await {
        streams.aside(dogs::DOG_ENTRY_KEPT_NOTICE, &dogs::kept_entry_report(&kept));
    }
}

/// Reloads the sheep matching `args.selector`, replacing each instance with
/// a fresh one so the app has a window in which it can hand over
///
/// The rows printed are acceptances: a selector matching several is walked
/// in stages daemon-side, so the table is stitched from one per stage.
///
/// Sent with [`RELOAD_DEADLINE`]: the walk holds each stage until its swaps land.
pub async fn reload(client: &Client, streams: &mut Streams<'_>, args: &SelectorArgs) -> ExitCode {
    let selectors = match parse_selectors(streams, &args.selectors) {
        Ok(selectors) => selectors,
        Err(code) => return code,
    };
    let mut refused: Vec<SheepRefusal> = Vec::new();
    let (procs, failure) = request_each(
        client,
        streams,
        &selectors,
        Some(RELOAD_DEADLINE),
        |selector| Request::Reload { selector },
        |response| match response {
            Response::Reloading {
                accepted,
                refused: rows,
            } => {
                refused.extend(rows);
                Some(accepted)
            }
            _ => None,
        },
    )
    .await;
    // Printed whenever the verb did its work; see `load`. The refused apps
    // are named after it, not instead of it: what did reload is still the
    // answer to the question the operator asked.
    let carried = !procs.is_empty() || failure.is_none();
    if carried {
        let wrote = render_partial(client, streams, "reload", procs, &refused).await;
        if wrote != ExitCode::Success {
            return wrote;
        }
    }
    finish_walk(streams, "reload", &refused, carried, failure)
}

/// [`render_outcome`] for a staged walk, which can have both halves of an
/// answer to render at once.
///
/// The table half is [`render_outcome`] unchanged: a fresh flock listing,
/// with the refused apps named on stderr by the caller. The JSON half is one
/// envelope carrying both, since `cli.rs` publishes `--format json` as one
/// object per invocation.
async fn render_partial(
    client: &Client,
    streams: &mut Streams<'_>,
    command: &str,
    accepted: Vec<ProcessInfo>,
    refused: &[SheepRefusal],
) -> ExitCode {
    if streams.fmt == Format::Json {
        return write_outcome(emit_partial(
            &mut *streams.out,
            command,
            FlockRows(accepted),
            refused,
        ));
    }
    render_outcome(client, streams, command, FlockRows(accepted)).await
}

/// What a refused app inside an otherwise successful walk exits with
///
/// A staged reload or restart asks the shepherd per app, so `shep reload
/// all` can reload thirty-nine and be refused the fortieth.
///
/// Not [`ExitCode::Internal`]: that code tells an operator to look for a
/// daemon bug where the fold reloaded around one busy app.
const WALK_REFUSED_EXIT: ExitCode = ExitCode::Failure;

/// The tail `restart` and `reload` share: what a walk that refused part of
/// its work prints, and what it exits with
///
/// Under `--format json` the refusals rode out in the envelope
/// [`render_partial`] printed, so stderr names them only when it never printed.
///
/// `failure` is the first per-selector failure `request_each` reported.
fn finish_walk(
    streams: &mut Streams<'_>,
    verb: &str,
    refused: &[SheepRefusal],
    carried: bool,
    failure: Option<ExitCode>,
) -> ExitCode {
    let refusal = refused_line(verb, refused).map(|message| {
        if streams.fmt == Format::Json && carried {
            WALK_REFUSED_EXIT
        } else {
            streams.fail(WALK_REFUSED_EXIT, &message)
        }
    });
    failure.or(refusal).unwrap_or(ExitCode::Success)
}

/// One line for the apps `verb` could not reach, or `None` when it reached
/// every one it named
///
/// `did not <verb>: name: reason`, carrying the `name: reason` shape
/// [`super::configure::applied_line`] prints a load's refusal in. The
/// reason is the shepherd's own sentence.
pub(crate) fn refused_line(verb: &str, refused: &[SheepRefusal]) -> Option<String> {
    if refused.is_empty() {
        return None;
    }
    let listed: Vec<String> = refused
        .iter()
        .map(|sheep| format!("{}: {}", sheep.name, sheep.reason))
        .collect();
    Some(format!("did not {verb}: {}", listed.join("; ")))
}

/// Deletes (stops and deregisters) the sheep matching `args.selectors`
///
/// The sheep half of `shep delete`; see [`super::remove::delete`] for the
/// verb an operator types, which sends a dog's name elsewhere.
pub async fn delete_sheep(
    client: &Client,
    streams: &mut Streams<'_>,
    args: &SelectorArgs,
) -> ExitCode {
    let selectors = match parse_selectors(streams, &args.selectors) {
        Ok(selectors) => selectors,
        Err(code) => return code,
    };
    let (ids, failure) = request_each(
        client,
        streams,
        &selectors,
        None,
        |selector| Request::Delete { selector },
        |response| match response {
            Response::Deleted(ids) => Some(ids),
            _ => None,
        },
    )
    .await;
    // Printed whenever the verb did its work; see `load`. The aside below is
    // guarded separately: there is nothing to name when nothing was deleted.
    if !ids.is_empty() || failure.is_none() {
        // The listing this prints does not hold what was deleted, so the
        // ids go to stderr. Ids and not names: ids are all
        // `Response::Deleted` carries.
        let count = ids.len();
        let listed = ids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<String>>()
            .join(", ");
        if count > 0 && streams.fmt != Format::Json {
            let message = match count {
                1 => format!("deleted 1 sheep, id {listed}"),
                n => format!("deleted {n} sheep, ids {listed}"),
            };
            streams.aside("delete", &message);
        }
        let wrote = render_outcome(client, streams, "delete", DeletedIds(ids)).await;
        if wrote != ExitCode::Success {
            return wrote;
        }
    }
    failure.unwrap_or(ExitCode::Success)
}

/// Sets `args.name`'s instance count (the stocking rate), and renders the
/// instances that remain
///
/// No `parse_selector` call, unlike every other verb here: `stock` takes a
/// name. `START_DEADLINE` rather than the client's default, since a stock-up
/// spawns processes.
pub async fn stock(client: &Client, streams: &mut Streams<'_>, args: &StockArgs) -> ExitCode {
    let body = Request::Scale {
        name: args.name.clone(),
        count: args.count,
    };
    let rows =
        request_payload(
            client,
            streams,
            body,
            Some(START_DEADLINE),
            |response| match response {
                Response::Scaled(procs) => Some(FlockRows(procs)),
                _ => None,
            },
        )
        .await;
    match rows {
        Ok(rows) => render_outcome(client, streams, "stock", rows).await,
        Err(code) => code,
    }
}
