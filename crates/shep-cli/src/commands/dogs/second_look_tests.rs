//! `shep enable` on a dog that already holds grants: a list written at
//! upgrade nobody has looked at, and permissions a newer build waits on.

use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;

use super::consent::Input;
use super::enable::enable_asking;
use super::test_support::streams;
use crate::commands::shep_toml::{GrantRecord, ShepToml};
use crate::exit::ExitCode;

fn typing(typed: Option<&'static str>) -> Input {
    Input::reading(
        typed.is_some(),
        std::io::Cursor::new(typed.unwrap_or("").as_bytes()),
    )
}

async fn enable(paths: &ShepPaths, yes: bool, mut input: Input) -> (ExitCode, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = enable_asking(
        &mut streams(&mut out, &mut err),
        paths,
        "bark",
        yes,
        &mut input,
    )
    .await;
    (code, String::from_utf8(err).unwrap())
}

fn declared() -> Grants {
    Grants::from_names(
        crate::dog::builtin_manifest("bark")
            .expect("bark declares itself")
            .permissions
            .into_iter()
            .map(|permission| permission.name),
    )
}

fn recorded(paths: &ShepPaths) -> Option<GrantRecord> {
    ShepToml::read_only(&paths.daemon_config)
        .unwrap()
        .grant_record("bark")
        .unwrap()
}

/// A home from before grants, with `bark` already enabled.
fn before_grants() -> (tempfile::TempDir, ShepPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(
        &paths.daemon_config,
        "[daemon]\nenabled_dogs = [\"bark\"]\n",
    )
    .unwrap();
    (dir, paths)
}

/// A home where `bark` holds nothing and its build declares more.
fn waiting_on_everything() -> (tempfile::TempDir, ShepPaths) {
    let (dir, paths) = before_grants();
    ShepToml::try_edit(&paths.daemon_config, |cfg| {
        cfg.set_grant_record("bark", &GrantRecord::reviewed(Grants::default()))
    })
    .unwrap();
    (dir, paths)
}

#[tokio::test]
async fn a_list_written_at_upgrade_is_marked_reviewed_by_a_yes() {
    let (_dir, paths) = before_grants();

    let (code, said) = enable(&paths, false, typing(Some("y\n"))).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(said.contains("granted at upgrade, not reviewed"), "{said}");
    assert_eq!(recorded(&paths), Some(GrantRecord::reviewed(declared())));
}

#[tokio::test]
async fn a_no_leaves_the_list_marked_and_the_dog_enabled_on_what_it_holds() {
    let (_dir, paths) = before_grants();

    let (code, said) = enable(&paths, false, typing(Some("n\n"))).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert_eq!(recorded(&paths), Some(GrantRecord::at_upgrade(declared())));
}

#[tokio::test]
async fn with_no_terminal_the_dog_carries_on_and_the_mark_stays() {
    let (_dir, paths) = before_grants();

    let (code, said) = enable(&paths, false, typing(None)).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(said.contains("no terminal to ask on"), "{said}");
    assert!(said.contains("--yes"), "{said}");
    assert_eq!(recorded(&paths), Some(GrantRecord::at_upgrade(declared())));
}

#[tokio::test]
async fn yes_approves_what_a_newer_build_waits_on() {
    let (_dir, paths) = waiting_on_everything();

    let (code, said) = enable(&paths, true, typing(None)).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert_eq!(recorded(&paths), Some(GrantRecord::reviewed(declared())));
}

#[tokio::test]
async fn what_a_newer_build_waits_on_stays_waiting_without_a_yes() {
    let (_dir, paths) = waiting_on_everything();
    let waiting = GrantRecord {
        waiting: declared(),
        ..GrantRecord::reviewed(Grants::default())
    };

    let (code, said) = enable(&paths, false, typing(Some("n\n"))).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(said.contains("waits on"), "{said}");
    assert_eq!(recorded(&paths), Some(waiting));
}

#[tokio::test]
async fn a_reviewed_dog_with_nothing_waiting_is_not_asked() {
    let (_dir, paths) = before_grants();
    ShepToml::try_edit(&paths.daemon_config, |cfg| {
        cfg.set_grant_record("bark", &GrantRecord::reviewed(declared()))
    })
    .unwrap();

    let (code, said) = enable(&paths, false, typing(None)).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(!said.contains("not reviewed"), "{said}");
}
