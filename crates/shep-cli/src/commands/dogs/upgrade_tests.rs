//! The upgrade path of a dog's grants: a home from before grants, and a dog
//! rebuilt to ask for more. What an operator sees is `shep.toml`.

use std::path::{Path, PathBuf};

use shep_core::paths::ShepPaths;
use shep_core::permissions::{Grants, Permission};

use super::upgrade::{Declaration, plan, upgrade_grants, upgrade_notice};
use crate::commands::shep_toml::{GrantRecord, ShepToml};

fn grants(names: &[&str]) -> Grants {
    Grants::parse(names).unwrap()
}

fn record(granted: &[&str], waiting: &[&str], revoked: &[&str], reviewed: bool) -> GrantRecord {
    GrantRecord {
        granted: grants(granted),
        waiting: grants(waiting),
        revoked: grants(revoked),
        reviewed,
        events: Vec::new(),
    }
}

fn declares(names: &[&str]) -> Declaration {
    Declaration::Declares(grants(names), Vec::new())
}

#[test]
fn an_enabled_dog_with_a_manifest_and_no_record_is_granted_its_list_not_reviewed() {
    let planned = plan(None, true, &declares(&["flock.read", "logs.read"]));

    assert_eq!(
        planned,
        Some(record(&["flock.read", "logs.read"], &[], &[], false))
    );
}

#[test]
fn a_new_record_carries_the_declared_event_kinds() {
    let declaration = Declaration::Declares(grants(&[]), vec!["exported".to_string()]);

    let planned = plan(None, true, &declaration).unwrap();

    assert_eq!(planned.events, ["exported".to_string()]);
}

#[test]
fn a_rebuilt_dog_that_declares_other_event_kinds_has_them_follow_with_no_approval() {
    let before = record(&["flock.read"], &[], &[], true).with_events(vec!["old".to_string()]);
    let declaration = Declaration::Declares(grants(&["flock.read"]), vec!["new".to_string()]);

    let planned = plan(Some(&before), true, &declaration).unwrap();

    assert_eq!(planned.events, ["new".to_string()]);
    assert_eq!(planned.granted, before.granted);
    assert!(planned.waiting.names().next().is_none());
    assert!(planned.reviewed);
}

#[test]
fn a_dog_declaring_the_event_kinds_it_has_recorded_changes_nothing() {
    let before = record(&["flock.read"], &[], &[], true).with_events(vec!["same".to_string()]);
    let declaration = Declaration::Declares(grants(&["flock.read"]), vec!["same".to_string()]);

    assert_eq!(plan(Some(&before), true, &declaration), None);
}

#[test]
fn a_dog_that_declares_nothing_has_nothing_to_review() {
    assert_eq!(
        plan(None, true, &declares(&[])),
        Some(record(&[], &[], &[], true))
    );
}

#[test]
fn a_disabled_dog_with_no_record_is_left_for_the_enable_that_asks() {
    assert_eq!(plan(None, false, &declares(&["flock.read"])), None);
}

#[test]
fn an_undeclared_dog_and_an_unreadable_one_are_left_alone() {
    for declaration in [Declaration::Undeclared, Declaration::Unknown] {
        assert_eq!(plan(None, true, &declaration), None);
        let held = record(&["flock.read"], &[], &[], true);
        assert_eq!(plan(Some(&held), true, &declaration), None);
    }
}

#[test]
fn a_rebuilt_dog_that_asks_for_more_keeps_its_grants_and_waits_on_the_rest() {
    let held = record(&["flock.read"], &[], &[], true);

    let planned = plan(Some(&held), true, &declares(&["flock.read", "logs.read"]));

    assert_eq!(
        planned,
        Some(record(&["flock.read"], &["logs.read"], &[], true))
    );
}

#[test]
fn a_dog_asking_for_what_it_holds_changes_nothing() {
    let held = record(&["flock.read"], &[], &[], false);

    assert_eq!(plan(Some(&held), true, &declares(&["flock.read"])), None);
}

#[test]
fn a_revoked_permission_the_dog_still_declares_is_not_read_as_new() {
    let held = record(&["flock.read"], &[], &["logs.read"], true);

    assert_eq!(
        plan(Some(&held), true, &declares(&["flock.read", "logs.read"])),
        None
    );
}

#[test]
fn a_waiting_permission_a_newer_build_dropped_stops_waiting() {
    let held = record(&["flock.read"], &["logs.read"], &["flock.edit"], true);

    let planned = plan(Some(&held), true, &declares(&["flock.read"]));

    assert_eq!(planned, Some(record(&["flock.read"], &[], &[], true)));
}

fn home() -> (tempfile::TempDir, ShepPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    (dir, paths)
}

fn recorded(paths: &ShepPaths, name: &str) -> Option<GrantRecord> {
    ShepToml::read_only(&paths.daemon_config)
        .unwrap()
        .grant_record(name)
        .unwrap()
}

fn declared_by_builtin(name: &str) -> Grants {
    Grants::from_names(
        crate::dog::builtin_manifest(name)
            .expect("a built-in declares itself")
            .permissions
            .into_iter()
            .map(|permission| permission.name),
    )
}

#[test]
fn a_home_from_before_grants_gets_each_enabled_built_in_its_list_once_marked() {
    let (_dir, paths) = home();
    std::fs::write(
        &paths.daemon_config,
        "[daemon]\nenabled_dogs = [\"bark\"]\n",
    )
    .unwrap();

    let changed = upgrade_grants(&paths).unwrap();

    assert_eq!(changed, ["bark"]);
    assert_eq!(
        recorded(&paths, "bark"),
        Some(GrantRecord::at_upgrade(declared_by_builtin("bark")))
    );
    assert_eq!(recorded(&paths, "metrics"), None, "metrics is not enabled");
}

#[test]
fn the_second_upgrade_writes_nothing_and_leaves_the_file_as_it_was() {
    let (_dir, paths) = home();
    std::fs::write(
        &paths.daemon_config,
        "[daemon]\nenabled_dogs = [\"bark\"]\n",
    )
    .unwrap();
    upgrade_grants(&paths).unwrap();
    let after_first = std::fs::read_to_string(&paths.daemon_config).unwrap();

    let changed = upgrade_grants(&paths).unwrap();

    assert!(changed.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        after_first
    );
}

#[test]
fn a_home_with_no_shep_toml_is_left_without_one() {
    let (_dir, paths) = home();

    assert!(upgrade_grants(&paths).unwrap().is_empty());
    assert!(!paths.daemon_config.exists());
}

fn write_dog(dir: &Path, manifest: &str) -> PathBuf {
    let binary = dir.join("shep-otel");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nexit 0\nfi\nexit 0\n"
    );
    std::fs::write(&binary, script).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    binary
}

fn manifest_declaring(names: &[&str]) -> String {
    let permissions = names
        .iter()
        .map(|name| format!(r#"{{"name":"{name}","required":true,"reason":"because"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{permissions}]}}"#
    )
}

fn adopt_enabled(paths: &ShepPaths, binary: &Path, extra: &str) {
    std::fs::write(
        &paths.daemon_config,
        format!(
            "[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs]\notel = \"{}\"\n{extra}",
            binary.display()
        ),
    )
    .unwrap();
}

#[test]
fn an_adopted_dog_enabled_before_grants_is_granted_what_its_binary_declares() {
    let (_dir, paths) = home();
    let binary = write_dog(&paths.home, &manifest_declaring(&["flock.read"]));
    adopt_enabled(&paths, &binary, "");

    upgrade_grants(&paths).unwrap();

    assert_eq!(
        recorded(&paths, "otel"),
        Some(GrantRecord::at_upgrade(Grants::new([
            Permission::FlockRead
        ])))
    );
}

#[test]
fn an_adopted_dog_whose_binary_declares_nothing_stays_undeclared() {
    let (_dir, paths) = home();
    let binary = write_dog(&paths.home, "not a manifest");
    adopt_enabled(&paths, &binary, "");

    let changed = upgrade_grants(&paths).unwrap();

    assert!(changed.is_empty());
    assert_eq!(recorded(&paths, "otel"), None);
}

#[test]
fn a_rebuilt_dog_that_asks_for_more_is_recorded_as_waiting_and_keeps_its_grants() {
    let (_dir, paths) = home();
    let binary = write_dog(&paths.home, &manifest_declaring(&["flock.read"]));
    adopt_enabled(
        &paths,
        &binary,
        "\n[daemon.dog_grants.otel]\ngranted = [\"flock.read\"]\n",
    );
    write_dog(
        &paths.home,
        &manifest_declaring(&["flock.read", "logs.read"]),
    );

    let changed = upgrade_grants(&paths).unwrap();

    assert_eq!(changed, ["otel"]);
    assert_eq!(
        recorded(&paths, "otel"),
        Some(record(&["flock.read"], &["logs.read"], &[], true))
    );
}

#[test]
fn the_notice_names_each_dog_and_claims_nothing_about_what_changed() {
    let notice = upgrade_notice(&["bark".to_owned(), "otel".to_owned()]);

    assert!(notice.contains("bark, otel"), "{notice}");
    assert!(notice.contains("`shep dogs` shows"), "{notice}");
    assert!(
        !notice.contains("is marked") && !notice.contains("are marked"),
        "{notice}"
    );
}
