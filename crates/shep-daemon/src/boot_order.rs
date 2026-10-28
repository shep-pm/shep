//! Running a flock in dependency order.
//!
//! The sort itself is `shep_core::config::graph`. This module runs what it
//! produces: start a stage, wait for it, advance.
//!
//! It lives outside the supervisor actor deliberately. `do_start` is a
//! synchronous `fn` reached from the actor's own message loop, and that loop
//! is what delivers `Msg::ReadyResult`, so a wait inside it could never end.

use core::time::Duration;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;

use shep_core::config::ResolvedApp;
use shep_core::config::graph::{BootNode, BootPlan, NodeKind};
use shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo};
use shep_core::selector::ProcessSelector;
use shep_core::status::ProcStatus;
use tokio::sync::broadcast::{self, error::RecvError};

use crate::bus::{Bus, SharedEvent};
use crate::supervisor::{BatchPolicy, SupervisorError, SupervisorHandle};

/// How much longer than the stage's own longest `listen_timeout` the driver
/// waits before giving up on it.
///
/// Every member is already bounded by its own readiness task, which reports
/// at its own deadline, so this covers scheduling jitter only. The same
/// reasoning as `RELOAD_DEADLINE_SLACK`, and the same figure.
pub(crate) const STAGE_SLACK: Duration = Duration::from_secs(5);

/// Graph nodes for a flock plus its dogs, with no dog promoted ahead of the
/// flock.
// Only this module's own tests reach the two-argument spelling: every caller
// so far has a promoted list to pass. `expect` rather than `allow`, so it
// deletes itself the moment one does not.
#[cfg_attr(not(test), expect(dead_code))]
#[must_use]
pub(crate) fn nodes_for(apps: &[ResolvedApp], dogs: &[String]) -> Vec<BootNode> {
    nodes_for_with_dogs(apps, dogs, &[])
}

/// The plan for a set of apps plus the dogs this shepherd holds.
///
/// The one place a plan is built, so every caller positions dogs the same
/// way.
#[must_use]
pub(crate) fn plan_for(apps: &[ResolvedApp], dogs: &[String], boot_first: &[String]) -> BootPlan {
    shep_core::config::graph::plan(&nodes_for_with_dogs(apps, dogs, boot_first))
}

/// The plan for a flock given as names and the names each one waits for.
///
/// The teardown's entry point, beside [`plan_for`], which needs
/// [`ResolvedApp`]s and so only suits a caller that has just normalized one.
/// Every node here is a [`NodeKind::Sheep`], so no dog reaches these stages.
/// The plan's `unresolved` and `cycles` are dropped, not warned: the boot
/// already reported both.
#[must_use]
pub(crate) fn plan_for_names(edges: &BTreeMap<String, Vec<String>>) -> BootPlan {
    let nodes: Vec<BootNode> = edges
        .iter()
        .map(|(name, depends_on)| BootNode {
            name: name.clone(),
            depends_on: depends_on.clone(),
            kind: NodeKind::Sheep,
        })
        .collect();
    shep_core::config::graph::plan(&nodes)
}

/// Graph nodes for a flock plus its dogs.
///
/// `boot_first` names the dogs `[daemon] boot_first_dogs` promotes ahead of
/// every sheep. A dog carries no `depends_on` of its own: `dog_app` builds a
/// dog's config from `AppConfig::minimal`, so its list is always empty. A
/// dog sharing a sheep's name is dropped rather than added a second time,
/// since a second node would start that sheep in two stages.
#[must_use]
pub(crate) fn nodes_for_with_dogs(
    apps: &[ResolvedApp],
    dogs: &[String],
    boot_first: &[String],
) -> Vec<BootNode> {
    let promoted: BTreeSet<&str> = boot_first.iter().map(String::as_str).collect();
    let taken: BTreeSet<&str> = apps.iter().map(|app| app.config().name.as_str()).collect();
    apps.iter()
        .map(|app| BootNode {
            name: app.config().name.clone(),
            depends_on: app.config().depends_on.clone(),
            kind: NodeKind::Sheep,
        })
        .chain(
            dogs.iter()
                .filter(|name| !taken.contains(name.as_str()))
                .map(|name| BootNode {
                    name: name.clone(),
                    depends_on: Vec::new(),
                    kind: NodeKind::Dog {
                        boot_first: promoted.contains(name.as_str()),
                    },
                }),
        )
        .collect()
}

/// Starts `apps` stage by stage, holding each until every member a later stage waits on settles.
///
/// Dogs in `plan` are skipped: `boot` spawns the promoted dogs before this
/// driver and the rest after every stage, so a sheep waiting on an
/// unpromoted dog starts while that dog is not running.
///
/// An [`BatchPolicy::AllOrNothing`] batch that fails here leaves a PARTIAL
/// flock, and nothing is rolled back: [`left_running`] names what stayed up.
/// Each stage announces its `bound` to `notify_socket`, so a unit's
/// `TimeoutStartSec` follows the walk instead of summing the stages.
///
/// # Errors
/// - [`SupervisorError`]: under [`BatchPolicy::AllOrNothing`] only, the first failed stage.
pub(crate) async fn start_in_stages(
    plan: &BootPlan,
    apps: &[ResolvedApp],
    supervisor: &SupervisorHandle,
    events: &Bus,
    policy: BatchPolicy,
    notify_socket: Option<&OsStr>,
) -> Result<Vec<ProcessInfo>, SupervisorError> {
    // Every name anything in the flock waits on, read once. That is usually
    // "what follows them", since the sort puts a dependency in an earlier
    // stage than its dependants. A cycle is the exception: every member of a
    // cyclic stage depends on another member of that same stage.
    let mut depended_on: BTreeSet<&str> = BTreeSet::new();
    for app in apps {
        depended_on.extend(app.config().depends_on.iter().map(String::as_str));
    }

    let mut started = Vec::new();
    for (index, stage) in plan.stages.iter().enumerate() {
        // Walked over `apps` rather than over `stage`, so a stage keeps the
        // order the caller handed it. A stage's names are sorted, and under
        // `BatchPolicy::AllOrNothing` a failed spawn ends the batch where it
        // stands, so the sorted order would pick survivors by name.
        let names: BTreeSet<&str> = stage.iter().map(String::as_str).collect();
        let members: Vec<ResolvedApp> = apps
            .iter()
            .filter(|app| names.contains(app.config().name.as_str()))
            .cloned()
            .collect();
        if members.is_empty() {
            continue;
        }
        // The gate rides on this one `Command::Start` and so is a property of
        // the first spawn only: `respawn` reads a sheep's own readiness source
        // and never this set. A gated sheep still parks at `Starting`, but
        // neither kind re-enters this wait, which is the part that matters.
        let gate: BTreeSet<String> = members
            .iter()
            .map(|app| app.config().name.clone())
            .filter(|name| depended_on.contains(name.as_str()))
            .collect();
        let waiting = gate.clone();
        let bound = members
            .iter()
            .map(|app| app.config().listen_timeout.as_duration())
            .max()
            .unwrap_or_default()
            + STAGE_SLACK;

        // Subscribed before the spawn, not after it: a broadcast receiver
        // starts at the channel's tail, so one taken afterwards would begin
        // reading past a fast app's `Online`. `await_stage` also reads the
        // flock, but an early cursor makes that handoff gapless.
        let rx = events.subscribe();
        tracing::info!(stage = index, members = ?stage, "boot stage starting");
        extend_start_deadline(notify_socket, bound);
        match supervisor.start_staged(members, gate, policy).await {
            Ok(infos) => started.extend(infos),
            Err(err) if policy == BatchPolicy::AllOrNothing => {
                let left = left_running(supervisor, &started, &names).await;
                return Err(corrected_for_earlier_stages(err, &left));
            }
            Err(err) => tracing::warn!(stage = index, %err, "a boot stage did not start"),
        }
        if waiting.is_empty() {
            continue;
        }
        // Again, because `await_stage` starts its own `bound` only now, after
        // the spawns above returned. Each message sets a fresh deadline from
        // its receipt, so the first covers the spawns and this one the wait.
        extend_start_deadline(notify_socket, bound);
        let unsettled = await_stage(rx, waiting, bound, supervisor).await;
        if !unsettled.is_empty() {
            let names: Vec<&str> = unsettled.iter().map(String::as_str).collect();
            tracing::warn!(
                stage = index,
                unsettled = ?names,
                "a boot stage did not settle inside its bound; advancing anyway"
            );
        }
    }
    Ok(started)
}

/// Tells the init system at `notify_socket`, if any, to hold its start
/// deadline for `bound` more, counted from when it receives this.
///
/// A failed send is a `warn!`: the walk carries on, and only the init
/// system's deadline is then the one it had. Nothing is sent off unix, where
/// there is no `$NOTIFY_SOCKET`.
pub(crate) fn extend_start_deadline(notify_socket: Option<&OsStr>, bound: Duration) {
    #[cfg(unix)]
    if let Some(target) = notify_socket
        && let Err(err) = crate::notify::extend_timeout(target, bound)
    {
        tracing::warn!(
            %err,
            "the start deadline could not be extended through $NOTIFY_SOCKET; carrying on"
        );
    }
    #[cfg(not(unix))]
    let _ = (notify_socket, bound);
}

/// What this start left running: every earlier stage, plus whatever of the
/// failing stage was up before the failure.
///
/// The second half is why the flock is read at all: a spawn that fails
/// leaves the batch part-registered, and those apps are in no stage this
/// walk completed. A stage member the flock holds as `Stopped` or `Errored`
/// is left out, since the message is about children that are up.
async fn left_running(
    supervisor: &SupervisorHandle,
    started: &[ProcessInfo],
    stage: &BTreeSet<&str>,
) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = started.iter().map(|info| info.name.clone()).collect();
    let Ok(flock) = supervisor.list_checked().await else {
        return names;
    };
    names.extend(
        flock
            .iter()
            .filter(|info| stage.contains(info.name.as_str()))
            .filter(|info| !matches!(info.status, ProcStatus::Stopped | ProcStatus::Errored))
            .map(|info| info.name.clone()),
    );
    names
}

/// `err`, told to name what this start already left running.
///
/// [`BatchPolicy::AllOrNothing`] refuses one `Command::Start` before it
/// registers anything, and both its messages say so. That is now true of
/// ONE stage, and not all of it: stage 0 is running by the time stage 1
/// proves unstartable. Nothing rolls any of it back, so the message says
/// which ones they are instead.
fn corrected_for_earlier_stages(
    err: SupervisorError,
    left_running: &BTreeSet<String>,
) -> SupervisorError {
    if left_running.is_empty() {
        return err;
    }
    let running: Vec<&str> = left_running.iter().map(String::as_str).collect();
    let note = format!(
        " (this start already has children running and leaves them alone: {})",
        running.join(", ")
    );
    match err {
        SupervisorError::CannotStart(message) => SupervisorError::CannotStart(message + &note),
        SupervisorError::SpawnFailed(message) => SupervisorError::SpawnFailed(message + &note),
        other => other,
    }
}

/// Waits until every name in `waiting` has reached a settled answer, or until
/// `bound` elapses. Answers with the names that never settled.
///
/// A member that dies resolves at once, which keeps a missing binary from
/// costing the boot its whole budget. The flock decides whether a name has
/// settled; the bus decides only when to ask, since an event is a trigger.
/// Everything is inside `bound`, the flock read included.
pub(crate) async fn await_stage(
    mut rx: broadcast::Receiver<SharedEvent>,
    mut waiting: BTreeSet<String>,
    bound: Duration,
    supervisor: &SupervisorHandle,
) -> BTreeSet<String> {
    let settle = async {
        drop_settled(supervisor, &mut waiting).await;
        while !waiting.is_empty() {
            match rx.recv().await {
                Ok(event) => {
                    let BusEvent::Process { info, .. } = &*event else {
                        continue;
                    };
                    if waiting.contains(&info.name) && is_settled(info.status) {
                        drop_settled(supervisor, &mut waiting).await;
                    }
                }
                // The events a lagged receiver skipped are gone, so treating
                // this as `continue` would leave the bound as the only thing
                // that could end the wait. Ask the flock instead: it holds
                // the state those events were reporting.
                Err(RecvError::Lagged(_)) => drop_settled(supervisor, &mut waiting).await,
                Err(RecvError::Closed) => return,
            }
        }
    };
    let _ = tokio::time::timeout(bound, settle).await;
    waiting
}

/// Waits until every name in `waiting` has finished reloading, or until
/// `bound` elapses. Answers with the names that did not finish.
///
/// The reload half of [`await_stage`], which it cannot share: a reload
/// leaves an app serving throughout, so no flock status says whether a swap
/// happened and the bus is the only source. `waiting` therefore carries a
/// count, and a `ReloadAbandoned` finishes a name outright.
pub(crate) async fn await_reloads(
    mut rx: broadcast::Receiver<SharedEvent>,
    mut waiting: BTreeMap<String, usize>,
    bound: Duration,
) -> BTreeSet<String> {
    let settle = async {
        while !waiting.is_empty() {
            match rx.recv().await {
                Ok(event) => {
                    let BusEvent::Process {
                        event: kind, info, ..
                    } = &*event
                    else {
                        continue;
                    };
                    match kind {
                        ProcessEventKind::Reloaded => {
                            if let Some(left) = waiting.get_mut(&info.name) {
                                *left = left.saturating_sub(1);
                                if *left == 0 {
                                    waiting.remove(&info.name);
                                }
                            }
                        }
                        ProcessEventKind::ReloadAbandoned => {
                            waiting.remove(&info.name);
                        }
                        _ => {}
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
        }
    };
    let _ = tokio::time::timeout(bound, settle).await;
    waiting.into_keys().collect()
}

/// Whether a boot stage has its answer about an instance in this status.
///
/// `Starting` is the one status that is still a question: it is where the
/// supervisor parks a gated sheep until its readiness task reports. Every
/// other status is an answer, including the bad ones. Read off the event as
/// well as off the flock, rather than matching on `ProcessEventKind`: an
/// `autorestart = false` sheep exits as `process.stop`, not `process.exit`.
fn is_settled(status: ProcStatus) -> bool {
    !matches!(status, ProcStatus::Starting)
}

/// Drops from `waiting` every name the flock has no unanswered instance of.
///
/// A name the flock does not hold at all is settled too: the start meant to
/// register it has already returned, and nothing else will. A name with
/// several instances is kept while any one is still starting, because every
/// instance publishes under the same `ProcessInfo::name` and settling on an
/// event would release the name on whichever instance answered first.
async fn drop_settled(supervisor: &SupervisorHandle, waiting: &mut BTreeSet<String>) {
    let Ok(flock) = supervisor.list_checked().await else {
        waiting.clear();
        return;
    };
    waiting.retain(|name| {
        flock
            .iter()
            .any(|info| &info.name == name && !is_settled(info.status))
    });
}

/// Stops every sheep in `plan`, last stage first.
///
/// Dogs are stopped by `SupervisorHandle::shutdown`, after every sheep,
/// because monitoring should outlive what it monitors and a strict reverse
/// would kill the bark dog before the flock it reports on. So a plan handed
/// here is the sheep-only one; a dog named in it would be stopped here
/// instead.
pub(crate) async fn stop_in_reverse(plan: &BootPlan, supervisor: &SupervisorHandle) {
    for stage in plan.stages.iter().rev() {
        // `join_all` inside a stage, a serial walk across them, for the reason
        // `spawn_send_line_task` gives: every `stop` here runs its own kill
        // ladder to the end, so awaiting them in turn would make a teardown
        // cost the SUM of the stage's `kill_timeout`s rather than the longest.
        futures_util::future::join_all(stage.iter().map(|name| async move {
            if let Err(err) = supervisor.stop(ProcessSelector::Name(name.clone())).await {
                tracing::warn!(sheep = %name, %err, "a sheep did not stop in its stage");
            }
        }))
        .await;
    }
}

/// Stops every sheep `edges` names, last stage first, leaving the dogs
/// running.
///
/// The caller reads the edges: `shep dev` clears the registry ahead of the
/// final roll, so a walk reading it here would plan nothing. `dogs` is the
/// boot-time spawn list, filtered out because [`stop_in_reverse`] stops
/// whatever it is handed, and monitoring should outlive what it monitors.
pub(crate) async fn stop_edges_in_reverse(
    mut edges: BTreeMap<String, Vec<String>>,
    dogs: &[String],
    supervisor: &SupervisorHandle,
) {
    for dog in dogs {
        edges.remove(dog);
    }
    stop_in_reverse(&plan_for_names(&edges), supervisor).await;
}

#[cfg(test)]
mod tests;
