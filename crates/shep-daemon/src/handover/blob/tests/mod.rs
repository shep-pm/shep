//! What a blob this daemon wrote must still be when it comes back.
//!
//! The fixtures here build one, so [`compat`] can take a current blob apart
//! key by key. The cases in this file are the round trip, the redaction that
//! keeps a sheep's environment out of a `Debug` line, the `0600` the file
//! reaches disk at, and the version gate.

use std::os::unix::fs::PermissionsExt;

use super::{Handover, VERSION};
use crate::entry::ProcessEntry;
use crate::handover::fixtures::{carried, entry_fixture};
use crate::testing::test_paths;

mod compat;

fn handover_over(entry: &ProcessEntry) -> Handover {
    Handover {
        version: VERSION,
        sheep: vec![carried(entry)],
        listener_fd: 3,
        pidfile_fd: 4,
        next_id: 9,
        next_deadline: 5,
        next_action_stamp: 2,
        reloads: Some(Vec::new()),
    }
}

fn sample_handover() -> Handover {
    handover_over(&entry_fixture(|_| {}))
}

fn sample_handover_with_secret_env() -> Handover {
    let entry = entry_fixture(|app| {
        app.env.insert("TOKEN".to_owned(), "hunter2".to_owned());
    });
    assert!(
        entry.spec.config().env.values().any(|v| v == "hunter2"),
        "the fixture must really carry the secret it is testing for"
    );
    handover_over(&entry)
}

#[test]
fn a_blob_round_trips() {
    let h = sample_handover();
    let back: Handover = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
    assert_eq!(back, h);
}

#[test]
fn a_blob_round_trips_a_sheeps_environment_intact() {
    let text = serde_json::to_string(&sample_handover_with_secret_env()).unwrap();
    let back: Handover = serde_json::from_str(&text).unwrap();
    assert_eq!(
        back.sheep[0].app.env.get("TOKEN").map(String::as_str),
        Some("hunter2"),
        "{text}"
    );
}

/// The other file a resolved value could reach, and the same claim the
/// muster roll's own case makes: a blob carries the `AppConfig` a sheep
/// was registered with, references and all, never the `SpawnSpec` a
/// spawn assembled from it. Reachable without a real handover because a
/// blob is built from the entries, and an entry holds the config.
///
/// Assembles first, so the fixture is one that really resolves.
#[test]
fn a_resolved_secret_never_reaches_the_blob() {
    const SENTINEL: &str = "hunter2-that-must-never-reach-disk";

    let entry = entry_fixture(|app| {
        app.env
            .insert("PW".to_owned(), "{{secret:DB_PASSWORD}}".to_owned());
    });
    let dir = tempfile::tempdir().unwrap();
    let view = shep_core::secrets::SecretView::new(
        "production".to_string(),
        std::collections::BTreeMap::from([(
            "DB_PASSWORD".to_string(),
            std::collections::BTreeMap::from([(
                "production".to_string(),
                SENTINEL.to_string(),
            )]),
        )]),
        shep_core::secrets::ProviderCache::default(),
    );
    let spec = crate::assemble::assemble(
        &entry.spec,
        entry.instance,
        &crate::testing::test_paths(&dir),
        None,
        &view,
    )
    .unwrap();
    assert_eq!(
        spec.env["PW"], SENTINEL,
        "the fixture must really resolve, or this case proves nothing"
    );

    let text = serde_json::to_string(&handover_over(&entry)).unwrap();
    assert!(!text.contains(SENTINEL), "the blob carries a value: {text}");
    assert!(
        text.contains("{{secret:DB_PASSWORD}}"),
        "the reference is what it carries instead: {text}"
    );
}

#[test]
fn debug_redacts_a_carried_sheeps_environment() {
    // An exact string, not a `contains`: a field added later that prints
    // env cannot be named in a substring check.
    let entry = entry_fixture(|app| {
        app.env.insert("TOKEN".to_owned(), "hunter2".to_owned());
    });
    assert_eq!(
        format!("{:?}", carried(&entry)),
        "CarriedSheep { id: 1, name: \"web\", instance: 0, pid: Some(100), restarts: 0, \
         epoch: 7, status: Online, last_exit: None, credentials: Resolved(None), fds: \
         CarriedFds { out_pipe: Some(11), err_pipe: Some(12), out_log: Some(13), err_log: \
         Some(14), stdin: Some(15), channel: Some(16) }, pending_delete: Some(false), \
         manual: None, reload: Some(None), ready_failed: Some(false), restart_due: None, \
         dog: None, pending: None, pending_reidentifies: None, app: AppConfig { \
         name: \"web\", script: \"./srv\", env: <1 vars>, .. } }"
    );
    // The whole blob too, which holds fields of its own.
    let text = format!("{:?}", sample_handover_with_secret_env());
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("TOKEN"), "{text}");
}

#[test]
fn debug_redacts_the_environment_on_a_process_entry() {
    // `ProcessEntry` derives `Debug`, so its safety rests on
    // `AppConfig`'s redacted rendering reaching it through `spec`.
    let entry = entry_fixture(|app| {
        app.env.insert("TOKEN".to_owned(), "hunter2".to_owned());
    });
    assert_eq!(
        format!("{entry:?}"),
        "ProcessEntry { id: 1, spec: ResolvedApp { config: AppConfig { name: \"web\", \
         script: \"./srv\", env: <1 vars>, .. } }, pending: None, pending_reidentifies: \
         false, overridden: [], instance: 0, status: Online, pid: Some(100), restarts: 0, \
         started_at: None, budget: RestartBudget { unstable_count: 0 }, reload: None, \
         credentials: Resolved(None), out_file: \"/tmp/shep-handover-test-out.log\", \
         err_file: \"/tmp/shep-handover-test-err.log\", dog: None, last_exit: None }"
    );
}

#[test]
fn a_written_blob_is_readable_only_by_its_owner() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(&paths.run).unwrap();

    let written = sample_handover().write(&paths).unwrap();

    assert_eq!(written, Handover::path(&paths));
    let mode = std::fs::metadata(&written).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    assert_eq!(Handover::read(&written).unwrap(), sample_handover());
}

#[test]
fn a_stale_blob_does_not_lend_its_mode_to_the_next_one() {
    // `OpenOptions::mode` applies only when the open creates the file,
    // so a leftover blob left in place would keep whatever mode it had.
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    std::fs::create_dir_all(&paths.run).unwrap();
    let path = Handover::path(&paths);
    std::fs::write(&path, "stale").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    sample_handover().write(&paths).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
}

#[test]
fn a_blob_from_a_future_version_is_refused_not_guessed_at() {
    let mut v = serde_json::to_value(sample_handover()).unwrap();
    v["version"] = serde_json::json!(u32::MAX);
    assert!(Handover::load_value(v).is_err());
}
