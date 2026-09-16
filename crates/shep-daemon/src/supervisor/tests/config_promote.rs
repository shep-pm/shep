//! Tests for a parked change taking effect.
//!
//! A pending field lands when the process next restarts, by reload or by
//! restart. Until then the listing has to keep reporting the spec that is
//! actually running.

use super::*;

/// `spawn_replacement` carries `restarts`, `dog` and `last_exit` off the
/// drainee on the same grounds: the replacement is the same instance
/// continuing, not a new one.
#[tokio::test(start_paused = true)]
async fn reload_carries_the_overridden_cache_to_its_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    actor.sheep.get_mut(&0).unwrap().entry.overridden = vec!["max_restarts".to_string()];

    actor.advance_reload("web", VecDeque::from([0]));

    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id].entry.overridden,
        vec!["max_restarts".to_string()],
        "the replacement must carry the drainee's own overridden cache, not start blank"
    );
}

/// Without this the pending slot is written and never read, so an operator
/// sees a pending field forever with no way to apply it.
#[tokio::test(start_paused = true)]
async fn reload_promotes_pending_config() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    actor.advance_reload("web", VecDeque::from([0]));

    let new_id = actor.reloads["web"]
        .swap
        .new_id
        .expect("an overlapping reload spawns its replacement at once");
    assert_eq!(
        actor.sheep[&new_id]
            .entry
            .spec
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the replacement must come up on the config the load parked"
    );
    assert!(
        actor.sheep[&new_id].entry.pending.is_none(),
        "and it is owed nothing further, having been built from what was owed"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the drainee keeps its copy until it is deregistered: this swap can still be \
         abandoned, and the child it would go back to serving has not got the change"
    );
    assert_ne!(
        actor.sheep[&new_id].entry.pid,
        Some(APPLY_FIRST_PID),
        "a promotion is only reachable through a process that actually replaced the old one"
    );
}

/// Both verbs replace the child, so both are chances to apply what is owed.
#[tokio::test(start_paused = true)]
async fn restart_promotes_pending_config() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    // The door `shep restart` takes on a running sheep: `begin_manual`
    // claims the next exit and `handle_exited` respawns. The other door is
    // `apply_immediate`'s `Restart` arm; both end in `respawn`.
    let (reply, _answer) = oneshot::channel();
    actor.begin_manual(
        ProcessSelector::Name("web".to_string()),
        ManualKind::Restart,
        CommandOrigin::Operator,
        ReplyKind::Info(reply),
    );
    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );

    let entry = &actor.sheep[&0].entry;
    assert_eq!(
        entry.spec.config().env.get("MODE").map(String::as_str),
        Some("blue"),
        "the restarted child must come up on the config the load parked"
    );
    assert!(
        entry.pending.is_none(),
        "a promoted config is owed no longer, so the slot must be empty"
    );
    assert_ne!(
        entry.pid,
        Some(APPLY_FIRST_PID),
        "a promotion is only reachable through a process that actually replaced the old one"
    );
}

/// Without this refresh, `to_info` keeps naming a respawned child's old
/// log path forever: `out_file`/`err_file` are `ApplyGroup::NeedsRespawn`,
/// so a restart is the one moment they take effect, and every reader
/// built on `to_info` (`shep describe`, the muster roll) inherits it.
#[tokio::test(start_paused = true)]
async fn restart_refreshes_the_reported_log_paths_from_the_new_spec() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/var/log/moved-out.log".to_string());
    file.err_file = Some("/var/log/moved-err.log".to_string());
    apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "out_file", "err_file"],
        )],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    let (reply, _answer) = oneshot::channel();
    actor.begin_manual(
        ProcessSelector::Name("web".to_string()),
        ManualKind::Restart,
        CommandOrigin::Operator,
        ReplyKind::Info(reply),
    );
    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );

    let after = to_info(&actor.sheep[&0].entry, &actor.smits);
    assert_eq!(
        after.out_file.as_deref(),
        Some("/var/log/moved-out.log"),
        "the restarted child writes to the moved path; the listing must say so"
    );
    assert_eq!(
        after.err_file.as_deref(),
        Some("/var/log/moved-err.log"),
        "and the same for stderr"
    );
}

/// The mirror case: a load parks a moved `out_file`/`err_file`, but the
/// child has not respawned yet and is still appending to the old path.
/// Reporting the parked path early would be this same bug pointed the
/// other way, naming a file nothing writes to yet.
#[tokio::test(start_paused = true)]
async fn a_parked_log_path_change_does_not_reach_the_listing_before_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let logs = actor.paths.logs.clone();

    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/var/log/moved-out.log".to_string());
    file.err_file = Some("/var/log/moved-err.log".to_string());
    apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "out_file", "err_file"],
        )],
        ResetDepth::None,
    )
    .await;
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "the fixture must really park the change, or this case proves nothing"
    );

    let still_reported = to_info(&actor.sheep[&0].entry, &actor.smits);
    assert_eq!(
        still_reported.out_file.as_deref(),
        logs.join("web-0-out.log").to_str(),
        "the child is still writing to the old path until it respawns"
    );
    assert_eq!(
        still_reported.err_file.as_deref(),
        logs.join("web-0-err.log").to_str(),
        "and the same for stderr"
    );
}
