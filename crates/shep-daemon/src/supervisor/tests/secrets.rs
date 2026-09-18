//! Tests for resolving a sheep's secrets before it spawns.
//!
//! A value that is not there yet leaves the sheep waiting rather than started,
//! and one that will never arrive errors it once the budget runs out. Which
//! value a sheep reads depends on its environment.

use super::*;

/// A secret nobody has set is a person's to fix, so a [`BatchPolicy::PerApp`]
/// batch leaves the sheep `Errored` at once rather than spawning it. A
/// restart ladder in front of it would only postpone the same report by
/// sixteen turns.
///
/// `PerApp` because that is the policy under which such an app is still
/// registered: a boot restore, or a dog.
/// `a_batch_with_one_unresolvable_secret_registers_none_of_it` is the
/// other half.
#[tokio::test(start_paused = true)]
async fn a_key_nobody_has_set_errors_the_sheep_without_a_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:ABSENT}}".to_string());

    let err = actor
        .do_start(
            vec![normalize(app).unwrap()],
            None,
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )
        .unwrap_err();

    assert!(matches!(err, SupervisorError::SpawnFailed(_)), "{err:?}");
    let rendered = err.to_string();
    assert!(
        rendered.contains("ABSENT"),
        "names the reference: {rendered}"
    );
    let entry = &actor.sheep.values().next().expect("one row").entry;
    assert_eq!(entry.status, ProcStatus::Errored);
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may reach the runner"
    );
}

/// A store this build cannot read refuses every reference with the same
/// words a key nobody set does, which sends an operator to `shep secret
/// set` for a file that is corrupt or newer than this build. The empty
/// view it falls back to has to say so on its way past.
#[test]
fn an_unreadable_store_warns_before_it_falls_back_to_an_empty_view() {
    let dir = tempfile::tempdir().unwrap();
    let actor = actor_with_an_empty_flock(&dir, vec![]);
    std::fs::write(&actor.paths.secrets, "{ not json").unwrap();
    let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();

    let logs = capture_logs(|| {
        let view = actor.secret_view(&app);
        let reference = shep_core::secrets::SecretRef {
            namespace: None,
            key: "K",
        };
        assert!(
            matches!(
                view.resolve(&reference),
                shep_core::secrets::Resolution::MissingKey
            ),
            "the fallback is an empty view, not a failed spawn"
        );
    });

    assert!(logs.contains("WARN"), "loud enough to read: {logs}");
    assert!(logs.contains("secrets.json"), "names the file: {logs}");
}

/// A namespace no provider dog has pushed to clears itself, so the sheep
/// waits on the ordinary ladder instead of erroring. Collapsing this into
/// the case above would strand an app whose provider is merely late.
#[tokio::test(start_paused = true)]
async fn a_namespace_no_dog_has_pushed_to_leaves_the_sheep_waiting() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:vault/K}}".to_string());

    let started = handle
        .start(vec![normalize(app).unwrap()])
        .await
        .expect("a provider that has not reported yet is not a failed start");

    assert_eq!(started[0].status, ProcStatus::WaitingRestart);
    assert_eq!(handle.list().await[0].status, ProcStatus::WaitingRestart);
    assert_eq!(runner.spawn_count(), 0, "nothing may reach the runner");
}

/// A push carries one `(namespace, environment)` pair, so a provider
/// that has done `production` and not yet `staging` has said nothing
/// about staging. Keying the refusal on the namespace alone `Errored`s
/// a staging sheep permanently the moment the first push lands, which is
/// the ordinary shape for a provider dog polling one environment at a
/// time, and the cache makes it survive a reboot.
#[tokio::test(start_paused = true)]
async fn a_namespace_pushed_for_another_environment_leaves_the_sheep_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    actor
        .provider_secrets
        .put(
            "vercel",
            "production",
            BTreeMap::from([("API_KEY".to_string(), "sk_live".to_string())]),
            false,
        )
        .expect("no cache is written with persist off");
    let mut app = AppConfig::minimal("web", "./srv");
    app.environment = Some("staging".to_string());
    app.env
        .insert("K".to_string(), "{{secret:vercel/API_KEY}}".to_string());

    actor
        .do_start(
            vec![normalize(app).unwrap()],
            None,
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )
        .expect("a provider that has not pushed staging yet is not a failed start");

    let entry = &actor.sheep.values().next().expect("one row").entry;
    assert_eq!(entry.status, ProcStatus::WaitingRestart);
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may reach the runner"
    );
}

/// The other half, which must stay permanent: the pair has been pushed
/// and the key is not in it, so the provider genuinely does not have it
/// and sixteen retries would report the same thing sixteen turns later.
#[tokio::test(start_paused = true)]
async fn a_key_absent_from_a_pushed_pair_errors_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    actor
        .provider_secrets
        .put(
            "vercel",
            "production",
            BTreeMap::from([("OTHER".to_string(), "1".to_string())]),
            false,
        )
        .expect("no cache is written with persist off");
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("K".to_string(), "{{secret:vercel/API_KEY}}".to_string());

    let err = actor
        .do_start(
            vec![normalize(app).unwrap()],
            None,
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )
        .unwrap_err();

    assert!(matches!(err, SupervisorError::SpawnFailed(_)), "{err:?}");
    let entry = &actor.sheep.values().next().expect("one row").entry;
    assert_eq!(entry.status, ProcStatus::Errored);
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing may reach the runner"
    );
}

/// The ladder is bounded by the same budget a crash loop spends, so a
/// provider that never arrives ends as an error rather than retrying for
/// the daemon's life.
#[tokio::test(start_paused = true)]
async fn a_namespace_that_never_arrives_errors_once_the_budget_runs_out() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:vault/K}}".to_string());
    // Two turns, and a fixed wait so the clock below knows what to skip.
    app.max_restarts = 2;
    app.restart_delay = Some(UpDuration::from_millis(100));

    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    assert_eq!(handle.list().await[0].status, ProcStatus::WaitingRestart);

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        handle.list().await[0].status,
        ProcStatus::Errored,
        "the second refusal exhausts the budget"
    );
}

/// The store is read from disk at the spawn, so a value set before the
/// start reaches the child without a daemon restart.
#[tokio::test(start_paused = true)]
async fn a_value_in_the_store_lets_the_sheep_start() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    shep_core::secrets::set(&paths.secrets, "DB_PASSWORD", "production", "hunter2").unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), paths, events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("PW".to_string(), "{{secret:DB_PASSWORD}}".to_string());

    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].status, ProcStatus::Online);
    assert_eq!(runner.spawn_count(), 1);
}

/// A sheep's own `environment` picks which slot it reads, and there is no
/// fallback to another named one: a `staging` value must not answer for a
/// `production` sheep.
#[tokio::test(start_paused = true)]
async fn a_sheeps_environment_decides_which_value_it_reads() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    shep_core::secrets::set(&paths.secrets, "K", "staging", "v").unwrap();
    let handle = spawn_supervisor(runner, paths, events);
    let templated = |name: &str, environment: Option<&str>| {
        let mut app = AppConfig::minimal(name, "./srv");
        app.environment = environment.map(str::to_string);
        app.env.insert("K".to_string(), "{{secret:K}}".to_string());
        normalize(app).unwrap()
    };

    handle
        .start(vec![templated("staged", Some("staging"))])
        .await
        .expect("the staging slot holds a value");
    let err = handle
        .start(vec![templated("live", None)])
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("production"),
        "the host default is what the second one asked for: {err}"
    );
}

/// `environment` is a `NeedsRespawn` field and a promotion rewrites one
/// slot at a time, so two instances of a name can hold different
/// environments at once. Each prober resolves against its own slot.
#[test]
fn a_rearm_resolves_each_instance_against_its_own_environment() {
    let dir = tempfile::tempdir().unwrap();
    let actor = actor_with_an_empty_flock(&dir, vec![]);
    shep_core::secrets::set(&actor.paths.secrets, "K", "production", "live").unwrap();
    shep_core::secrets::set(&actor.paths.secrets, "K", "staging", "rehearsal").unwrap();
    // `armed_entry` assembles against an empty view, which a templated
    // app cannot resolve, so the reference goes on afterwards.
    let entry_of = |id: u32, environment: &str| {
        let mut entry = armed_entry(id, id, 4300 + id, app_with("web", |_| {}), &actor.paths);
        entry.spec = app_with("web", |app| {
            app.environment = Some(environment.to_string());
            app.env.insert("K".to_string(), "{{secret:K}}".to_string());
        });
        entry
    };
    let promoted = entry_of(0, "staging");
    let waiting = entry_of(1, "production");

    let specs = actor.rearm_specs(&[&promoted, &waiting]);

    assert_eq!(
        specs[&0].env.get("K").map(String::as_str),
        Some("rehearsal"),
        "the promoted instance reads its own staging slot: {:?}",
        specs[&0]
    );
    assert_eq!(
        specs[&1].env.get("K").map(String::as_str),
        Some("live"),
        "the instance still on the old config keeps production: {:?}",
        specs[&1]
    );
}

/// [`BatchPolicy::AllOrNothing`] promises to register none of a batch it
/// cannot start whole, and a reference nobody has set is as knowable
/// before the batch as a missing binary is. Refusing it only at the spawn
/// leaves every app ahead of it in the file running.
#[tokio::test(start_paused = true)]
async fn a_batch_with_one_unresolvable_secret_registers_none_of_it() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    let sound = normalize(AppConfig::minimal("first", "./srv")).unwrap();
    let mut broken = AppConfig::minimal("second", "./srv");
    broken
        .env
        .insert("PW".to_string(), "{{secret:TYPO}}".to_string());

    let err = handle
        .start(vec![sound, normalize(broken).unwrap()])
        .await
        .unwrap_err();

    assert!(
        handle.list().await.is_empty(),
        "the app ahead of the refusal must not survive the batch"
    );
    assert_eq!(runner.spawn_count(), 0, "nothing may reach the runner");
    assert!(matches!(err, SupervisorError::CannotStart(_)), "{err:?}");
    assert!(
        err.to_string().contains("TYPO"),
        "names the reference: {err}"
    );
}
