//! Atomic file I/O: `write_atomic`/[`read`] round-trip, the temp file leaves
//! nothing behind, the roll is owner-only on unix, and a corrupt or
//! future-schema file is rejected rather than misread.

use super::super::*;
use super::info;

use shep_core::config::{AppConfig, normalize};
use shep_core::status::ProcStatus;

#[test]
fn write_atomic_round_trips_with_no_leftovers() {
    // The `0600` half lives in `write_atomic_is_owner_only_on_unix`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.json");
    let registry = FlockRegistry::new();
    registry.record(&[normalize(AppConfig::minimal("web", "./srv")).unwrap()]);
    let roll = registry.roll(&[info(0, "web", ProcStatus::Online)], 42);

    write_atomic(&path, &roll).unwrap();
    write_atomic(&path, &roll).unwrap(); // overwriting keeps the guarantees

    assert_eq!(read(&path).unwrap(), roll);
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(
        entries.len(),
        1,
        "no temp file may survive a completed write"
    );
}

#[cfg(unix)]
#[test]
fn write_atomic_is_owner_only_on_unix() {
    // The roll stores app env verbatim (spec §10): owner-only, always.
    // Unix-gated because `0600` has no Windows ACL equivalent.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.json");
    let registry = FlockRegistry::new();
    registry.record(&[normalize(AppConfig::minimal("web", "./srv")).unwrap()]);
    let roll = registry.roll(&[info(0, "web", ProcStatus::Online)], 42);

    write_atomic(&path, &roll).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the muster roll holds app env in cleartext");
}

#[test]
fn read_rejects_corrupt_json_and_unknown_schema_versions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.json");
    std::fs::write(&path, b"{not json").unwrap();
    assert!(matches!(read(&path), Err(SnapshotError::Parse { .. })));

    let future = format!(
        "{{\"version\":{},\"saved_at_ms\":0,\"apps\":[]}}",
        SNAPSHOT_VERSION + 1
    );
    std::fs::write(&path, future.as_bytes()).unwrap();
    assert!(matches!(read(&path), Err(SnapshotError::Parse { .. })));
}
