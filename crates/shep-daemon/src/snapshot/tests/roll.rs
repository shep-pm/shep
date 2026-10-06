//! `FlockRegistry::roll`: counting running instances and pruning deleted
//! names out of the map.

use super::super::*;
use super::info;

use shep_core::config::{AppConfig, normalize};
use shep_core::status::ProcStatus;

// fails if `is_running` starts counting `ProcStatus::Stopping`, the status
// a reload's drainee wears once its replacement is spawned. Both share one
// instance slot for the swap, so counting the drainee would roll a
// one-instance app at two.
#[test]
fn roll_counts_running_instances_and_prunes_deleted_names() {
    let registry = FlockRegistry::new();
    let web = normalize(AppConfig::minimal("web", "./srv")).unwrap();
    let job = normalize(AppConfig::minimal("job", "./job")).unwrap();
    registry.record(&[web, job]);

    let infos = [
        info(0, "web", ProcStatus::Online),
        info(1, "web", ProcStatus::WaitingRestart),
        info(2, "web", ProcStatus::Stopped),
        info(3, "web", ProcStatus::Stopping),
    ]; // `job` was deleted: no entries left
    let roll = registry.roll(&infos, 1_700_000_000_000);

    assert_eq!(roll.version, SNAPSHOT_VERSION);
    assert_eq!(roll.saved_at_ms, 1_700_000_000_000);
    assert_eq!(roll.apps.len(), 1, "a name with no live entry is pruned");
    assert_eq!(roll.apps[0].app.name, "web");
    // online + waiting-restart; neither the stopped one nor the drainee
    assert_eq!(roll.apps[0].instances_running, 2);
    // The prune is sticky: a second roll must not resurrect `job`.
    assert_eq!(registry.roll(&infos, 0).apps.len(), 1);
}

// fails if the count is kept per listing position rather than per name: the
// listing interleaves the names, and `idle` has entries but none running.
#[test]
fn roll_counts_each_name_separately_when_the_listing_interleaves_them() {
    let registry = FlockRegistry::new();
    let apps =
        ["web", "job", "idle"].map(|name| normalize(AppConfig::minimal(name, "./srv")).unwrap());
    registry.record(&apps);

    let infos = [
        info(0, "web", ProcStatus::Online),
        info(1, "job", ProcStatus::Starting),
        info(2, "idle", ProcStatus::Stopped),
        info(3, "web", ProcStatus::Online),
        info(4, "job", ProcStatus::Errored),
        info(5, "web", ProcStatus::Online),
    ];
    let roll = registry.roll(&infos, 0);

    let counts: Vec<(&str, u32)> = roll
        .apps
        .iter()
        .map(|saved| (saved.app.name.as_str(), saved.instances_running))
        .collect();
    assert_eq!(
        counts,
        [("idle", 0), ("job", 1), ("web", 3)],
        "a name with only stopped entries is kept, at zero"
    );
}
