//! A dog's defaults at boot: an unreadable store refuses the boot rather than
//! serving every sheep's table without the defaults an operator set.

use crate::boot::*;
use crate::fake::ScriptedRunner;
use crate::testing::test_paths;

/// A store that is not JSON has defaults no one can name, and a shepherd that
/// started without them would hand every dog the unlayered table and rewrite
/// the file over the operator's edits on the next write.
#[tokio::test]
async fn an_unreadable_dog_defaults_store_refuses_the_boot_and_names_the_file() {
    // Real time: binds a real socket. Locked per SIGNAL_TEST_LOCK's rule.
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);
    init_dirs(&paths).unwrap();
    std::fs::write(&paths.dog_defaults, "{ not json").unwrap();

    let refused = boot_with_idle_sweep(
        ScriptedRunner::new(vec![]),
        paths.clone(),
        BootOptions::default(),
    )
    .await;

    let Err(BootError::DogDefaults { path, .. }) = refused else {
        panic!("expected the boot to be refused over the store");
    };
    assert_eq!(path, paths.dog_defaults);
}
