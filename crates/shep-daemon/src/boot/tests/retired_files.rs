//! A file an older shepherd wrote and this one no longer reads: the boot
//! says so once and leaves it where it is.

use crate::boot::*;
use crate::fake::ScriptedRunner;
use crate::testing::{capture_logs, test_paths};

/// An operator who set `<dog>.defaults` on an older build still has the file.
/// Deleting it would be this boot's decision, not theirs.
#[test]
fn a_leftover_dog_defaults_file_boots_warns_once_and_stays() {
    let _guard = SIGNAL_TEST_LOCK.blocking_lock();
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();
    let file = paths.home.join("dog-defaults.json");
    let left = r#"{"version":1,"dogs":{"jobs":{"model":"opus"}}}"#;
    std::fs::write(&file, left).unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut booted = None;
    let logs = capture_logs(|| {
        booted = Some(rt.block_on(boot_with_idle_sweep(
            ScriptedRunner::new(vec![]),
            paths,
            BootOptions::default(),
        )));
    });
    let daemon = booted
        .unwrap()
        .expect("a leftover file must not stop the boot");
    drop(daemon);

    assert_eq!(logs.matches("no longer read").count(), 1, "{logs}");
    assert!(logs.contains("WARN"), "{logs}");
    assert!(logs.contains("dog-defaults.json"), "{logs}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), left);
}

/// A home with no such file has nothing to say.
#[test]
fn a_boot_with_no_dog_defaults_file_says_nothing_of_one() {
    let _guard = SIGNAL_TEST_LOCK.blocking_lock();
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut booted = None;
    let logs = capture_logs(|| {
        booted = Some(rt.block_on(boot_with_idle_sweep(
            ScriptedRunner::new(vec![]),
            paths,
            BootOptions::default(),
        )));
    });
    drop(booted.unwrap().unwrap());

    assert!(!logs.contains("no longer read"), "{logs}");
}
