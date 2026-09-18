//! Tests for dogs, the plugin processes the shepherd supervises itself.
//!
//! A dog is not a sheep and must not answer to a wildcard, survive a restart as
//! anything else, or be scaled. It stays a dog through every path that could
//! quietly turn it back into an ordinary process.

use super::*;

/// The `bark` row of a listing, or a panic naming what was there instead.
fn dog_row(listed: &[ProcessInfo], id: u32) -> ProcessInfo {
    listed
        .iter()
        .find(|info| info.id == id)
        .unwrap_or_else(|| panic!("id {id} left the flock: {listed:?}"))
        .clone()
}

/// A marker written by the start path rather than carried by the entry is
/// invisible until a dog crashes once: the dog leaves the dogs table and
/// reappears among the flock, with no error anywhere.
///
/// Two scripts, both used by a correct run: the crash, and the process the
/// restart produces.
#[tokio::test(start_paused = true)]
async fn a_dog_that_restarts_is_still_a_dog() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(1), ProcScript::never_exits()]);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);

    let dog = handle
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .unwrap();
    assert_eq!(dog.dog, Some(DogSource::BuiltIn));

    // `Restart` is emitted from inside the respawn, after the entry has been
    // rewritten, so a listing taken once it lands cannot read the entry
    // mid-flight.
    expect_event(&mut rx, dog.id, ProcessEventKind::Restart).await;
    let after = dog_row(&handle.list().await, dog.id);

    assert_eq!(
        after.restarts, 1,
        "the ordinary restart path, not a dog one"
    );
    assert_eq!(after.dog, Some(DogSource::BuiltIn));
}

/// A dog whose binary is not there, which is what `adopt` with a bad path
/// produces, has to be visible in the dogs table as `Errored`; an unmarked
/// one is a sheep nobody started.
///
/// No scripts: [`ScriptedRunner`] fails a spawn by running out of them.
#[tokio::test(start_paused = true)]
async fn a_dog_that_cannot_be_spawned_is_still_a_dog() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(ScriptedRunner::new(Vec::new()), test_paths(&dir), events);

    let failed = handle
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .expect_err("a spawn with no script behind it cannot succeed");
    assert!(matches!(failed, SupervisorError::SpawnFailed(_)));

    let listed = handle.list().await;
    let errored = dog_row(&listed, 0);
    assert_eq!(errored.status, ProcStatus::Errored);
    assert_eq!(errored.dog, Some(DogSource::BuiltIn));
}

/// Fails if the gate is ignored: with no `wait_ready` and no
/// `readiness_probe`, `spawn_fresh`'s ungated arm reports `Online` at once
/// and a stage gate on this app would wait for nothing.
///
/// Paused time, since the daemon still arms a readiness task for the
/// gated fallback: the assertion below reads the status `handle_command`
/// answers with synchronously, before that task's `listen_timeout` runs
/// out either way, but pausing keeps the fixture's background tasks from
/// costing real wall clock while the case holds them.
#[tokio::test(start_paused = true)]
async fn an_app_in_the_gate_set_holds_at_starting_without_a_probe() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    let (reply, rx) = oneshot::channel();
    let mut app = AppConfig::minimal("db", "./db");
    app.listen_timeout = UpDuration::from_millis(50);
    actor.handle_command(Command::Start {
        apps: vec![normalize(app).unwrap()],
        policy: BatchPolicy::AllOrNothing,
        gate: BTreeSet::from(["db".to_string()]),
        reply,
    });
    let started = rx.await.unwrap().unwrap();
    assert_eq!(started[0].status, ProcStatus::Starting);
}

/// Fails if gating leaks to every app, which would hold a plain
/// `shep start db` at `Starting` for its whole `listen_timeout`.
#[tokio::test(start_paused = true)]
async fn an_app_outside_the_gate_set_is_online_at_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    let (reply, rx) = oneshot::channel();
    actor.handle_command(Command::Start {
        apps: vec![normalize(AppConfig::minimal("db", "./db")).unwrap()],
        policy: BatchPolicy::AllOrNothing,
        gate: BTreeSet::new(),
        reply,
    });
    let started = rx.await.unwrap().unwrap();
    assert_eq!(started[0].status, ProcStatus::Online);
}

/// `PerApp` skips an app whose credentials do not resolve, so `credentials`
/// is shorter than `apps` while still in order: a naive `zip` against `apps`
/// pairs app 2 with app 3's credentials and drops the last app. The first of
/// three fails, the ordering where the drift starts at the next app.
///
/// Asserts the identity each app got, not merely that it is registered:
/// under the broken pairing all three are present, holding each other's
/// credentials.
///
/// `#[cfg(unix)]`: `nix` is a unix-only dependency, and `privilege::resolve`
/// refuses any user request off-platform.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_credential_failure_never_shifts_another_apps_identity() {
    let dir = tempfile::tempdir().unwrap();
    // Two scripts, for the two apps that must survive the first's failure.
    let (mut actor, _mailbox) =
        actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits(); 2]);

    let own_uid = nix::unistd::geteuid();
    let own_user = nix::unistd::User::from_uid(own_uid)
        .expect("the passwd database is readable")
        .expect("this process's own uid has a passwd entry")
        .name;

    let mut bad = AppConfig::minimal("a-bad", "./a");
    bad.user = Some("definitely-not-a-real-shep-user".to_string());
    let mut mine = AppConfig::minimal("b-mine", "./b");
    mine.user = Some(own_user);
    let plain = AppConfig::minimal("c-plain", "./c");

    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Start {
        apps: vec![
            normalize(bad).unwrap(),
            normalize(mine).unwrap(),
            normalize(plain).unwrap(),
        ],
        policy: BatchPolicy::PerApp,
        gate: BTreeSet::new(),
        reply,
    });

    let err = answer
        .await
        .expect("the actor answers every Start")
        .expect_err("one app has no resolvable user");
    assert!(
        err.to_string().contains("a-bad"),
        "the failure must name the app whose user could not be resolved: {err}"
    );

    let identity = |name: &str| {
        actor
            .sheep
            .values()
            .find(|slot| slot.entry.spec.config().name == name)
            .map(|slot| slot.entry.credentials)
    };
    assert_eq!(
        identity("a-bad"),
        Some(SpawnIdentity::Unresolved),
        "an app with no resolvable user is registered `Errored` so it is \
         visible, and must hold NO identity: `Unresolved` is what stops a \
         later restart reusing one, and it is also the proof this app did \
         not inherit b-mine's"
    );
    assert_eq!(
        identity("b-mine"),
        Some(SpawnIdentity::Resolved(Some(Credentials {
            uid: own_uid.as_raw(),
            gid: None
        }))),
        "the app that asked for a user must run as that user"
    );
    assert_eq!(
        identity("c-plain"),
        Some(SpawnIdentity::Resolved(None)),
        "the app that asked for no user must be registered, and must run \
         as the daemon rather than as somebody else's uid. `Resolved(None)` \
         rather than a bare `None`: this app was ASKED, and answered \
         nobody, which is not the same fact as never having been asked"
    );
}

/// The policy is a parameter: carrying on past a failure is right for a
/// muster restore and wrong for an operator's `shep start`.
///
/// `failing_to_spawn` on the first app, so the assertion reads the second
/// app's absence rather than its failure.
#[tokio::test(start_paused = true)]
async fn an_all_or_nothing_start_stops_at_the_first_failed_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        // A script for the second app, which must not get as far as using
        // it: an exhausted pool would fail it for a reason of its own.
        ScriptedRunner::new(vec![ProcScript::never_exits()]).failing_to_spawn(&["first"]),
        test_paths(&dir),
        events,
    );

    let err = handle
        .start(vec![
            normalize(AppConfig::minimal("first", "./first")).unwrap(),
            normalize(AppConfig::minimal("second", "./second")).unwrap(),
        ])
        .await
        .expect_err("the first app cannot spawn");

    assert!(matches!(err, SupervisorError::SpawnFailed(_)), "{err:?}");
    let listed = handle.list().await;
    assert_eq!(
        listed
            .iter()
            .map(|i| (i.name.as_str(), i.status))
            .collect::<Vec<_>>(),
        vec![("first", ProcStatus::Errored)],
        "an operator's `shep start` must not go on past a failure: the \
         second app is never reached: {listed:?}"
    );
    handle.shutdown().await;
}

/// `do_start_dog` shares `do_start` with `Request::Start`, so a
/// pre-registration check written for a Flockfile batch can reach dogs and
/// leave no trace of the wreck. [`Actor::spawn_fresh`]'s failure arm
/// registers the row and `shep dogs` renders it.
///
/// `refusing` is load-bearing: without it `ScriptedRunner`'s `preflight`
/// answers `Unknown` and the assertions hold whichever policy was passed.
#[tokio::test(start_paused = true)]
async fn a_dog_that_cannot_spawn_is_registered_errored_rather_than_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    // Empty scripts, so the spawn fails on its own.
    let runner = ScriptedRunner::new(Vec::new()).refusing(&["bark"]);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);

    let err = handle
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .expect_err("nothing can spawn against an empty script pool");

    assert!(
        matches!(err, SupervisorError::SpawnFailed(_)),
        "a dog must reach its spawn and fail there, not be refused \
         before it registers: {err:?}"
    );
    let listed = handle.list().await;
    assert_eq!(
        listed
            .iter()
            .map(|i| (i.name.as_str(), i.status, i.dog.clone()))
            .collect::<Vec<_>>(),
        vec![("bark", ProcStatus::Errored, Some(DogSource::BuiltIn))],
        "the dogs table must still show the dog, and show it broken"
    );
    handle.shutdown().await;
}

/// `shep reload bark` names the dog exactly, so it reaches it where a
/// wildcard would not, and an unmarked replacement turns the dog into a
/// sheep while the swap reports success either way.
///
/// Three scripts, of which a correct run uses two: the third lets a broken
/// run's extra spawn land as a live entry.
#[tokio::test(start_paused = true)]
async fn a_reloaded_dog_is_still_a_dog() {
    let dir = tempfile::tempdir().unwrap();
    let (events, mut rx) = crate::bus::test_bus(256);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 3]);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);

    let dog = handle
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .unwrap();
    handle
        .reload(ProcessSelector::Name("bark".to_string()))
        .await
        .expect("a reload that names the dog is accepted");

    // The replacement is the next id the actor hands out; `Reloaded` on it
    // means the drainee is already deregistered.
    let replacement = dog.id + 1;
    expect_event(&mut rx, replacement, ProcessEventKind::Reloaded).await;
    let listed = handle.list().await;

    assert_eq!(
        listed.len(),
        1,
        "the swap is over, not in flight: {listed:?}"
    );
    assert_eq!(
        dog_row(&listed, replacement).dog,
        Some(DogSource::BuiltIn),
        "the half that arrived is the same dog the half that left was"
    );
}

/// The rule `Start` follows: the shutdown aggregation's `online` snapshot
/// was fixed when it ran, so a child registered after it is one nothing will
/// kill. The runner carries a script so a broken run's spawn succeeds.
#[tokio::test(start_paused = true)]
async fn a_dog_is_refused_once_a_shutdown_has_begun() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![ProcScript::never_exits()]);
    actor.shutting_down = true;

    let (reply, rx) = oneshot::channel();
    actor.handle_command(Command::StartDog {
        app: Box::new(dog_app("bark")),
        source: DogSource::BuiltIn,
        reply,
    });

    assert_eq!(rx.await, Ok(Err(SupervisorError::EngineStopped)));
    assert_eq!(actor.sheep.len(), 1, "nothing new was registered");
}

/// `shep enable` runs against a daemon that may already have the dog from
/// `enabled_dogs` at boot, and a second live process under one name gives
/// two metrics listeners on one port and two copies of every bark.
///
/// Two scripts, of which a correct run uses one: the second lets a
/// non-idempotent `start_dog` show up as the extra entry it is.
#[tokio::test(start_paused = true)]
async fn enabling_a_dog_twice_starts_one_process() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(); 2]);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);

    let first = handle
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .unwrap();
    let second = handle
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(second.pid, first.pid, "the same process, not a fresh one");
    let listed = handle.list().await;
    assert_eq!(listed.iter().filter(|i| i.name == "bark").count(), 1);
}
