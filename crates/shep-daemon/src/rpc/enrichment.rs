//! Decorates a [`ProcessInfo`] listing with what a supervisor snapshot
//! does not carry: live CPU/memory, dog handshake facts, and lamb
//! trees.
//!
//! [`dog_staleness`] and [`handover_refusal`] answer daemon-wide
//! questions rather than per-row ones.

#[cfg(feature = "vcs")]
use std::collections::BTreeSet;
use std::collections::HashMap;
#[cfg(feature = "vcs")]
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "vcs")]
use std::sync::{LazyLock, Mutex, PoisonError};
#[cfg(feature = "vcs")]
use std::time::Duration;

#[cfg(feature = "vcs")]
use shep_core::protocol::Vcs;
use shep_core::protocol::{Lamb, ProcessInfo};
#[cfg(feature = "vcs")]
use tokio::sync::Semaphore;

use crate::limits::stats::StatsState;

use super::context::RpcContext;
#[cfg(feature = "vcs")]
use crate::supervisor::SupervisorHandle;
#[cfg(feature = "vcs")]
use crate::vcs::{Absent, revision_of};

/// How long one checkout's read may take before its rows go without a `vcs`.
///
/// A `cwd` on a hung network mount blocks its read indefinitely, and a
/// `Describe` reply must not wait on it.
#[cfg(feature = "vcs")]
const VCS_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// The directories a revision read is still running for, one read each.
///
/// A timed-out read cannot be cancelled: its blocking thread stays parked until
/// the mount answers, and one more per `Describe` would fill the pool every
/// other `spawn_blocking` in the shepherd shares. A directory holds one permit,
/// owned by its read until the read ends, and a later request waits on it
/// inside its own timeout.
#[cfg(feature = "vcs")]
#[derive(Debug, Default)]
pub(super) struct VcsReads {
    slots: Mutex<HashMap<PathBuf, Arc<Semaphore>>>,
}

#[cfg(feature = "vcs")]
impl VcsReads {
    /// The permit for `dir`, made on first use.
    fn slot(&self, dir: &Path) -> Arc<Semaphore> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            slots
                .entry(dir.to_path_buf())
                .or_insert_with(|| Arc::new(Semaphore::new(1))),
        )
    }

    /// Drops every entry no read holds, so the table stays the size of the
    /// directories with a read still running.
    fn forget_idle(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.retain(|_, slot| Arc::strong_count(slot) > 1);
    }
}

/// The dogs this daemon has given up on, and the dogs it is still waiting to
/// hear from.
///
/// Stale is [`DogRefusals::stale`](crate::dogs::DogRefusals::stale): refused,
/// restarted from the binary on disk, refused again. A version cannot answer
/// it, since two dog builds differing only in protocol report the same one.
/// Pending has two sources: a dog refused once is mid-restart, and a supervised dog that has never handshaken.
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
/// gives: the client does nothing with it but print it.
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
/// Joined by pid, not by id: [`StatsState`] keys on the root pid it was armed against.
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
/// per row, and `stale()` called once for the whole listing.
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
/// every process on the machine, and a flock listing is the thing an
/// operator leaves running in a loop.
///
/// A row with no pid is left `None` rather than `Some(vec![])`.
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

/// Fills each row's `vcs` from the revision checked out in its sheep's `cwd`.
///
/// Applied to `Describe` and to nothing else: it opens files on disk, and a
/// flock listing is polled in a loop. A row stays `None` when its sheep names
/// no `cwd` (never the shepherd's own directory), when the directory is not a
/// checkout this reader recognises, or when the read outlasts
/// [`VCS_READ_TIMEOUT`]. Each of those is logged at debug level.
#[cfg(feature = "vcs")]
pub(super) async fn with_vcs(
    supervisor: &SupervisorHandle,
    infos: Vec<ProcessInfo>,
) -> Vec<ProcessInfo> {
    let Ok(dirs) = supervisor.working_dirs().await else {
        // The engine is stopping: describe the sheep without a revision
        // rather than fail the request over a decoration.
        return infos;
    };
    // One table for the whole shepherd: a stuck read outlives the request
    // that started it.
    static READS: LazyLock<VcsReads> = LazyLock::new(VcsReads::default);
    fill_vcs(
        infos,
        &dirs,
        VCS_READ_TIMEOUT,
        &READS,
        Arc::new(revision_of),
    )
    .await
}

/// [`with_vcs`] over a directory table and a reader the caller supplies.
///
/// One read per distinct directory, each on the blocking pool under its own
/// `timeout`, so instances of one app share a read and one hung mount does not
/// cost the rows beside it. `reads` keeps a hung mount to one parked thread
/// however often it is asked: see [`VcsReads`].
#[cfg(feature = "vcs")]
pub(super) async fn fill_vcs<R>(
    mut infos: Vec<ProcessInfo>,
    dirs: &HashMap<u32, PathBuf>,
    timeout: Duration,
    reads: &VcsReads,
    read: Arc<R>,
) -> Vec<ProcessInfo>
where
    R: Fn(&Path) -> Result<Vcs, Absent> + Send + Sync + 'static,
{
    let distinct: BTreeSet<&PathBuf> = infos.iter().filter_map(|info| dirs.get(&info.id)).collect();
    let pending = distinct.into_iter().map(|dir| {
        let dir = dir.clone();
        let read = Arc::clone(&read);
        let slot = reads.slot(&dir);
        async move {
            let outcome = tokio::time::timeout(timeout, async {
                // Held by the blocking closure, so it frees when the read
                // ends and not when this request stops waiting for it.
                let permit = slot.acquire_owned().await.ok()?;
                let target = dir.clone();
                tokio::task::spawn_blocking(move || {
                    let found = read(&target);
                    drop(permit);
                    found
                })
                .await
                .ok()
            })
            .await;
            (dir, outcome)
        }
    });
    let mut found: HashMap<PathBuf, Vcs> = HashMap::new();
    for (dir, outcome) in futures_util::future::join_all(pending).await {
        match outcome {
            Ok(Some(Ok(vcs))) => {
                found.insert(dir, vcs);
            }
            Ok(Some(Err(absent))) => {
                tracing::debug!(dir = %dir.display(), %absent, "no revision to report")
            }
            Ok(None) => tracing::debug!(dir = %dir.display(), "revision read did not finish"),
            Err(_) => tracing::debug!(
                dir = %dir.display(),
                "revision read timed out, or an earlier read of it is still stuck"
            ),
        }
    }
    reads.forget_idle();
    for info in &mut infos {
        match dirs.get(&info.id) {
            Some(dir) => info.vcs = found.get(dir).cloned(),
            None => tracing::debug!(sheep = %info.name, "no cwd, so no revision to report"),
        }
    }
    infos
}
