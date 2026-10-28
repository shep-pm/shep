//! Blobs committed as the bytes a named release wrote.
//!
//! Every file under `crates/shep-daemon/fixtures/handover/` is the output of
//! that release's own serialiser, run against its own `sample_handover()`,
//! copied here unchanged. Nothing regenerates them from this daemon's
//! [`Handover`]: a fixture built here would follow a field rename, and the
//! whole point of these is that they cannot.
//!
//! Each test loads one file through [`Handover::load_value`] and asserts the
//! whole key set that release wrote at every level the shape policy counts.

use std::os::fd::RawFd;

use shep_core::status::ProcStatus;

use crate::entry::ReloadState;
use crate::handover::{Counters, Handover};
use crate::privilege::SpawnIdentity;

use super::old_blob_shape::{
    V0_1_18, V0_1_20, V0_1_21, V0_1_22, V0_1_23, V0_1_30, V0_5_0, V0_6_2, V0_8_0, V0_11_0, V0_12_0,
    V0_13_0, assert_the_shape, fixture_path, fixture_value, fixtures_dir,
};

/// Every fixture, oldest first, with the test that loads it. Each tag is the
/// first `shep-daemon-v*` tag that wrote the shape its file holds.
///
/// The pairing is the guard, not a table of convenience: a per-fixture test
/// deleted from this file breaks it at compile time, so the guard below cannot
/// pass with a committed blob nothing loads. Each entry names its own test, and
/// each test reads only the fixture its tag names.
const LOADERS: [(&str, fn()); 12] = [
    (V0_1_18, a_blob_from_shep_daemon_v0_1_18_still_loads),
    (V0_1_20, a_blob_from_shep_daemon_v0_1_20_still_loads),
    (V0_1_21, a_blob_from_shep_daemon_v0_1_21_still_loads),
    (V0_1_22, a_blob_from_shep_daemon_v0_1_22_still_loads),
    (V0_1_23, a_blob_from_shep_daemon_v0_1_23_still_loads),
    (V0_1_30, a_blob_from_shep_daemon_v0_1_30_still_loads),
    (V0_5_0, a_blob_from_shep_daemon_v0_5_0_still_loads),
    (V0_6_2, a_blob_from_shep_daemon_v0_6_2_still_loads),
    (V0_8_0, a_blob_from_shep_daemon_v0_8_0_still_loads),
    (V0_11_0, a_blob_from_shep_daemon_v0_11_0_still_loads),
    (V0_12_0, a_blob_from_shep_daemon_v0_12_0_still_loads),
    (V0_13_0, a_blob_from_shep_daemon_v0_13_0_still_loads),
];

/// What every one of these blobs says, unchanged from the first to the last:
/// the plain `web` sheep `entry_fixture` builds, the daemon's two descriptor
/// numbers, and the three counters a successor must not reissue.
fn assert_the_plain_flock(blob: &Handover) {
    assert_eq!(
        blob.version, 1,
        "every release in this set wrote format version 1"
    );
    assert_eq!(blob.sheep.len(), 1, "one sheep: the fixture's `web` app");
    let sheep = &blob.sheep[0];
    assert_eq!(sheep.id(), 1);
    assert_eq!(sheep.name(), "web");
    assert_eq!(sheep.instance(), 0);
    assert_eq!(sheep.pid(), Some(100));
    assert_eq!(sheep.restarts(), 0);
    assert_eq!(sheep.epoch(), 7);
    assert_eq!(sheep.status(), ProcStatus::Online);
    assert_eq!(sheep.last_exit(), None);
    assert_eq!(sheep.credentials(), SpawnIdentity::Resolved(None));
    assert_eq!(sheep.app().name, "web");
    assert_eq!(sheep.app().script, "./srv");
    assert_eq!(
        blob.listener_fd, 3,
        "the control listener descriptor every one of these samples carried"
    );
    assert_eq!(
        blob.pidfile_fd, 4,
        "the pidfile lock descriptor every one of these samples carried"
    );
    assert_eq!(
        blob.counters(),
        Counters {
            next_id: 9,
            next_deadline: 5,
            next_action_stamp: 2,
        }
    );
}

/// The four descriptors every release in this set carried, plus the two a
/// release from `stdin` onwards carried beside them.
fn assert_the_descriptors(blob: &Handover, stdin: Option<RawFd>, channel: Option<RawFd>) {
    let fds = blob.sheep[0].fds();
    assert_eq!(fds.out_pipe, Some(11));
    assert_eq!(fds.err_pipe, Some(12));
    assert_eq!(fds.out_log, Some(13));
    assert_eq!(fds.err_log, Some(14));
    assert_eq!(fds.stdin, stdin);
    assert_eq!(fds.channel, channel);
}

/// Every marker a pre-v0.1.21 release never wrote, read the way a successor
/// reads it: through the accessor, as `None`.
fn assert_nothing_marked(blob: &Handover) {
    let sheep = &blob.sheep[0];
    assert_eq!(sheep.pending_delete(), None);
    assert_eq!(sheep.manual(), None);
    assert_eq!(sheep.reload(), None);
    assert_eq!(sheep.ready_failed(), None);
    assert_eq!(sheep.restart_due(), None);
    assert_eq!(sheep.dog(), None);
    assert_eq!(sheep.pending(), None);
    assert_eq!(sheep.pending_reidentifies(), None);
    assert_eq!(sheep.disabled(), None);
    assert!(blob.reloads().is_empty());
}

/// The five keys v0.1.21 added, each carrying its own "nothing is happening
/// to this sheep". A successor that read them as absent instead of as these
/// would respawn a drainee into its own slot and forget a delete was asked for.
/// [`assert_the_shape`] pins that the keys are there at all; this reads what
/// they say.
fn assert_the_plain_markers(blob: &Handover) {
    let sheep = &blob.sheep[0];
    assert_eq!(sheep.pending_delete(), Some(false));
    assert_eq!(
        sheep.manual(),
        None,
        "the key is there and says no command owns the exit"
    );
    assert_eq!(sheep.reload(), Some(ReloadState::None));
    assert_eq!(sheep.ready_failed(), Some(false));
    assert!(
        blob.reloads().is_empty(),
        "`reloads` is there and empty, which is the same answer as absent"
    );
}

#[test]
fn a_blob_from_shep_daemon_v0_1_18_still_loads() {
    let raw = fixture_value(V0_1_18);
    let blob = Handover::load_value(raw.clone())
        .expect("the oldest blob on disk must still load, or a successor refuses a flock");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_1_18, 1);

    assert_the_descriptors(&blob, None, None);
    assert_nothing_marked(&blob);

    // The config this release carried, and the keys later releases added to
    // it. `increment_var` is one this image has no field for at all: an
    // unknown key is dropped, not refused.
    let app = &raw["sheep"][0]["app"];
    assert!(app.get("increment_var").is_some());
    for added in [
        "depends_on",
        "environment",
        "level_rules",
        "dogs",
        "log_timestamps",
    ] {
        assert!(app.get(added).is_none(), "{added} is a later release's key");
    }
    assert!(blob.sheep[0].app().depends_on.is_empty());
    assert_eq!(blob.sheep[0].app().environment, None);
    assert!(blob.sheep[0].app().level_rules.is_empty());
    assert!(blob.sheep[0].app().dogs.is_empty());
}

#[test]
fn a_blob_from_shep_daemon_v0_1_20_still_loads() {
    let blob = Handover::load_value(fixture_value(V0_1_20)).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_1_20, 1);

    // The two descriptors this release added, carried whole.
    assert_the_descriptors(&blob, Some(15), Some(16));
    assert_nothing_marked(&blob);
}

#[test]
fn a_blob_from_shep_daemon_v0_1_21_still_loads() {
    let blob = Handover::load_value(fixture_value(V0_1_21)).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_1_21, 1);

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));

    // What it predates.
    let sheep = &blob.sheep[0];
    assert_eq!(sheep.restart_due(), None);
    assert_eq!(sheep.dog(), None);
    assert_eq!(sheep.pending(), None);
    assert_eq!(sheep.pending_reidentifies(), None);
    assert_eq!(sheep.disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_1_22_still_loads() {
    let blob = Handover::load_value(fixture_value(V0_1_22)).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_1_22, 1);

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));

    // The key this release added, carrying that an `Online` sheep is owed no
    // respawn.
    let sheep = &blob.sheep[0];
    assert_eq!(sheep.restart_due(), None);
    assert_eq!(sheep.dog(), None);
    assert_eq!(sheep.pending(), None);
    assert_eq!(sheep.pending_reidentifies(), None);
    assert_eq!(sheep.disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_1_23_still_loads() {
    let blob = Handover::load_value(fixture_value(V0_1_23)).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_1_23, 1);

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));

    let sheep = &blob.sheep[0];
    assert_eq!(sheep.restart_due(), None);
    // The marker this release added: an ordinary sheep, not a dog.
    assert_eq!(sheep.dog(), None);
    assert_eq!(sheep.pending(), None);
    assert_eq!(sheep.pending_reidentifies(), None);
    assert_eq!(sheep.disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_1_30_still_loads() {
    let blob = Handover::load_value(fixture_value(V0_1_30)).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_1_30, 1);

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));

    let sheep = &blob.sheep[0];
    assert_eq!(sheep.restart_due(), None);
    assert_eq!(sheep.dog(), None);
    // The pair this release added, written together and here both empty:
    // nothing is parked for this sheep.
    assert_eq!(sheep.pending(), None);
    assert_eq!(sheep.pending_reidentifies(), None);
    assert_eq!(sheep.disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_5_0_still_loads() {
    let raw = fixture_value(V0_5_0);
    let blob = Handover::load_value(raw.clone()).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_5_0, 1);

    // The key this release added to the config a sheep carries.
    assert_eq!(raw["sheep"][0]["app"]["depends_on"], serde_json::json!([]));
    assert!(blob.sheep[0].app().depends_on.is_empty());

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));
    assert_eq!(blob.sheep[0].dog(), None);
    assert_eq!(blob.sheep[0].pending(), None);
    assert_eq!(blob.sheep[0].disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_6_2_still_loads() {
    let raw = fixture_value(V0_6_2);
    let blob = Handover::load_value(raw.clone()).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_6_2, 1);

    // The key this release added, carrying no environment name.
    assert_eq!(
        raw["sheep"][0]["app"]["environment"],
        serde_json::Value::Null
    );
    assert_eq!(blob.sheep[0].app().environment, None);

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));
    assert_eq!(blob.sheep[0].pending(), None);
    assert_eq!(blob.sheep[0].disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_8_0_still_loads() {
    let raw = fixture_value(V0_8_0);
    let blob = Handover::load_value(raw.clone()).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_8_0, 1);

    // The key this release added, and the one it dropped. A blob from the
    // release before still carries `increment_var`, which eight fixtures from
    // v0.1.18 through v0.6.2 hold.
    let app = &raw["sheep"][0]["app"];
    assert_eq!(app["level_rules"], serde_json::json!([]));
    assert!(app.get("increment_var").is_none());
    assert!(blob.sheep[0].app().level_rules.is_empty());

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));
    assert_eq!(blob.sheep[0].pending(), None);
    assert_eq!(blob.sheep[0].disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_11_0_still_loads() {
    let raw = fixture_value(V0_11_0);
    let blob = Handover::load_value(raw.clone()).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_11_0, 1);

    // The key this release added: an app with no dog table.
    assert_eq!(raw["sheep"][0]["app"]["dogs"], serde_json::json!({}));
    assert!(blob.sheep[0].app().dogs.is_empty());

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));
    assert_eq!(blob.sheep[0].dog(), None);
    assert_eq!(blob.sheep[0].disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_12_0_still_loads() {
    let raw = fixture_value(V0_12_0);
    let blob = Handover::load_value(raw.clone()).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_12_0, 1);

    // The key this release added, carrying the stamp-on default it wrote.
    assert_eq!(
        raw["sheep"][0]["app"]["log_timestamps"],
        serde_json::json!(true)
    );
    assert!(blob.sheep[0].app().log_timestamps);

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));
    assert_eq!(blob.sheep[0].pending(), None);
    assert_eq!(blob.sheep[0].disabled(), None);
}

#[test]
fn a_blob_from_shep_daemon_v0_13_0_still_loads() {
    let blob = Handover::load_value(fixture_value(V0_13_0)).expect("an older blob must still load");
    assert_the_plain_flock(&blob);

    assert_the_shape(V0_13_0, 1);

    // The key this release added, carrying that nothing is disabled.
    assert_eq!(blob.sheep[0].disabled(), None);

    assert_the_plain_markers(&blob);
    assert_the_descriptors(&blob, Some(15), Some(16));
    assert_eq!(blob.sheep[0].pending(), None);
}

/// The legacy arm of the boot: a predecessor from before the envelope left
/// its blob at `$SHEP_HOME/run/handover.json`, and
/// read it through [`Handover::read`]. The oldest fixture is the blob that
/// path is for.
#[test]
fn the_oldest_blob_loads_through_the_legacy_read_path() {
    let dir = tempfile::tempdir().expect("a temp home for the legacy blob");
    let written = dir.path().join("handover.json");
    std::fs::write(
        &written,
        std::fs::read(fixture_path(V0_1_18)).expect("the fixture is readable"),
    )
    .expect("the legacy blob is writable");

    let blob = Handover::read(&written).expect("the legacy arm reads what v0.1.18 wrote");
    assert_the_plain_flock(&blob);
    assert_the_descriptors(&blob, None, None);
    assert_nothing_marked(&blob);
}

/// Which fixtures carry `increment_var`, the `AppConfig` key v0.8.0 dropped
/// and this image has no field for at all. Every blob from v0.1.18 through
/// v0.7.4 wrote it, so eight of these twelve hold it, as `null`. The list is
/// here rather than only in `fixtures/handover/README.md` because swapping one
/// fixture for another release's bytes is otherwise silent.
#[test]
fn the_dropped_app_config_key_is_in_the_fixtures_that_wrote_it() {
    const WROTE_IT: [&str; 8] = [
        V0_1_18, V0_1_20, V0_1_21, V0_1_22, V0_1_23, V0_1_30, V0_5_0, V0_6_2,
    ];
    for (tag, _) in LOADERS {
        let app = &fixture_value(tag)["sheep"][0]["app"];
        assert_eq!(
            app.get("increment_var").is_some(),
            WROTE_IT.contains(&tag),
            "{tag} and this file disagree about who wrote `increment_var`"
        );
    }
}

/// The fixtures on disk and the tags [`LOADERS`] and
/// [`POPULATED_FIXTURES`](super::old_blob_state::POPULATED_FIXTURES) pair with
/// tests are one set. A tag with no test is a compile error in those tables; a
/// tag with no file is a claim about bytes that do not exist, which is what
/// this case catches.
#[test]
fn every_fixture_has_a_test_and_every_test_has_a_fixture() {
    let mut on_disk: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("the fixture directory is readable")
        .map(|entry| entry.expect("a fixture entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            path.file_stem()
                .expect("a fixture is named")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    on_disk.sort();

    let mut named: Vec<String> = LOADERS
        .iter()
        .map(|(tag, _)| (*tag).to_owned())
        .chain(
            super::old_blob_state::POPULATED_FIXTURES
                .iter()
                .map(|(stem, _)| (*stem).to_owned()),
        )
        .collect();
    named.sort();

    assert_eq!(
        on_disk, named,
        "fixtures/handover and these files' lists of fixtures have drifted apart"
    );
}

/// A fixture is only worth what its bytes are, and those bytes are sample
/// data. A home path or a hostname from the machine that dumped one is a
/// record of that machine, not of the release.
#[test]
fn no_fixture_carries_the_machine_that_dumped_it() {
    let stems: Vec<String> = LOADERS
        .iter()
        .map(|(tag, _)| (*tag).to_owned())
        .chain(
            super::old_blob_state::POPULATED_FIXTURES
                .iter()
                .map(|(stem, _)| (*stem).to_owned()),
        )
        .collect();
    for stem in stems {
        let text = std::fs::read_to_string(fixture_path(&stem)).expect("the fixture is readable");
        assert!(
            !text.contains("/Users") && !text.contains("/home"),
            "{stem} carries a home path from the machine that dumped it"
        );
        assert!(
            text.contains("\"name\":\"web\""),
            "{stem} is not the `web` app the handover fixtures build"
        );
    }
}
