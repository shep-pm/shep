//! Blobs an older image wrote, and the keys they do not carry.
//!
//! Every case builds a current blob, removes one key from its JSON, and
//! asserts both that the key survives when it is there and that its absence
//! loads as the documented default. An `Option` field that stopped loading
//! from an absent key would force [`VERSION`](super::super::VERSION) up, so
//! these are the closest thing the handover has to a wire-compatibility
//! suite.

use std::time::SystemTime;

use shep_core::protocol::DogSource;
use shep_core::status::ProcStatus;

use super::{handover_over, sample_handover};
use crate::entry::{ProcessEntry, ReloadState};
use crate::handover::fixtures::{carried, entry_fixture, fds_at};
use crate::handover::{CarriedSheep, Handover};
use crate::supervisor::{
    CarriedReload, CommandOrigin, ManualKind, PendingManual, ReloadMode, ReloadPhase, ReloadSwap,
};
use crate::testing::app_with;

#[test]
fn a_blob_written_before_stdin_was_carried_still_loads() {
    let mut value = serde_json::to_value(sample_handover()).unwrap();
    let fds = value["sheep"][0]["fds"]
        .as_object_mut()
        .expect("a carried sheep names its descriptors");
    assert!(
        fds.remove("stdin").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(value).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].fds.stdin, None);
    assert_eq!(
        loaded.sheep[0].fds.out_pipe,
        sample_handover().sheep[0].fds.out_pipe,
        "the other five are unchanged by the one that was absent"
    );
}

#[test]
fn a_blob_written_before_the_channel_was_carried_still_loads() {
    let mut value = serde_json::to_value(sample_handover()).unwrap();
    let fds = value["sheep"][0]["fds"]
        .as_object_mut()
        .expect("a carried sheep names its descriptors");
    assert!(
        fds.remove("channel").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(value).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].fds.channel, None);
    assert_eq!(
        loaded.sheep[0].fds.stdin,
        sample_handover().sheep[0].fds.stdin,
        "the other five are unchanged by the one that was absent"
    );
}

#[test]
fn a_blob_written_before_pending_delete_was_carried_still_loads() {
    let mut value = serde_json::to_value(sample_handover()).unwrap();
    let sheep = value["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    assert!(
        sheep.remove("pending_delete").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(value).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].pending_delete(), None);
    assert_eq!(
        loaded.sheep[0].id(),
        sample_handover().sheep[0].id(),
        "the rest of the row is unchanged by the one field that was absent"
    );
}

#[test]
fn a_sheep_with_nothing_parked_carries_neither_parking_key() {
    let entry = entry_fixture(|_| {});
    assert!(
        entry.pending.is_none(),
        "fixture check: this case is about a sheep with nothing parked"
    );
    let blob = handover_over(&entry);

    let round_tripped = Handover::load_value(serde_json::to_value(&blob).unwrap())
        .expect("a blob this daemon wrote must load");
    assert_eq!(round_tripped.sheep[0].pending(), None);
    assert_eq!(
        round_tripped.sheep[0].pending_reidentifies(),
        None,
        "a reset flag with no parked config to apply it to is a key that means nothing"
    );
}

#[test]
fn a_blob_written_before_pending_was_carried_still_loads() {
    let mut entry = entry_fixture(|_| {});
    entry.pending = Some(app_with("web", |app| {
        app.env.insert("MODE".to_owned(), "blue".to_owned());
    }));
    entry.pending_reidentifies = true;
    let blob = handover_over(&entry);

    // A blob that has the two keys carries both.
    let round_tripped = Handover::load_value(serde_json::to_value(&blob).unwrap())
        .expect("a blob this daemon wrote must load");
    assert_eq!(
        round_tripped.sheep[0]
            .pending()
            .expect("the parked config crosses the exec")
            .env
            .get("MODE")
            .map(String::as_str),
        Some("blue")
    );
    assert_eq!(
        round_tripped.sheep[0].pending_reidentifies(),
        Some(true),
        "and its reset flag crosses with it: a config that arrived without one would \
         promote on the identity the flag exists to replace"
    );

    // An older blob, which has neither key.
    let mut value = serde_json::to_value(&blob).unwrap();
    let sheep = value["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    for key in ["pending", "pending_reidentifies"] {
        assert!(
            sheep.remove(key).is_some(),
            "the field this case removes must be there to remove: {key}"
        );
    }

    let loaded = Handover::load_value(value).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].pending(), None);
    assert_eq!(loaded.sheep[0].pending_reidentifies(), None);
    assert_eq!(
        loaded.sheep[0].id(),
        blob.sheep[0].id(),
        "the rest of the row is unchanged by the two fields that were absent"
    );
}

#[test]
fn a_blob_written_before_a_manual_marker_was_carried_still_loads() {
    let marker = PendingManual {
        kind: ManualKind::Restart,
        origin: CommandOrigin::Automatic,
    };
    let mut blob = sample_handover();
    blob.sheep[0] = CarriedSheep::from_entry(
        &entry_fixture(|_| {}),
        7,
        fds_at(11),
        false,
        Some(marker),
        false,
        None,
    );
    let value = serde_json::to_value(&blob).unwrap();

    assert_eq!(
        Handover::load_value(value.clone())
            .expect("a current blob loads")
            .sheep[0]
            .manual(),
        Some(marker),
        "a marker on the wire must come back whole, kind and origin both"
    );

    let mut older = value;
    let sheep = older["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    assert!(
        sheep.remove("manual").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(older).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].manual(), None);
    assert_eq!(
        loaded.sheep[0].id(),
        blob.sheep[0].id(),
        "the rest of the row is unchanged by the one field that was absent"
    );
}

/// Both keys an older blob lacks: `sheep[].reload` and the top-level
/// `reloads`. The job is asserted as well as the marker.
#[test]
fn a_blob_written_before_a_swap_was_carried_still_loads() {
    let job = CarriedReload {
        app: "web".to_owned(),
        queue: vec![11, 12],
        mode: ReloadMode::Serial,
        swap: ReloadSwap {
            old_id: 1,
            new_id: Some(9),
            phase: ReloadPhase::AwaitReady,
        },
    };
    let mut blob = sample_handover();
    let mut drainee = entry_fixture(|_| {});
    drainee.reload = ReloadState::Drainee { new_id: Some(9) };
    blob.sheep[0] = carried(&drainee);
    blob.reloads = Some(vec![job.clone()]);
    let value = serde_json::to_value(&blob).unwrap();

    let current = Handover::load_value(value.clone()).expect("a current blob loads");
    assert_eq!(
        current.sheep[0].reload(),
        Some(ReloadState::Drainee { new_id: Some(9) }),
        "the marker on the wire must come back whole, role and linked id both"
    );
    assert_eq!(
        current.reloads(),
        &[job],
        "the job must come back whole: queue, mode and every field of the swap"
    );

    let mut older = value;
    let object = older.as_object_mut().expect("a blob is an object");
    assert!(
        object.remove("reloads").is_some(),
        "the field this case removes must be there to remove"
    );
    let sheep = older["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    assert!(
        sheep.remove("reload").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(older).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].reload(), None);
    assert!(loaded.reloads().is_empty());
    assert_eq!(
        loaded.sheep[0].id(),
        blob.sheep[0].id(),
        "the rest of the row is unchanged by the two fields that were absent"
    );
}

#[test]
fn a_blob_written_before_ready_failed_was_carried_still_loads() {
    let mut blob = sample_handover();
    blob.sheep[0] = CarriedSheep::from_entry(
        &entry_fixture(|_| {}),
        7,
        fds_at(11),
        false,
        None,
        true,
        None,
    );
    let value = serde_json::to_value(&blob).unwrap();

    assert_eq!(
        Handover::load_value(value.clone())
            .expect("a current blob loads")
            .sheep[0]
            .ready_failed(),
        Some(true),
        "a verdict on the wire must come back as one, or the rollback it keeps reachable \
         cannot reach anything"
    );

    let mut older = value;
    let sheep = older["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    assert!(
        sheep.remove("ready_failed").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(older).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].ready_failed(), None);
    assert_eq!(
        loaded.sheep[0].id(),
        blob.sheep[0].id(),
        "the rest of the row is unchanged by the one field that was absent"
    );
}

/// The whole `DogSource`, not a boolean: [`CarriedSheep::dog`] says what
/// the marker is read for.
#[test]
fn a_dogs_marker_crosses_the_blob() {
    let mut entry = entry_fixture(|_| {});
    entry.dog = Some(DogSource::Adopted {
        path: "/opt/bin/shep-log-rotate".to_string(),
    });
    let mut blob = sample_handover();
    blob.sheep[0] = CarriedSheep::from_entry(&entry, 7, fds_at(11), false, None, false, None);

    let loaded = Handover::load_value(serde_json::to_value(&blob).unwrap())
        .expect("a current blob loads");

    assert_eq!(
        loaded.sheep[0].dog(),
        Some(&DogSource::Adopted {
            path: "/opt/bin/shep-log-rotate".to_string(),
        }),
        "a dog that crossed the exec as an ordinary sheep is one `shep dogs` has lost"
    );
}

/// Not implied by the case above: a `from_entry` that hardcoded a source
/// would pass that one and turn every carried app into a dog.
#[test]
fn a_plain_sheep_crosses_the_blob_without_one() {
    let mut blob = sample_handover();
    blob.sheep[0] = CarriedSheep::from_entry(
        &entry_fixture(|_| {}),
        7,
        fds_at(11),
        false,
        None,
        false,
        None,
    );

    let loaded = Handover::load_value(serde_json::to_value(&blob).unwrap())
        .expect("a current blob loads");

    assert_eq!(loaded.sheep[0].dog(), None);
}

#[test]
fn a_blob_written_before_a_dog_was_carried_still_loads() {
    let mut entry = entry_fixture(|_| {});
    entry.dog = Some(DogSource::BuiltIn);
    let mut blob = sample_handover();
    blob.sheep[0] = CarriedSheep::from_entry(&entry, 7, fds_at(11), false, None, false, None);
    let value = serde_json::to_value(&blob).unwrap();

    assert_eq!(
        Handover::load_value(value.clone())
            .expect("a current blob loads")
            .sheep[0]
            .dog(),
        Some(&DogSource::BuiltIn),
        "a marker on the wire must come back as one"
    );

    let mut older = value;
    let sheep = older["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    assert!(
        sheep.remove("dog").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(older).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].dog(), None);
    assert_eq!(
        loaded.sheep[0].id(),
        blob.sheep[0].id(),
        "the rest of the row is unchanged by the one field that was absent"
    );
}

/// One entry owed a respawn, which is the only status the deadline is
/// carried for.
fn owed_a_restart() -> ProcessEntry {
    let mut entry = entry_fixture(|_| {});
    entry.status = ProcStatus::WaitingRestart;
    entry.pid = None;
    entry
}

/// A whole second of slack on the round trip: this pins the value rather
/// than the precision.
#[test]
fn a_blob_written_before_a_restart_deadline_was_carried_still_loads() {
    let due = SystemTime::now() + core::time::Duration::from_secs(600);
    let mut blob = sample_handover();
    blob.sheep[0] = CarriedSheep::from_entry(
        &owed_a_restart(),
        7,
        fds_at(11),
        false,
        None,
        false,
        Some(due),
    );
    let value = serde_json::to_value(&blob).unwrap();

    let carried = Handover::load_value(value.clone())
        .expect("a current blob loads")
        .sheep[0]
        .restart_due()
        .expect("a deadline on the wire must come back as one");
    let drift = carried
        .duration_since(due)
        .or_else(|_| due.duration_since(carried))
        .unwrap();
    assert!(
        drift < core::time::Duration::from_secs(1),
        "a moment that does not survive the wire is a moment the successor cannot re-arm \
         from: {drift:?} of drift"
    );

    let mut older = value;
    let sheep = older["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    assert!(
        sheep.remove("restart_due").is_some(),
        "the field this case removes must be there to remove"
    );

    let loaded = Handover::load_value(older).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].restart_due(), None);
    assert_eq!(
        loaded.sheep[0].id(),
        blob.sheep[0].id(),
        "the rest of the row is unchanged by the one field that was absent"
    );
}

/// Both halves, since a gate that dropped the field unconditionally would
/// pass the second assertion on its own.
#[test]
fn a_deadline_is_carried_only_for_a_sheep_owed_a_respawn() {
    let due = SystemTime::now() + core::time::Duration::from_secs(600);

    let waiting = CarriedSheep::from_entry(
        &owed_a_restart(),
        7,
        fds_at(11),
        false,
        None,
        false,
        Some(due),
    );
    assert!(
        waiting.restart_due().is_some(),
        "a sheep that IS owed a respawn must carry its deadline, or there is nothing to gate"
    );

    let online = CarriedSheep::from_entry(
        &entry_fixture(|_| {}),
        7,
        fds_at(11),
        false,
        None,
        false,
        Some(due),
    );
    assert_eq!(
        online.restart_due(),
        None,
        "a running sheep must not carry a deadline left over from an earlier exit"
    );
}
