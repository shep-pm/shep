//! Replacement installs through the CLI's install flow.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::install_tests::{Fixture, args, from_the_row, row, run, typing};
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
    }).unwrap();
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
    let (code, _, err) = run(&fixture, &cargo_row(), &args(false, false), &reach, typing(Some("n\n"))).await;
    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(err.contains(&format!("Replace adopted otel at {} with the catalogue build?", old.display())), "{err}");
    assert!(!fixture.argv().exists(), "nothing was built before confirmation");
    assert_eq!(std::fs::read(&fixture.paths.daemon_config).unwrap(), before);
    assert_eq!(std::fs::read_to_string(old).unwrap(), "old binary");
    assert!(!fixture.paths.dogs_dir.join("otel/receipt.json").exists());
}
