//! Tests for the drain half of a reload.
//!
//! Whether a reload can overlap depends on the probe: an app whose probe can
//! lie has to drain before its replacement starts. These cover that choice and
//! what a parked or abandoned swap leaves behind.

use super::*;

/// An app that probes a TCP address, the shape whose readiness answer the
/// wrong instance can give.
fn probed_app(name: &str) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.readiness_probe = Some(probe_config(ProbeKind::Tcp, "127.0.0.1:9"));
    app
}

// The `wait_ready` row has a probe configured and no `reuse_port`, so a
// rule reading the config's two fields directly would serialise it.
// `ReadinessSource::of` prefers the channel, and a replacement's channel is
// its own, so the mode derives from the source.
#[test]
fn reload_mode_serialises_exactly_the_apps_whose_probe_can_lie() {
    let cases = [
        (false, false, false, ReloadMode::Overlap),
        (false, true, false, ReloadMode::Overlap),
        (true, false, false, ReloadMode::Serial),
        (true, true, false, ReloadMode::Overlap),
        (true, false, true, ReloadMode::Overlap),
    ];
    for (probe, wait_ready, reuse_port, want) in cases {
        let mut app = if probe {
            probed_app("web")
        } else {
            AppConfig::minimal("web", "./srv")
        };
        app.wait_ready = wait_ready;
        app.reuse_port = reuse_port;

        let source = ReadinessSource::of(&app).expect("the target parses");
        assert_eq!(
            ReloadMode::of(&app, &source),
            want,
            "probe={probe} wait_ready={wait_ready} reuse_port={reuse_port}"
        );
    }
}

// An overlap would let the drainee answer the replacement's probe: the
// first probe lands at t=0, with the drainee still bound to the address the
// probe names. No scripts, so a `SpawnNew` here would panic.
#[tokio::test(start_paused = true)]
async fn a_probed_reload_drains_before_it_spawns_anything() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, probed_app("web"), vec![]);
    let (ctl_tx, mut ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    actor.sheep.get_mut(&0).expect("the fixture's sheep").ctl = Some(ctl_tx);

    actor.advance_reload("web", VecDeque::from([0]));

    let job = &actor.reloads["web"];
    assert_eq!(job.mode, ReloadMode::Serial);
    assert_eq!(job.swap.phase, ReloadPhase::DrainFirst);
    assert_eq!(
        job.swap.new_id, None,
        "a serial reload has no replacement until the drain is over"
    );
    assert_eq!(
        actor.sheep.len(),
        1,
        "nothing was spawned into the slot the drain is emptying"
    );

    let drainee = &actor.sheep[&0];
    assert_eq!(drainee.entry.status, ProcStatus::Stopping);
    assert_eq!(
        drainee.entry.reload,
        ReloadState::Drainee { new_id: None },
        "the marker is what keeps this exit off the ordinary respawn path"
    );
    assert!(
        drainee.manual.is_some(),
        "the drain owns the drainee's next exit"
    );
    assert!(
        ctl_rx.try_recv().is_ok(),
        "the drain asked the instance to go"
    );
}

// The case above's app with one line added: spawn the replacement at once,
// and leave the instance being replaced running.
#[tokio::test(start_paused = true)]
async fn reuse_port_buys_back_the_overlap_a_probe_would_otherwise_cost() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = probed_app("web");
    app.reuse_port = true;
    // One script for the replacement: the fixture's own instance is
    // registered without spawning.
    let (mut actor, _mailbox) =
        actor_with_one_online_sheep_of(&dir, app, vec![ProcScript::never_exits()]);

    actor.advance_reload("web", VecDeque::from([0]));

    let job = &actor.reloads["web"];
    assert_eq!(job.mode, ReloadMode::Overlap);
    assert_eq!(job.swap.phase, ReloadPhase::AwaitReady);
    let new_id = job
        .swap
        .new_id
        .expect("an overlapping reload spawns at once");
    assert_eq!(
        actor.sheep[&new_id].entry.instance, actor.sheep[&0].entry.instance,
        "the replacement takes the same instance slot"
    );
    assert_eq!(
        actor.sheep[&0].entry.status,
        ProcStatus::Stopping,
        "the instance being replaced is still there, marked and serving"
    );
}

// The replacement inherits what the drainee's entry is the last copy of,
// the instance slot most of all: `assemble` writes it into the child's
// environment, and a different slot binds a different port.
#[tokio::test(start_paused = true)]
async fn a_serial_reload_spawns_into_the_slot_the_drain_emptied() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(
        &dir,
        probed_app("web"),
        vec![ProcScript::never_exits()],
    );
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    actor.sheep.get_mut(&0).expect("the fixture's sheep").ctl = Some(ctl_tx);
    let instance = actor.sheep[&0].entry.instance;
    let restarts = actor.sheep[&0].entry.restarts;

    actor.advance_reload("web", VecDeque::from([0]));
    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );

    assert!(
        !actor.sheep.contains_key(&0),
        "the drained instance is deregistered, not left as a second row"
    );
    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("the reap is where a serial reload spawns");
    let replacement = &actor.sheep[&new_id].entry;
    assert_eq!(replacement.instance, instance);
    assert_eq!(replacement.restarts, restarts);
    assert_eq!(replacement.status, ProcStatus::Starting);
    assert_eq!(replacement.reload, ReloadState::Replacement);
    assert_eq!(
        actor.reloads["web"].swap.phase,
        ReloadPhase::DrainOld,
        "with the drainee gone there is nothing left to abandon back to"
    );
}

// A reload replaces `Online` instances and a parked replacement is not one,
// so without this a rollback gets `Ok` from a daemon that skipped the app's
// only instance.
#[tokio::test(start_paused = true)]
async fn a_reload_can_still_replace_the_instance_a_failed_reload_parked() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, probed_app("web"), vec![]);
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    let slot = actor.sheep.get_mut(&0).expect("the fixture's sheep");
    slot.ctl = Some(ctl_tx);
    // What `reload_ready_result` leaves behind when a replacement never
    // answers and there is no drainee to hand back to.
    slot.entry.status = ProcStatus::Starting;
    slot.ready_failed = true;

    // Through `handle_reload`: the selector pass has its own eligibility
    // filter, and it is the one a rollback meets first.
    let (reply, _rx) = oneshot::channel();
    actor.handle_reload(&ProcessSelector::Name("web".to_string()), reply);

    assert!(
        actor.reloads.contains_key("web"),
        "a parked instance is still replaceable, or nothing can roll it back"
    );
    assert_eq!(actor.reloads["web"].swap.old_id, 0);
}

// An instance still inside its own readiness wait has a verdict coming, so
// draining it would throw away a start already under way.
#[tokio::test(start_paused = true)]
async fn an_ordinarily_starting_sheep_is_still_skipped_by_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, probed_app("web"), vec![]);
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    let slot = actor.sheep.get_mut(&0).expect("the fixture's sheep");
    slot.ctl = Some(ctl_tx);
    slot.entry.status = ProcStatus::Starting;

    let (reply, _rx) = oneshot::channel();
    actor.handle_reload(&ProcessSelector::Name("web".to_string()), reply);

    assert!(
        actor.reloads.is_empty(),
        "a sheep still waiting on its own readiness is left to finish"
    );
    assert_eq!(actor.sheep[&0].entry.status, ProcStatus::Starting);
}

// A parked instance can be a drainee, so a rollback whose replacement fails
// to spawn would leave the app reading `online` while nothing answers.
#[tokio::test(start_paused = true)]
async fn undoing_a_swap_never_promotes_a_parked_drainee_to_online() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = probed_app("web");
    // Overlap, so the drainee is marked and then restored in place; a
    // serial swap has no drainee left to restore when it can fail.
    app.reuse_port = true;
    // No scripts, so `runner.spawn` fails and `spawn_replacement` takes
    // the `Err` arm that does the restoring.
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, app, vec![]);
    let slot = actor.sheep.get_mut(&0).expect("the fixture's sheep");
    slot.entry.status = ProcStatus::Starting;
    slot.ready_failed = true;

    actor
        .spawn_replacement(0, ReloadMode::Overlap)
        .expect_err("a runner with no scripts left cannot spawn");

    let slot = actor.sheep.get(&0).expect("the drainee stays registered");
    assert_eq!(
        slot.entry.status,
        ProcStatus::Starting,
        "an instance that never proved itself is not restored to online"
    );
    assert_eq!(slot.entry.reload, ReloadState::None);
    assert!(
        slot.ready_failed,
        "and it stays replaceable, or the next attempt cannot reach it"
    );
}

// The `abort_reload` half: the sibling above covers the spawn that never
// happened, this the swap that started and was given up.
#[tokio::test(start_paused = true)]
async fn abandoning_a_swap_never_promotes_a_parked_drainee_to_online() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = probed_app("web");
    app.reuse_port = true;
    let (mut actor, _mailbox) =
        actor_with_one_online_sheep_of(&dir, app, vec![ProcScript::never_exits()]);
    // A live control sender says this instance's task is still there to go
    // back to; the fixture leaves it `None`.
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    let slot = actor.sheep.get_mut(&0).expect("the fixture's sheep");
    slot.ctl = Some(ctl_tx);
    slot.entry.status = ProcStatus::Starting;
    slot.ready_failed = true;

    let (reply, _rx) = oneshot::channel();
    actor.handle_reload(&ProcessSelector::Name("web".to_string()), reply);
    assert!(
        actor.reloads.contains_key("web"),
        "the parked instance is the one being replaced"
    );

    actor.abort_reload("web", "the replacement was not ready inside listen_timeout");

    assert_eq!(
        actor.sheep[&0].entry.status,
        ProcStatus::Starting,
        "an instance that never proved itself is not restored to online"
    );
    assert!(actor.sheep[&0].ready_failed);
}

// Only an overlapping reload of a probed app has a first answer the reaped
// instance could have given: a serial one asked with the slot empty, a
// channel is per instance, and a heuristic has nothing to re-run.
#[test]
fn only_an_overlapping_probe_is_re_asked_after_the_drain() {
    let dir = tempfile::tempdir().unwrap();
    let mut probed = probed_app("web");
    probed.reuse_port = true;
    let mut channel = AppConfig::minimal("web", "./srv");
    channel.wait_ready = true;

    for (app, mode, want) in [
        (probed.clone(), ReloadMode::Overlap, true),
        (probed, ReloadMode::Serial, false),
        (channel, ReloadMode::Overlap, false),
        (
            AppConfig::minimal("web", "./srv"),
            ReloadMode::Overlap,
            false,
        ),
    ] {
        let (actor, _mailbox) = actor_with_one_online_sheep_of(&dir, app, vec![]);
        assert_eq!(
            actor.post_drain_probe(0, mode).is_some(),
            want,
            "mode={mode:?}"
        );
    }
}

/// An actor holding one `Online` instance whose reload has reached
/// [`ReloadPhase::Verify`]: the drainee reaped, the replacement serving,
/// and only the second probe's verdict left to arrive.
fn actor_awaiting_a_verdict(
    dir: &tempfile::TempDir,
) -> (Actor<ScriptedRunner>, mpsc::Receiver<Msg>) {
    let mut app = probed_app("web");
    app.reuse_port = true;
    let (mut actor, mailbox) = actor_with_one_online_sheep_of(dir, app, vec![]);
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture's sheep")
        .entry
        .reload = ReloadState::Replacement;
    actor.reloads.insert(
        "web".to_string(),
        ReloadJob {
            queue: VecDeque::new(),
            mode: ReloadMode::Overlap,
            deadline: 0,
            swap: ReloadSwap {
                // Reaped, which is what puts the swap in `Verify` at all.
                old_id: 99,
                new_id: Some(0),
                phase: ReloadPhase::Verify,
            },
        },
    );
    (actor, mailbox)
}

// Both instances were in one `SO_REUSEPORT` group, so the first probe
// proves nothing about which of them answered.
#[tokio::test(start_paused = true)]
async fn a_replacement_that_fails_the_second_probe_is_demoted_and_abandoned() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_awaiting_a_verdict(&dir);
    let mut rx = actor.events.subscribe();

    actor.handle_reload_verified("web", 0, Readiness::TimedOut);

    assert!(actor.reloads.is_empty(), "the reload is over either way");
    assert!(
        actor.sheep.contains_key(&0),
        "the only instance the app has left is not killed as well"
    );
    assert_eq!(
        actor.sheep[&0].entry.status,
        ProcStatus::Starting,
        "an instance that never answered alone is not online"
    );
    assert_eq!(
        drained_process_kinds(&mut rx),
        vec![ProcessEventKind::ReloadAbandoned],
        "an abandonment, and never the `Reloaded` that claims a success"
    );
}

// The control for the case above: same swap, same handler, opposite
// verdict.
#[tokio::test(start_paused = true)]
async fn a_replacement_that_answers_alone_finishes_the_reload() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_awaiting_a_verdict(&dir);
    let mut rx = actor.events.subscribe();

    actor.handle_reload_verified("web", 0, Readiness::Ready);

    assert!(actor.reloads.is_empty());
    assert_eq!(actor.sheep[&0].entry.status, ProcStatus::Online);
    assert_eq!(actor.sheep[&0].entry.reload, ReloadState::None);
    assert_eq!(
        drained_process_kinds(&mut rx),
        vec![ProcessEventKind::Reloaded]
    );
}

// Each arming replaces the last, so only the stamp the job carries may end
// the swap. The second probe takes up to another `listen_timeout`, so a job
// whose first probe and drain ate the original window would otherwise be
// abandoned mid-verify with the rest of the queue on the old code.
#[tokio::test(start_paused = true)]
async fn a_re_armed_watchdog_retires_the_one_it_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_awaiting_a_verdict(&dir);
    // Both armings are made here: the fixture builds its job by hand, so its
    // stamp is not one this counter issued.
    actor.arm_reload_deadline("web", 0);
    let first = actor.reloads["web"].deadline;

    actor.arm_reload_deadline("web", 0);
    let second = actor.reloads["web"].deadline;
    assert_ne!(first, second, "each arming takes a stamp of its own");

    actor.handle_reload_deadline("web", first);
    assert!(
        actor.reloads.contains_key("web"),
        "the retired watchdog must not end a swap the live one is still watching"
    );

    actor.handle_reload_deadline("web", second);
    assert!(actor.reloads.is_empty(), "the live one still ends it");
}

// `DrainFirst` is the one phase an operator's command reaches without
// `uncommitted_swap_of` seeing it: the instance has already been asked to
// go, so there is nothing to abandon back to.
#[tokio::test(start_paused = true)]
async fn a_delete_during_a_serial_drain_spawns_no_replacement() {
    let dir = tempfile::tempdir().unwrap();
    // No scripts: a spawn here would panic, which is half the assertion.
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, probed_app("web"), vec![]);
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    actor.sheep.get_mut(&0).expect("the fixture's sheep").ctl = Some(ctl_tx);

    actor.advance_reload("web", VecDeque::from([0]));
    assert_eq!(actor.reloads["web"].swap.phase, ReloadPhase::DrainFirst);
    // What `begin_manual` leaves behind for a `delete` that matched a
    // running sheep whose `manual` marker the drain already owns.
    actor.sheep.get_mut(&0).expect("the drainee").pending_delete = true;

    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );

    assert!(actor.sheep.is_empty(), "the delete emptied the flock");
    assert!(actor.reloads.is_empty(), "and the reload ended with it");
}

// `ready_failed` makes a parked instance replaceable; left set after its
// process exits without a respawn it makes a `Stopped` row replaceable, and
// a reload against one of those drains a row with nothing behind it.
#[tokio::test(start_paused = true)]
async fn a_parked_instances_verdict_does_not_outlive_its_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = probed_app("web");
    // The arrangement that reaches `Stopped` rather than a respawn.
    app.autorestart = false;
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, app, vec![]);
    let (ctl_tx, _ctl_rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
    let slot = actor.sheep.get_mut(&0).expect("the fixture's sheep");
    slot.ctl = Some(ctl_tx);
    slot.entry.status = ProcStatus::Starting;
    slot.ready_failed = true;

    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );

    let slot = actor.sheep.get(&0).expect("a clean stop keeps the row");
    assert_ne!(slot.entry.status, ProcStatus::Online);
    assert!(!slot.ready_failed);
    assert!(
        !reload_eligible(slot),
        "a row with no process behind it is not something a reload may replace"
    );
}
