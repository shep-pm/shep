//! Secret redaction: a resolved reference must never reach the roll on disk,
//! and `FlockSnapshot`'s `Debug` must not print env values into daemon logs.

use super::super::*;
use super::info;

use shep_core::config::{AppConfig, normalize};
use shep_core::status::ProcStatus;

use crate::testing::{roll_of, test_paths};

/// Spec decision 2's argument for shipping the store unencrypted is that
/// a reference keeps plaintext in exactly two places: the store, and the
/// child's own environment. The roll is the file that would break it,
/// since it is written on every registry change and read back at boot.
///
/// Assembles the same app first, so the fixture is one that really
/// resolves rather than one whose reference never had a value to leak.
#[test]
fn a_resolved_secret_never_reaches_the_muster_roll() {
    const SENTINEL: &str = "hunter2-that-must-never-reach-disk";

    let mut config = AppConfig::minimal("web", "./srv");
    config
        .env
        .insert("PW".to_string(), "{{secret:DB_PASSWORD}}".to_string());
    let app = normalize(config).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    let view = shep_core::secrets::SecretView::new(
        "production".to_string(),
        std::collections::BTreeMap::from([(
            "DB_PASSWORD".to_string(),
            std::collections::BTreeMap::from([("production".to_string(), SENTINEL.to_string())]),
        )]),
        shep_core::secrets::ProviderCache::default(),
    );
    let spec = crate::assemble::assemble(&app, 0, &paths, None, &view).unwrap();
    assert_eq!(
        spec.env["PW"], SENTINEL,
        "the fixture must really resolve, or this case proves nothing"
    );

    let registry = FlockRegistry::new();
    registry.record(&[app]);
    let roll = registry.roll(&[info(0, "web", ProcStatus::Online)], 0);

    let json = serde_json::to_string(&roll).unwrap();
    assert!(!json.contains(SENTINEL), "the roll carries a value: {json}");
    assert!(
        json.contains("{{secret:DB_PASSWORD}}"),
        "the reference is what it carries instead: {json}"
    );

    // And through the writer, which is the file that lands on disk.
    let path = dir.path().join("flock.json");
    write_atomic(&path, &roll).unwrap();
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        !on_disk.contains(SENTINEL),
        "the roll on disk carries a value: {on_disk}"
    );
}

#[test]
fn debug_does_not_leak_env_values() {
    // The roll carries env, and its Debug lands in daemon logs.
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("DATABASE_URL".to_string(), "postgres://secret".to_string());
    let roll = roll_of(vec![app]);
    let rendered = format!("{roll:?}");
    assert!(!rendered.contains("postgres://secret"), "{rendered}");
    assert!(rendered.contains("<1 vars>"), "{rendered}");
}
