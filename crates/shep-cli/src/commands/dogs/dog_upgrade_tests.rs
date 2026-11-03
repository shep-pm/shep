//! `shep upgrade` end to end with a stub `cargo`: what an upgrade writes, what
//! it keeps for `--rollback`, and what it leaves alone.

use std::collections::BTreeMap;

use super::dog_upgrade::{Goal, upgrade, upgrade_all, upgrade_dog};
use super::install_depends::Catalog;
use super::install_tests::{
    COMMIT, Fixture, NEWER_COMMIT, args, config, from_the_row, row, run, typing,
};
use super::receipt;
use super::test_support::streams;
use crate::cli::{AdoptArgs, Format, UpgradeArgs};
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::exit::ExitCode;

const URL: &str = "https://shep-pm.com/dogs.json";

/// The manifest of the build an upgrade fetches, asking for one permission more.
const WIDER: &str = r#"{"manifest":1,"name":"otel","version":"0.3.0","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock to report on it"},{"name":"logs.read","required":false,"reason":"reads logs"}]}"#;

fn at(version: &str) -> AvailableDog {
    row(DogSourceKind::Cargo {
        version: Some(version.to_owned()),
        sha256: BTreeMap::new(),
    })
}

fn otel(fixture: &Fixture) -> std::path::PathBuf {
    fixture.paths.dogs_dir.join("otel")
}

fn held(fixture: &Fixture) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(otel(fixture).join("receipt.json")).unwrap()).unwrap()
}

/// `shep install otel` of `version`, granting what it declares.
async fn installed_at(fixture: &Fixture, version: &str) {
    let reach = fixture.reach(from_the_row());
    let (code, _out, err) = run(
        fixture,
        &at(version),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;
    assert_eq!(code, ExitCode::Success, "{err}");
}

/// `shep install otel --ref <git_ref>`, granting what it declares.
async fn installed_from(fixture: &Fixture, git_ref: &str) {
    let reach = fixture.reach(from_the_row());
    let mut ask = args(true, false);
    ask.git_ref = Some(git_ref.to_owned());
    let (code, _out, err) = run(fixture, &at("0.2.4"), &ask, &reach, typing(None)).await;
    assert_eq!(code, ExitCode::Success, "{err}");
}

/// `shep upgrade otel` towards `goal`, against an index holding `rows`.
async fn upgraded(
    fixture: &Fixture,
    rows: &[AvailableDog],
    goal: &Goal,
) -> (ExitCode, String, String) {
    let reach = fixture.reach(from_the_row());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = streams(&mut out, &mut err);
    streams.fmt = Format::Json;
    let code = upgrade_dog(
        &mut streams,
        &fixture.paths,
        &reach,
        &mut typing(None),
        &Catalog::new(URL, rows),
        "otel",
        goal,
    )
    .await;
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[tokio::test]
async fn a_newer_index_version_installs_into_its_own_directory_and_writes_a_new_receipt() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    let old_binary = otel(&fixture).join("0.2.4/bin/shep-otel");

    let (code, out, err) = upgraded(&fixture, &[at("0.3.0")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let binary = otel(&fixture).join("0.3.0/bin/shep-otel");
    assert!(binary.is_file());
    assert_eq!(
        config(&fixture).adopted_dog_path("otel").unwrap(),
        binary.canonicalize().unwrap()
    );
    let receipt = held(&fixture);
    assert_eq!(receipt["version"], "0.3.0");
    assert_eq!(receipt["binary"], binary.to_str().unwrap());
    assert_eq!(
        receipt["previous"],
        otel(&fixture).join("0.2.4").to_str().unwrap()
    );
    // The replaced directory is kept, with the receipt it had.
    assert!(old_binary.is_file());
    let kept: serde_json::Value =
        serde_json::from_slice(&std::fs::read(otel(&fixture).join("0.2.4/receipt.json")).unwrap())
            .unwrap();
    assert_eq!(kept["version"], "0.2.4");
    assert!(kept.get("previous").is_none());
    // The row is an upgrade's, and the dog is on: with no shepherd the next one starts it.
    let reply: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(reply["command"], "upgrade");
    assert_eq!(reply["data"]["name"], "otel");
    assert!(
        config(&fixture)
            .enabled_dog_names()
            .contains(&"otel".to_owned())
    );
}

#[tokio::test]
async fn a_dog_at_the_index_version_is_left_alone_and_says_so() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    let before = std::fs::read(otel(&fixture).join("receipt.json")).unwrap();
    std::fs::remove_file(fixture.argv()).unwrap();

    let (code, out, err) = upgraded(&fixture, &[at("0.2.4")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(err.contains("already at 0.2.4"), "{err}");
    assert!(out.contains("already current"), "{out}");
    assert!(!fixture.argv().exists(), "cargo must not have run");
    assert_eq!(
        std::fs::read(otel(&fixture).join("receipt.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn a_ref_install_rebuilds_its_ref_at_the_newest_commit_into_a_new_ref_directory() {
    let fixture = Fixture::new();
    installed_from(&fixture, "main").await;
    fixture.builds_commit(NEWER_COMMIT);

    let (code, _out, err) = upgraded(&fixture, &[at("0.2.4")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let binary = otel(&fixture)
        .join(format!("ref-{NEWER_COMMIT}"))
        .join("bin/shep-otel");
    assert!(binary.is_file(), "{}", binary.display());
    let receipt = held(&fixture);
    assert_eq!(receipt["ref"], "main");
    assert_eq!(receipt["commit"], NEWER_COMMIT);
    assert_eq!(
        receipt["previous"],
        otel(&fixture)
            .join(format!("ref-{COMMIT}"))
            .to_str()
            .unwrap()
    );
    assert!(
        otel(&fixture)
            .join(format!("ref-{COMMIT}/bin/shep-otel"))
            .is_file()
    );
    assert_eq!(
        config(&fixture).adopted_dog_path("otel").unwrap(),
        binary.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn a_ref_with_no_new_commit_is_already_current() {
    let fixture = Fixture::new();
    installed_from(&fixture, "main").await;
    let before = std::fs::read(otel(&fixture).join("receipt.json")).unwrap();

    let (code, _out, err) = upgraded(&fixture, &[at("0.2.4")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(err.contains("already at main at 3f9c2a1b7d4e"), "{err}");
    assert_eq!(
        std::fs::read(otel(&fixture).join("receipt.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn ref_switches_to_another_ref_and_release_returns_to_the_index_version() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    fixture.builds_commit(NEWER_COMMIT);

    let (code, _out, err) = upgraded(&fixture, &[at("0.2.4")], &Goal::Ref("next".into())).await;
    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(held(&fixture)["ref"], "next");
    assert_eq!(held(&fixture)["commit"], NEWER_COMMIT);

    let (code, _out, err) = upgraded(&fixture, &[at("0.2.4")], &Goal::Release).await;
    assert_eq!(code, ExitCode::Success, "{err}");
    let receipt = held(&fixture);
    assert_eq!(receipt["version"], "0.2.4");
    assert!(receipt.get("ref").is_none());
}

#[tokio::test]
async fn rollback_puts_back_the_replaced_directory_and_its_receipt() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    let first = held(&fixture);
    let (code, _out, err) = upgraded(&fixture, &[at("0.3.0")], &Goal::Follow).await;
    assert_eq!(code, ExitCode::Success, "{err}");

    let (code, _out, err) = upgraded(&fixture, &[], &Goal::Rollback).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let receipt = held(&fixture);
    assert_eq!(receipt["version"], "0.2.4");
    assert_eq!(receipt["binary"], first["binary"]);
    assert_eq!(receipt["sha256"], first["sha256"]);
    assert_eq!(
        config(&fixture).adopted_dog_path("otel").unwrap(),
        otel(&fixture)
            .join("0.2.4/bin/shep-otel")
            .canonicalize()
            .unwrap()
    );
    // Nothing was thrown away: the build it left is what a second rollback returns to.
    assert_eq!(
        receipt["previous"],
        otel(&fixture).join("0.3.0").to_str().unwrap()
    );
    assert!(otel(&fixture).join("0.3.0/bin/shep-otel").is_file());
    let (code, _out, err) = upgraded(&fixture, &[], &Goal::Rollback).await;
    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(held(&fixture)["version"], "0.3.0");
}

#[tokio::test]
async fn rollback_with_nothing_replaced_refuses_and_changes_nothing() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    let before = std::fs::read(otel(&fixture).join("receipt.json")).unwrap();

    let (code, _out, err) = upgraded(&fixture, &[], &Goal::Rollback).await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains("nothing to roll `otel` back to"), "{err}");
    assert_eq!(
        std::fs::read(otel(&fixture).join("receipt.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn only_one_replaced_directory_is_kept() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    for version in ["0.3.0", "0.4.0"] {
        let (code, _out, err) = upgraded(&fixture, &[at(version)], &Goal::Follow).await;
        assert_eq!(code, ExitCode::Success, "{err}");
    }

    assert!(!otel(&fixture).join("0.2.4").exists());
    assert!(otel(&fixture).join("0.3.0").is_dir());
    assert!(otel(&fixture).join("0.4.0").is_dir());
    assert_eq!(
        held(&fixture)["previous"],
        otel(&fixture).join("0.3.0").to_str().unwrap()
    );
}

#[tokio::test]
async fn a_permission_the_new_build_declares_waits_while_the_old_grants_stay() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    fixture.builds_manifest(WIDER);

    let (code, _out, err) = upgraded(&fixture, &[at("0.3.0")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let record = config(&fixture).grant_record("otel").unwrap().unwrap();
    assert_eq!(record.granted.names().collect::<Vec<_>>(), ["flock.read"]);
    assert_eq!(record.waiting.names().collect::<Vec<_>>(), ["logs.read"]);
    assert!(err.contains("logs.read"), "{err}");
}

#[tokio::test]
async fn the_cargo_path_prints_the_diff_rs_link_with_both_versions() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;

    let (code, _out, err) = upgraded(&fixture, &[at("0.3.0")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(
        err.contains("https://diff.rs/shep-otel/0.2.4/0.3.0"),
        "{err}"
    );
}

#[tokio::test]
async fn a_dog_adopted_from_a_path_has_no_receipt_and_is_refused_naming_shep_adopt() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    // Built by the stub, then adopted by hand and its receipt removed.
    installed_at(&fixture, "0.2.4").await;
    std::fs::remove_file(otel(&fixture).join("receipt.json")).unwrap();
    let _ = reach;

    let (code, _out, err) = upgraded(&fixture, &[at("0.3.0")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains("no receipt"), "{err}");
    assert!(err.contains("shep adopt"), "{err}");
    assert!(
        !fixture.dir.path().join("attempts").exists() || {
            // Only the install's own build ran.
            std::fs::read_to_string(fixture.attempts())
                .unwrap()
                .lines()
                .count()
                == 1
        }
    );
}

#[tokio::test]
async fn a_name_nobody_adopted_is_not_found() {
    let fixture = Fixture::new();

    let (code, _out, err) = upgraded(&fixture, &[at("0.3.0")], &Goal::Follow).await;

    assert_eq!(code, ExitCode::NotFound);
    assert!(err.contains("not an adopted dog"), "{err}");
}

#[tokio::test]
async fn bare_upgrade_is_not_implemented_and_says_so() {
    let fixture = Fixture::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());

    let code = upgrade(
        &mut streams(&mut out, &mut err),
        &fixture.paths,
        &UpgradeArgs {
            dog: None,
            all: false,
            git_ref: None,
            release: false,
            rollback: false,
            refresh: false,
        },
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("not implemented"), "{err}");
}

#[tokio::test]
async fn dogs_upgrades_each_installed_dog_and_skips_one_adopted_from_a_path() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    // A second dog with no receipt.
    let plain = fixture.dir.path().join("shep-plain");
    std::fs::copy(fixture.dir.path().join("built-dog"), &plain).unwrap();
    std::fs::set_permissions(&plain, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = super::adopt::adopt_asking(
        &mut streams(&mut out, &mut err),
        &fixture.paths,
        &AdoptArgs {
            path: plain,
            name: Some("plain".to_owned()),
            yes: true,
        },
        &mut typing(None),
    )
    .await;
    assert_eq!(code, ExitCode::Success, "{}", String::from_utf8_lossy(&err));
    let reach = fixture.reach(from_the_row());
    let rows = [at("0.3.0")];
    let (mut out, mut err) = (Vec::new(), Vec::new());

    let code = upgrade_all(
        &mut streams(&mut out, &mut err),
        &fixture.paths,
        &reach,
        &mut typing(None),
        &Catalog::new(URL, &rows),
    )
    .await;

    let err = String::from_utf8(err).unwrap();
    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(held(&fixture)["version"], "0.3.0");
    assert!(err.contains("skipping plain"), "{err}");
    assert!(receipt::read(&fixture.paths, "plain").unwrap().is_none());
    let _ = ShepToml::read_only(&fixture.paths.daemon_config).unwrap();
}

#[tokio::test]
async fn dogs_in_json_answers_one_document_holding_each_dogs_row() {
    let fixture = Fixture::new();
    installed_at(&fixture, "0.2.4").await;
    let reach = fixture.reach(from_the_row());
    let mut second = at("0.2.4");
    second.adopt_as = "second".to_owned();
    let (code, _out, err) = run(&fixture, &second, &args(true, false), &reach, typing(None)).await;
    assert_eq!(code, ExitCode::Success, "{err}");
    let mut newer = second.clone();
    newer.source = at("0.3.0").source;
    let rows = [at("0.3.0"), newer];
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut json = streams(&mut out, &mut err);
    json.fmt = Format::Json;

    let code = upgrade_all(
        &mut json,
        &fixture.paths,
        &reach,
        &mut typing(None),
        &Catalog::new(URL, &rows),
    )
    .await;

    let err = String::from_utf8(err).unwrap();
    assert_eq!(code, ExitCode::Success, "{err}");
    let document: serde_json::Value = serde_json::from_slice(&out).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON document ({e}): {}",
            String::from_utf8_lossy(&out)
        )
    });
    assert_eq!(document["command"], "upgrade");
    let names: Vec<_> = document["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["otel", "second"]);
}

#[tokio::test]
async fn dogs_in_json_with_no_installed_dog_answers_an_empty_list() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut json = streams(&mut out, &mut err);
    json.fmt = Format::Json;

    let code = upgrade_all(
        &mut json,
        &fixture.paths,
        &reach,
        &mut typing(None),
        &Catalog::new(URL, &[]),
    )
    .await;

    assert_eq!(code, ExitCode::Success);
    let document: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(document["data"], serde_json::json!([]));
}
