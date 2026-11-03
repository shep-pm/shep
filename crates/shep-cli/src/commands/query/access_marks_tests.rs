//! The marks a local listing puts on each dog row, read from `shep.toml`.

use super::access_marks::mark_access;
use crate::commands::shep_toml::{GrantRecord, ShepToml};
use shep_core::paths::ShepPaths;
use shep_core::permissions::{Grants, Permission};
use shep_core::protocol::{DogAccess, DogSource, ProcessInfo};
use shep_core::status::ProcStatus;

fn paths() -> (tempfile::TempDir, ShepPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    (dir, paths)
}

fn dog(name: &str, grants: Option<Grants>) -> ProcessInfo {
    ProcessInfo::builder(1, name, ProcStatus::Online)
        .dog(Some(DogSource::BuiltIn {
            grants,
            events: Vec::new(),
        }))
        .build()
}

fn record(paths: &ShepPaths, name: &str, record: &GrantRecord) {
    ShepToml::try_edit(&paths.daemon_config, |cfg| {
        cfg.set_grant_record(name, record)
    })
    .unwrap();
}

fn read_only() -> Grants {
    Grants::new([Permission::FlockRead])
}

#[test]
fn a_dog_with_no_grants_is_marked_undeclared() {
    let (_dir, paths) = paths();
    let mut rows = vec![dog("otel", None)];

    mark_access(&mut rows, &paths);

    assert_eq!(
        rows[0].access,
        Some(DogAccess {
            undeclared: true,
            ..DogAccess::default()
        })
    );
}

#[test]
fn a_disabled_dog_with_no_grants_is_not_running_with_full_access() {
    let (_dir, paths) = paths();
    let mut rows = vec![
        ProcessInfo::builder(1, "bark", ProcStatus::Stopped)
            .dog(Some(DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            }))
            .disabled(Some(true))
            .build(),
    ];

    mark_access(&mut rows, &paths);

    assert_eq!(rows[0].access, Some(DogAccess::default()));
}

#[test]
fn a_list_written_at_upgrade_is_marked_not_reviewed() {
    let (_dir, paths) = paths();
    record(&paths, "otel", &GrantRecord::at_upgrade(read_only()));
    let mut rows = vec![dog("otel", Some(read_only()))];

    mark_access(&mut rows, &paths);

    assert_eq!(
        rows[0].access,
        Some(DogAccess {
            not_reviewed: true,
            ..DogAccess::default()
        })
    );
}

#[test]
fn permissions_waiting_for_approval_are_named() {
    let (_dir, paths) = paths();
    record(
        &paths,
        "otel",
        &GrantRecord {
            waiting: Grants::new([Permission::LogsRead]),
            ..GrantRecord::reviewed(read_only())
        },
    );
    let mut rows = vec![dog("otel", Some(read_only()))];

    mark_access(&mut rows, &paths);

    assert_eq!(
        rows[0].access,
        Some(DogAccess {
            waiting: vec!["logs.read".to_owned()],
            ..DogAccess::default()
        })
    );
}

#[test]
fn a_reviewed_dog_carries_clear_marks() {
    let (_dir, paths) = paths();
    record(&paths, "otel", &GrantRecord::reviewed(read_only()));
    let mut rows = vec![dog("otel", Some(read_only()))];

    mark_access(&mut rows, &paths);

    assert_eq!(rows[0].access, Some(DogAccess::default()));
}

#[test]
fn a_sheep_is_left_unmarked() {
    let (_dir, paths) = paths();
    let mut rows = vec![ProcessInfo::builder(2, "web", ProcStatus::Online).build()];

    mark_access(&mut rows, &paths);

    assert_eq!(rows[0].access, None);
}

#[test]
fn an_unreadable_shep_toml_leaves_the_listing_as_it_came() {
    let (_dir, paths) = paths();
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(&paths.daemon_config, "[daemon\n").unwrap();
    let mut rows = vec![dog("otel", Some(read_only()))];

    mark_access(&mut rows, &paths);

    assert_eq!(rows[0].access, None);
}
