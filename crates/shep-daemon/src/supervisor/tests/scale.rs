//! Tests for changing an app's instance count.
//!
//! Scaling up fills the lowest free slots and scaling down takes the highest,
//! so a round trip returns to where it started. The refusals matter as much:
//! zero, a dog, an app mid-reload, or one whose last scale is still leaving.

use super::*;

/// The instance slot of every registered instance of `name`, ascending.
///
/// Read off `out_file` rather than off the entry: a build deriving the log
/// path from something else would pass an assertion on the internal field.
///
/// # Panics
///
/// If a matched row carries no `out_file`, or one this fixture cannot parse.
async fn instance_slots_of(h: &Harness, name: &str) -> Vec<u32> {
    let mut slots: Vec<u32> = h
        .ctx
        .supervisor
        .list()
        .await
        .iter()
        .filter(|info| info.name == name)
        .map(|info| {
            let out = info
                .out_file
                .as_deref()
                .expect("a listed sheep has a log path");
            let stem = std::path::Path::new(out)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .and_then(|file| file.strip_suffix("-out.log"))
                .and_then(|stem| stem.strip_prefix(&format!("{name}-")))
                .expect("a derived log path is `<name>-<instance>-out.log`");
            stem.parse().expect("the instance slot is a number")
        })
        .collect();
    slots.sort_unstable();
    slots
}

/// Waits until `name` has exactly `count` registered instances, or fails.
///
/// A scale-down's reply is the survivors and does not wait for the
/// departures, so a case asserting on the flock afterwards has to wait for
/// the kill ladders it started. Bounded, or the poll loop never ends.
async fn settle_to(h: &Harness, name: &str, count: usize) {
    let settled = tokio::time::timeout(SWAP_WINDOW, async {
        loop {
            let live = h
                .ctx
                .supervisor
                .list()
                .await
                .iter()
                .filter(|info| info.name == name)
                .count();
            if live == count {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(settled.is_ok(), "{name} never settled to {count} instances");
}

/// A bare actor holding `instances` online instances of one app, all
/// carrying the same normalized spec. The stored-count cases need it,
/// since that field reaches no reply.
fn actor_with_a_scaled_app(
    dir: &tempfile::TempDir,
    instances: u32,
    scripts: Vec<ProcScript>,
) -> Actor<ScriptedRunner> {
    let paths = test_paths(dir);
    let app = normalize(AppConfig {
        instances,
        ..AppConfig::minimal("web", "./srv")
    })
    .unwrap();
    let mut sheep = HashMap::new();
    for instance in 0..instances {
        sheep.insert(
            instance,
            SheepSlot::new(armed_entry(
                instance,
                instance,
                1111 + instance,
                app.clone(),
                &paths,
            )),
        );
    }
    let (tx, _rx) = mpsc::channel(MAILBOX_CAPACITY);
    test_actor(paths, scripts, sheep, tx)
}

/// Every registered slot's stored instance count, ascending by id.
fn stored_instance_counts(actor: &Actor<ScriptedRunner>) -> Vec<u32> {
    let mut ids: Vec<u32> = actor.sheep.keys().copied().collect();
    ids.sort_unstable();
    ids.iter()
        .map(|id| actor.sheep[id].entry.spec.config().instances)
        .collect()
}

/// A `ReloadJob` built the way `advance_reload` builds one, for a case that
/// only needs `self.reloads` to hold an entry under `name`. `name` is for
/// the call site's readability: the map key is what associates a job with
/// an app.
fn reload_job_for(name: &str) -> ReloadJob {
    let _ = name;
    ReloadJob {
        queue: VecDeque::new(),
        mode: ReloadMode::Overlap,
        deadline: 0,
        swap: ReloadSwap {
            old_id: 0,
            new_id: Some(1),
            phase: ReloadPhase::AwaitReady,
        },
    }
}

/// Slot numbers reach the app (`SHEP_INSTANCE`) and the filesystem
/// (`web-2-out.log`), so which ones a scale hands out is a contract.
#[tokio::test(start_paused = true)]
async fn scaling_up_fills_the_lowest_free_slots() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_app(
        &h,
        AppConfig {
            instances: 2,
            ..AppConfig::minimal("web", "./srv")
        },
    )
    .await;

    let scaled = h.ctx.supervisor.scale("web", 4).await.unwrap();

    assert_eq!(scaled.instances.len(), 4);
    assert_eq!(instance_slots_of(&h, "web").await, vec![0, 1, 2, 3]);
}

/// Taking the highest makes 2 -> 4 -> 2 a round trip back to slots 0 and 1;
/// taking the lowest leaves 2 and 3, with different log files.
#[tokio::test(start_paused = true)]
async fn scaling_down_removes_the_highest_slots_so_a_round_trip_returns() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_app(
        &h,
        AppConfig {
            instances: 2,
            ..AppConfig::minimal("web", "./srv")
        },
    )
    .await;

    h.ctx.supervisor.scale("web", 4).await.unwrap();
    let scaled = h.ctx.supervisor.scale("web", 2).await.unwrap();

    assert_eq!(scaled.instances.len(), 2);
    // The reply is the survivors: the two removals still have ladders to run.
    settle_to(&h, "web", 2).await;
    assert_eq!(instance_slots_of(&h, "web").await, vec![0, 1]);
}

/// An operator re-running a provisioning script must not restart the flock.
#[tokio::test(start_paused = true)]
async fn scaling_to_the_current_count_is_a_no_op() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    start_app(
        &h,
        AppConfig {
            instances: 2,
            ..AppConfig::minimal("web", "./srv")
        },
    )
    .await;
    let before = h.ctx.supervisor.list().await;

    let scaled = h.ctx.supervisor.scale("web", 2).await.unwrap();

    assert_eq!(scaled.instances.len(), 2);
    let after = h.ctx.supervisor.list().await;
    assert_eq!(
        after.iter().map(|i| i.id).collect::<Vec<_>>(),
        before.iter().map(|i| i.id).collect::<Vec<_>>(),
        "a no-op scale replaced processes"
    );
    // Two scripts for two spawns: a scale that respawned would need a third.
}

/// `normalize` refuses `instances == 0` on every other path into the daemon.
#[tokio::test(start_paused = true)]
async fn scaling_to_zero_is_refused_and_names_delete() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_app(&h, AppConfig::minimal("web", "./srv")).await;

    let err = h.ctx.supervisor.scale("web", 0).await.unwrap_err();

    let SupervisorError::InvalidScale(message) = err else {
        panic!("expected InvalidScale, got {err:?}");
    };
    assert!(message.contains("delete"), "{message}");
}

/// `shep stock typo 4` must not exit 0.
#[tokio::test(start_paused = true)]
async fn scaling_an_unregistered_app_is_not_found() {
    let h = harness(vec![]);
    assert_eq!(
        h.ctx.supervisor.scale("ghost", 2).await.unwrap_err(),
        SupervisorError::NotFound
    );
}

/// A dog is one process by contract: two metrics dogs would race for the
/// same listen port, and two bark dogs would double every alert.
#[tokio::test(start_paused = true)]
async fn a_dog_cannot_be_scaled() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_a_sheep_and_a_dog(&dir);

    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Scale {
        name: "bark".to_string(),
        count: 2,
        reply,
    });

    let SupervisorError::InvalidScale(message) = answer.await.unwrap().unwrap_err() else {
        panic!("expected InvalidScale");
    };
    assert!(message.contains("dog"), "{message}");
}

/// A reload holds two live processes in one instance slot; a scale-down
/// picking that slot removes one and leaves the swap with nothing to finish.
///
/// Actor-tier: the guard reads `Actor::reloads`, which has no reply-side
/// spelling to fill from a handle.
#[tokio::test(start_paused = true)]
async fn an_app_mid_reload_refuses_a_scale() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_a_scaled_app(&dir, 2, vec![]);
    actor
        .reloads
        .insert("web".to_string(), reload_job_for("web"));

    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Scale {
        name: "web".to_string(),
        count: 4,
        reply,
    });

    assert_eq!(
        answer.await.unwrap().unwrap_err(),
        SupervisorError::ReloadInFlight("web".to_string())
    );
}

/// A scale-down's reply is the survivors and does not wait for the
/// departures, so those slots stay registered: a second scale counting them
/// calls itself a no-op and lets `rpc` record `instances = 4` for a flock
/// that settles to one.
///
/// The doomed three `never_reports_its_exit`, so the case is deterministic
/// rather than a race against its own kill ladders.
#[tokio::test(start_paused = true)]
async fn a_scale_is_refused_while_an_earlier_ones_departures_are_still_leaving() {
    // Scripts go out in spawn order, so the survivor gets the first.
    let h = harness(vec![
        ProcScript::never_exits(),
        ProcScript::never_reports_its_exit(),
        ProcScript::never_reports_its_exit(),
        ProcScript::never_reports_its_exit(),
    ]);
    start_app(
        &h,
        AppConfig {
            instances: 4,
            ..AppConfig::minimal("web", "./srv")
        },
    )
    .await;

    let down = h.ctx.supervisor.scale("web", 1).await.unwrap();
    assert_eq!(down.instances.len(), 1);

    let err = h.ctx.supervisor.scale("web", 4).await.unwrap_err();

    let SupervisorError::InvalidScale(message) = err else {
        panic!("expected InvalidScale, got {err:?}");
    };
    assert!(
        message.contains("3 instance(s) still shutting down"),
        "the refusal has to say how much of the flock is still moving: {message}"
    );
    assert!(
        message.contains("shep flock"),
        "the refusal has to name what to wait for: {message}"
    );
}

/// A refusal that never lifts leaves the operator unable to scale the app.
///
/// `never_exits` rather than the case above's wedged scripts: these obey
/// `SIGTERM`, so `settle_to` is the forcing mechanism.
#[tokio::test(start_paused = true)]
async fn a_scale_is_accepted_again_once_the_departures_have_left() {
    // Four for the first flock, three for the scale back up.
    let h = harness(vec![ProcScript::never_exits(); 7]);
    start_app(
        &h,
        AppConfig {
            instances: 4,
            ..AppConfig::minimal("web", "./srv")
        },
    )
    .await;

    h.ctx.supervisor.scale("web", 1).await.unwrap();
    settle_to(&h, "web", 1).await;

    let up = h.ctx.supervisor.scale("web", 4).await.unwrap();

    assert_eq!(up.instances.len(), 4);
    assert_eq!(up.shortfall, None);
    assert_eq!(instance_slots_of(&h, "web").await, vec![0, 1, 2, 3]);
}

/// Without the write-back, `shep stock web 4 && shep save` records
/// `instances = 2` and the next reboot reverts the scale.
///
/// Actor-tier: the stored count is `SheepSlot::entry.spec`, which reaches no
/// reply and no bus event. `Scaled::app` is checked too, since a build that
/// returned the right config and stored the wrong one passes either alone.
#[tokio::test(start_paused = true)]
async fn a_scale_updates_the_stored_instance_count_on_every_slot() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_a_scaled_app(&dir, 2, vec![ProcScript::never_exits(); 2]);

    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Scale {
        name: "web".to_string(),
        count: 4,
        reply,
    });

    let scaled = answer.await.unwrap().unwrap();
    assert_eq!(scaled.app.config().instances, 4);
    assert_eq!(stored_instance_counts(&actor), vec![4, 4, 4, 4]);
}

/// The instances that did spawn stay, since unwinding them would turn one
/// failed spawn into an outage, but every registered slot must claim the
/// number really running.
///
/// One script for two requested spawns, this module's way to make exactly
/// one spawn fail. Three entries, not two: `spawn_fresh` registers an
/// `Errored` slot for the failed attempt, which a later scale counts.
#[tokio::test(start_paused = true)]
async fn a_partial_scale_up_stores_the_count_it_achieved() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_a_scaled_app(&dir, 1, vec![ProcScript::never_exits()]);

    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Scale {
        name: "web".to_string(),
        count: 3,
        reply,
    });

    let scaled = answer.await.unwrap().expect(
        "a partial scale-up is a partial success: an `Err` here would take \
         the achieved config with it and leave the roll pre-scale",
    );
    assert_eq!(scaled.requested, 3);
    assert_eq!(scaled.achieved(), 2);
    assert!(
        scaled.shortfall.is_some(),
        "the shortfall has to survive the reply, or nothing downstream can \
         tell the operator they got two of three"
    );
    assert_eq!(scaled.app.config().instances, 2);
    assert_eq!(
        stored_instance_counts(&actor),
        vec![2, 2, 2],
        "the flock achieved two, so every registered slot — including the \
         errored attempt — must say two"
    );
}

/// Built so id order and name order disagree: a fixture whose two orders
/// coincide cannot tell the two implementations apart.
#[tokio::test]
async fn a_listing_groups_an_apps_instances_under_its_name() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    start_app(
        &h,
        AppConfig {
            instances: 2,
            ..AppConfig::minimal("zebra", "./z")
        },
    )
    .await;
    start_app(
        &h,
        AppConfig {
            instances: 2,
            ..AppConfig::minimal("alpha", "./a")
        },
    )
    .await;

    let listed = h.ctx.supervisor.list().await;
    let names: Vec<&str> = listed.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["alpha", "alpha", "zebra", "zebra"]);
    let ids: Vec<u32> = listed.iter().map(|i| i.id).collect();
    assert_ne!(
        ids,
        {
            let mut sorted = ids.clone();
            sorted.sort_unstable();
            sorted
        },
        "the fixture must make id order and name order disagree, or it proves nothing"
    );
}
