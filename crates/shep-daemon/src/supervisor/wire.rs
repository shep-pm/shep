//! Turning actor state into what a caller sees.
//!
//! `to_info` is the one that matters: it renders a live lifecycle entry as
//! the `ProcessInfo` every list, describe and status answer is built from.
//! The rest are small predicates and conversions the reply path needs.

use super::*;

/// Delivers a deferred (or immediate) reply, converting to the payload
/// shape each [`ReplyKind`] variant expects.
pub(super) fn send_reply(reply: ReplyKind, outcome: Result<Vec<ProcessInfo>, SupervisorError>) {
    match reply {
        ReplyKind::Info(tx) => {
            let _ = tx.send(outcome);
        }
        ReplyKind::Ids(tx) => {
            let ids = outcome.map(|infos| infos.into_iter().map(|info| info.id).collect());
            let _ = tx.send(ids);
        }
        ReplyKind::Shutdown(tx) => {
            let _ = tx.send(());
        }
    }
}

/// Snapshots one entry into the wire-facing [`ProcessInfo`] shape.
///
/// Takes the smit map rather than hanging off `&self`: several call sites hold
/// a `&mut` borrow of `self.sheep` here, and a method on `Actor` would borrow
/// the whole actor. A free function lets the two fields stay disjoint.
pub(super) fn to_info(entry: &ProcessEntry, smits: &Smits) -> ProcessInfo {
    let uptime_ms = entry.started_at.map_or(0, |started_at| {
        tokio::time::Instant::now()
            .saturating_duration_since(started_at)
            .as_millis() as u64
    });
    ProcessInfo::builder(entry.id, entry.spec.config().name.clone(), entry.status)
        .pid(entry.pid)
        .restarts(entry.restarts)
        .uptime_ms(uptime_ms)
        .fold(entry.spec.config().fold.clone())
        .depends_on(entry.spec.config().depends_on.clone())
        // Lossy on purpose: a non-UTF-8 log path must not fail serialization
        // of the whole reply and blank the listing for every other sheep.
        .out_file(Some(entry.out_file.to_string_lossy().into_owned()))
        .err_file(Some(entry.err_file.to_string_lossy().into_owned()))
        // Filled in by the RPC layer for the two verbs that read resource
        // usage: the numbers cost a syscall walk over the host's whole process
        // table, and the actor must never block.
        .cpu_percent(None)
        .memory_bytes(None)
        .dog(entry.dog.clone())
        .last_exit(entry.last_exit)
        // By name: every instance shows the same mark, including one spawned
        // after it was painted.
        .smit(
            smits
                .get(&entry.spec.config().name)
                .map(|(_, smit)| smit.clone()),
        )
        .instance(Some(entry.instance))
        // The field names `spec` and `pending` differ on, or `None` when
        // nothing is parked. Emptiness collapses to `None` here rather than
        // in `pending_fields`: a parked config identical to `spec` must
        // report `None`, per `ProcessInfo::pending`.
        .pending({
            let fields = pending_fields(entry);
            (!fields.is_empty()).then_some(fields)
        })
        // Read off the cached field, never the override store: every path
        // that can register or replace a sheep keeps `ProcessEntry::overridden`
        // correct, so this listing path does no I/O.
        .overridden((!entry.overridden.is_empty()).then(|| entry.overridden.clone()))
        .max_memory(entry.spec.config().max_memory.map(MemSize::bytes))
        // Cloned per row rather than fetched on demand: a client classifies
        // every line it draws, and an empty list costs the wire nothing.
        .level_rules(entry.spec.config().level_rules.clone())
        .build()
}

/// Converts the spawn-runner's own exit observation into the wire-facing shape
/// `ProcessEntry::last_exit` stores.
///
/// A separate `From` rather than [`ExitOutcome`] on the wire directly: that
/// type lives behind the [`ProcessRunner`] seam and is free to grow without
/// dragging a breaking wire change behind it.
impl From<ExitOutcome> for ExitInfo {
    fn from(outcome: ExitOutcome) -> Self {
        Self {
            code: outcome.code,
            signal: outcome.signal,
        }
    }
}

/// How long ONE swap of an instance is given: its own `listen_timeout`,
/// then its `graceful_timeout`, then [`RELOAD_DEADLINE_SLACK`].
///
/// One source for two readers, [`Actor::arm_reload_deadline`], which arms
/// the watchdog with it, and [`Actor::handle_reload`], which reports it on
/// [`ProcessInfo::reload_deadline_ms`]. A second copy could tell a dog a
/// number the shepherd was not keeping to.
pub(super) fn swap_budget(config: &AppConfig) -> Duration {
    config.listen_timeout.as_duration()
        + config.graceful_timeout.as_duration()
        + RELOAD_DEADLINE_SLACK
}

/// Whether this instance's status lets a reload replace it.
///
/// `Online` is the ordinary answer; `ready_failed` is the exception, an
/// instance a failed reload left up and not serving (see
/// [`SheepSlot::ready_failed`]). Both doors into a reload ask this,
/// [`Actor::handle_reload`]'s selector pass and [`Actor::advance_reload`], so
/// a reload cannot drop an instance silently or reach one it had ruled out.
/// `advance_reload` adds the `manual` half on its own.
pub(super) fn reload_eligible(slot: &SheepSlot) -> bool {
    slot.entry.status == ProcStatus::Online || slot.ready_failed
}

/// The status a drainee goes back to when its swap is undone.
///
/// `Online` for an instance that was serving. `Starting` for one an earlier
/// reload had already parked, the case [`reload_eligible`] opened up: it never
/// proved it could serve. `ready_failed` stays set, so the next attempt can
/// still replace it.
///
/// Shared by the two sites that undo a swap, as [`reload_eligible`] is shared
/// by the two that start one.
pub(super) fn restored_status(slot: &SheepSlot) -> ProcStatus {
    if slot.ready_failed {
        ProcStatus::Starting
    } else {
        ProcStatus::Online
    }
}
