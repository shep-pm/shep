//! The dependency graph: boot order, a cycle that must not refuse the
//! restore, `autostart = false` overriding `depends_on`, a dependency the
//! roll still holds versus one that is truly missing, a dog the boot has not
//! promoted yet, a dog holding a saved sheep's name, and naming every member
//! of a knot rather than only the representative path through it.

use super::super::*;

use crate::fake::{ProcScript, ScriptedRunner};
use crate::supervisor::spawn_supervisor;
use crate::testing::{capture_logs, roll_of, test_paths};
use shep_core::config::graph::{BootNode, NodeKind, plan};
use shep_core::config::{AppConfig, normalize};
use shep_core::protocol::{BusEvent, DogSource, ProcessEventKind, ProcessInfo};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

/// Every `process.start` name waiting on `rx`, in the order the bus
/// carried them.
fn start_order(rx: &mut broadcast::Receiver<SharedEvent>) -> Vec<String> {
    let mut order = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let BusEvent::Process {
            event: ProcessEventKind::Start,
            info,
            ..
        } = &*event
        {
            order.push(info.name.clone());
        }
    }
    order
}

/// fails if the restore still hands the whole roll over as one batch. The
/// roll is written `api` first, which is the order that starts a sheep
/// before the one it waits for.
#[tokio::test(start_paused = true)]
async fn a_restore_starts_the_roll_in_dependency_order() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    let mut db = AppConfig::minimal("db", "./srv");
    db.listen_timeout = UpDuration::from_millis(50);
    let mut api = AppConfig::minimal("api", "./srv");
    api.depends_on = vec!["db".to_string()];
    write_atomic(&paths.snapshot, &roll_of(vec![api, db])).unwrap();

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits(); 2]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();
    let mut seen = events.subscribe();

    muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
        .await
        .unwrap();

    assert_eq!(
        start_order(&mut seen),
        vec!["db", "api"],
        "the roll's order must not decide boot order"
    );
    handle.shutdown().await;
}

/// fails if a cycle refuses the restore, which would strand an unattended
/// boot on a typo nobody is there to read.
#[tokio::test(start_paused = true)]
async fn a_cyclic_roll_still_brings_the_flock_up() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    let mut a = AppConfig::minimal("a", "./srv");
    a.depends_on = vec!["b".to_string()];
    let mut b = AppConfig::minimal("b", "./srv");
    b.depends_on = vec!["a".to_string()];
    write_atomic(&paths.snapshot, &roll_of(vec![a, b])).unwrap();

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits(); 2]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();

    let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
        .await
        .expect("a cycle must not refuse the restore");

    assert_eq!(restored, vec!["a".to_string(), "b".to_string()]);
    let mut listed = handle.list().await;
    listed.sort_by(|left, right| left.name.cmp(&right.name));
    let seen: Vec<(&str, ProcStatus)> = listed
        .iter()
        .map(|info| (info.name.as_str(), info.status))
        .collect();
    assert_eq!(
        seen,
        vec![("a", ProcStatus::Online), ("b", ProcStatus::Online)],
        "both members of the knot run; neither waits for the other"
    );
    handle.shutdown().await;
}

/// fails if `depends_on` overrides `autostart`, which would let one app's
/// file start a sheep another app's file said not to start.
#[tokio::test(start_paused = true)]
async fn a_dependency_with_autostart_off_is_warned_about_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
    let mut db = AppConfig::minimal("db", "./srv");
    db.autostart = false;
    let mut api = AppConfig::minimal("api", "./srv");
    api.depends_on = vec!["db".to_string()];
    write_atomic(&paths.snapshot, &roll_of(vec![db, api])).unwrap();

    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        paths.clone(),
        events.clone(),
    );
    let registry = FlockRegistry::new();
    let mut seen = events.subscribe();

    muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
        .await
        .unwrap();

    assert_eq!(
        start_order(&mut seen),
        vec!["api"],
        "db opted out; api starts without it"
    );
    handle.shutdown().await;
}

/// An app named `name` that waits for `deps`, normalized.
fn dependant(name: &str, deps: &[&str]) -> ResolvedApp {
    let mut app = AppConfig::minimal(name, "./srv");
    app.depends_on = deps.iter().map(|dep| (*dep).to_string()).collect();
    normalize(app).expect("a minimal app with edges normalizes")
}

/// fails if a dependency the roll still holds is reported as naming
/// nothing this flock has. The plan is built from `to_start` alone, so on
/// `shep muster` against a live flock every already-running dependency is
/// unresolved, and this warning fired on the commonest path there is.
///
/// The second half is the control: a name the roll does not hold either
/// really is missing, and must still be said out loud.
#[test]
fn a_dependency_the_roll_still_holds_is_not_called_missing() {
    let api = dependant("api", &["db"]);
    let db = normalize(AppConfig::minimal("db", "./srv")).expect("a minimal app normalizes");
    let to_start = vec![api.clone()];
    let plan = crate::boot_order::plan_for(&to_start, &[], &[]);
    assert_eq!(
        plan.unresolved.len(),
        1,
        "the plan only ever sees what this restore starts"
    );

    let held =
        capture_logs(|| warn_about_the_graph(&plan, &to_start, &[db, api.clone()], &[], &[], &[]));
    assert!(!held.contains("names nothing this flock has"), "{held}");

    let absent = capture_logs(|| warn_about_the_graph(&plan, &to_start, &[api], &[], &[], &[]));
    assert!(absent.contains("names nothing this flock has"), "{absent}");
}

/// fails if a sheep that waits for an unpromoted dog is ordered behind it
/// in silence. `boot` spawns dogs in two groups, promoted before the
/// restore and the rest after every stage, so only a promoted dog is
/// running by the time a sheep that names it starts.
///
/// The control is the same graph with the dog promoted: there the wait is
/// honoured and there is nothing to say.
#[test]
fn a_dependency_on_a_dog_the_boot_never_promotes_is_warned_about() {
    let to_start = vec![dependant("api", &["metrics"])];
    let dogs = ["metrics".to_string()];

    let late = capture_logs(|| {
        let plan = crate::boot_order::plan_for(&to_start, &dogs, &[]);
        warn_about_the_graph(&plan, &to_start, &to_start, &[], &dogs, &[]);
    });
    assert!(late.contains("starts after the whole flock"), "{late}");

    let promoted = capture_logs(|| {
        let plan = crate::boot_order::plan_for(&to_start, &dogs, &dogs);
        warn_about_the_graph(&plan, &to_start, &to_start, &[], &dogs, &dogs);
    });
    assert!(
        !promoted.contains("starts after the whole flock"),
        "{promoted}"
    );
}

/// fails if the unpromoted-dog warning reads the promotion list alone.
/// [`muster`] is also the handler for an operator's `Request::Muster`,
/// reached long after boot with both dog groups up, so a sheep waiting on
/// an unpromoted but LIVE dog would be warned about on every
/// `shep muster`. That is the same false-positive class `restorable`
/// closes for a sheep-to-sheep edge.
///
/// Driven through `muster` rather than through `warn_about_the_graph`
/// with a synthetic list: the sibling cases all hand it their own
/// arguments, so none of them can see a flock at all. The control is the
/// same roll against a shepherd holding no dog, which is boot's case and
/// must still warn.
///
/// `#[test]` with a `block_on` of its own, not `#[tokio::test]`:
/// `capture_logs` scopes its subscriber to a synchronous closure.
#[test]
fn a_dependency_on_an_unpromoted_dog_that_is_already_running_is_not_warned_about() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // `metrics` is a dog this shepherd holds and does not promote, and
    // `api` waits for it.
    let dogs = ["metrics".to_string()];
    let restore = |dog_running: bool| {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let mut api = AppConfig::minimal("api", "./srv");
        api.depends_on = vec!["metrics".to_string()];
        write_atomic(&paths.snapshot, &roll_of(vec![api])).unwrap();

        capture_logs(|| {
            rt.block_on(async {
                let (events, _rx) = crate::bus::test_bus(64);
                let handle = spawn_supervisor(
                    ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]),
                    paths.clone(),
                    events.clone(),
                );
                if dog_running {
                    let dog = normalize(AppConfig::minimal("metrics", "./srv"))
                        .expect("a minimal dog normalizes");
                    handle
                        .start_dog(dog, DogSource::BuiltIn)
                        .await
                        .expect("a scripted dog that never exits starts");
                }
                muster(
                    &paths.snapshot,
                    &FlockRegistry::new(),
                    &handle,
                    &events,
                    &dogs,
                    &[],
                )
                .await
                .expect("the roll parses");
            });
        })
    };

    let live = restore(true);
    assert!(!live.contains("starts after the whole flock"), "{live}");

    let at_boot = restore(false);
    assert!(
        at_boot.contains("starts after the whole flock"),
        "{at_boot}"
    );
}

/// fails if a promoted dog takes a saved sheep's name in silence. It
/// registers against an empty flock before the restore, so `muster` reads
/// the name as already running and drops the sheep from both the
/// membership pass and the start while still reporting it restored.
///
/// The control is the same name running as an ordinary sheep, which is
/// the idempotent restore this must not start warning about.
#[test]
fn a_dog_holding_a_saved_sheeps_name_is_warned_about() {
    let members = vec![normalize(AppConfig::minimal("metrics", "./srv")).expect("normalizes")];
    let as_dog = [ProcessInfo::builder(0, "metrics", ProcStatus::Online)
        .dog(Some(DogSource::BuiltIn))
        .build()];
    let as_sheep = [ProcessInfo::builder(0, "metrics", ProcStatus::Online).build()];

    let collided = capture_logs(|| warn_about_dogs_holding_sheep_names(&members, &as_dog));
    assert!(collided.contains("is not restored"), "{collided}");

    let plain = capture_logs(|| warn_about_dogs_holding_sheep_names(&members, &as_sheep));
    assert!(!plain.contains("is not restored"), "{plain}");
}

/// fails if a plain two-node cycle prints its membership twice. `plan`
/// puts every knot into one stage, so the membership line is flock-wide,
/// and on two nodes it repeats the representative path verbatim.
///
/// The three-node knot is the control: there the path names two of the
/// three, so the membership line is the only thing that names the third.
#[test]
fn a_two_node_knot_is_not_reported_twice_over() {
    let pair = vec![dependant("a", &["b"]), dependant("b", &["a"])];
    let plan = crate::boot_order::plan_for(&pair, &[], &[]);

    let two = capture_logs(|| warn_about_the_graph(&plan, &pair, &pair, &[], &[], &[]));
    assert!(
        !two.contains("every sheep a dependency cycle holds"),
        "{two}"
    );

    let trio = vec![
        dependant("a", &["b"]),
        dependant("b", &["a", "c"]),
        dependant("c", &["b"]),
    ];
    let plan = crate::boot_order::plan_for(&trio, &[], &[]);
    let three = capture_logs(|| warn_about_the_graph(&plan, &trio, &trio, &[], &[], &[]));
    assert!(
        three.contains("every sheep a dependency cycle holds"),
        "{three}"
    );
}

/// fails if the cycle warning names only the representative path. Every
/// one of these three is stuck, and the path through the knot names two.
#[test]
fn the_cyclic_stage_names_every_sheep_in_the_knot() {
    let node = |name: &str, deps: &[&str]| BootNode {
        name: name.to_string(),
        depends_on: deps.iter().map(|dep| (*dep).to_string()).collect(),
        kind: NodeKind::Sheep,
    };
    let plan = plan(&[node("a", &["b"]), node("b", &["a", "c"]), node("c", &["b"])]);
    assert_eq!(plan.cycles.len(), 1, "one knot, one representative path");
    assert!(
        plan.cycles[0].len() < 3,
        "the point of the extra report is a member the path leaves out"
    );

    assert_eq!(
        cyclic_stage(&plan),
        Some(&vec!["a".to_string(), "b".to_string(), "c".to_string()]),
    );
}
