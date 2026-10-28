//! A saved sheep keeps its name when a dog wants it
//!
//! Listing the built-in dogs at rest must never cost anyone a sheep, so on
//! an upgrade a roll's sheep named `metrics` stays, whatever the dog's
//! standing, and boot says which of the two to rename.
//!
//! `#[test]` with a `block_on` of its own, not `#[tokio::test]`, because all
//! three go through [`boot_blocking`]: it captures the logs through a
//! synchronous closure, which needs a runtime it can drive itself.

use super::boot_blocking;
use crate::boot::*;
use crate::dogs::DogSpec;
use crate::fake::ProcScript;
use shep_core::config::AppConfig;
use shep_core::protocol::{DogSource, ProcessInfo};
use std::collections::BTreeMap;

fn built_in(name: &str) -> DogSpec {
    DogSpec {
        name: name.to_string(),
        source: DogSource::BuiltIn { grants: None },
        env: BTreeMap::new(),
    }
}

/// Boots over a roll holding one sheep named `metrics`, and returns the
/// flock with everything the boot logged.
///
/// Restores, since every case here is about a saved sheep: which of the two
/// the boot tells the operator to rename is only visible once the roll is
/// back.
fn boot_over_a_saved_metrics(
    scripts: Vec<ProcScript>,
    options: BootOptions,
) -> (Vec<ProcessInfo>, String) {
    boot_blocking(
        vec![AppConfig::minimal("metrics", "./srv")],
        scripts,
        BootOptions {
            restore: true,
            ..options
        },
    )
}

#[test]
fn a_saved_sheep_named_after_a_listed_built_in_survives_and_the_built_in_is_not_registered() {
    let (flock, logs) = boot_over_a_saved_metrics(
        vec![ProcScript::never_exits()],
        BootOptions {
            known_dogs: vec![built_in("metrics"), built_in("bark")],
            ..BootOptions::default()
        },
    );

    let held: Vec<&ProcessInfo> = flock.iter().filter(|info| info.name == "metrics").collect();
    assert_eq!(held.len(), 1, "one name is one entry: {flock:?}");
    assert!(held[0].dog.is_none(), "the sheep keeps the name: {flock:?}");
    assert!(
        flock
            .iter()
            .any(|info| info.name == "bark" && info.dog.is_some()),
        "the other built-in is still listed: {flock:?}"
    );
    assert!(
        logs.contains("WARN") && logs.contains("rename"),
        "the warning must say to rename one: {logs:?}"
    );
}

#[test]
fn a_saved_sheep_named_after_an_enabled_built_in_survives_and_the_built_in_does_not_start() {
    let (flock, logs) = boot_over_a_saved_metrics(
        // One script: the sheep's. The dog must not consume another.
        vec![ProcScript::never_exits()],
        BootOptions {
            dogs: vec![built_in("metrics")],
            known_dogs: vec![built_in("metrics")],
            ..BootOptions::default()
        },
    );

    assert_eq!(flock.len(), 1, "{flock:?}");
    assert!(flock[0].dog.is_none(), "{flock:?}");
    assert!(logs.contains("rename"), "{logs:?}");
}

#[test]
fn a_boot_first_dog_colliding_with_a_saved_sheep_keeps_the_sheep() {
    let (flock, logs) = boot_over_a_saved_metrics(
        // One script: the sheep's. The promoted dog must not take it.
        vec![ProcScript::never_exits()],
        BootOptions {
            dogs: vec![built_in("metrics")],
            known_dogs: vec![built_in("metrics")],
            boot_first_dogs: vec!["metrics".to_string()],
            ..BootOptions::default()
        },
    );

    assert_eq!(flock.len(), 1, "one name is one entry: {flock:?}");
    assert!(
        flock[0].dog.is_none(),
        "the sheep holds the name: {flock:?}"
    );
    assert_eq!(
        flock[0].status,
        shep_core::status::ProcStatus::Online,
        "the sheep was running when the roll was written: {flock:?}"
    );
    assert!(logs.contains("rename"), "{logs:?}");
}
