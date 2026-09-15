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
