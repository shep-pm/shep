//! Staged, dependency-ordered restart and reload.
//!
//! [`ordered_walk`] groups a selector's matches into stages by
//! dependency depth; [`restart_in_stages`] and [`reload_in_stages`]
//! run each stage in turn and collect every refusal rather than
//! stopping at the first.

use core::time::Duration;

use std::collections::{BTreeMap, BTreeSet};

use shep_core::protocol::{ProcessInfo, Reply, Response, RpcError, SelectorSpec, SheepRefusal};
use shep_core::selector::ProcessSelector;
use shep_core::status::ProcStatus;

use crate::supervisor::SupervisorError;

use super::context::{Outcome, RpcContext};
use super::error::rpc_error;
use super::selector_verbs::selector_of;

/// A restart or reload the selector matched more than one sheep for, grouped
/// into the stages it walks.
pub(super) struct OrderedWalk {
    /// The matched names, dependencies first, one `Vec` per stage.
    pub(super) stages: Vec<Vec<String>>,
    /// The matched names another matched name waits for.
    ///
    /// The only ones a stage has to be held for: a sheep nothing in this
    /// request depends on can settle after the walk has moved on, and waiting
    /// for it would let one slow app that nobody waits for cost every later
    /// stage its bound. The same gate `start_in_stages` builds.
    pub(super) depended_on: BTreeSet<String>,
}

/// How `selector` is walked, or `None` when it names at most one sheep and
/// there is nothing to order.
///
/// A single-target `shep restart web` therefore goes out as the one
/// supervisor call it has always been, deadlines and refusal and all. Two
/// matches or more is where a fold's shape starts to matter.
///
/// The match is computed the way `Actor::matching_ids` computes it, dogs and
/// all, so the walk covers what the supervisor would have matched. It is
/// still a second pass over a listing the actor has since moved on from: a
/// sheep that exits between the two is one this walk names and the supervisor
/// no longer matches, which costs a `NotFound` warning for that name.
pub(super) fn ordered_walk(
    ctx: &RpcContext,
    selector: &ProcessSelector,
    flock: &[ProcessInfo],
) -> Option<OrderedWalk> {
    let exact = selector.is_exact();
    let matched: BTreeSet<String> = flock
        .iter()
        .filter(|info| exact || info.dog.is_none())
        .filter(|info| selector.matches(&info.name, info.id, info.fold.as_deref(), info.instance))
        .map(|info| info.name.clone())
        .collect();
    if matched.len() < 2 {
        return None;
    }

    let edges = ctx.registry.depends_on_by_name();
    let plan = crate::boot_order::plan_for_names(&edges);
    let mut stages: Vec<Vec<String>> = plan
        .stages
        .iter()
        .map(|stage| {
            stage
                .iter()
                .filter(|name| matched.contains(name.as_str()))
                .cloned()
                .collect::<Vec<String>>()
        })
        .filter(|stage| !stage.is_empty())
        .collect();
    // A matched name the registry does not hold has no node in the plan, and
    // dropping it here would leave a sheep the operator named unrestarted
    // while the reply says otherwise. It goes last, since nothing here can say
    // what waits for it. A `shep dev` teardown clearing the registry under a
    // live flock is the shape that gets here; a dog does not, since a selector
    // reaching one is exact and an exact selector matches one name.
    let placed: BTreeSet<&str> = stages.iter().flatten().map(String::as_str).collect();
    let unplaced: Vec<String> = matched
        .iter()
        .filter(|name| !placed.contains(name.as_str()))
        .cloned()
        .collect();
    if !unplaced.is_empty() {
        stages.push(unplaced);
    }

    let depended_on = matched
        .iter()
        .flat_map(|name| matched_dependencies(&edges, name, &matched))
        .collect();
    Some(OrderedWalk {
        stages,
        depended_on,
    })
}

/// Every name in `matched` that `start` waits for, however many hops away.
///
/// The transitive walk is the point. A direct-edge intersection loses a
/// dependency whose intermediate the selector missed: with `web -> mid ->
/// db` matched at the ends only, `mid` is not in `matched`, so `web` reads
/// as waiting for nothing and its stage restarts against a `db` no stage
/// held. A regex or a fold selector is how that shape arrives; `All` cannot
/// draw it, since it matches every hop.
///
/// `start` itself is never returned, so a name in a knot does not end up
/// waiting for its own stage.
fn matched_dependencies(
    edges: &BTreeMap<String, Vec<String>>,
    start: &str,
    matched: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut seen: BTreeSet<&str> = [start].into_iter().collect();
    let mut frontier: Vec<&str> = vec![start];
    while let Some(name) = frontier.pop() {
        for dependency in edges.get(name).into_iter().flatten() {
            if !seen.insert(dependency.as_str()) {
                continue;
            }
            if matched.contains(dependency.as_str()) {
                found.insert(dependency.clone());
            }
            frontier.push(dependency.as_str());
        }
    }
    found
}

/// How long ONE INSTANCE gets to settle, before the stage's slack.
///
/// A restart waits out `listen_timeout`; a reload waits out
/// `graceful_timeout` as well, which is the pair its own swap is already
/// bounded by (`Actor::arm_reload_deadline`). A name the registry does not
/// hold falls back to nothing at all, leaving its stage the slack: nothing
/// else here knows what that sheep's deadlines are, and a stage that
/// advances early is better than one that hangs on a guess.
fn settle_bound(ctx: &RpcContext, name: &str, reloading: bool) -> Duration {
    let (readiness, drain) = ctx.registry.timeouts_of(name).unwrap_or_default();
    let drain = if reloading { drain } else { Duration::ZERO };
    readiness + drain
}

/// The longest bound any of a restart stage's members asks for.
///
/// One instance's worth, unlike [`reload_stage_bound`]: a restart's
/// instances go down and come back together, so the stage costs the
/// slowest of them rather than the sum.
fn stage_bound(ctx: &RpcContext, stage: &[String]) -> Duration {
    stage
        .iter()
        .map(|name| settle_bound(ctx, name, false))
        .max()
        .unwrap_or_default()
        + crate::boot_order::STAGE_SLACK
}

/// [`stage_bound`] for a reload, sized by the swaps each member still owes.
///
/// `advance_reload` replaces one instance at a time, so a three-instance app
/// costs three drains and three readiness waits, not one of each. A per-app
/// bound is under a third of that at the defaults, so the stage times out,
/// logs, and lets the dependant reload against a dependency that is still
/// half swapped, which is the failure the walk exists to prevent, reached
/// quietly.
///
/// The counts come from `waiting`, which is the instances the reload
/// answered as `Online`. An instance a failed earlier reload left up and not
/// serving reads `Starting` there and is missing from the count, so this
/// bound can only be short, never long: such a stage can advance early and
/// can never hang.
pub(super) fn reload_stage_bound(ctx: &RpcContext, waiting: &BTreeMap<String, usize>) -> Duration {
    waiting
        .iter()
        .map(|(name, swaps)| {
            settle_bound(ctx, name, true).saturating_mul(u32::try_from(*swaps).unwrap_or(u32::MAX))
        })
        .max()
        .unwrap_or_default()
        + crate::boot_order::STAGE_SLACK
}

/// `Restart`'s arm: ordered when the selector matches several sheep, the
/// plain supervisor call when it matches one.
///
/// The refused half of the reply is a walk's alone: the supervisor refuses a
/// selector matching one app whole, so that arm answers `Err` and has
/// nothing to name.
pub(super) async fn restart_request(id: u64, spec: SelectorSpec, ctx: &RpcContext) -> Outcome {
    let result = match selector_of(spec) {
        Err(err) => Err(err),
        Ok(selector) => match walk_for(ctx, &selector).await {
            None => ctx
                .supervisor
                .restart(selector)
                .await
                .map(|accepted| Response::Restarted {
                    accepted,
                    refused: Vec::new(),
                })
                .map_err(|err| rpc_error(&err)),
            Some(walk) => restart_in_stages(ctx, &walk)
                .await
                .map(|(accepted, refused)| Response::Restarted { accepted, refused }),
        },
    };
    Outcome::Reply(Reply { id, result })
}

/// `Reload`'s arm, mirroring [`restart_request`] down to the refused half of
/// its reply.
pub(super) async fn reload_request(id: u64, spec: SelectorSpec, ctx: &RpcContext) -> Outcome {
    let result = match selector_of(spec) {
        Err(err) => Err(err),
        Ok(selector) => match walk_for(ctx, &selector).await {
            None => ctx
                .supervisor
                .reload(selector)
                .await
                .map(|accepted| Response::Reloading {
                    accepted,
                    refused: Vec::new(),
                })
                .map_err(|err| rpc_error(&err)),
            Some(walk) => reload_in_stages(ctx, &walk)
                .await
                .map(|(accepted, refused)| Response::Reloading { accepted, refused }),
        },
    };
    Outcome::Reply(Reply { id, result })
}

/// [`ordered_walk`] over a fresh listing, or `None` when there is nothing to
/// order, which includes an actor that could not answer: the supervisor call
/// the caller falls back to reports that failure itself, in its own words.
pub(super) async fn walk_for(ctx: &RpcContext, selector: &ProcessSelector) -> Option<OrderedWalk> {
    let flock = ctx.supervisor.list_checked().await.ok()?;
    ordered_walk(ctx, selector, &flock)
}

/// Restarts each stage's members at once, then holds the walk until the ones
/// a later stage waits for are back.
///
/// A member that fails is warned about and the walk continues, the rule
/// `start_in_stages` takes under `BatchPolicy::PerApp` and for its reason: a
/// fold half restarted is worse than a fold restarted around one bad app.
///
/// Every member that was refused is named in the second half of the answer,
/// the rule `reload_in_stages` states in full: a walk that restarted
/// something still answers `Ok`, and exit 0 with a row silently missing is
/// what carrying the names replaces.
///
/// # Errors
///
/// - The first member's refusal, when no member restarted at all. A request
///   that moved nothing has to say so, and a selector matching one dog that
///   is mid-shutdown would otherwise answer `Ok` with an empty table.
pub(super) async fn restart_in_stages(
    ctx: &RpcContext,
    walk: &OrderedWalk,
) -> Result<(Vec<ProcessInfo>, Vec<SheepRefusal>), RpcError> {
    let mut restarted = Vec::new();
    let mut refused: Vec<SheepRefusal> = Vec::new();
    let mut refusal = None;
    for stage in &walk.stages {
        // Subscribed before the calls for the reason `start_in_stages` gives:
        // a receiver taken afterwards starts past a fast sheep's `Online`.
        let rx = ctx.events.subscribe();
        // Concurrently inside a stage, serially across them. Awaiting members
        // in turn would make the walk cost the SUM of their kill ladders and
        // readiness deadlines where an unordered restart costs the longest
        // one; `stop_in_reverse` carries the same note.
        let outcomes = futures_util::future::join_all(stage.iter().map(|name| async move {
            (
                name,
                ctx.supervisor
                    .restart(ProcessSelector::Name(name.clone()))
                    .await,
            )
        }))
        .await;
        for (name, outcome) in outcomes {
            match outcome {
                Ok(infos) => restarted.extend(infos),
                // A `NotFound` here is not necessarily a defect: the walk
                // was planned from a listing the actor has since moved on
                // from, so a sheep that exited in between is one this names
                // and the supervisor no longer matches. The message says so
                // rather than sending an operator looking for a bug.
                Err(err) => {
                    tracing::warn!(
                        sheep = %name,
                        %err,
                        "a sheep did not restart in its stage; it may have left the flock since \
                         the walk was planned"
                    );
                    // Every one, where `refusal` below keeps the first:
                    // that one is only ever read when nothing restarted.
                    refused.push(SheepRefusal::new(name.clone(), err.to_string()));
                    refusal.get_or_insert(err);
                }
            }
        }

        let waiting: BTreeSet<String> = stage
            .iter()
            .filter(|name| walk.depended_on.contains(name.as_str()))
            .cloned()
            .collect();
        if waiting.is_empty() {
            continue;
        }
        let bound = stage_bound(ctx, stage);
        let unsettled = crate::boot_order::await_stage(rx, waiting, bound, &ctx.supervisor).await;
        warn_about_unsettled(&unsettled);
    }
    finished(restarted, refusal).map(|rows| (rows, refused))
}

/// [`restart_in_stages`] for a reload: same walk, a different call and a
/// different definition of done.
///
/// A stage is done when every member has emitted a `Reloaded` for each
/// instance the reload accepted, or a `ReloadAbandoned` for the app. Which of
/// the two reloads an app gets is still `ReloadMode::of`'s call, and the
/// abandonment path is untouched: this waits on it rather than around it.
///
/// Only the instances the reload answered as `Online` are counted, since
/// those are the ones `reload_eligible` lets a swap replace. An instance a
/// failed earlier reload left up and not serving is `Starting` here and is
/// undercounted, so a stage holding one can advance while its last swap is
/// still running; the alternative is counting instances no swap will ever
/// reach and paying the bound on every ordinary reload.
///
/// Every member that was refused is named in the second half of the answer,
/// so the client can print the apps this walk went around and exit non-zero
/// over them. A walk that reloaded something still answers `Ok`: refusing
/// forty apps because one is busy is the whole-selector rule this walk
/// exists to get away from, and exit 0 with a row silently missing is what
/// carrying the names replaces.
///
/// # Errors
///
/// - The first member's refusal, when no member was accepted. `ReloadInFlight`
///   is the one an operator meets: the supervisor refuses a selector whole
///   when any app it names is already reloading, and a staged walk asks per
///   app, so an app already reloading now refuses its own stage while the
///   rest of the fold goes ahead. Its stage is still held for the reload
///   already running, which a dependant has the same reason to wait out.
async fn reload_in_stages(
    ctx: &RpcContext,
    walk: &OrderedWalk,
) -> Result<(Vec<ProcessInfo>, Vec<SheepRefusal>), RpcError> {
    let mut accepted = Vec::new();
    let mut refused: Vec<SheepRefusal> = Vec::new();
    let mut refusal = None;
    for stage in &walk.stages {
        let rx = ctx.events.subscribe();
        let outcomes = futures_util::future::join_all(stage.iter().map(|name| async move {
            (
                name,
                ctx.supervisor
                    .reload(ProcessSelector::Name(name.clone()))
                    .await,
            )
        }))
        .await;
        let mut waiting: BTreeMap<String, usize> = BTreeMap::new();
        for (name, outcome) in outcomes {
            match outcome {
                Ok(infos) => {
                    if walk.depended_on.contains(name.as_str()) {
                        let swaps = infos
                            .iter()
                            .filter(|info| info.status == ProcStatus::Online)
                            .count();
                        if swaps > 0 {
                            waiting.insert(name.clone(), swaps);
                        }
                    }
                    accepted.extend(infos);
                }
                // `restart_in_stages`' note about a `NotFound` applies here
                // too.
                Err(err) => {
                    // An app already reloading is refused per app now, so it
                    // contributes no swaps of its own and the stage would
                    // return at once, letting a dependant swap against a
                    // dependency that is mid-swap. The reload in flight is
                    // still a reload this stage's dependants have to wait
                    // out, so it is waited for as one: any `Reloaded` or the
                    // `ReloadAbandoned` that ends it finishes the name.
                    //
                    // One swap, and a clustered app owes one per instance.
                    // Nothing here can see how far the reload in flight has
                    // got, so a three-instance dependency finishes this wait
                    // at whichever swap lands next and a dependant can go a
                    // swap or two early. Accepted, for `reload_in_stages`'
                    // own reason: a wait sized by a count this side is
                    // guessing at can hang, and an early stage cannot.
                    if matches!(err, SupervisorError::ReloadInFlight(_))
                        && walk.depended_on.contains(name.as_str())
                    {
                        waiting.insert(name.clone(), 1);
                    }
                    tracing::warn!(
                        sheep = %name,
                        %err,
                        "a sheep did not reload in its stage; it may have left the flock since \
                         the walk was planned"
                    );
                    // The daemon log is not somewhere a deploy script looks,
                    // so the name rides back on the reply as well.
                    refused.push(SheepRefusal::new(name.clone(), err.to_string()));
                    refusal.get_or_insert(err);
                }
            }
        }

        if waiting.is_empty() {
            continue;
        }
        let bound = reload_stage_bound(ctx, &waiting);
        let unsettled = crate::boot_order::await_reloads(rx, waiting, bound).await;
        warn_about_unsettled(&unsettled);
    }
    finished(accepted, refusal).map(|rows| (rows, refused))
}

/// A stage that ran out of time, named. Nothing is retried and the walk goes
/// on: a stage held to its bound has already cost the operator that wait, and
/// stopping here would leave the fold half done.
fn warn_about_unsettled(unsettled: &BTreeSet<String>) {
    if unsettled.is_empty() {
        return;
    }
    let names: Vec<&str> = unsettled.iter().map(String::as_str).collect();
    tracing::warn!(
        unsettled = ?names,
        "a stage did not settle inside its bound; advancing anyway"
    );
}

/// The walk's answer: the rows it moved, sorted as every operator-facing
/// listing is, or the refusal it kept when it moved nothing.
fn finished(
    mut rows: Vec<ProcessInfo>,
    refusal: Option<SupervisorError>,
) -> Result<Vec<ProcessInfo>, RpcError> {
    match refusal {
        Some(err) if rows.is_empty() => Err(rpc_error(&err)),
        _ => {
            // Sorted here rather than left in stage order: each supervisor
            // call sorts its own answer, and a table stitched from several is
            // otherwise ordered by the graph, which is not what an operator
            // reading a flock listing expects.
            shep_core::protocol::sort_flock(&mut rows);
            Ok(rows)
        }
    }
}
