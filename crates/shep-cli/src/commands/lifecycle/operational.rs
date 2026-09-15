//! The selector-driven operational verbs.
//!
//! `stop`, `restart`, `reload`, `delete` and `stock`, plus the refusal
//! rendering they share for a staged walk over a fold.

use std::time::Duration;

use shep_client::{Client, RELOAD_DEADLINE, START_DEADLINE};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{ProcessInfo, Request, Response, SheepRefusal};

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
/// Sent with [`START_DEADLINE`] rather than the client's 5s default. A
/// restart matching several sheep now walks the dependency stages INSIDE the
/// request handler, and one edge routinely clears 5s: the daemon holds each
/// stage for its members' `listen_timeout` before issuing the next, so a
/// client on the default abandons a restart the shepherd is still doing.
/// Not [`super::staged_start_deadline`], which needs the `AppConfig`s a load
/// holds
/// and a selector does not: nothing the CLI has says how many stages this
/// selector spans or what their timeouts are, and asking would cost a round
/// trip and still race the actor. The daemon clamps at its own 60s ceiling
/// either way.
///
/// That walk asks the shepherd per app, so an app it could not restart is
/// named on stderr and exits [`WALK_REFUSED_EXIT`] with the rest of the fold
/// still printed, exactly as [`reload`] does. Distinct from a respawn that
/// could not exec, which is an `errored` row inside an `Ok` and exits
/// [`ExitCode::SpawnFailed`]; both can happen in one invocation, and the
/// spawn failure is the louder of the two.
pub async fn restart(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
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
    paths: &ShepPaths,
    args: &SelectorArgs,
    probe: Duration,
) -> ExitCode {
    let selectors = match parse_selectors(streams, &args.selectors) {
        Ok(selectors) => selectors,
        Err(code) => return code,
    };
    // Before `request_each`: a warning that arrives after the restart is a
    // description of a state the operator is already in.
    dogs::warn_of_a_dog_a_restart_would_break(streams, paths, &selectors, probe);
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
        // `cli.rs`' one-object-per-invocation rule forbids. They ride this
        // sentence rather than being dropped, which is the whole point of
        // carrying them.
        if let Some(refusal) = refused_line("restart", &refused) {
            message.push_str("; ");
            message.push_str(&refusal);
        }
        return streams.fail(ExitCode::SpawnFailed, &message);
    }

    // Under `--format json` the refusals rode out in the envelope above, so
    // saying them again on stderr is the second top-level object that guard
    // exists to stop; `reload` carries the same one and for the same reason.
    let refusal = refused_line("restart", &refused).map(|message| {
        if streams.fmt == Format::Json && carried {
            WALK_REFUSED_EXIT
        } else {
            streams.fail(WALK_REFUSED_EXIT, &message)
        }
    });
    failure.or(refusal).unwrap_or(ExitCode::Success)
}

/// Reloads the sheep matching `args.selector`, replacing each instance with
/// a fresh one so the app has a window in which it can hand over
///
/// The rows printed are acceptances. One sheep is the flock as it stood when
/// its reload was accepted; a selector matching several is walked in stages
/// daemon-side, so the table is stitched from one acceptance per stage and
/// the earlier stages' swaps have already happened by the time it prints.
///
/// Sent with [`RELOAD_DEADLINE`], for `restart`'s reason and then some. A
/// reload matching several sheep no longer answers at acceptance: the staged
/// walk runs in the request handler and holds each stage until its swaps
/// land, so the reply waits on a drain AND a readiness wait per stage, which
/// is the longest budget any of these verbs needs. The client's 5s default
/// would abandon a fold with one edge as a matter of routine, and
/// [`START_DEADLINE`]'s 30s is provably short too: two stages at the default
/// timeouts already cost more than that, and `with_deadline` DROPS the walk,
/// so the later stages are never issued and the operator holds a timeout
/// over a half-reloaded fold. 60s is the daemon's own ceiling, so asking for
/// it costs nothing the shepherd would not already honour.
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
    // Under `--format json` the refusals rode out in the envelope above, so
    // saying them again on stderr is the second top-level object this guard
    // exists to stop. `carried` is the condition and not the format alone:
    // an envelope that never printed took the names with it, and stderr is
    // the only stream left to name them on.
    let refusal = refused_line("reload", &refused).map(|message| {
        if streams.fmt == Format::Json && carried {
            WALK_REFUSED_EXIT
        } else {
            streams.fail(WALK_REFUSED_EXIT, &message)
        }
    });
    failure.or(refusal).unwrap_or(ExitCode::Success)
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
/// all` can reload thirty-nine and be refused the fortieth. Not the
/// [`ExitCode::Internal`] a single-target refusal exits with: that code is
/// what the wire spells a conflict as for want of one of its own, and it
/// tells an operator to go looking for a daemon bug where the fold in fact
/// reloaded around one busy app.
const WALK_REFUSED_EXIT: ExitCode = ExitCode::Failure;

/// One line for the apps `verb` could not reach, or `None` when it reached
/// every one it named
///
/// `did not <verb>: name: reason`, carrying the `name: reason` shape
/// [`super::configure::applied_line`] prints a load's refusal in, so every
/// verb that can
/// refuse part of its work reads alike. The reason is the shepherd's own
/// sentence.
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

/// Deletes (stops and deregisters) the sheep matching `args.selector`.
pub async fn delete(client: &Client, streams: &mut Streams<'_>, args: &SelectorArgs) -> ExitCode {
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
