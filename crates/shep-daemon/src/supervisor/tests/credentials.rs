//! Tests for resolving the user a sheep runs as.
//!
//! A user that cannot be resolved must refuse that app and leave every other
//! identity alone. A resolved identity is reused across a restart rather than
//! looked up again, and a no-op scale asks no passwd question at all.

use super::*;

// `register_at_rest` records membership and resolves nothing, so the
// identity is unresolved until the first spawn; reading that as "asked for
// nobody" hands the child the daemon's own identity.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_restored_app_restarts_under_its_configured_user() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(own_user_name()));

    let info = actor.register_at_rest(&app);
    assert_eq!(
        actor.sheep[&info.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a membership record is not a `Start`: nothing has looked this app's user up yet"
    );

    actor.respawn(info.id, true);

    let wanted = Credentials {
        uid: nix::unistd::geteuid().as_raw(),
        gid: None,
    };
    assert_eq!(
        actor.runner.spawned_as(0),
        Some(wanted),
        "the restarted child must carry the identity its `user` resolves to; `None` here is \
         the child running as the shepherd, which is the downgrade"
    );
    assert_eq!(
        actor.sheep[&info.id].entry.credentials,
        SpawnIdentity::Resolved(Some(wanted)),
        "resolved once at the first spawn and stored, so no later restart looks it up again"
    );
}

// A `Start` refuses this config outright; reaching a spawn through the
// muster roll must not be a way around that.
#[tokio::test(start_paused = true)]
async fn a_restored_app_whose_user_cannot_be_resolved_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    let info = actor.register_at_rest(&app);
    let after = actor.respawn(info.id, true);

    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may be spawned for an app whose identity could not be resolved"
    );
    assert_eq!(after.status, ProcStatus::Errored);
    assert_eq!(
        actor.sheep[&info.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a failed resolution settles nothing: the next restart must ask again"
    );
}

// The pinned uid below is one no lookup of this app's `user` could produce,
// so a respawn that touched the passwd database again would be seen.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_restart_reuses_a_resolved_identity_rather_than_looking_it_up_again() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _rx) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    let pinned = Credentials {
        uid: 4242,
        gid: Some(4243),
    };
    let entry = &mut actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers id 0")
        .entry;
    entry.spec = app_with("web", |app| app.user = Some(own_user_name()));
    entry.credentials = SpawnIdentity::Resolved(Some(pinned));

    actor.respawn(0, true);

    assert_eq!(
        actor.runner.spawned_as(0),
        Some(pinned),
        "a running app's identity must survive its restart untouched"
    );
}

// `PerApp` is the muster restore and the dog, where nobody reads the
// shepherd's log, so a refused app has to be visible as `Errored` rather
// than missing. `#[cfg(unix)]`: the refusal needs a passwd database.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_start_refused_over_credentials_leaves_an_errored_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    let err = actor
        .do_start(vec![app], None, BatchPolicy::PerApp, &BTreeSet::new())
        .expect_err("an unresolvable user must refuse the start");
    assert!(
        err.to_string().contains(NO_SUCH_USER),
        "the refusal must name the user it could not resolve: {err}"
    );

    assert_eq!(actor.runner.spawn_count(), 0);
    let slot = actor
        .sheep
        .values()
        .next()
        .expect("the refusal must leave a row rather than vanishing");
    assert_eq!(slot.entry.status, ProcStatus::Errored);
    assert_eq!(
        slot.entry.credentials,
        SpawnIdentity::Unresolved,
        "the row must not claim a settled identity: a `restart` would reuse it and bring the \
         sheep up as the shepherd, which is the bug the row was added to make visible"
    );

    // And the row is inert: restarting it meets the same refusal.
    let id = slot.entry.id;
    let after = actor.respawn(id, true);
    assert_eq!(after.status, ProcStatus::Errored);
    assert_eq!(actor.runner.spawn_count(), 0);
}

// One `Errored` row and no others is the half-registered flock
// `AllOrNothing` exists to prevent. `#[cfg(unix)]`: the refusal needs a
// passwd database.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn an_all_or_nothing_start_refused_over_credentials_registers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    let err = actor
        .do_start(vec![app], None, BatchPolicy::AllOrNothing, &BTreeSet::new())
        .expect_err("an unresolvable user must refuse the start");
    assert!(
        err.to_string().contains(NO_SUCH_USER),
        "the refusal must name the user it could not resolve: {err}"
    );
    assert!(
        actor.sheep.is_empty(),
        "`AllOrNothing` registers nothing at all, this app included"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
}

// An app registered at rest has never resolved its identity, so the scale
// is the first call that asks.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn scaling_an_app_whose_user_will_not_resolve_leaves_the_flock_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 4]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
    let registered = actor.register_at_rest(&app);

    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 3, reply);
    let err = answer
        .await
        .expect("handle_scale answers every call")
        .expect_err("an unresolvable user must refuse the scale");

    let SupervisorError::CannotStart(message) = &err else {
        panic!("expected CannotStart, got {err:?}");
    };
    assert!(
        message.contains("web") && message.contains(NO_SUCH_USER),
        "the refusal must name the app and the user it could not resolve: {message}"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
    assert_eq!(
        actor.sheep.len(),
        1,
        "no instance may be registered by a scale that could not resolve an identity"
    );
    assert_eq!(
        actor.sheep[&registered.id].entry.spec.config().instances,
        1,
        "the stored count must not be written back for a scale that did not happen"
    );
    assert_eq!(
        actor.sheep[&registered.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a failed resolution settles nothing: the next attempt must ask again"
    );
}

// `register_without_spawning` is idempotent: the second restore finds the
// row it made, so nothing transitioned and no event is owed. An emit keyed
// on the row's status cannot tell the two apart, both being `Errored`.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_second_restore_does_not_re_announce_an_errored_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 2]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
    let mut events = actor.events.subscribe();

    for attempt in 1..=2 {
        assert!(
            actor
                .do_start(
                    vec![app.clone()],
                    None,
                    BatchPolicy::PerApp,
                    &BTreeSet::new()
                )
                .is_err(),
            "restore {attempt} must refuse the app"
        );
    }

    assert_eq!(
        actor.sheep.len(),
        1,
        "the second restore must find the row, not add a second one"
    );

    let mut errored = 0;
    while let Ok(event) = events.try_recv().map(|event| event.to_event()) {
        if let BusEvent::Process {
            event: ProcessEventKind::Errored,
            info,
            ..
        } = event
            && info.name == "web"
        {
            errored += 1;
        }
    }
    assert_eq!(
        errored, 1,
        "one Errored event, for the restore that actually registered the row: a second is a \
         transition that did not happen"
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn scaling_up_the_row_a_refused_start_left_meets_the_same_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 4]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));

    // The row, made the way an unattended boot makes it.
    actor
        .do_start(vec![app], None, BatchPolicy::PerApp, &BTreeSet::new())
        .expect_err("an unresolvable user must refuse the start");
    let id = actor
        .sheep
        .values()
        .next()
        .expect("the refused start leaves a row")
        .entry
        .id;
    assert_eq!(actor.sheep[&id].entry.status, ProcStatus::Errored);

    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 3, reply);
    let err = answer
        .await
        .expect("handle_scale answers every call")
        .expect_err("the row's user still will not resolve");

    assert!(
        matches!(err, SupervisorError::CannotStart(_)),
        "expected CannotStart, got {err:?}"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
    assert_eq!(
        actor.sheep.len(),
        1,
        "no instance may be registered by a scale that could not resolve an identity"
    );
}

// The user is resolvable, so an identity still `Unresolved` afterwards is
// what says nothing asked.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_no_op_scale_asks_no_passwd_question() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 2]);
    let app = app_with("web", |app| app.user = Some(own_user_name()));
    let registered = actor.register_at_rest(&app);

    // `register_at_rest` registers `instance: 0` whatever `instances`
    // says, so `current` is 1 and this is the `Ordering::Equal` arm.
    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 1, reply);
    answer
        .await
        .expect("handle_scale answers every call")
        .expect("scaling to the count an app already has is a no-op");

    assert_eq!(
        actor.sheep[&registered.id].entry.credentials,
        SpawnIdentity::Unresolved,
        "a scale that spawns nothing must not resolve an identity: nothing would use it, \
         and asking is what lets the answer refuse the call"
    );
    assert_eq!(actor.runner.spawn_count(), 0);
}

// The user cannot be resolved, so a scale that asks is a scale that fails.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_no_op_scale_survives_a_user_that_will_not_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 2]);
    let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
    let registered = actor.register_at_rest(&app);

    let (reply, answer) = oneshot::channel();
    actor.handle_scale("web", 1, reply);
    let scaled = answer
        .await
        .expect("handle_scale answers every call")
        .expect("a no-op scale must not be refused over an identity it never needed");

    assert_eq!(scaled.instances.len(), 1);
    assert_eq!(actor.runner.spawn_count(), 0);
    assert_eq!(
        actor.sheep[&registered.id].entry.status,
        ProcStatus::Stopped,
        "a no-op must leave the sheep exactly as it found it"
    );
}

// The `Errored` event carries no reason and the deferred reply has no
// per-id error slot, so this log line is all an operator has. `#[test]`
// with a `block_on` inside: `capture_logs` scopes its subscriber to one
// thread and needs a synchronous closure.
#[cfg(unix)]
#[test]
fn a_restart_that_starts_nothing_says_why_in_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // Route one: no script to pop, so the runner refuses the spawn.
    let refused = capture_logs(|| {
        let mut actor = actor_with_an_empty_flock(&dir, Vec::new());
        let info = actor.register_at_rest(&app_with("web", |_| {}));
        rt.block_on(async { actor.respawn(info.id, true) });
    });
    assert!(
        refused.contains("script exhausted"),
        "the runner's own reason must reach the log: {refused}"
    );

    // Route two: the identity cannot be resolved, so no spawn is
    // attempted at all.
    let unresolved = capture_logs(|| {
        let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
        let app = app_with("web", |app| app.user = Some(NO_SUCH_USER.to_string()));
        let info = actor.register_at_rest(&app);
        rt.block_on(async { actor.respawn(info.id, true) });
    });
    assert!(
        unresolved.contains(NO_SUCH_USER),
        "the unresolvable user must reach the log by name: {unresolved}"
    );
}
