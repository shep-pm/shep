//! `shep install --ref` with a stub `cargo`: the command it runs against the
//! row's repo, where the build lands, the line in the grant prompt, and the
//! receipt.

use clap::Parser;

use super::install_tests::{
    COMMIT, Fixture, REPO, args, config, enabled, from_the_row, row, run, typing,
};
use crate::cli::{Cli, Commands, InstallArgs};
use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::exit::ExitCode;

fn with_ref(git_ref: &str) -> InstallArgs {
    InstallArgs {
        git_ref: Some(git_ref.to_owned()),
        ..args(true, false)
    }
}

/// A cargo row the index approves a version of.
fn cargo_row() -> AvailableDog {
    row(DogSourceKind::Cargo {
        version: Some("0.2.4".to_owned()),
        sha256: std::collections::BTreeMap::new(),
    })
}

fn attempts(fixture: &Fixture) -> Vec<String> {
    std::fs::read_to_string(fixture.attempts())
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn ref_root(fixture: &Fixture) -> std::path::PathBuf {
    fixture
        .paths
        .dogs_dir
        .join("otel")
        .join(format!("ref-{COMMIT}"))
}

#[tokio::test]
async fn a_branch_builds_from_the_rows_repo_and_lands_in_ref_sha() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &with_ref("main"),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    // Not a tag, so the second try is the branch.
    let runs = attempts(&fixture);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        runs[0].starts_with(&format!("install --locked --git {REPO} --tag main --root ")),
        "{runs:?}"
    );
    assert!(
        runs[1].starts_with(&format!(
            "install --locked --git {REPO} --branch main --root "
        )),
        "{runs:?}"
    );
    assert!(runs[1].ends_with(" shep-otel"), "{runs:?}");
    let binary = ref_root(&fixture).join("bin").join("shep-otel");
    assert!(binary.is_file(), "{}", binary.display());
    assert_eq!(
        config(&fixture).adopted_dog_path("otel").unwrap(),
        binary.canonicalize().unwrap()
    );
    assert!(enabled(&fixture));
    // The temporary root was moved, not copied.
    let left: Vec<_> = std::fs::read_dir(fixture.paths.dogs_dir.join("otel"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        left.iter().all(|name| !name.starts_with('.')),
        "temporary root left behind: {left:?}"
    );
}

#[tokio::test]
async fn a_tag_the_repo_has_builds_once_with_tag() {
    let fixture = Fixture::new();
    fixture.has_tag("v0.3.0");
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &with_ref("v0.3.0"),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let runs = attempts(&fixture);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].contains(" --tag v0.3.0 "), "{runs:?}");
}

#[tokio::test]
async fn a_hex_string_is_a_commit_and_goes_to_cargo_as_rev_alone() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &with_ref("3f9c2a1"),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let runs = attempts(&fixture);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].contains(" --rev 3f9c2a1 "), "{runs:?}");
    assert!(ref_root(&fixture).join("bin/shep-otel").is_file());
}

#[tokio::test]
async fn a_row_without_an_approved_version_still_takes_a_ref() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    let dog = row(DogSourceKind::Cargo {
        version: None,
        sha256: std::collections::BTreeMap::new(),
    });

    let (code, _out, err) = run(&fixture, &dog, &with_ref("3f9c2a1"), &reach, typing(None)).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(ref_root(&fixture).join("bin/shep-otel").is_file());
}

#[tokio::test]
async fn the_prompt_says_the_index_has_not_reviewed_the_ref_directly_above_the_question() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    let args = InstallArgs {
        yes: false,
        ..with_ref("3f9c2a1")
    };

    let (code, _out, err) = run(&fixture, &cargo_row(), &args, &reach, typing(Some("n\n"))).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let lines: Vec<_> = err.lines().collect();
    let question = lines
        .iter()
        .position(|line| line.contains("[y/N]"))
        .unwrap_or_else(|| panic!("no question was asked:\n{err}"));
    assert_eq!(
        lines[question - 1].trim_start(),
        format!("built from 3f9c2a1 at {COMMIT}, which the index has not reviewed"),
        "{err}"
    );
}

#[tokio::test]
async fn the_unreviewed_line_is_said_once_when_yes_asks_nothing() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &with_ref("main"),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(
        err.matches("which the index has not reviewed").count(),
        1,
        "{err}"
    );
}

#[tokio::test]
async fn the_receipt_records_the_ref_and_the_commit_and_no_version() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &with_ref("main"),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.paths.dogs_dir.join("otel/receipt.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["ref"], "main");
    assert_eq!(receipt["commit"], COMMIT);
    assert!(receipt.get("version").is_none(), "{receipt}");
    assert_eq!(receipt["package"], "shep-otel");
    assert_eq!(receipt["command"][0], "cargo");
    assert!(
        receipt["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|word| word == "--branch"),
        "{receipt}"
    );
    assert_eq!(
        receipt["binary"],
        ref_root(&fixture).join("bin/shep-otel").to_str().unwrap()
    );
}

#[tokio::test]
async fn a_build_that_fails_as_tag_and_as_branch_adopts_nothing() {
    let fixture = Fixture::new();
    let mut reach = fixture.reach(from_the_row());
    // A cargo that always fails.
    let failing = fixture.dir.path().join("failing-bin");
    std::fs::create_dir_all(&failing).unwrap();
    let cargo = failing.join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\nexit 101\n").unwrap();
    std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    reach.cargo = Some(cargo);

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &with_ref("nope"),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains("nope"), "{err}");
    assert!(err.contains("tag") && err.contains("branch"), "{err}");
    assert!(config(&fixture).adopted_dog_path("otel").is_none());
    assert!(!ref_root(&fixture).exists());
}

#[tokio::test]
async fn a_ref_that_could_pass_for_a_cargo_flag_is_refused_before_cargo_runs() {
    for bad in ["", "--config", "-x", "a b", "a\nb"] {
        let fixture = Fixture::new();
        let reach = fixture.reach(from_the_row());

        let (code, _out, err) =
            run(&fixture, &cargo_row(), &with_ref(bad), &reach, typing(None)).await;

        assert_eq!(code, ExitCode::InvalidConfig, "{bad:?}");
        assert!(err.contains("--ref"), "{err}");
        assert!(
            attempts(&fixture).is_empty(),
            "cargo must not have run for {bad:?}"
        );
    }
}

#[tokio::test]
async fn a_row_that_is_not_a_cargo_row_takes_no_ref() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    let dog = row(DogSourceKind::GoInstall {
        module: "example.com/otel".to_owned(),
    });

    let (code, out, _err) = run(&fixture, &dog, &with_ref("main"), &reach, typing(None)).await;

    assert_eq!(code, ExitCode::Failure);
    assert!(out.contains("go install example.com/otel@latest"), "{out}");
    assert!(attempts(&fixture).is_empty());
}

#[test]
fn ref_with_cargo_is_a_usage_error() {
    let err =
        Cli::try_parse_from(["shep", "install", "otel", "--ref", "main", "--cargo"]).unwrap_err();

    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    assert_eq!(err.exit_code(), 2);
}

#[test]
fn ref_parses_into_the_install_args() {
    let parsed = Cli::try_parse_from(["shep", "install", "otel", "--ref", "main"]).unwrap();

    let Commands::Install(args) = parsed.command else {
        panic!("expected install");
    };
    assert_eq!(args.git_ref.as_deref(), Some("main"));
}
