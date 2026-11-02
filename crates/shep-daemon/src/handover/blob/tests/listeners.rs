//! The `listen` sockets a blob carries: their order on disk, and the blob a
//! flock without any leaves unchanged.

use super::sample_handover;
use crate::handover::HeldListener;
use std::os::fd::OwnedFd;

fn held(sheep: &str, name: &str) -> HeldListener {
    HeldListener {
        sheep: sheep.to_owned(),
        name: name.to_owned(),
        address: "127.0.0.1:8080".to_owned(),
        fd: OwnedFd::from(tempfile::tempfile().unwrap()),
    }
}

#[test]
fn listeners_are_written_by_sheep_then_name_and_round_trip() {
    let mut blob = sample_handover();

    blob.carry_listeners(&[
        held("web", "metrics"),
        held("api", "http"),
        held("web", "http"),
    ]);

    let order: Vec<_> = blob
        .listeners()
        .iter()
        .map(|l| (l.sheep(), l.name()))
        .collect();
    assert_eq!(
        order,
        [("api", "http"), ("web", "http"), ("web", "metrics")]
    );
    let back: super::Handover =
        serde_json::from_str(&serde_json::to_string(&blob).unwrap()).unwrap();
    assert_eq!(back, blob);
}

#[test]
fn a_flock_without_listeners_writes_no_key() {
    let mut blob = sample_handover();

    blob.carry_listeners(&[]);

    let json = serde_json::to_value(&blob).unwrap();
    assert!(json.get("listeners").is_none(), "{json}");
    assert!(blob.listeners().is_empty());
}
