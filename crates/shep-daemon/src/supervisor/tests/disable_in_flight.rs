//! `disable` against work already in flight, and a reload against a disabled
//! app, with the actor driven directly.

use super::*;

/// The exit of a running instance: `handle_exited` treats a missing
/// `started_at` as a duplicate report.
fn exit_clean(actor: &mut Actor<ScriptedRunner>, id: u32) {
    actor
        .sheep
        .get_mut(&id)
        .expect("a registered id")
        .entry
        .started_at = Some(tokio::time::Instant::now());
    actor.handle_exited(
        id,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );
}

fn probed_app(name: &str) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.readiness_probe = Some(probe_config(ProbeKind::Tcp, "127.0.0.1:9"));
    app
}

// No scripts: a replacement spawned anyway would panic.
#[tokio::test(start_paused = true)]
async fn a_reload_of_a_disabled_app_is_refused_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![]);
    actor.disabled.insert("web".to_string());
    let (reply, answer) = oneshot::channel();

    actor.handle_reload(&ProcessSelector::Name("web".into()), reply);

    let refusal = answer.await.expect("the reload is answered");
    assert!(
        matches!(refusal, Err(SupervisorError::Disabled(ref name)) if name == "web"),
        "got {refusal:?}"
    );
    assert!(actor.reloads.is_empty(), "no reload was started");
    assert_eq!(actor.sheep.len(), 1, "nothing was spawned beside it");
}

// An instance that was already draining when the mark landed: the guard in
// the replacement spawn is the only thing left between it and a new id.
#[tokio::test(start_paused = true)]
async fn a_replacement_is_never_spawned_for_a_disabled_app() {
    let dir = tempfile::tempdir().unwrap();
    // A script, so a spawn the guard let through would succeed.
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    actor.disabled.insert("web".to_string());

    let spawned = actor.spawn_replacement(0, ReloadMode::Overlap);

    assert!(spawned.is_err(), "the guard must refuse the spawn");
    assert_eq!(actor.sheep.len(), 1);
    assert_eq!(actor.sheep[&0].entry.status, ProcStatus::Online);
}

#[tokio::test(start_paused = true)]
async fn a_disable_during_a_serial_drain_spawns_no_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, probed_app("web"), vec![]);
    let _ctl_rx = attach_ctl(&mut actor);
    actor.advance_reload("web", VecDeque::from([0]));
    assert_eq!(actor.reloads["web"].swap.phase, ReloadPhase::DrainFirst);

    let (reply, _answer) = oneshot::channel();
    actor.handle_disable(&ProcessSelector::Name("web".into()), reply);
    exit_clean(&mut actor, 0);

    assert!(
        actor.reloads.is_empty(),
        "the reload ended with the disable"
    );
    assert_eq!(actor.sheep.len(), 1, "nothing was spawned under a new id");
    let slot = actor.sheep.values().next().expect("the app stays listed");
    assert_eq!(slot.entry.status, ProcStatus::Stopped);
}

#[tokio::test(start_paused = true)]
async fn a_disable_during_a_restart_in_flight_leaves_the_app_stopped() {
    let dir = tempfile::tempdir().unwrap();
    // No scripts: a respawn here would panic.
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![]);
    let _ctl_rx = attach_ctl(&mut actor);
    let (restart_reply, _restarted) = oneshot::channel();
    actor.restart_unless_disabled(
        &ProcessSelector::Name("web".into()),
        CommandOrigin::Operator,
        ReplyKind::Info(restart_reply),
    );
    assert_eq!(
        actor.sheep[&0].manual.map(|m| m.kind),
        Some(ManualKind::Restart)
    );

    let (reply, _answer) = oneshot::channel();
    actor.handle_disable(&ProcessSelector::Name("web".into()), reply);

    assert_eq!(
        actor.sheep[&0].manual.map(|m| m.kind),
        Some(ManualKind::Stop),
        "the pending kill marker is now a stop"
    );
    exit_clean(&mut actor, 0);
    assert_eq!(actor.sheep[&0].entry.status, ProcStatus::Stopped);
    assert_eq!(actor.sheep[&0].entry.pid, None, "nothing respawned");
}

// The old instance is already gone, so the replacement is the app's only
// row: its exit under the reload marker would deregister it.
#[tokio::test(start_paused = true)]
async fn a_disable_while_a_serial_replacement_awaits_readiness_keeps_the_row() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) =
        actor_with_one_online_sheep_of(&dir, probed_app("web"), vec![ProcScript::never_exits()]);
    let _ctl_rx = attach_ctl(&mut actor);
    actor.advance_reload("web", VecDeque::from([0]));
    exit_clean(&mut actor, 0);
    let replacement = actor.reloads["web"].swap.new_id.expect("a replacement");

    let (reply, _answer) = oneshot::channel();
    actor.handle_disable(&ProcessSelector::Name("web".into()), reply);
    exit_clean(&mut actor, replacement);

    assert!(
        actor.reloads.is_empty(),
        "the reload ended with the disable"
    );
    assert_eq!(actor.sheep.len(), 1, "the app keeps its row");
    assert_eq!(actor.sheep[&replacement].entry.status, ProcStatus::Stopped);
    assert!(actor.is_disabled_id(replacement), "and its mark");
}
