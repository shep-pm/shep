//! The record `[daemon.dog_grants.<name>]` keeps of a dog's upgrade path:
//! what waits for approval, what was revoked, and whether the operator has
//! looked.

use super::*;
use shep_core::permissions::{Grants, Permission};

fn record() -> GrantRecord {
    GrantRecord {
        granted: Grants::new([Permission::FlockRead]),
        waiting: Grants::new([Permission::Events("process.".to_string())]),
        revoked: Grants::new([Permission::LogsRead]),
        reviewed: false,
        events: vec!["exported".to_string()],
    }
}

#[test]
fn a_record_reads_back_what_was_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    ShepToml::try_edit(&path, |cfg| cfg.set_grant_record("otel", &record())).unwrap();

    assert_eq!(
        ShepToml::read_only(&path)
            .unwrap()
            .grant_record("otel")
            .unwrap(),
        Some(record())
    );
}

#[test]
fn a_clear_record_writes_only_granted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let granted = Grants::new([Permission::FlockRead]);

    ShepToml::try_edit(&path, |cfg| cfg.set_granted("otel", &granted)).unwrap();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[daemon]\n\n[daemon.dog_grants.otel]\ngranted = [\"flock.read\"]\n"
    );
    assert_eq!(
        ShepToml::read_only(&path)
            .unwrap()
            .grant_record("otel")
            .unwrap(),
        Some(GrantRecord::reviewed(granted))
    );
}

#[test]
fn the_marks_are_written_only_when_they_say_something() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    ShepToml::try_edit(&path, |cfg| cfg.set_grant_record("otel", &record())).unwrap();

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[daemon]\n\n\
         [daemon.dog_grants.otel]\n\
         granted = [\"flock.read\"]\n\
         waiting = [\"events:process.\"]\n\
         revoked = [\"logs.read\"]\n\
         reviewed = false\n\
         events = [\"exported\"]\n"
    );
}

#[test]
fn an_events_list_that_holds_a_non_string_is_refused_as_the_boot_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon.dog_grants.otel]\ngranted = []\nevents = [3]\n",
    )
    .unwrap();

    let err = ShepToml::read_only(&path)
        .unwrap()
        .grant_record("otel")
        .unwrap_err();

    assert!(err.to_string().contains("`events` must be a list"), "{err}");
}

#[test]
fn a_reviewed_that_is_not_a_bool_is_refused_as_the_boot_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon.dog_grants.otel]\ngranted = []\nreviewed = \"no\"\n",
    )
    .unwrap();

    let err = ShepToml::read_only(&path)
        .unwrap()
        .grant_record("otel")
        .unwrap_err();

    assert!(matches!(err, ShepTomlError::BadGrants { .. }), "{err:?}");
    assert!(err.to_string().contains("reviewed"), "{err}");
}

#[test]
fn a_misspelt_waiting_permission_is_refused_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon.dog_grants.otel]\ngranted = []\nwaiting = [\"flock.reed\"]\n",
    )
    .unwrap();

    let err = ShepToml::read_only(&path)
        .unwrap()
        .grant_record("otel")
        .unwrap_err();

    assert!(err.to_string().contains("flock.reed"), "{err}");
}
