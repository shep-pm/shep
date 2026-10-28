//! Tests for resolving a sheep's secrets before it spawns.
//!
//! A value that is not there yet leaves the sheep waiting rather than started,
//! and one that will never arrive errors it once the budget runs out. Which
//! value a sheep reads depends on its environment.

use super::*;

/// A secret nobody has set is a person's to fix, so a [`BatchPolicy::PerApp`]
/// batch leaves the sheep `Errored` at once rather than spawning it. A restart
/// ladder in front of it would only postpone the same report by sixteen turns.
///
/// `PerApp` because that is the policy under which such an app is still
/// registered: a boot restore, or a dog.
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
            crate::supervisor::DEFAULT_ENVIRONMENT,
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
            crate::supervisor::DEFAULT_ENVIRONMENT,
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

/// One `Start` of a multi-instance app resolves its references once for the
/// whole batch, and every instance of it still gets the value.
///
/// The pin is the spawn count, not a read count: `assemble` refuses a key
/// nobody has set, so an instance that resolved nothing never reaches the
/// runner and lands `Errored` instead.
#[tokio::test(start_paused = true)]
async fn a_multi_instance_start_resolves_a_secret_reference_for_every_instance() {
    let dir = tempfile::tempdir().unwrap();
    shep_core::secrets::set(&test_paths(&dir).secrets, "K", "production", "v").unwrap();
    let mut config = AppConfig::minimal("web", "./srv");
    config.instances = 3;
    config
        .env
        .insert("K".to_string(), "{{secret:K}}".to_string());

    let (handle, runner, _rx) = started(&dir, config, vec![ProcScript::never_exits(); 3]).await;

    assert_eq!(
        runner.spawn_count(),
        3,
        "an instance whose reference resolved nothing never reaches the runner"
    );
    let listed = handle.list().await;
    assert_eq!(listed.len(), 3);
    assert!(
        listed.iter().all(|info| info.status == ProcStatus::Online),
        "every instance of the batch came up: {listed:?}"
    );
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

/// The lifecycle handles `slot` still holds, so a failure names the one
/// that survived rather than the whole slot.
fn live_handles(slot: &SheepSlot) -> Vec<&'static str> {
    [
        ("ctl", slot.ctl.is_some()),
        ("to_child", slot.to_child.is_some()),
        ("signals", slot.signals.is_some()),
        ("to_stdin", slot.to_stdin.is_some()),
        ("ready_tx", slot.ready_tx.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, live)| live.then_some(name))
    .collect()
}

/// A respawn that could not start leaves no handle behind on either status it
/// can land on: a sender kept past its process leaks the task parked on the
/// other end of it. `log_ctl` is the one exception, for the reason on
/// `SheepSlot::log_ctl`.
///
/// The two routes are the two refusals a secret can make: `respawn_failed`
/// lands a key nobody has set `Errored`.
#[tokio::test(start_paused = true)]
async fn a_refused_respawn_clears_every_handle_but_the_log_pump() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits(); 2]);
    shep_core::secrets::set(&actor.paths.secrets, "K", "production", "v").unwrap();
    actor
        .provider_secrets
        .put(
            crate::supervisor::DEFAULT_ENVIRONMENT,
            "vault",
            "production",
            BTreeMap::from([("K".to_string(), "v".to_string())]),
            false,
        )
        .expect("no cache is written with persist off");

    // `wait_ready` on both, so each slot holds a `ready_tx` to be cleared.
    let gated = |name: &str, reference: &str, environment: Option<&str>| {
        let mut app = AppConfig::minimal(name, "./srv");
        app.wait_ready = true;
        app.environment = environment.map(str::to_string);
        app.env
            .insert("PW".to_string(), format!("{{{{secret:{reference}}}}}"));
        normalize(app).unwrap()
    };
    let started = actor
        .do_start(
            vec![gated("plain", "K", None), gated("pushed", "vault/K", None)],
            None,
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )
        .expect("both references resolve for the first spawn");
    let (plain_id, pushed_id) = (started[0].id, started[1].id);
    let all_handles = ["ctl", "to_child", "signals", "to_stdin", "ready_tx"];
    for id in [plain_id, pushed_id] {
        assert_eq!(
            live_handles(&actor.sheep[&id]),
            all_handles,
            "a sheep that spawned holds every handle, or there is nothing to clear"
        );
    }

    // The `Errored` route: the store no longer has the key, and no retry
    // produces one.
    shep_core::secrets::unset(&actor.paths.secrets, "K", "production").unwrap();
    let info = actor.respawn(plain_id, true);
    assert_eq!(info.status, ProcStatus::Errored);
    let slot = &actor.sheep[&plain_id];
    assert_eq!(slot.entry.status, ProcStatus::Errored);
    assert_eq!(slot.entry.pid, None);
    assert_eq!(slot.entry.started_at, None);
    assert_eq!(
        slot.entry.last_exit, None,
        "nothing exited on this route, so no code stands for a crash"
    );
    assert_eq!(
        live_handles(slot),
        Vec::<&str>::new(),
        "every handle is cleared on the route that errors"
    );
    assert!(
        slot.log_ctl.is_some(),
        "the log pump's sender is the one kept"
    );

    // The `WaitingRestart` route: the config this spawn is about to be made
    // from asks `vault` for a value under `staging`, which no dog has
    // pushed to yet.
    actor
        .sheep
        .get_mut(&pushed_id)
        .expect("registered a moment ago")
        .entry
        .pending = Some(gated("pushed", "vault/K", Some("staging")));
    let info = actor.respawn(pushed_id, true);
    assert_eq!(info.status, ProcStatus::WaitingRestart);
    let slot = &actor.sheep[&pushed_id];
    assert_eq!(slot.entry.status, ProcStatus::WaitingRestart);
    assert_eq!(slot.entry.pid, None);
    assert_eq!(slot.entry.started_at, None);
    assert_eq!(
        slot.entry.last_exit, None,
        "the same reason as above, on the route that keeps a ladder"
    );
    assert_eq!(
        live_handles(slot),
        Vec::<&str>::new(),
        "every handle is cleared on the route that waits"
    );
    assert!(
        slot.log_ctl.is_some(),
        "the log pump's sender is the one kept"
    );
    assert!(
        slot.restart_due.is_some(),
        "the wait carries the wall-clock half of its timer, which is what a handover successor re-arms from"
    );

    assert_eq!(
        actor.runner.spawn_count(),
        2,
        "both refusals happen before the runner is reached, so neither status comes from a spawn that failed"
    );
}
