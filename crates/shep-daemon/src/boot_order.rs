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
/// A shutdown has the registry's own names and edges, and nothing there may
/// fail: every node is a [`NodeKind::Sheep`], so no dog can reach the stages
/// through this door.
///
/// The plan's `unresolved` and `cycles` are dropped on the floor rather than
/// warned about, deliberately: the boot already reported both, and a flock
/// with a bad edge would then say so a second time at the one moment nobody
/// can act on it, in a log an operator reads after the machine is down.
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
/// dog's config from `AppConfig::minimal`, so its list is always empty.
///
/// A dog sharing a sheep's name is dropped rather than added a second time: a
/// second node would put that one sheep in two stages and start it twice.
/// Nothing refuses the collision, and which of the two runs depends on which
/// went first. An unpromoted dog is spawned after the flock, finds the name
/// registered, and returns, so the sheep is what runs. A promoted one spawns
/// before the restore against an empty flock, and the sheep is the one that
/// never starts; `snapshot::warn_about_dogs_holding_sheep_names` is what says
/// so.
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

/// Starts `apps` stage by stage, holding each stage until every member a
/// later stage waits on has settled.
///
/// Dogs in `plan` are skipped, and only `[daemon] boot_first_dogs` dogs are
/// positioned by it in any real sense: `boot` runs `dogs::spawn_enabled_dogs`
/// twice, once for the promoted ones before this driver is reached at all and
/// once for the rest after every stage, rather than at each dog's own stage
/// boundary. So a sheep waiting on an unpromoted dog starts while that dog is
/// not running, whatever stage the plan gave it;
/// `snapshot::warn_about_the_graph` reports the edge.
/// Answers with every instance started, in stage order.
///
/// `notify_socket` is the boot restore's own `$NOTIFY_SOCKET`; every other
/// walk passes `None`. With one, each stage announces its `bound` to it just
/// before it starts, so a unit's `TimeoutStartSec` follows the walk instead of
/// having to cover the sum of the stages. A stage nothing waits on announces
/// too, since its spawns take time. A stage that is waited on announces a
/// second time once its spawns have returned, because its wait starts then.
///
/// **An [`BatchPolicy::AllOrNothing`] batch that fails here leaves a PARTIAL
/// flock.** That policy refuses one `Command::Start` before it registers
/// anything, and while `shep start` was one such call the refusal really did
/// mean an untouched flock. It now covers the checks one stage can make in
/// advance, and not even the whole of that stage: stage 0 is running by the
/// time stage 1 proves unstartable, and a spawn that fails part way through a
/// stage leaves the members ahead of it running too. Nothing is rolled back,
/// deliberately, since rolling back means stopping apps that came up fine on
/// a guess about what the operator wanted. [`left_running`] and
/// [`corrected_for_earlier_stages`] are what make the refusal say so.
///
/// # Errors
///
/// - [`SupervisorError`]: under [`BatchPolicy::AllOrNothing`] only, the first
///   stage that did not start, with everything this start left running named
///   in its message. That policy is the operator's, and an operator
///   who typed `shep start` gets the failure back rather than a warning in a
///   log they are not reading; a later stage waits on the stage that failed,
///   so there is nothing useful to run after it either. Under
///   [`BatchPolicy::PerApp`], the boot's policy, a failed stage is warned
///   about and the walk continues, for the reason `spawn_enabled_dogs` gives:
///   a gap is better than an outage.
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
    // stage than its dependants; a cycle is the exception the graph keeps,
    // and there every member depends on another member of its own stage, so
    // a two-node cycle with nothing downstream still costs the stage a wait
    // gated purely by what sits beside it.
    let mut depended_on: BTreeSet<&str> = BTreeSet::new();
    for app in apps {
        depended_on.extend(app.config().depends_on.iter().map(String::as_str));
    }

    let mut started = Vec::new();
    for (index, stage) in plan.stages.iter().enumerate() {
        // Walked over `apps` rather than over `stage`, so a stage keeps the
        // order the caller handed it. A stage's names are sorted, and under
        // `BatchPolicy::AllOrNothing` a spawn that fails ends the batch where
        // it stands, so taking the sorted order would decide which of an
        // operator's apps came up by how their names happen to compare.
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
        // the first spawn only: `respawn` reads a sheep's own readiness
        // source and never this set. A sheep with neither a `readiness_probe`
        // nor `wait_ready` therefore comes back `Online` at once; one with
        // either still parks at `Starting` and re-arms its readiness task,
        // exactly as on the first spawn. What neither re-enters is THIS wait,
        // which is the part that matters. A crashed app has already settled
        // its stage, by crashing, and no later stage is still waiting to
        // learn its fate.
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
        // reading past a fast app's `Online`. `await_stage` reads the flock
        // too and would recover that particular case, but only as a snapshot
        // taken once; an early cursor is what makes the handoff from that
        // snapshot to the stream gapless. Same reasoning `boot` gives for
        // subscribing `spawn_dog_watch` ahead of the supervisor.
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
/// The second half is why the flock is read at all. `do_start` refuses a
/// whole batch before registering anything only for the checks it can make
/// in advance; a spawn that fails anyway leaves the batch part-registered,
/// because only exec knows for certain, and those apps are in no stage this
/// walk has completed. `started` alone therefore named some of what a staged
/// start left running rather than all of it.
///
/// One `Command::List` on a failure path, and only under
/// [`BatchPolicy::AllOrNothing`]: the walk is ending either way.
///
/// A stage member the flock holds as `Stopped` or `Errored` is left out. It
/// is registered, so `shep delete` still has something to do about it, but
/// the message this feeds is about children that are up and are being left
/// alone.
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
/// registers anything, and both its messages say so. That was the whole truth
/// while `shep start` was one such call. It is now the truth about ONE stage,
/// and not even all of that: stage 0 is running by the time stage 1 proves
/// unstartable, and a spawn failure part way through stage 1 leaves the
/// members it reached first running too. Nothing rolls any of it back,
/// because rolling back means stopping apps that came up fine on a guess
/// about what the operator wanted. So the message says which ones they are
/// instead, and an operator who wants them down types `shep stop`.
///
/// Only the two variants carrying a message about registration are touched.
/// Every other one is about a single sheep and says nothing this could
/// correct.
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
/// A member that dies resolves at once rather than holding its stage for the
/// full deadline, which is what keeps a missing binary from costing the boot
/// its whole budget.
///
/// Two sources with one definition. The flock, read through `drop_settled`,
/// is the only thing that decides a name has settled, because a name is an
/// app and an app can hold several instances. The bus decides only *when* to
/// ask: an event is a trigger, never an answer. The read alone would be a
/// snapshot that misses whatever settles after it, and the stream alone would
/// miss what settled while the start call was still returning.
///
/// The cost of that is one `Command::List` round trip per settling event of a
/// name this stage is waiting on, which is bounded by the stage's own size.
///
/// Everything is inside `bound`, the flock read included: it awaits an mpsc
/// send and a oneshot on the actor, neither of which carries a deadline of
/// its own.
///
/// `rpc/dispatch.rs`'s ordered restart reaches this too, for the same question: a
/// restart is done when the sheep it respawned has stopped being `Starting`.
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
/// The reload half of [`await_stage`], and it cannot share that function's
/// shape. A reload leaves an app serving from start to finish, so no status
/// a flock read could return says whether a swap has happened; the bus is
/// the only source there is. `waiting` therefore carries a count rather than
/// a name alone: `advance_reload` replaces one instance at a time and emits
/// a `Reloaded` for each, so a name is finished when it has emitted as many
/// as the reload accepted.
///
/// A `ReloadAbandoned` finishes a name outright, whatever its count: an
/// abandonment ends the whole reload and leaves the instances it had not
/// reached alone, so nothing further about that name is coming.
///
/// A lagged receiver is left to the bound rather than recovered from, which
/// is where this parts company with [`await_stage`]: that one asks the flock,
/// and there is no flock read that could answer this question. A lag costs
/// the stage its bound and no more, and the walk advances either way.
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
/// other status is an answer, including the ones that are bad news, since a
/// stage waits to learn its members' fate rather than to see them succeed.
///
/// Read off the event as well as off the flock, rather than matching on
/// `ProcessEventKind`: an `autorestart = false` sheep that exits is announced
/// as `process.stop`, not `process.exit`, so a kind list written from the
/// boot path's vocabulary misses exactly the member this wait exists to not
/// hang on.
fn is_settled(status: ProcStatus) -> bool {
    !matches!(status, ProcStatus::Starting)
}

/// Drops from `waiting` every name the flock has no unanswered instance of.
///
/// A name the flock does not hold at all is settled too: the start meant to
/// register it has already returned, and nothing else will.
///
/// A name with several instances is kept while any one of them is still
/// starting. That is the whole reason this is the only definition of settled
/// and the bus is a trigger: every instance of an app publishes under the
/// same `ProcessInfo::name`, so settling on an event would release the name
/// on whichever instance answered first. `AppConfig::depends_on` promises the
/// opposite, and with probe-gated instances warming up unevenly the gap is
/// seconds, not a window.
///
/// An actor that has gone empties `waiting` outright. Nothing will ever
/// report now, so holding the boot for the bound would only delay the same
/// answer.
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
        // cost the SUM of the stage's `kill_timeout`s rather than the longest
        // one. That is what the flock-wide `shutdown` this replaced cost, the
        // ladder is 1600ms by default with no upper bound, and nothing rescues
        // a slow teardown: a repeat signal is a documented no-op while one
        // runs, and the platform kills the daemon on its own clock. Nothing
        // is lost by overlapping them: a Kahn stage's members have no edges
        // between each other, and the one stage that does is the cyclic one
        // the sort lifts in unordered, where every member depends on another
        // member and no order is the correct one anyway.
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
/// The edges are read by the caller rather than here, because the one caller
/// there is reads them before it can call: `shep dev` clears the registry
/// ahead of the final roll, so a walk that read it at this point would plan
/// nothing and that session would get no ordered teardown at all. A name the
/// flock no longer holds costs a `NotFound` warning and nothing else.
///
/// `dogs` names the dogs this shepherd spawned at boot, and each one is
/// dropped from the plan before it is built. [`stop_in_reverse`] stops
/// whatever it is handed, so this is the only place the dogs rule can be
/// enforced: they are stopped afterwards by `SupervisorHandle::shutdown`,
/// because monitoring should outlive what it monitors. The registry holds
/// sheep alone, so the filter usually removes nothing.
///
/// It is the boot-time spawn list specifically, `RpcContext::dog_names`, and
/// not every name this shepherd knows: a dog adopted against a running
/// shepherd lands in `RpcContext::known_dogs` instead and is not filtered
/// here. No live bug follows, because every other door into the registry
/// refuses a dog, and widening the filter to `known_dogs` would cost more
/// than it bought: that list carries dogs nobody enabled, so a sheep sharing
/// a name with one would drop out of the ordered walk and be killed by the
/// backstop with its dependants still running.
///
/// A sheep sharing a dog's name is dropped with it, and killed in that same
/// `shutdown`, which is where every sheep this walk misses is killed anyway.
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
