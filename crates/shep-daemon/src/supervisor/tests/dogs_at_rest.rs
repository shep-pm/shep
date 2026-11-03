//! Tests for dogs registered at rest: listed without a process, and spawned in
//! place when one is enabled.

use super::*;

fn dog_from(name: &str, program: &str) -> ResolvedApp {
    normalize(AppConfig::minimal(name, program)).unwrap()
}

fn names(wanted: &[&str]) -> BTreeSet<String> {
    wanted.iter().map(|name| (*name).to_string()).collect()
}

fn listed(actor: &Actor<ScriptedRunner>) -> Vec<ProcessInfo> {
    let mut rows: Vec<ProcessInfo> = actor
        .sheep
        .values()
        .map(|slot| actor.with_mark(to_info(&slot.entry, &actor.smits)))
        .collect();
    rows.sort_by_key(|info| info.id);
    rows
}

#[tokio::test(start_paused = true)]
async fn a_dog_at_rest_is_listed_stopped_and_disabled_and_nothing_is_spawned() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, Vec::new());

    let registered = actor.register_dog_at_rest(
        &dog_from("metrics", "/dogs/metrics"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&["metrics"]),
    );

    assert_eq!(registered.status, ProcStatus::Stopped);
    assert_eq!(registered.disabled, Some(true));
    assert_eq!(
        registered.dog,
        Some(DogSource::BuiltIn {
            grants: None,
            events: Vec::new()
        })
    );
    assert_eq!(registered.pid, None);
    assert_eq!(actor.runner.spawn_count(), 0);
    assert_eq!(listed(&actor), vec![registered]);
}

#[tokio::test(start_paused = true)]
async fn a_dog_the_operator_enabled_is_listed_without_the_mark() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, Vec::new());

    let registered = actor.register_dog_at_rest(
        &dog_from("bark", "/dogs/bark"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&[]),
    );

    assert_eq!(registered.status, ProcStatus::Stopped);
    assert_eq!(registered.disabled, None);
}

#[tokio::test(start_paused = true)]
async fn a_sheep_already_holding_the_name_is_neither_replaced_nor_marked() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, Vec::new());
    let sheep = actor.register_at_rest(&dog_from("metrics", "/sheep/metrics"));

    let answered = actor.register_dog_at_rest(
        &dog_from("metrics", "/dogs/metrics"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&["metrics"]),
    );

    assert_eq!(answered.id, sheep.id);
    assert_eq!(answered.dog, None);
    assert_eq!(listed(&actor).len(), 1);
    assert!(!actor.disabled.contains("metrics"));
}

#[tokio::test(start_paused = true)]
async fn starting_a_dog_at_rest_runs_it_under_the_id_it_was_listed_with() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let api = actor.register_at_rest(&dog_from("api", "/sheep/api"));
    let metrics = actor.register_dog_at_rest(
        &dog_from("metrics", "/dogs/metrics"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&["metrics"]),
    );
    let bark = actor.register_dog_at_rest(
        &dog_from("bark", "/dogs/bark"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&["bark"]),
    );
    assert_eq!((api.id, metrics.id, bark.id), (0, 1, 2));

    let started = actor
        .do_start_dog(
            dog_from("metrics", "/dogs/metrics"),
            DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(started.id, metrics.id);
    assert_eq!(started.status, ProcStatus::Online);
    assert_eq!(started.disabled, None);
    assert_eq!(started.restarts, 0);
    assert_eq!(actor.runner.spawn_count(), 1);
    let rows = listed(&actor);
    assert_eq!(rows.len(), 3, "the dog took its own row back: {rows:?}");
    assert_eq!(rows[2].status, ProcStatus::Stopped, "bark is still at rest");
    assert_eq!(rows[2].disabled, Some(true));
}

#[tokio::test(start_paused = true)]
async fn the_next_sheep_after_an_enable_still_takes_the_next_free_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(
        &dir,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    );
    actor.register_dog_at_rest(
        &dog_from("metrics", "/dogs/metrics"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&["metrics"]),
    );
    actor.register_dog_at_rest(
        &dog_from("bark", "/dogs/bark"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&["bark"]),
    );

    actor
        .do_start_dog(
            dog_from("metrics", "/dogs/metrics"),
            DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            },
        )
        .unwrap();
    let after = actor.register_at_rest(&dog_from("web", "/sheep/web"));

    assert_eq!(after.id, 2);
}

/// `shep adopt <new-path> --name same` after the boot registered the old path.
#[tokio::test(start_paused = true)]
async fn enabling_after_a_re_adopt_spawns_the_path_in_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let adopted = |path: &str| DogSource::Adopted {
        grants: None,
        path: path.into(),
        channel: false,
        events: Vec::new(),
    };
    actor.register_dog_at_rest(
        &dog_from("mine", "/old/mine"),
        adopted("/old/mine"),
        &names(&["mine"]),
    );

    let started = actor
        .do_start_dog(dog_from("mine", "/new/mine"), adopted("/new/mine"))
        .unwrap();

    assert_eq!(actor.runner.spawned_program(0), "/new/mine");
    assert_eq!(started.dog, Some(adopted("/new/mine")));
}

#[tokio::test(start_paused = true)]
async fn starting_a_dog_that_is_running_answers_with_it_and_spawns_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let first = actor
        .do_start_dog(
            dog_from("bark", "/dogs/bark"),
            DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            },
        )
        .unwrap();

    let again = actor
        .do_start_dog(
            dog_from("bark", "/dogs/bark"),
            DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(again.id, first.id);
    assert_eq!(actor.runner.spawn_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_dog_at_rest_that_cannot_spawn_is_errored_and_keeps_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, Vec::new());
    let at_rest = actor.register_dog_at_rest(
        &dog_from("bark", "/dogs/bark"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        &names(&["bark"]),
    );

    let failed = actor.do_start_dog(
        dog_from("bark", "/dogs/bark"),
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
    );

    assert!(matches!(failed, Err(SupervisorError::SpawnFailed(_))));
    let rows = listed(&actor);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, at_rest.id);
    assert_eq!(rows[0].status, ProcStatus::Errored);
    assert_eq!(
        rows[0].dog,
        Some(DogSource::BuiltIn {
            grants: None,
            events: Vec::new()
        })
    );
}

/// A dog the operator stopped is `Stopped` like one at rest, and a boot that
/// names it again (a handover successor's) must leave it stopped.
#[tokio::test(start_paused = true)]
async fn a_dog_that_ran_and_stopped_is_not_started_again_by_a_second_start_dog() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let running = actor
        .do_start_dog(
            dog_from("metrics", "/dogs/metrics"),
            DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            },
        )
        .unwrap();
    let slot = actor.sheep.get_mut(&running.id).unwrap();
    slot.entry.status = ProcStatus::Stopped;
    slot.entry.pid = None;
    slot.entry.last_exit = Some(ExitInfo {
        code: None,
        signal: Some(15),
    });

    let again = actor
        .do_start_dog(
            dog_from("metrics", "/dogs/metrics"),
            DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(again.id, running.id);
    assert_eq!(again.status, ProcStatus::Stopped);
    assert_eq!(actor.runner.spawn_count(), 1, "nothing was spawned again");
}
