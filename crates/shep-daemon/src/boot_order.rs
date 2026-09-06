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

use shep_core::config::ResolvedApp;
use shep_core::config::graph::{BootNode, BootPlan, NodeKind};
use shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo};
use shep_core::selector::ProcessSelector;
use shep_core::status::ProcStatus;
use tokio::sync::broadcast::{self, error::RecvError};

use crate::bus::{Bus, SharedEvent};
use crate::snapshot::FlockRegistry;
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
/// `rpc.rs`'s ordered restart reaches this too, for the same question: a
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
        // runs, and the platform kills the daemon on its own clock. Nothing is
        // lost by overlapping them, since a stage's members have no edges
        // between each other by construction.
        futures_util::future::join_all(stage.iter().map(|name| async move {
            if let Err(err) = supervisor.stop(ProcessSelector::Name(name.clone())).await {
                tracing::warn!(sheep = %name, %err, "a sheep did not stop in its stage");
            }
        }))
        .await;
    }
}

/// Stops every sheep the registry holds, last stage first, leaving the dogs
/// running.
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
pub(crate) async fn stop_registered_in_reverse(
    registry: &FlockRegistry,
    dogs: &[String],
    supervisor: &SupervisorHandle,
) {
    stop_edges_in_reverse(registry.depends_on_by_name(), dogs, supervisor).await;
}

/// [`stop_registered_in_reverse`] against edges read earlier.
///
/// For the one caller whose registry is empty by the time the walk runs:
/// `shep dev` clears it before the final roll is written, so that session
/// reads its edges first and stops against them here. A name the flock no
/// longer holds costs a `NotFound` warning and nothing else.
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
mod tests {
    use super::*;

    use shep_core::config::graph::plan;
    use shep_core::config::{AppConfig, normalize_all};
    use shep_core::protocol::DogSource;
    use shep_core::values::UpDuration;

    use crate::fake::ProcScript;
    use crate::testing::{harness, harness_failing_to_spawn, harness_refusing};

    /// The bound a test passes `await_stage` when it expects the wait to end
    /// on its own answer rather than on the clock.
    ///
    /// Short, because a test that reaches it either failed already or is
    /// asserting that nothing settled. Every caller runs under
    /// `start_paused`, so an idle runtime advances the clock to it rather
    /// than a test sitting through it; the value is still short enough to
    /// read as a bound rather than as a duration under test.
    const SHORT_BOUND: Duration = Duration::from_millis(500);

    /// `AppConfig`'s own default `kill_timeout`, which every app built by
    /// `AppConfig::minimal` here carries.
    const DEFAULT_KILL_TIMEOUT: Duration = Duration::from_millis(1600);

    /// Every `process.*` event waiting on `rx`, as `"<kind> <name>"`, in the
    /// order the bus carried them.
    fn drain(rx: &mut broadcast::Receiver<SharedEvent>) -> Vec<String> {
        let mut order = Vec::new();
        while let Ok(event) = rx.try_recv() {
            let BusEvent::Process {
                event: kind, info, ..
            } = &*event
            else {
                continue;
            };
            order.push(format!("{kind:?} {}", info.name));
        }
        order
    }

    /// One `process.online` for `name`, as a synthetic event a test can put on
    /// the bus itself.
    fn online_event(info: ProcessInfo) -> BusEvent {
        BusEvent::Process {
            event: ProcessEventKind::Online,
            info,
            manually: true,
            at_ms: 0,
        }
    }

    /// `db` at a 50ms readiness deadline, plus an `api` that waits for it.
    fn db_then_api() -> Vec<ResolvedApp> {
        let mut db = AppConfig::minimal("db", "./sleep");
        db.listen_timeout = UpDuration::from_millis(50);
        let mut api = AppConfig::minimal("api", "./sleep");
        api.depends_on = vec!["db".to_string()];
        normalize_all(vec![db, api]).expect("two apps, one edge, no cycle")
    }

    /// `db`, plus a `worker` that waits for it.
    ///
    /// Named so that the stop order (`worker`, then `db`) is not the order
    /// the names sort in: a walk that lost the stages and stopped the whole
    /// registry in one pass would still pass an `api`/`db` assertion.
    fn db_then_worker() -> Vec<ResolvedApp> {
        let db = AppConfig::minimal("db", "./sleep");
        let mut worker = AppConfig::minimal("worker", "./sleep");
        worker.depends_on = vec!["db".to_string()];
        normalize_all(vec![db, worker]).expect("two apps, one edge, no cycle")
    }

    /// fails if a dog sharing a sheep's name becomes a second node. The
    /// sheep would then sit in two stages and be started twice, which is what
    /// `boot`'s own collision test sees as a second registered entry.
    #[test]
    fn a_dog_named_after_a_sheep_is_not_a_node_of_its_own() {
        let apps = normalize_all(vec![AppConfig::minimal("metrics", "./sleep")])
            .expect("one app, no edges");
        let nodes = nodes_for_with_dogs(&apps, &["metrics".to_string()], &[]);

        assert_eq!(nodes.len(), 1, "one name is one node: {nodes:?}");
        assert_eq!(nodes[0].kind, NodeKind::Sheep);
    }

    #[tokio::test(start_paused = true)]
    async fn a_later_stage_does_not_start_until_the_earlier_one_is_online() {
        // fails if the driver fires every stage at once, which is what the
        // supervisor already does and what this exists to change. Asserted as
        // a sequence rather than as two start names: walking the stages in
        // order gets the names right on its own, and only `db`'s `Online`
        // landing before `api`'s `Start` proves anything was waited for.
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let apps = db_then_api();
        let plan = plan(&nodes_for(&apps, &[]));
        assert_eq!(plan.stages, vec![vec!["db"], vec!["api"]]);

        let mut rx = h.ctx.events.subscribe();
        start_in_stages(
            &plan,
            &apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::PerApp,
        )
        .await
        .expect("`PerApp` never refuses a stage");

        assert_eq!(
            drain(&mut rx),
            vec!["Start db", "Online db", "Start api", "Online api"],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn one_stage_starts_its_members_in_the_order_the_caller_handed_them() {
        // fails if a stage is walked over its own sorted names: under
        // `AllOrNothing` a failed spawn ends the batch where it stands, so
        // the walk order decides which of an operator's apps come up, and
        // deciding it by how the names compare is arbitrary
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let apps = normalize_all(vec![
            AppConfig::minimal("zulu", "./sleep"),
            AppConfig::minimal("alpha", "./sleep"),
        ])
        .expect("two apps, no edges");
        let plan = plan(&nodes_for(&apps, &[]));
        assert_eq!(plan.stages, vec![vec!["alpha", "zulu"]], "one sorted stage");

        let mut rx = h.ctx.events.subscribe();
        start_in_stages(
            &plan,
            &apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::AllOrNothing,
        )
        .await
        .expect("both scripts spawn");

        let starts: Vec<String> = drain(&mut rx)
            .into_iter()
            .filter(|line| line.starts_with("Start "))
            .collect();
        assert_eq!(starts, vec!["Start zulu", "Start alpha"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_member_that_exits_does_not_hold_its_stage() {
        // fails if the driver waits for a live sheep only: a dependency whose
        // binary is missing would then hold every later stage for the full
        // deadline instead of resolving at once. The first script exits at
        // once, standing in for a binary that is not there. Both durations
        // are virtual under `start_paused`, so the 30s deadline this must not
        // pay and the 5s bound that catches it cost nothing either way.
        let h = harness(vec![ProcScript::const_exit(1), ProcScript::never_exits()]);
        let mut db = AppConfig::minimal("db", "./does-not-exist");
        db.listen_timeout = UpDuration::from_millis(30_000);
        db.autorestart = false;
        let mut api = AppConfig::minimal("api", "./sleep");
        api.depends_on = vec!["db".to_string()];
        let apps = normalize_all(vec![db, api]).expect("two apps, one edge, no cycle");
        let plan = plan(&nodes_for(&apps, &[]));

        let started = tokio::time::timeout(
            Duration::from_secs(5),
            start_in_stages(
                &plan,
                &apps,
                &h.ctx.supervisor,
                &h.ctx.events,
                BatchPolicy::PerApp,
            ),
        )
        .await
        .expect("a dead dependency must not hold the stage for its deadline")
        .expect("`PerApp` never refuses a stage");
        assert!(started.iter().any(|info| info.name == "api"));
    }

    #[tokio::test]
    async fn a_dog_lands_in_the_final_stage_and_boot_first_moves_it() {
        // fails if the dogs-last default is lost, which would move every
        // existing install's boot order
        let apps = normalize_all(vec![AppConfig::minimal("web", "./sleep")]).expect("one app");
        let dogs = ["metrics".to_string()];
        let plain = plan(&nodes_for(&apps, &dogs));
        assert_eq!(plain.stages, vec![vec!["web"], vec!["metrics"]]);
        let promoted = plan_for(&apps, &dogs, &["metrics".to_string()]);
        assert_eq!(promoted.stages, vec![vec!["metrics"], vec!["web"]]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_stage_names_what_earlier_stages_already_started() {
        // fails if the refusal still promises an untouched flock. `shep start`
        // was one `Command::Start`, so `AllOrNothing` really did register
        // nothing and the message saying so was true. A staged batch refuses
        // one stage at a time, and `db` is up by the time `api` is refused.
        let h = harness_refusing(vec![ProcScript::never_exits()], &["api"]);
        let apps = db_then_api();
        let plan = plan(&nodes_for(&apps, &[]));

        let err = start_in_stages(
            &plan,
            &apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::AllOrNothing,
        )
        .await
        .expect_err("a refused preflight ends an `AllOrNothing` batch");

        let SupervisorError::CannotStart(message) = &err else {
            panic!("a refused preflight is `CannotStart`, got {err:?}");
        };
        assert!(
            message.contains("db"),
            "the refusal must name the stage that is running: {message}"
        );
        let flock = h
            .ctx
            .supervisor
            .list_checked()
            .await
            .expect("the actor is up");
        assert_eq!(
            flock
                .iter()
                .map(|info| info.name.as_str())
                .collect::<Vec<_>>(),
            vec!["db"],
            "the earlier stage is deliberately left running"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stage_that_fails_part_way_names_what_it_left_running() {
        // fails if the refusal names completed stages only: `do_start`
        // refuses a batch up front for the checks it can make in advance,
        // and a spawn that fails anyway leaves the members ahead of it
        // running, in a stage no completed-stage list holds. One stage here,
        // so `started` is empty and the note exists only if the flock was
        // read.
        let h = harness_failing_to_spawn(
            vec![ProcScript::never_exits(), ProcScript::never_exits()],
            &["zulu"],
        );
        let apps = normalize_all(vec![
            AppConfig::minimal("alpha", "./sleep"),
            AppConfig::minimal("zulu", "./sleep"),
        ])
        .expect("two apps, no edges");
        let plan = plan(&nodes_for(&apps, &[]));
        assert_eq!(plan.stages, vec![vec!["alpha", "zulu"]], "one stage");

        let err = start_in_stages(
            &plan,
            &apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::AllOrNothing,
        )
        .await
        .expect_err("a failed spawn ends an `AllOrNothing` batch");

        let SupervisorError::SpawnFailed(message) = &err else {
            panic!("a failed spawn is `SpawnFailed`, got {err:?}");
        };
        assert!(
            message.contains("alpha"),
            "the member that came up first must be named: {message}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn one_stage_stops_its_members_at_the_same_time() {
        // fails if the walk awaits each stop to full termination before it
        // arms the next, which costs a teardown the SUM of its members' kill
        // ladders where the flock-wide shutdown it replaced cost the longest
        // one. Two sheep in one stage that ignore SIGTERM, so each burns its
        // whole 1600ms ladder: overlapped that is one ladder, serialized it is
        // two. Virtual time under `start_paused`, which advances only when
        // every task is idle, so the two are exact rather than close.
        let h = harness(vec![
            ProcScript::ignores_signals(),
            ProcScript::ignores_signals(),
        ]);
        let apps = normalize_all(vec![
            AppConfig::minimal("alpha", "./sleep"),
            AppConfig::minimal("zulu", "./sleep"),
        ])
        .expect("two apps, no edges");
        let plan = plan(&nodes_for(&apps, &[]));
        assert_eq!(plan.stages, vec![vec!["alpha", "zulu"]], "one stage");
        h.ctx
            .supervisor
            .start(apps)
            .await
            .expect("two scripted apps start");

        let began = tokio::time::Instant::now();
        stop_in_reverse(&plan, &h.ctx.supervisor).await;
        let spent = began.elapsed();

        assert!(
            spent < DEFAULT_KILL_TIMEOUT * 2,
            "one stage's ladders must overlap; spent {spent:?} on two of them"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_walks_the_stages_backwards() {
        // fails if shutdown stays parallel, which gives a worker and its
        // database the same SIGTERM millisecond. Asserted per stage rather
        // than as a flat sequence of names: a stage's members are stopped
        // concurrently, so which of `api` and `worker` reaches the bus first
        // is not something this walk decides. What it does decide is that
        // both of them stop before `db` does.
        //
        // The later stage's members ignore signals and `db` does not, so
        // they have a kill ladder to burn and it does not. That is what
        // makes the assertion read stage boundaries rather than poll order:
        // with every stop flattened into one `join_all`, `db` answers first
        // and the bus carries `Stop db` at the front. Scripts are handed out
        // in spawn order, which is the order `apps` lists them.
        let h = harness(vec![
            ProcScript::never_exits(),
            ProcScript::ignores_signals(),
            ProcScript::ignores_signals(),
        ]);
        let db = AppConfig::minimal("db", "./sleep");
        let mut api = AppConfig::minimal("api", "./sleep");
        api.depends_on = vec!["db".to_string()];
        let mut worker = AppConfig::minimal("worker", "./sleep");
        worker.depends_on = vec!["db".to_string()];
        let apps = normalize_all(vec![db, api, worker]).expect("three apps, two edges, no cycle");
        let plan = plan(&nodes_for(&apps, &[]));
        assert_eq!(plan.stages, vec![vec!["db"], vec!["api", "worker"]]);
        start_in_stages(
            &plan,
            &apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::PerApp,
        )
        .await
        .expect("`PerApp` never refuses a stage");

        let mut rx = h.ctx.events.subscribe();
        stop_in_reverse(&plan, &h.ctx.supervisor).await;

        let stops = drain(&mut rx);
        let (last_stage, first_stage) = stops.split_at(2);
        assert_eq!(
            last_stage.iter().collect::<BTreeSet<_>>(),
            ["Stop api".to_string(), "Stop worker".to_string()]
                .iter()
                .collect::<BTreeSet<_>>(),
            "the later stage stops first, in no order of its own: {stops:?}"
        );
        assert_eq!(first_stage, ["Stop db".to_string()], "{stops:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_shutdown_stops_dependents_before_their_dependencies() {
        // fails if the teardown claims every online sheep at once, which
        // gives a worker and its database the same SIGTERM millisecond.
        // `worker` ignores signals and `db` does not, so the later stage has
        // a kill ladder to burn: flatten the walk and `db` answers first,
        // which is the only thing that makes this assertion about stages
        // rather than about poll order.
        let h = harness(vec![
            ProcScript::never_exits(),
            ProcScript::ignores_signals(),
        ]);
        let apps = db_then_worker();
        h.ctx.registry.record(&apps);
        let plan = plan(&nodes_for(&apps, &[]));
        start_in_stages(
            &plan,
            &apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::PerApp,
        )
        .await
        .expect("`PerApp` never refuses a stage");

        let mut rx = h.ctx.events.subscribe();
        stop_registered_in_reverse(&h.ctx.registry, &h.ctx.dog_names, &h.ctx.supervisor).await;

        assert_eq!(drain(&mut rx), vec!["Stop worker", "Stop db"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dog_stops_after_every_sheep() {
        // fails if dogs join the reverse stages, which would kill the bark
        // dog before the flock it reports on
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let apps = normalize_all(vec![AppConfig::minimal("web", "./sleep")]).expect("one app");
        h.ctx.registry.record(&apps);
        h.ctx
            .supervisor
            .start(apps)
            .await
            .expect("a scripted app that never exits starts");
        // Recorded as if it were a sheep, on purpose: the registry holds
        // sheep only, so a dog absent from it would pass this test whether
        // the dogs list is consulted or not.
        let dog = normalize_all(vec![AppConfig::minimal("metrics", "./sleep")])
            .expect("one dog app")
            .remove(0);
        h.ctx.registry.record(std::slice::from_ref(&dog));
        h.ctx
            .supervisor
            .start_dog(dog, DogSource::BuiltIn)
            .await
            .expect("a scripted dog that never exits starts");

        let mut rx = h.ctx.events.subscribe();
        stop_registered_in_reverse(&h.ctx.registry, &["metrics".to_string()], &h.ctx.supervisor)
            .await;
        assert_eq!(
            drain(&mut rx),
            vec!["Stop web"],
            "the staged walk must leave the dog running"
        );

        h.ctx.supervisor.shutdown().await;
        assert_eq!(
            drain(&mut rx),
            vec!["Stop metrics"],
            "the backstop is what stops a dog"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stage_whose_members_have_already_settled_waits_for_no_event_at_all() {
        // fails if the driver reads the bus alone: an app that reached its
        // answer while the start call was still returning would then be
        // waited on until the bound, since its event is behind the cursor
        // rather than ahead of it. Nothing will ever put that event on the bus
        // again, so only the flock read can end this wait, and the short bound
        // is what it costs when it does not happen.
        let h = harness(vec![ProcScript::never_exits()]);
        let apps = normalize_all(vec![AppConfig::minimal("db", "./sleep")]).expect("one app");
        h.ctx
            .supervisor
            .start(apps)
            .await
            .expect("a scripted app that never exits starts");

        let rx = h.ctx.events.subscribe();
        let waiting = ["db".to_string()].into_iter().collect();
        let unsettled = await_stage(rx, waiting, SHORT_BOUND, &h.ctx.supervisor).await;

        assert!(unsettled.is_empty(), "db is online, so nothing is waiting");
    }

    #[tokio::test(start_paused = true)]
    async fn one_instance_going_online_does_not_settle_a_multi_instance_dependency() {
        // fails if the wait settles a name off the event that triggered it:
        // every instance of an app publishes under the same `ProcessInfo::name`,
        // so instance 0 passing its probe would release a name whose other two
        // instances are seconds away, against what `AppConfig::depends_on`
        // promises. The event is put on the bus before the wait starts, so
        // there is no race over whether it was read; the bound is what the
        // wait costs when nothing settles, not a duration under test.
        let h = harness(vec![
            ProcScript::never_exits(),
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ]);
        let mut web = AppConfig::minimal("web", "./sleep");
        web.instances = 3;
        web.listen_timeout = UpDuration::from_millis(30_000);
        let apps = normalize_all(vec![web]).expect("one app");
        let gate = ["web".to_string()].into_iter().collect();

        let rx = h.ctx.events.subscribe();
        let started = h
            .ctx
            .supervisor
            .start_staged(apps, gate, BatchPolicy::PerApp)
            .await
            .expect("a scripted app that never exits starts");
        assert_eq!(started.len(), 3);
        assert!(
            h.ctx
                .supervisor
                .list_checked()
                .await
                .expect("the actor is up")
                .iter()
                .all(|info| info.status == ProcStatus::Starting),
            "a gated app parks every instance at Starting"
        );

        let mut first = started[0].clone();
        first.status = ProcStatus::Online;
        h.ctx
            .events
            .send(online_event(first).into())
            .expect("the harness holds a receiver open");

        let waiting = ["web".to_string()].into_iter().collect();
        let unsettled = tokio::time::timeout(
            Duration::from_secs(5),
            await_stage(rx, waiting, SHORT_BOUND, &h.ctx.supervisor),
        )
        .await
        .expect("the wait is bounded, so it ends either way");

        assert!(
            unsettled.contains("web"),
            "two of web's three instances are still starting"
        );
    }
}
