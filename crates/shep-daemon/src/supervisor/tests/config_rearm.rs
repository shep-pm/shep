//! Tests for what a load does to armed subsystems and to instance counts.
//!
//! Changing an extras field has to rearm the sheep against the new spec. A
//! plain load never scales and a reset does, and a change that cannot be parked
//! must not be reported as parked.

use super::*;

/// `max_memory`, `watch` and the cron pair are read when a worker is
/// armed, so a spec write alone leaves the old value enforced for as long
/// as that arming lives.
#[tokio::test(start_paused = true)]
async fn a_changed_extras_field_rearms_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(100 << 20));
        })],
    );

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_memory = Some(MemSize::from_bytes(512 << 20));
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_memory"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        enforcer
            .arms()
            .last()
            .expect("a changed ceiling re-arms the name")
            .limit,
        MemSize::from_bytes(512 << 20),
        "the registry was re-armed with the old ceiling"
    );
}

/// `PollingEnforcer` computes a breach under its lock and sends it after
/// releasing that lock, so a re-arm landing in between leaves a report in
/// flight speaking for a limit nobody enforces. Reachable only because a
/// load re-arms an id that is already armed.
#[tokio::test(start_paused = true)]
async fn a_breach_measured_under_a_since_raised_ceiling_does_not_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(100 << 20));
        })],
    );

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_memory = Some(MemSize::from_bytes(512 << 20));
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_memory"])],
        ResetDepth::None,
    )
    .await;

    // Over the ceiling that was armed when the sample was taken, well
    // under the one the load just put in force.
    actor.handle_extra_restart(
        0,
        APPLY_FIRST_PID,
        None,
        Some(MemSize::from_bytes(200 << 20)),
    );

    let slot = &actor.sheep[&0];
    assert!(
        slot.manual.is_none(),
        "a breach against a ceiling the operator has raised must never claim the manual marker"
    );
    assert_eq!(slot.entry.pid, Some(APPLY_FIRST_PID));
}

/// The merge builds on the app's intended config, the parked one when
/// there is one, not on what the running child was spawned from: on the
/// second load the key is established, a plain load skips it, and a merge
/// based on the running config would carry the old value forward over the
/// parked one.
#[tokio::test(start_paused = true)]
async fn a_second_load_of_the_same_file_keeps_the_first_loads_parked_config() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
        declared_app(file, &["name", "script", "env"])
    };

    let first = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert_eq!(first[0].pending, vec!["env".to_string()]);

    let second = apply_config(&mut actor, vec![file()], ResetDepth::None).await;

    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("the parked config survives a second load")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the second load erased what the first parked"
    );
    assert_eq!(
        second[0]
            .app
            .as_ref()
            .expect("an applied app is recorded")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the recorded app lost it too, so a reboot would come up without it"
    );
    assert!(
        second[0].pending.is_empty(),
        "the second load changes nothing that was not already coming: {second:?}"
    );
}

/// An instance with no live task is deregistered synchronously inside
/// `handle_scale`, so the id list read before the scale can name a slot
/// that is already gone by the time the spec write walks it.
#[tokio::test(start_paused = true)]
async fn a_scale_down_removing_a_non_running_instance_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // One instance exited and never came back. No `ctl`, so its delete
    // resolves on the spot rather than through a kill ladder.
    let stopped = actor.sheep.get_mut(&1).expect("the fixture registers two");
    stopped.entry.status = ProcStatus::Stopped;
    stopped.entry.pid = None;

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 1;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "instances"])],
        ResetDepth::Policy,
    )
    .await;

    assert_eq!(actor.ids_of_name("web"), vec![0]);
    assert_eq!(reply[0].applied, vec!["instances".to_string()]);
    assert_eq!(actor.sheep[&0].entry.spec.config().instances, 1);
}

/// `Applied::refused` with two empty lists promises nothing happened, so a
/// reply with `app` as `None` leaves the muster roll on the old count while
/// a second instance runs. A file declaring `watch` and `cwd` together
/// merges cleanly and cannot be reached by a running instance, since `cwd`
/// needs a respawn and `watch` does not.
#[tokio::test(start_paused = true)]
async fn a_load_that_scales_and_cannot_reach_the_running_spec_reports_what_landed() {
    let root = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    file.watch = true;
    file.cwd = Some(root.path().display().to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(
            file,
            &["name", "script", "instances", "watch", "cwd"],
        )],
        ResetDepth::Policy,
    )
    .await;

    assert_eq!(
        actor.ids_of_name("web").len(),
        2,
        "the scale really did happen"
    );
    assert!(
        reply[0].refused.is_none(),
        "a merge that normalizes is not an invalid file: {reply:?}"
    );
    assert!(
        reply[0].app.is_some(),
        "the muster roll must not be left on the pre-load config: {reply:?}"
    );
    assert_eq!(reply[0].applied, vec!["instances".to_string()]);
    assert!(
        reply[0].pending.contains(&"watch".to_string())
            && reply[0].pending.contains(&"cwd".to_string()),
        "a change no running instance can take must park, not vanish: {reply:?}"
    );
    assert!(
        actor.sheep[&0].entry.pending.is_some(),
        "and it must be parked on the entry, not only reported"
    );
}

/// Group membership is decided by the config rather than by what is
/// running, and both group triggers restart every member, so arming a
/// stopped instance lets a cron occurrence or a file save start it again.
/// Nothing heals that: the member is terminal, so no transition calls
/// `disarm` for it.
#[tokio::test(start_paused = true)]
async fn a_load_does_not_arm_a_stopped_instance() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })],
    );
    let stopped = actor.sheep.get_mut(&0).expect("the fixture registers one");
    stopped.entry.status = ProcStatus::Stopped;
    stopped.entry.pid = None;

    let mut file = AppConfig::minimal("web", "./srv");
    file.cron_restart = Some("*/5 * * * *".to_string());
    apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "cron_restart"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(
        actor.registry.group_members("web"),
        None,
        "a load armed a schedule that will start a sheep the operator stopped"
    );
}

/// A plain load skips the count and says why; a `--reset` takes it. The
/// override store cannot tell a stocked count from an untouched one, so a
/// plain load acting on the field would delete instances.
#[tokio::test(start_paused = true)]
async fn a_plain_load_never_scales_and_a_reset_does() {
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.instances = 1;
        declared_app(file, &["name", "script", "instances"])
    };

    let plain_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&plain_dir, &[app_with("web", |app| app.instances = 2)]);
    let reply = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        2,
        "a plain load deleted an instance"
    );
    assert!(!reply[0].applied.contains(&"instances".to_string()));
    assert_eq!(
        reply[0].refused.as_deref(),
        Some(
            "instances: this load never reshapes a flock; no mode scales without also \
             putting back every setting the file declares, and `--reset=file` is the \
             narrowest that does, taking the file's count of 1"
        ),
        "the refusal must name a mode an operator can actually type, \
         not the bare flag `shep start --reset` now refuses on its \
         own, and it must name what following the advice costs: {reply:?}"
    );
    assert_eq!(
        reply[0]
            .app
            .as_ref()
            .expect("an applied app is recorded")
            .config()
            .instances,
        2,
        "the recorded count must be the one really running"
    );

    let reset_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) =
        actor_over(&reset_dir, &[app_with("web", |app| app.instances = 2)]);
    let reply = apply_config(&mut actor, vec![file()], ResetDepth::Policy).await;
    assert_eq!(actor.ids_of_name("web"), vec![0], "--reset must take it");
    assert_eq!(reply[0].applied, vec!["instances".to_string()]);
}

/// All eight [`EXTRAS_FIELDS`] are read when a worker is armed, so a spec
/// write alone leaves the old value enforced for the life of that worker.
/// The observable is the memory ceiling, unchanged across these apps: a
/// re-arm arms every instance, so any arming recorded is proof of one.
#[tokio::test(start_paused = true)]
async fn every_extras_field_triggers_a_rearm() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().display().to_string();
    let ceiling = MemSize::from_bytes(100 << 20);
    // The "before" values every case edits away from.
    let base = |cwd: &str| {
        let cwd = cwd.to_string();
        move |app: &mut AppConfig| {
            app.max_memory = Some(ceiling);
            app.cwd = Some(cwd.clone());
            app.watch = true;
            app.ignore_watch = vec!["target/**".to_string()];
            app.watch_delay = Some(UpDuration::from_millis(1000));
            app.watch_options = vec!["src/**".to_string()];
            app.cron_restart = Some("0 * * * *".to_string());
            app.cron_timezone = Some("UTC".to_string());
            // An interval no paused-clock case advances to, so the probe
            // never runs and this stays about arming.
            app.liveness_probe = Some(ProbeConfig {
                failure_threshold: 3,
                interval: UpDuration::from_millis(600_000),
                timeout: UpDuration::from_millis(1000),
                ..probe_config(ProbeKind::Tcp, "127.0.0.1:1")
            });
        }
    };
    // One edit per entry in `EXTRAS_FIELDS`, named by the field it moves,
    // so a field dropped from that list goes red under its own name.
    type Edit = (&'static str, fn(&mut AppConfig));
    let edits: Vec<Edit> = vec![
        ("max_memory", |app| {
            app.max_memory = Some(MemSize::from_bytes(512 << 20));
        }),
        ("watch", |app| app.watch = false),
        ("ignore_watch", |app| {
            app.ignore_watch = vec!["dist/**".to_string()];
        }),
        ("watch_delay", |app| {
            app.watch_delay = Some(UpDuration::from_millis(2500));
        }),
        ("watch_options", |app| {
            app.watch_options = vec!["lib/**".to_string()];
        }),
        ("cron_restart", |app| {
            app.cron_restart = Some("*/5 * * * *".to_string());
        }),
        ("cron_timezone", |app| {
            app.cron_timezone = Some("Europe/Berlin".to_string());
        }),
        ("liveness_probe", |app| app.liveness_probe = None),
    ];

    for (field, edit) in edits {
        let dir = tempfile::tempdir().unwrap();
        let (mut actor, enforcer) = actor_over(&dir, &[app_with("web", base(&cwd))]);
        let mut file = AppConfig::minimal("web", "./srv");
        base(&cwd)(&mut file);
        edit(&mut file);
        let reply = apply_config(
            &mut actor,
            vec![declared_app(
                file,
                &[
                    "name",
                    "script",
                    "cwd",
                    "max_memory",
                    "watch",
                    "ignore_watch",
                    "watch_delay",
                    "watch_options",
                    "cron_restart",
                    "cron_timezone",
                    "liveness_probe",
                ],
            )],
            ResetDepth::Policy,
        )
        .await;
        assert!(
            reply[0].applied.contains(&field.to_string()),
            "{field} did not apply at all: {reply:?}"
        );
        assert!(
            !enforcer.arms().is_empty(),
            "changing {field} left the armed worker on the old value"
        );
    }
}

/// A name whose instances are all momentarily non-`Online`, each inside a
/// crash-restart backoff, has nothing to arm, and an early return there
/// would skip the teardown too. `disarm_extras` leaves a `WaitingRestart`
/// sheep armed, so it stays a group member and the group is never torn
/// down. Arms first, which makes this a test of the teardown;
/// `a_load_does_not_arm_a_stopped_instance` arms nothing, so its assertion
/// holds either way.
#[tokio::test(start_paused = true)]
async fn a_load_tears_down_a_group_whose_instances_are_all_down() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.cron_restart = Some("0 * * * *".to_string());
        })],
    );
    actor.arm_extras(0);
    assert_eq!(
        actor.registry.group_members("web"),
        Some(vec![0]),
        "the fixture must really be armed, or this pins nothing"
    );

    // Mid-backoff: no pid, not online, still registered and still a group
    // member.
    let waiting = actor.sheep.get_mut(&0).expect("the fixture registers one");
    waiting.entry.status = ProcStatus::WaitingRestart;
    waiting.entry.pid = None;

    let mut file = AppConfig::minimal("web", "./srv");
    file.cron_restart = Some("*/5 * * * *".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "cron_restart"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(reply[0].applied, vec!["cron_restart".to_string()]);
    assert_eq!(
        actor.registry.group_members("web"),
        None,
        "a worker built from the replaced config survived a load that reported the \
         field as applied"
    );
}

/// The merge normalizes against the count the intended config carries, and
/// the flock can be running a different one, where the same config
/// refuses. The earlier parked config is left alone, still the one a
/// respawn picks up, so the report must not claim this load's fields are
/// in it.
#[tokio::test(start_paused = true)]
async fn a_change_that_cannot_be_parked_is_not_reported_as_parked() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // An earlier load parked a one-instance config. Two are running.
    let earlier = app_with("web", |app| {
        app.instances = 1;
        app.env = BTreeMap::from([("MODE".to_string(), "blue".to_string())]);
    });
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }

    // One explicit log path, no `{{instance}}` and no `merge_logs`: legal
    // for the one instance the parked config declares, refused for the
    // two really running.
    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/tmp/web.log".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "out_file"])],
        ResetDepth::None,
    )
    .await;

    assert!(
        !reply[0].pending.contains(&"out_file".to_string()),
        "a field that went nowhere must not be reported as coming: {reply:?}"
    );
    assert!(
        reply[0]
            .refused
            .as_deref()
            .is_some_and(|why| why.contains("out_file")),
        "and the operator must be told which field it was: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("the earlier load's parked config survives")
            .config()
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue"),
        "the earlier load's parked config must not be cleared"
    );
    assert!(
        reply[0].app.is_none(),
        "there is no config that holds at the count really running, so nothing may be \
         recorded: {reply:?}"
    );
}

/// A plain load holds `instances` out of the merge, so the sibling case
/// above does not cover the depth every `shep start` uses. Two running
/// instances plus one shared explicit `out_file` is the reachable shape.
#[tokio::test(start_paused = true)]
async fn a_plain_load_whose_merge_cannot_normalize_refuses_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);

    let mut file = AppConfig::minimal("web", "./srv");
    file.out_file = Some("/tmp/web.log".to_string());
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "out_file"])],
        ResetDepth::None,
    )
    .await;

    assert!(reply[0].refused.is_some(), "{reply:?}");
    assert!(reply[0].applied.is_empty() && reply[0].pending.is_empty());
    assert!(reply[0].app.is_none());
    assert!(actor.sheep[&0].entry.spec.config().out_file.is_none());
    assert!(actor.sheep[&0].entry.pending.is_none());
    assert_eq!(actor.ids_of_name("web").len(), 2);
}

/// `parked_wanted` is set by an earlier parked config as well as by this
/// load's own `NeedsRespawn` drift, so a load whose only drift is a Live
/// field can fail the rebuild with no field of its own to name. The stale
/// parked config then puts the old value back at the next respawn.
/// Reachable whenever the instance count moved between two loads.
#[tokio::test(start_paused = true)]
async fn a_parked_config_that_cannot_be_rebuilt_is_reported_even_with_no_field_to_name() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // Parked when the app ran one instance: a shared explicit log path is
    // legal for one and refused for the two running now.
    let earlier = app_with("web", |app| {
        app.instances = 1;
        app.max_restarts = 10;
        app.out_file = Some("/tmp/web.log".to_string());
    });
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }

    let mut file = AppConfig::minimal("web", "./srv");
    file.max_restarts = 99;
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "max_restarts"])],
        ResetDepth::None,
    )
    .await;

    assert_eq!(actor.sheep[&0].entry.spec.config().max_restarts, 99);
    assert!(
        reply[0]
            .refused
            .as_deref()
            .is_some_and(|why| why.contains("could not be rebuilt")),
        "a respawn is going to put max_restarts back to 10 and nobody was told: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("the earlier parked config is still there")
            .config()
            .max_restarts,
        10,
        "and that is what makes the refusal true"
    );
}

/// The store's `declared` set is what `ResetDepth::None` skips over, so a
/// refused key entering it makes the refusal's own advice useless: the
/// retry meets silence rather than the same refusal.
#[tokio::test(start_paused = true)]
async fn a_refused_key_is_not_established_so_the_same_file_still_tries() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    let earlier = app_with("web", |app| app.instances = 1);
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }
    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.out_file = Some("/tmp/web.log".to_string());
        declared_app(file, &["name", "script", "out_file"])
    };

    let names_it = |why: &str| why.contains("out_file");
    let first = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert!(
        first[0].refused.as_deref().is_some_and(names_it),
        "{first:?}"
    );

    let second = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert!(
        second[0].refused.as_deref().is_some_and(names_it),
        "a retry of the same file must meet the same refusal, not silence: {second:?}"
    );
    assert!(
        !shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .expect("the load recorded what it established")
            .declared
            .contains("out_file"),
        "a key that went nowhere was established by nobody"
    );
}

/// `ResetDepth::None` skips a key somebody has established, so a load that
/// records nothing leaves every key permanently re-writable. Three loads,
/// because two cannot tell the difference: the first establishes the key,
/// the second drops it from the file, and the third re-adds it with a
/// different value, which only a record written by the first can refuse.
#[tokio::test(start_paused = true)]
async fn a_key_a_load_took_is_established_against_the_next_load() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    let with_budget = |budget: u32| {
        let mut file = AppConfig::minimal("web", "./srv");
        file.max_restarts = budget;
        declared_app(file, &["name", "script", "max_restarts"])
    };

    let first = apply_config(&mut actor, vec![with_budget(99)], ResetDepth::None).await;
    assert_eq!(first[0].applied, vec!["max_restarts".to_string()]);
    assert_eq!(actor.sheep[&0].entry.spec.config().max_restarts, 99);

    // The key leaves the file. Nothing happens, and nothing may forget
    // that it was established.
    let dropped = apply_config(
        &mut actor,
        vec![declared_app(
            AppConfig::minimal("web", "./srv"),
            &["name", "script"],
        )],
        ResetDepth::None,
    )
    .await;
    assert!(dropped[0].applied.is_empty(), "{dropped:?}");

    // And comes back with a different value. The app has run on 99 since
    // the first load, so a plain load must not take it.
    let third = apply_config(&mut actor, vec![with_budget(5)], ResetDepth::None).await;

    assert_eq!(
        actor.sheep[&0].entry.spec.config().max_restarts,
        99,
        "a file overwrote a key an earlier load had established"
    );
    assert!(third[0].applied.is_empty(), "{third:?}");
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .expect("a load records what it established")
            .declared
            .contains("max_restarts"),
        "and the record is what makes that true"
    );
}

/// A key the file declares gives up its override during the merge, so a
/// load that then fails to park has to hand it back. `env` is the one
/// field that reaches this under the default depth: top-level keys hold
/// their override by being established and never spend anything, while
/// `env` merges one key at a time and spends the whole override table for
/// any key the file declares.
#[tokio::test(start_paused = true)]
async fn a_refused_env_change_gives_the_operators_override_back() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 2)]);
    // Parked when the app ran one instance, with a shared explicit log
    // path: legal for one, refused for the two running now.
    let earlier = app_with("web", |app| {
        app.instances = 1;
        app.out_file = Some("/tmp/web.log".to_string());
    });
    for id in [0, 1] {
        actor
            .sheep
            .get_mut(&id)
            .expect("the fixture registers two")
            .entry
            .pending = Some(earlier.clone());
    }
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(
            &["name", "script"],
            vec![("env", serde_json::json!({ "OPERATOR": "1" }))],
        ),
    )
    .unwrap();

    // Two env keys: the one the operator already holds, which spends the
    // override table, and one nobody has, which makes `env` drift.
    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([
        ("OPERATOR".to_string(), "2".to_string()),
        ("MODE".to_string(), "blue".to_string()),
    ]);
    let reply = apply_config(
        &mut actor,
        vec![declared_app(file, &["name", "script", "env"])],
        ResetDepth::None,
    )
    .await;
    assert!(
        reply[0]
            .refused
            .as_deref()
            .is_some_and(|why| why.contains("env")),
        "the fixture must really refuse the env change: {reply:?}"
    );

    let record = shep_core::overrides::get(&actor.paths.overrides, "web")
        .unwrap()
        .expect("a load records what it established");
    assert_eq!(
        record
            .fields
            .get("env")
            .and_then(|env| env.get("OPERATOR"))
            .and_then(serde_json::Value::as_str),
        Some("1"),
        "a load that changed nothing spent the operator's override"
    );
    assert!(
        record.declared_env.is_empty(),
        "and it established env keys that never landed: {:?}",
        record.declared_env
    );
}
