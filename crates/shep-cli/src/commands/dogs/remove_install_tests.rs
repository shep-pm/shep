//! What `shep delete` does to the directory `shep install` wrote.

use std::path::Path;

use shep_core::paths::ShepPaths;

use super::test_support::streams;
use super::{Removal, remove_dog};
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;

fn adopt(paths: &ShepPaths, name: &str, binary: &Path) {
    ShepToml::edit(&paths.daemon_config, |cfg| {
        cfg.adopt_dog(name, binary, false).unwrap();
    })
    .unwrap();
}

/// A dog as `shep install` leaves it: a versioned binary and a receipt.
fn installed(paths: &ShepPaths, name: &str) -> std::path::PathBuf {
    let bin = paths.dogs_dir.join(name).join("0.2.4/bin").join(name);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
    std::fs::write(paths.dogs_dir.join(name).join("receipt.json"), b"{}").unwrap();
    adopt(paths, name, &bin);
    bin
}

async fn run(paths: &ShepPaths, name: &str, removal: Removal) -> (ExitCode, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = remove_dog(&mut streams(&mut out, &mut err), paths, name, removal).await;
    (code, String::from_utf8(err).unwrap())
}

fn home() -> (tempfile::TempDir, ShepPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    (dir, paths)
}

#[tokio::test]
async fn deleting_an_installed_dog_removes_its_directory() {
    let (_dir, paths) = home();
    installed(&paths, "otel");
    installed(&paths, "metrics-extra");

    let (code, err) = run(&paths, "otel", Removal::Delete).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(!paths.dogs_dir.join("otel").exists());
    assert!(
        paths.dogs_dir.join("metrics-extra/receipt.json").exists(),
        "another dog's install is not this one's"
    );
    assert!(err.contains("removed"), "{err}");
}

#[tokio::test]
async fn deleting_an_adopted_dog_leaves_its_file() {
    let (_dir, paths) = home();
    let binary = paths.home.join("elsewhere/shep-otel");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
    adopt(&paths, "otel", &binary);

    let (code, err) = run(&paths, "otel", Removal::Delete).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(binary.exists(), "an adopted dog's file is the operator's");
}

#[tokio::test]
async fn a_directory_with_no_receipt_is_not_installs_to_remove() {
    let (_dir, paths) = home();
    let mine = paths.dogs_dir.join("otel/notes.txt");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, b"mine").unwrap();
    adopt(&paths, "otel", &mine);

    let (code, err) = run(&paths, "otel", Removal::Delete).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(mine.exists());
}

#[tokio::test]
async fn rehome_keeps_the_installed_directory() {
    let (_dir, paths) = home();
    installed(&paths, "otel");

    let (code, err) = run(&paths, "otel", Removal::Rehome).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(paths.dogs_dir.join("otel/receipt.json").exists());
}

#[tokio::test]
async fn a_name_that_climbs_out_of_the_dogs_directory_removes_nothing() {
    let (_dir, paths) = home();
    let victim = paths.home.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("receipt.json"), b"{}").unwrap();
    std::fs::create_dir_all(&paths.dogs_dir).unwrap();

    let (_code, _err) = run(&paths, "../victim", Removal::Delete).await;

    assert!(victim.join("receipt.json").exists());
}
