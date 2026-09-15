//! Decorates a [`ProcessInfo`] listing with what a supervisor snapshot
//! does not carry: live CPU/memory, dog handshake facts, and lamb
//! trees.
//!
//! [`dog_staleness`] and [`handover_refusal`] answer daemon-wide
//! questions rather than per-row ones.

use std::collections::HashMap;
use std::sync::Arc;

use shep_core::protocol::{Lamb, ProcessInfo};

use crate::limits::stats::StatsState;

use super::context::RpcContext;

/// The dogs this daemon has given up on, and the dogs it is still waiting to
/// hear from.
///
/// Stale is [`DogRefusals::stale`](crate::dogs::DogRefusals::stale): refused,
/// restarted from the binary on disk, refused again. A version cannot answer
/// it, since two dog builds differing only in protocol report the same one.
///
/// Pending has two sources: a dog refused once is mid-restart, and a
/// supervised dog that has never handshaken has not been asked yet, which is
/// what a carried dog is between the exec and its reconnect. Only a dog with
/// a process counts. `shep daemon reload` polls this every 50ms, so the
/// silent-dog ladder must stay on a clock rather than be driven from here.
pub(super) async fn dog_staleness(ctx: &RpcContext) -> (Vec<String>, Vec<String>) {
    let stale = ctx.dog_refusals.stale();
    let mut pending = ctx.dog_refusals.restarting();
    // A stopped engine has no dogs left to wait on, so its rows are not
    // worth an error: the refusal record above is still the honest answer.
    if let Ok(infos) = ctx.supervisor.list_checked().await {
        pending.extend(crate::dogs::silent_dogs(&infos, &ctx.dog_refusals));
    }
    pending.sort();
    pending.dedup();
    (stale, pending)
}

/// Why this shepherd cannot hand its flock to a successor in place, or
/// `None` when it can.
///
/// The sentence is rendered here rather than as a structured reason on the
/// wire, for the reason
/// [`Response::HandoverFitness`](shep_core::protocol::Response::HandoverFitness)
/// gives: the client does
/// nothing with it but print it.
///
/// An engine that has stopped is a refusal too, not an error: the caller
/// asked whether to signal a shepherd.
#[cfg(unix)]
pub(super) async fn handover_refusal(ctx: &RpcContext) -> Option<String> {
    match ctx.supervisor.handover_fitness().await {
        Ok(crate::handover::Fitness::Carryable) => None,
        Ok(crate::handover::Fitness::Refused(reason)) => Some(reason.to_string()),
        Err(err) => Some(format!(
            "this shepherd could not check whether its flock can be handed over ({err})"
        )),
    }
}

/// Windows has no `execve`, so there is no image for a successor to become
/// and every flock is refused.
///
/// A refusal rather than an unimplemented request: this one is answered, and
/// the answer sends `shep daemon reload` to the stop-and-start arm.
#[cfg(windows)]
#[expect(
    clippy::unused_async,
    reason = "one signature for both platforms; the unix arm awaits the supervisor"
)]
pub(super) async fn handover_refusal(_ctx: &RpcContext) -> Option<String> {
    Some(
        "this shepherd runs on Windows, which has no `execve`, so its flock cannot be handed to \
         a successor in place"
            .to_string(),
    )
}

/// Fills in each running sheep's live CPU and memory.
///
/// Sampled here rather than inside the supervisor: the actor must never
/// block, and the reading is a syscall walk over the host's whole process
/// table, so it runs on a blocking-pool thread.
///
/// Joined by pid, not by id: [`StatsState`] keys on the root pid it was armed
/// against, which is the number [`ProcessInfo::pid`] carries. Only `ListFlock`
/// and `Describe` call this; the lifecycle verbs answer with [`ProcessInfo`]
/// too, but none of them is where an operator reads resource usage.
pub(super) async fn with_live_stats(
    stats: &Arc<StatsState>,
    mut infos: Vec<ProcessInfo>,
) -> Vec<ProcessInfo> {
    let stats = Arc::clone(stats);
    let Ok(sample) = tokio::task::spawn_blocking(move || stats.sample_now()).await else {
        // The blocking pool is gone or the task panicked: report the flock
        // without stats rather than fail a listing over a decoration.
        return infos;
    };
    for info in &mut infos {
        if let Some(reading) = info.pid.and_then(|pid| sample.get(&pid)) {
            info.cpu_percent = reading.cpu_percent;
            info.memory_bytes = Some(reading.memory_bytes);
            info.cpu_ms = Some(reading.cpu_ms);
        }
    }
    infos
}

/// Fills in each dog's two connection facts, which no sheep has and the
/// supervisor does not hold: whether it has ever answered this shepherd, and
/// whether this shepherd has given up on it.
///
/// Connection state lives in [`DogRefusals`](crate::dogs::DogRefusals) on the
/// RPC context, so it is joined here as `with_live_stats` is: two map lookups
/// per row, and `stale()` called once for the whole listing. Both fields,
/// because a dog spawned a moment ago and one this shepherd has stopped
/// restarting are both `handshook: Some(false)` with a live process.
///
/// Applied to `ListFlock` and `Describe` alone. A sheep is skipped rather
/// than set to `Some(false)`, having no handshake with this shepherd at all.
pub(super) fn with_dog_contact(
    refusals: &crate::dogs::DogRefusals,
    mut infos: Vec<ProcessInfo>,
) -> Vec<ProcessInfo> {
    let stale = refusals.stale();
    for info in &mut infos {
        if info.dog.is_some() {
            info.handshook = Some(refusals.has_handshook(&info.name));
            info.dog_stale = Some(stale.contains(&info.name));
        }
    }
    infos
}

/// Fills each row's `lambs` from a fresh walk of the process table.
///
/// Applied to `Describe` and to nothing else: the walk is a second pass over
/// every process on the machine, and a flock listing is the thing an operator
/// leaves running in a loop.
///
/// A row with no pid is left `None` rather than `Some(vec![])`, which is the
/// "not walked" case the field's own doc distinguishes from "walked and
/// empty".
pub(super) async fn with_lambs(
    stats: &Arc<StatsState>,
    mut infos: Vec<ProcessInfo>,
) -> Vec<ProcessInfo> {
    if infos.iter().all(|info| info.pid.is_none()) {
        // Nothing to walk for: skip the table refresh entirely rather than
        // pay for it and assign `None` anyway.
        return infos;
    }
    let stats = Arc::clone(stats);
    let pids: Vec<u32> = infos.iter().filter_map(|info| info.pid).collect();
    let Ok(walked) = tokio::task::spawn_blocking(move || {
        // One index for the whole reply: `describe all` walks the machine's
        // process table once, not once per row.
        let index = stats.lamb_index();
        pids.into_iter()
            .map(|pid| (pid, stats.lambs_of(&index, pid)))
            .collect::<HashMap<u32, Vec<Lamb>>>()
    })
    .await
    else {
        // The blocking pool is gone or the task panicked: describe the sheep
        // without their trees rather than fail the request over a decoration.
        return infos;
    };
    for info in &mut infos {
        if let Some(lambs) = info.pid.and_then(|pid| walked.get(&pid)) {
            info.lambs = Some(lambs.clone());
        }
    }
    infos
}
