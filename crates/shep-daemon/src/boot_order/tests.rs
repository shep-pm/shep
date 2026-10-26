//! Tests for the boot-order driver.

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
    let apps =
        normalize_all(vec![AppConfig::minimal("metrics", "./sleep")]).expect("one app, no edges");
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
        None,
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
        None,
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
            None,
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
        None,
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
        None,
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
        None,
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
        None,
    )
    .await
    .expect("`PerApp` never refuses a stage");

    let mut rx = h.ctx.events.subscribe();
    stop_edges_in_reverse(
        h.ctx.registry.depends_on_by_name(),
        &h.ctx.dog_names,
        &h.ctx.supervisor,
    )
    .await;

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
        .start_dog(dog, DogSource::BuiltIn { grants: None })
        .await
        .expect("a scripted dog that never exits starts");

    let mut rx = h.ctx.events.subscribe();
    stop_edges_in_reverse(
        h.ctx.registry.depends_on_by_name(),
        &["metrics".to_string()],
        &h.ctx.supervisor,
    )
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
