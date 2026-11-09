//! Replacement installs through the CLI's install flow.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::consent::Input;
use super::install_tests::{
    Fixture, MANIFEST, args, config, enabled, from_the_row, row, run, typing,
};
use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::exit::ExitCode;

fn cargo_row() -> AvailableDog {
    row(DogSourceKind::Cargo {
        version: Some("0.2.4".to_owned()),
        sha256: BTreeMap::new(),
    })
}

fn adopted(fixture: &Fixture, enable: bool) -> PathBuf {
    let old = fixture.dir.path().join("adopted-otel");
    std::fs::write(&old, "old binary").unwrap();
    ShepToml::try_edit(&fixture.paths.daemon_config, |cfg| {
        cfg.adopt_dog("otel", &old, false)?;
        if !enable {
            cfg.disable_dog("otel");
        }
        Ok::<_, ShepTomlError>(())
    })
    .unwrap();
    let mut text = std::fs::read_to_string(&fixture.paths.daemon_config).unwrap();
    text.push_str("\n[dog.otel]\nlimit = 17\n");
    std::fs::write(&fixture.paths.daemon_config, text).unwrap();
    old
}

// Real cargo and manifest subprocesses need the wall clock.
#[tokio::test]
async fn declining_a_swap_names_the_adopted_path_and_changes_nothing() {
    let fixture = Fixture::new();
    let old = adopted(&fixture, true);
    let before = std::fs::read(&fixture.paths.daemon_config).unwrap();
    let reach = fixture.reach(from_the_row());
    let (code, _, err) = run(
        &fixture,
        &cargo_row(),
        &args(false, false),
        &reach,
        typing(Some("n\n")),
    )
    .await;
    assert_eq!(
        code,
        ExitCode::Failure,
        "a decline is not an install that landed: {err}"
    );
    assert!(
        err.contains("nothing was changed: otel stays as adopted"),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "Replace adopted otel at {} with the catalogue build?",
            old.display()
        )),
        "{err}"
    );
    assert!(
        !fixture.argv().exists(),
        "nothing was built before confirmation"
    );
    assert_eq!(std::fs::read(&fixture.paths.daemon_config).unwrap(), before);
    assert_eq!(std::fs::read_to_string(old).unwrap(), "old binary");
    assert!(!fixture.paths.dogs_dir.join("otel/receipt.json").exists());
}

// Real cargo and manifest subprocesses need the wall clock.
#[tokio::test]
async fn a_cli_swap_records_a_receipt_and_preserves_the_name_config_and_disabled_state() {
    let fixture = Fixture::new();
    let old = adopted(&fixture, false);
    let reach = fixture.reach(from_the_row());
    let (code, _, err) = run(
        &fixture,
        &cargo_row(),
        &args(false, false),
        &reach,
        typing(Some("y\ny\n")),
    )
    .await;
    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(!enabled(&fixture), "a disabled adopted dog stays disabled");
    let binary = fixture.paths.dogs_dir.join("otel/0.2.4/bin/shep-otel");
    assert_eq!(
        config(&fixture).adopted_dog_path("otel"),
        Some(binary.canonicalize().unwrap())
    );
    let text = std::fs::read_to_string(&fixture.paths.daemon_config).unwrap();
    assert!(text.contains("[dog.otel]\nlimit = 17"), "{text}");
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.paths.dogs_dir.join("otel/receipt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["row"]["adopt_as"], "otel");
    assert_eq!(receipt["package"], "shep-otel");
    assert_eq!(std::fs::read_to_string(&old).unwrap(), "old binary");
    assert!(!old.with_extension("kept").exists());
}

// Real cargo and manifest subprocesses need the wall clock.
#[tokio::test]
async fn an_adopted_file_the_install_does_not_write_is_never_copied() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let old = adopted(&fixture, true);
    let system = fixture.dir.path().join("system-bin");
    std::fs::create_dir(&system).unwrap();
    let adopted_path = system.join("shep-otel-system");
    std::fs::rename(&old, &adopted_path).unwrap();
    ShepToml::try_edit(&fixture.paths.daemon_config, |cfg| {
        cfg.adopt_dog("otel", &adopted_path, false)
    })
    .unwrap();
    // A directory the operator cannot write, as `/usr/bin` is.
    std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o555)).unwrap();
    let reach = fixture.reach(from_the_row());
    let (code, _, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;
    std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(
        std::fs::read_to_string(&adopted_path).unwrap(),
        "old binary"
    );
    assert!(!system.join("shep-otel-system.kept").exists());
    let binary = fixture.paths.dogs_dir.join("otel/0.2.4/bin/shep-otel");
    assert_eq!(
        config(&fixture).adopted_dog_path("otel"),
        Some(binary.canonicalize().unwrap())
    );
}

// Real cargo and manifest subprocesses need the wall clock.
#[tokio::test]
async fn the_binary_at_the_install_target_is_parked_until_the_swap_commits() {
    let fixture = Fixture::new();
    let old = adopted(&fixture, true);
    let binary = fixture.paths.dogs_dir.join("otel/0.2.4/bin/shep-otel");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::rename(old, &binary).unwrap();
    ShepToml::try_edit(&fixture.paths.daemon_config, |cfg| {
        cfg.adopt_dog("otel", &binary, false)
    })
    .unwrap();
    let parked = PathBuf::from(format!("{}.kept", binary.display()));
    let reach = fixture.reach(from_the_row());
    let parked_read = parked.clone();
    let input = Input::answering(move |prompt| {
        if prompt.question().starts_with("Replace adopted") {
            assert!(!parked_read.exists(), "confirmation precedes parking");
        } else {
            assert_eq!(std::fs::read_to_string(&parked_read).unwrap(), "old binary");
        }
        true
    });
    let (code, _, err) = run(&fixture, &cargo_row(), &args(false, false), &reach, input).await;
    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(enabled(&fixture));
    assert!(
        !parked.exists(),
        "the committed swap removes its parked copy"
    );
}

// Real cargo and manifest subprocesses need the wall clock.
#[tokio::test]
async fn no_enable_holds_a_swapped_dog_off_even_when_it_was_enabled() {
    let fixture = Fixture::new();
    adopted(&fixture, true);
    let reach = fixture.reach(from_the_row());
    let (code, _, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, true),
        &reach,
        typing(None),
    )
    .await;
    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(!enabled(&fixture), "--no-enable holds the swapped dog off");
}

// Real cargo and manifest subprocesses need the wall clock.
#[tokio::test]
async fn a_refused_swap_restores_an_adopted_binary_at_the_install_target_and_leaves_no_receipt() {
    let fixture = Fixture::new();
    let old = adopted(&fixture, true);
    let binary = fixture.paths.dogs_dir.join("otel/0.2.4/bin/shep-otel");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::rename(old, &binary).unwrap();
    ShepToml::try_edit(&fixture.paths.daemon_config, |cfg| {
        cfg.adopt_dog("otel", &binary, false)
    })
    .unwrap();
    let before = std::fs::read(&fixture.paths.daemon_config).unwrap();
    fixture.builds_manifest(&MANIFEST.replace("flock.read", "future.unknown"));
    let reach = fixture.reach(from_the_row());
    let (code, _, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;
    assert_eq!(code, ExitCode::InvalidConfig, "{err}");
    assert_eq!(std::fs::read_to_string(binary).unwrap(), "old binary");
    assert_eq!(std::fs::read(&fixture.paths.daemon_config).unwrap(), before);
    assert!(!fixture.paths.dogs_dir.join("otel/receipt.json").exists());
}
