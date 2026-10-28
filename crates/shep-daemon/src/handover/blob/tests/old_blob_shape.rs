//! The key set each committed handover fixture must hold.
//!
//! `crates/shep-daemon/fixtures/handover/README.md` states the policy: a
//! shape is the set of keys a serialised blob holds, at the blob, at
//! `sheep[]`, at `sheep[].fds` and at `sheep[].app`. This file is that
//! policy as data, one [`Shape`] per release, and [`assert_the_shape`] checks
//! a fixture against it.
//!
//! A key set alone catches a key going missing and nothing else: an absent
//! key and a `null` read the same. The value half is in [`old_blob_state`](super::old_blob_state).

use std::path::PathBuf;

/// The oldest blob in the set: the first release that handed a flock over at
/// all. Four descriptors per sheep, and no marker of any kind.
pub(super) const V0_1_18: &str = "shep-daemon-v0.1.18";
/// Added `stdin` and `channel` to a sheep's descriptors.
pub(super) const V0_1_20: &str = "shep-daemon-v0.1.20";
/// Added `pending_delete`, `manual`, `reload`, `ready_failed` and `reloads`.
pub(super) const V0_1_21: &str = "shep-daemon-v0.1.21";
/// Added `restart_due`.
pub(super) const V0_1_22: &str = "shep-daemon-v0.1.22";
/// Added `dog`.
pub(super) const V0_1_23: &str = "shep-daemon-v0.1.23";
/// Added `pending` and `pending_reidentifies`.
pub(super) const V0_1_30: &str = "shep-daemon-v0.1.30";
/// Added `depends_on` to the carried `AppConfig`.
pub(super) const V0_5_0: &str = "shep-daemon-v0.5.0";
/// Added `environment` to the carried `AppConfig`.
pub(super) const V0_6_2: &str = "shep-daemon-v0.6.2";
/// Added `level_rules` to the carried `AppConfig`, and dropped `increment_var`.
pub(super) const V0_8_0: &str = "shep-daemon-v0.8.0";
/// Added `dogs` to the carried `AppConfig`.
pub(super) const V0_11_0: &str = "shep-daemon-v0.11.0";
/// Added `log_timestamps` to the carried `AppConfig`.
pub(super) const V0_12_0: &str = "shep-daemon-v0.12.0";
/// Added `disabled`.
pub(super) const V0_13_0: &str = "shep-daemon-v0.13.0";

/// The keys every blob in this set has at the top level, from the first
/// release to the one before `reloads`.
const BLOB_BEFORE_RELOADS: [&str; 7] = [
    "listener_fd",
    "next_action_stamp",
    "next_deadline",
    "next_id",
    "pidfile_fd",
    "sheep",
    "version",
];

/// The same, from the release that added `reloads`.
const BLOB_WITH_RELOADS: [&str; 8] = [
    "listener_fd",
    "next_action_stamp",
    "next_deadline",
    "next_id",
    "pidfile_fd",
    "reloads",
    "sheep",
    "version",
];

/// The keys a carried sheep has from the release that wrote no marker at all.
const SHEEP_BEFORE_MARKERS: [&str; 11] = [
    "app",
    "credentials",
    "epoch",
    "fds",
    "id",
    "instance",
    "last_exit",
    "name",
    "pid",
    "restarts",
    "status",
];

/// The keys a carried sheep holds beyond [`SHEEP_BEFORE_MARKERS`] from the
/// release that wrote the first markers. Cumulative: each list below names
/// every marker its release wrote.
const SHEEP_KEYS_FROM_V0_1_21: [&str; 4] = ["manual", "pending_delete", "ready_failed", "reload"];
/// The same, plus the respawn deadline.
const SHEEP_KEYS_FROM_V0_1_22: [&str; 5] = [
    "manual",
    "pending_delete",
    "ready_failed",
    "reload",
    "restart_due",
];
/// The same, plus the dog marker.
const SHEEP_KEYS_FROM_V0_1_23: [&str; 6] = [
    "manual",
    "pending_delete",
    "ready_failed",
    "reload",
    "restart_due",
    "dog",
];
/// The same, plus the pair `pending` and `pending_reidentifies`.
const SHEEP_KEYS_FROM_V0_1_30: [&str; 8] = [
    "manual",
    "pending_delete",
    "ready_failed",
    "reload",
    "restart_due",
    "dog",
    "pending",
    "pending_reidentifies",
];
/// The same, plus `disabled`.
const SHEEP_KEYS_FROM_V0_13_0: [&str; 9] = [
    "manual",
    "pending_delete",
    "ready_failed",
    "reload",
    "restart_due",
    "dog",
    "pending",
    "pending_reidentifies",
    "disabled",
];

/// The four descriptors a sheep's output travelled on before `stdin`.
const FOUR_DESCRIPTORS: [&str; 4] = ["err_log", "err_pipe", "out_log", "out_pipe"];

/// The six, from the release that carried a stdin pipe and a channel.
const SIX_DESCRIPTORS: [&str; 6] = [
    "channel", "err_log", "err_pipe", "out_log", "out_pipe", "stdin",
];

/// The keys the `AppConfig` a sheep carries had at `shep-daemon-v0.1.18`, the
/// oldest blob in the set. Every later release's config is this list plus the
/// keys it added, minus the one v0.8.0 dropped.
const APP_AT_V0_1_18: [&str; 40] = [
    "action_timeout",
    "args",
    "autorestart",
    "autostart",
    "channel",
    "cron_restart",
    "cron_timezone",
    "cwd",
    "env",
    "err_file",
    "exp_backoff_restart_delay",
    "fold",
    "graceful_timeout",
    "group",
    "ignore_watch",
    "increment_var",
    "instances",
    "interpreter",
    "kill_signal",
    "kill_timeout",
    "listen_timeout",
    "liveness_probe",
    "max_memory",
    "max_restarts",
    "merge_logs",
    "min_uptime",
    "name",
    "out_file",
    "readiness_probe",
    "restart_delay",
    "reuse_port",
    "script",
    "shutdown_with_message",
    "stdin",
    "stop_exit_codes",
    "user",
    "wait_ready",
    "watch",
    "watch_delay",
    "watch_options",
];

/// The keys the carried config holds beyond [`APP_AT_V0_1_18`], per release
/// stage. Cumulative, as the sheep lists above are.
const APP_KEYS_FROM_V0_5_0: [&str; 1] = ["depends_on"];
/// The same, plus the environment name.
const APP_KEYS_FROM_V0_6_2: [&str; 2] = ["depends_on", "environment"];
/// The same, plus the level rules.
const APP_KEYS_FROM_V0_8_0: [&str; 3] = ["depends_on", "environment", "level_rules"];
/// The same, plus the dog table.
const APP_KEYS_FROM_V0_11_0: [&str; 4] = ["depends_on", "environment", "level_rules", "dogs"];
/// The same, plus the log timestamp switch.
const APP_KEYS_FROM_V0_12_0: [&str; 5] = [
    "depends_on",
    "environment",
    "level_rules",
    "dogs",
    "log_timestamps",
];
/// The config key v0.8.0 stopped writing, and every release after it.
const APP_KEYS_DROPPED_AT_V0_8_0: [&str; 1] = ["increment_var"];

/// Where the committed bytes live.
pub(super) fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/handover")
}

/// Where a release's committed bytes live. A fixture is named for the tag that
/// wrote it, so `stem` is that tag, or the tag plus `-populated`.
pub(super) fn fixture_path(stem: &str) -> PathBuf {
    fixtures_dir().join(format!("{stem}.json"))
}

/// A fixture as raw JSON, so a test can name the keys it holds rather than
/// only the fields this image has a place for.
pub(super) fn fixture_value(stem: &str) -> serde_json::Value {
    let path = fixture_path(stem);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("the fixture {} is not readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("the fixture {} is not JSON: {error}", path.display()))
}

/// The keys one JSON object holds, sorted: the shape its writer painted at
/// that level of the blob.
fn keys(object: &serde_json::Value) -> Vec<String> {
    let mut found: Vec<String> = object
        .as_object()
        .expect("a blob, a carried sheep, its descriptors and its config are all objects")
        .keys()
        .cloned()
        .collect();
    found.sort_unstable();
    found
}

/// A literal list of key names, sorted the way [`keys`] sorts a real one.
fn sorted<T: AsRef<str>>(names: &[T]) -> Vec<String> {
    let mut found: Vec<String> = names.iter().map(|key| key.as_ref().to_owned()).collect();
    found.sort_unstable();
    found
}

/// `base`'s keys plus the ones `added` names, sorted: the shape a later release
/// wrote, read off the earlier shape this file pins as a literal.
fn sorted_plus<T: AsRef<str>>(base: &[T], added: &[&str]) -> Vec<String> {
    let mut found = sorted(base);
    found.extend(added.iter().map(|key| (*key).to_owned()));
    found.sort_unstable();
    found
}

/// The config keys a release wrote: [`APP_AT_V0_1_18`], plus the ones it added,
/// minus the ones it stopped writing.
fn app_shape(added: &[&str], dropped: &[&str]) -> Vec<String> {
    let mut found = sorted(&APP_AT_V0_1_18);
    found.extend(added.iter().map(|key| (*key).to_owned()));
    found.retain(|key| !dropped.contains(&key.as_str()));
    found.sort_unstable();
    found
}

/// What one release wrote at every level the policy in
/// `fixtures/handover/README.md` counts as part of a shape.
struct Shape {
    /// The keys the blob holds at its own level.
    blob: &'static [&'static str],
    /// The keys a carried sheep holds beyond [`SHEEP_BEFORE_MARKERS`].
    sheep: &'static [&'static str],
    /// The descriptors a carried sheep carries.
    fds: &'static [&'static str],
    /// The config keys it holds beyond [`APP_AT_V0_1_18`].
    app_added: &'static [&'static str],
    /// The config keys it no longer holds.
    app_dropped: &'static [&'static str],
}

impl Shape {
    /// One release's shape, each level named as the keys beyond the level's
    /// own base list.
    const fn at(
        blob: &'static [&'static str],
        sheep: &'static [&'static str],
        fds: &'static [&'static str],
        app_added: &'static [&'static str],
        app_dropped: &'static [&'static str],
    ) -> Self {
        Self {
            blob,
            sheep,
            fds,
            app_added,
            app_dropped,
        }
    }
}

/// Each committed fixture's shape, oldest first. Every fixture in
/// `fixtures/handover/` has an entry here, which the guard in
/// [`old_blobs`](super::old_blobs) checks against the files on disk.
const SHAPES: [(&str, Shape); 12] = [
    (
        V0_1_18,
        Shape::at(&BLOB_BEFORE_RELOADS, &[], &FOUR_DESCRIPTORS, &[], &[]),
    ),
    (
        V0_1_20,
        Shape::at(&BLOB_BEFORE_RELOADS, &[], &SIX_DESCRIPTORS, &[], &[]),
    ),
    (
        V0_1_21,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_21,
            &SIX_DESCRIPTORS,
            &[],
            &[],
        ),
    ),
    (
        V0_1_22,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_22,
            &SIX_DESCRIPTORS,
            &[],
            &[],
        ),
    ),
    (
        V0_1_23,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_23,
            &SIX_DESCRIPTORS,
            &[],
            &[],
        ),
    ),
    (
        V0_1_30,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_30,
            &SIX_DESCRIPTORS,
            &[],
            &[],
        ),
    ),
    (
        V0_5_0,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_30,
            &SIX_DESCRIPTORS,
            &APP_KEYS_FROM_V0_5_0,
            &[],
        ),
    ),
    (
        V0_6_2,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_30,
            &SIX_DESCRIPTORS,
            &APP_KEYS_FROM_V0_6_2,
            &[],
        ),
    ),
    (
        V0_8_0,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_30,
            &SIX_DESCRIPTORS,
            &APP_KEYS_FROM_V0_8_0,
            &APP_KEYS_DROPPED_AT_V0_8_0,
        ),
    ),
    (
        V0_11_0,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_30,
            &SIX_DESCRIPTORS,
            &APP_KEYS_FROM_V0_11_0,
            &APP_KEYS_DROPPED_AT_V0_8_0,
        ),
    ),
    (
        V0_12_0,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_1_30,
            &SIX_DESCRIPTORS,
            &APP_KEYS_FROM_V0_12_0,
            &APP_KEYS_DROPPED_AT_V0_8_0,
        ),
    ),
    (
        V0_13_0,
        Shape::at(
            &BLOB_WITH_RELOADS,
            &SHEEP_KEYS_FROM_V0_13_0,
            &SIX_DESCRIPTORS,
            &APP_KEYS_FROM_V0_12_0,
            &APP_KEYS_DROPPED_AT_V0_8_0,
        ),
    ),
];

/// Assert a committed fixture holds exactly the key set the release it is
/// named for wrote, at the blob, at every sheep, at each sheep's descriptors
/// and at its config. `stem` is the fixture's file name without extension,
/// and `sheep_count` how many sheep that fixture carries.
/// # Panics
/// Panics if `stem` names no fixture in [`SHAPES`], if the file is unreadable
/// or not JSON, or if its `sheep` is not an array.
#[track_caller]
pub(super) fn assert_the_shape(stem: &str, sheep_count: usize) {
    let tag = stem.strip_suffix("-populated").unwrap_or(stem);
    let (_, shape) = SHAPES
        .iter()
        .find(|(name, _)| *name == tag)
        .unwrap_or_else(|| panic!("{stem} names no release in SHAPES"));
    let raw = fixture_value(stem);

    assert_eq!(
        keys(&raw),
        sorted(shape.blob),
        "{stem}: the keys the blob holds at its own level"
    );

    let flock = raw["sheep"]
        .as_array()
        .expect("a blob's sheep are an array");
    assert_eq!(
        flock.len(),
        sheep_count,
        "{stem}: how many sheep the blob carries"
    );
    for (index, carried) in flock.iter().enumerate() {
        assert_eq!(
            keys(carried),
            sorted_plus(&SHEEP_BEFORE_MARKERS, shape.sheep),
            "{stem}: the keys the sheep at index {index} holds"
        );
        assert_eq!(
            keys(&carried["fds"]),
            sorted(shape.fds),
            "{stem}: the descriptors the sheep at index {index} carries"
        );
        assert_eq!(
            keys(&carried["app"]),
            app_shape(shape.app_added, shape.app_dropped),
            "{stem}: the config keys the sheep at index {index} carries"
        );
    }
}
