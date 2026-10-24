//! Writing the grants an operator gave a dog, beside `adopted_dogs`.

use super::*;
use shep_core::permissions::{Grants, Permission};

#[test]
fn recorded_grants_read_back_and_sit_under_the_dogs_own_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let grants = Grants::new([Permission::FlockRead, Permission::LogsRead]);

    ShepToml::try_edit(&path, |cfg| cfg.set_granted("otel", &grants)).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("[daemon.dog_grants.otel]\ngranted = [\"flock.read\", \"logs.read\"]\n"),
        "{written}"
    );
    assert!(!written.contains("[daemon.dog_grants]"), "{written}");
    assert_eq!(
        ShepToml::read_only(&path).unwrap().granted("otel").unwrap(),
        Some(grants)
    );
}

#[test]
fn nothing_granted_is_written_as_an_empty_list_not_as_no_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    ShepToml::try_edit(&path, |cfg| cfg.set_granted("otel", &Grants::default())).unwrap();

    assert_eq!(
        ShepToml::read_only(&path).unwrap().granted("otel").unwrap(),
        Some(Grants::default()),
        "an empty entry grants nothing; no entry would be full access"
    );
}

#[test]
fn granting_one_dog_leaves_another_dogs_grants_and_the_rest_of_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon]\nenabled_dogs = [\"bark\"]\n\n[daemon.dog_grants.bark]\ngranted = [\"logs.read\"]\n",
    )
    .unwrap();

    ShepToml::try_edit(&path, |cfg| {
        cfg.set_granted("otel", &Grants::new([Permission::FlockRead]))
    })
    .unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(
        cfg.granted("bark").unwrap(),
        Some(Grants::new([Permission::LogsRead]))
    );
    assert_eq!(
        cfg.granted("otel").unwrap(),
        Some(Grants::new([Permission::FlockRead]))
    );
    assert_eq!(cfg.enabled_dog_names(), vec!["bark".to_string()]);
}

#[test]
fn a_dog_grants_that_is_not_a_table_is_refused_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\ndog_grants = 3\n").unwrap();

    let refused = ShepToml::try_edit(&path, |cfg| cfg.set_granted("otel", &Grants::default()));

    assert!(matches!(refused, Err(ShepTomlError::WrongShape { .. })));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[daemon]\ndog_grants = 3\n"
    );
}
