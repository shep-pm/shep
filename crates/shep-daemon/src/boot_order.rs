//! Running a flock in dependency order.
//!
//! The sort itself is `shep_core::config::graph`. This module runs what it
//! produces: start a stage, wait for it, advance.
//!
//! It lives outside the supervisor actor deliberately. `do_start` is a
//! synchronous `fn` reached from the actor's own message loop, and that loop
//! is what delivers `Msg::ReadyResult`, so a wait inside it could never end.

// Nothing calls in yet: the boot sequence and the RPC start path are the two
// callers, and both land after this module. `allow` rather than `expect`,
// which would be the self-removing spelling: the crate's own tests use every
// item here, so the expectation is unfulfilled in the `cfg(test)` build and
// `--all-targets` would refuse it. Delete this the moment a caller lands.
#![allow(dead_code)]

use core::time::Duration;

use std::collections::{BTreeMap, BTreeSet};

use shep_core::config::ResolvedApp;
use shep_core::config::graph::{BootNode, BootPlan, NodeKind};
use shep_core::protocol::{BusEvent, ProcessInfo};
use shep_core::selector::ProcessSelector;
use shep_core::status::ProcStatus;
use tokio::sync::broadcast::{self, error::RecvError};

use crate::bus::{Bus, SharedEvent};
use crate::supervisor::{BatchPolicy, SupervisorHandle};

/// How much longer than the stage's own longest `listen_timeout` the driver
/// waits before giving up on it.
///
/// Every member is already bounded by its own readiness task, which reports
/// at its own deadline, so this covers scheduling jitter only. The same
/// reasoning as `RELOAD_DEADLINE_SLACK`, and the same figure.
pub(crate) const STAGE_SLACK: Duration = Duration::from_secs(5);

/// Graph nodes for a flock plus its dogs, with no dog promoted ahead of the
/// flock.
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

/// Graph nodes for a flock plus its dogs.
///
/// `boot_first` names the dogs `[daemon] boot_first_dogs` promotes ahead of
/// every sheep. A dog carries no `depends_on` of its own: `dog_app` builds a
/// dog's config from `AppConfig::minimal`, so its list is always empty.
#[must_use]
pub(crate) fn nodes_for_with_dogs(
    apps: &[ResolvedApp],
    dogs: &[String],
    boot_first: &[String],
) -> Vec<BootNode> {
    let promoted: BTreeSet<&str> = boot_first.iter().map(String::as_str).collect();
    apps.iter()
        .map(|app| BootNode {
            name: app.config().name.clone(),
            depends_on: app.config().depends_on.clone(),
            kind: NodeKind::Sheep,
        })
        .chain(dogs.iter().map(|name| BootNode {
            name: name.clone(),
            depends_on: Vec::new(),
            kind: NodeKind::Dog {
                boot_first: promoted.contains(name.as_str()),
            },
        }))
        .collect()
}

/// Starts `apps` stage by stage, holding each stage until every member a
/// later stage waits on has settled.
///
/// Dogs in `plan` are skipped: they are spawned by `dogs::spawn_enabled_dogs`,
/// which the caller runs at the stage boundary this plan puts them in.
/// Answers with every instance started, in stage order.
pub(crate) async fn start_in_stages(
    plan: &BootPlan,
    apps: Vec<ResolvedApp>,
    supervisor: &SupervisorHandle,
    events: &Bus,
    policy: BatchPolicy,
) -> Vec<ProcessInfo> {
    let by_name: BTreeMap<&str, &ResolvedApp> = apps
        .iter()
        .map(|app| (app.config().name.as_str(), app))
        .collect();
    // Every name a later stage waits on. Read once, so a stage's own members
    // are gated by what follows them rather than by what sits beside them.
    let mut depended_on: BTreeSet<&str> = BTreeSet::new();
    for app in &apps {
        depended_on.extend(app.config().depends_on.iter().map(String::as_str));
    }

    let mut started = Vec::new();
    for (index, stage) in plan.stages.iter().enumerate() {
        let members: Vec<ResolvedApp> = stage
            .iter()
            .filter_map(|name| by_name.get(name.as_str()).map(|app| (*app).clone()))
            .collect();
        if members.is_empty() {
            continue;
        }
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
            // Never fails the boot for the reason `spawn_enabled_dogs` does
            // not: a stage that could not start is a gap, and refusing the
            // rest of the flock over it turns the gap into an outage.
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
    started
}

/// Waits until every name in `waiting` has reached a settled answer, or until
/// `bound` elapses. Answers with the names that never settled.
///
/// A member that dies resolves at once rather than holding its stage for the
/// full deadline, which is what keeps a missing binary from costing the boot
/// its whole budget.
///
/// Two sources, because neither alone closes the window. The flock's own
/// state, read once before any wait, answers for a member that settled while
/// the start call was still returning. The bus answers for a member that has
/// settled and moved on since, a restarted one being back at `Starting` by
/// the time the read lands.
async fn await_stage(
    mut rx: broadcast::Receiver<SharedEvent>,
    mut waiting: BTreeSet<String>,
    bound: Duration,
    supervisor: &SupervisorHandle,
) -> BTreeSet<String> {
    drop_settled(supervisor, &mut waiting).await;
    if waiting.is_empty() {
        return waiting;
    }
    let settle = async {
        while !waiting.is_empty() {
            match rx.recv().await {
                Ok(event) => {
                    let BusEvent::Process { info, .. } = &*event else {
                        continue;
                    };
                    if is_settled(info.status) {
                        waiting.remove(&info.name);
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
/// starting, which is stricter than the event path's first-instance-wins.
/// That is deliberate: this is a net under that path, and a net that
/// resolves earlier than the path it backs would be the bug.
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
        for name in stage {
            // One name at a time rather than concurrently: the members of one
            // stage do not depend on each other, so nothing is gained by
            // overlapping them, and a serial walk keeps the emitted order
            // readable in the log.
            if let Err(err) = supervisor.stop(ProcessSelector::Name(name.clone())).await {
                tracing::warn!(sheep = %name, %err, "a sheep did not stop in its stage");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use shep_core::config::graph::plan;
    use shep_core::config::{AppConfig, normalize_all};
    use shep_core::protocol::ProcessEventKind;
    use shep_core::values::UpDuration;

    use crate::fake::ProcScript;
    use crate::testing::harness;

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

    #[tokio::test]
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
            apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::PerApp,
        )
        .await;

        assert_eq!(
            drain(&mut rx),
            vec!["Start db", "Online db", "Start api", "Online api"],
        );
    }

    #[tokio::test]
    async fn a_member_that_exits_does_not_hold_its_stage() {
        // fails if the driver waits for a live sheep only: a dependency whose
        // binary is missing would then hold every later stage for the full
        // deadline instead of resolving at once. The first script exits at
        // once, standing in for a binary that is not there.
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
                apps,
                &h.ctx.supervisor,
                &h.ctx.events,
                BatchPolicy::PerApp,
            ),
        )
        .await
        .expect("a dead dependency must not hold the stage for its deadline");
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

    #[tokio::test]
    async fn stopping_walks_the_stages_backwards() {
        // fails if shutdown stays parallel, which gives a worker and its
        // database the same SIGTERM millisecond
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let apps = db_then_api();
        let plan = plan(&nodes_for(&apps, &[]));
        start_in_stages(
            &plan,
            apps,
            &h.ctx.supervisor,
            &h.ctx.events,
            BatchPolicy::PerApp,
        )
        .await;

        let mut rx = h.ctx.events.subscribe();
        stop_in_reverse(&plan, &h.ctx.supervisor).await;

        assert_eq!(drain(&mut rx), vec!["Stop api", "Stop db"]);
    }

    #[tokio::test]
    async fn a_stage_whose_members_have_already_settled_waits_for_no_event_at_all() {
        // fails if the driver reads the bus alone: an app that reached its
        // answer while the start call was still returning would then be
        // waited on until the bound, since its event is behind the cursor
        // rather than ahead of it. The bound here is zero, so anything the
        // wait does before reading the flock's own state fails the test.
        let h = harness(vec![ProcScript::never_exits()]);
        let apps = normalize_all(vec![AppConfig::minimal("db", "./sleep")]).expect("one app");
        h.ctx
            .supervisor
            .start(apps)
            .await
            .expect("a scripted app that never exits starts");

        let rx = h.ctx.events.subscribe();
        let waiting = ["db".to_string()].into_iter().collect();
        let unsettled = await_stage(rx, waiting, Duration::ZERO, &h.ctx.supervisor).await;

        assert!(unsettled.is_empty(), "db is online, so nothing is waiting");
    }

    #[tokio::test]
    async fn an_event_that_landed_before_the_wait_began_still_settles_the_stage() {
        // fails if the subscription is taken after the start rather than
        // before it: a fast app's `Online` lands in the gap and the stage
        // then waits out its whole bound for an event nobody will send again.
        // `db` is still `Starting` here, so only the buffered event can end
        // this wait.
        let h = harness(vec![ProcScript::never_exits()]);
        let mut db = AppConfig::minimal("db", "./sleep");
        db.listen_timeout = UpDuration::from_millis(30_000);
        let apps = normalize_all(vec![db]).expect("one app");
        let gate = ["db".to_string()].into_iter().collect();

        let rx = h.ctx.events.subscribe();
        let started = h
            .ctx
            .supervisor
            .start_staged(apps, gate, BatchPolicy::PerApp)
            .await
            .expect("a scripted app that never exits starts");
        let mut info = started[0].clone();
        assert_eq!(info.status, ProcStatus::Starting);
        info.status = ProcStatus::Online;
        h.ctx
            .events
            .send(online_event(info).into())
            .expect("the harness holds a receiver open");

        let waiting = ["db".to_string()].into_iter().collect();
        let unsettled = tokio::time::timeout(
            Duration::from_secs(5),
            await_stage(rx, waiting, Duration::from_secs(30), &h.ctx.supervisor),
        )
        .await
        .expect("an event already on the bus must end the wait");

        assert!(
            unsettled.is_empty(),
            "db reported online, so nothing is waiting"
        );
    }
}
