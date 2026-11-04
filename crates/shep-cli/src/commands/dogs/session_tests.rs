//! `session::run` end to end with a stub `cargo`: the steps it reports, the
//! output it hands back, and the answer it takes in place of a terminal.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::dog_upgrade::{Goal, upgrade, upgrade_dog};
use super::install::Reach;
use super::install_depends::Catalog;
use super::install_tests::{Fixture, config, enabled, from_the_row, row, typing};
use super::remove::{Removal, remove_dog};
use super::session::{Capture, Job, Progress, Step, run, run_with};
use crate::cli::{Format, UpgradeArgs};
use crate::dog_index::{DogSourceKind, index_url, write_cache};
use crate::exit::ExitCode;
use crate::output::Streams;

const URL: &str = "https://shep-pm.com/dogs.json";

/// Caches an index holding `otel` at `version` for [`URL`].
fn cache_index(fixture: &Fixture, version: &str) {
    cache_index_at(fixture, URL, version);
}

/// Caches an index holding `otel` at `version` for `url`.
fn cache_index_at(fixture: &Fixture, url: &str, version: &str) {
    let document = serde_json::json!({
        "version": 1,
        "dogs": [{
            "name": "Otel",
            "package": "shep-otel",
            "adopt_as": "otel",
            "description": "exports traces",
            "repo": "https://github.com/shep-pm/shep-otel",
            "license": "MIT",
            "category": "metrics",
            "source": { "kind": "cargo", "version": version }
        }]
    });
    write_cache(
        &fixture.paths.dog_index_cache,
        url,
        document.to_string().as_bytes(),
    )
    .unwrap();
}

/// Makes `reach` report to a list the test reads afterwards.
fn listening(reach: &mut Reach) -> Arc<Mutex<Vec<Step>>> {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    reach.progress = Some(Progress::new(move |step| sink.lock().unwrap().push(step)));
    heard
}

fn install_otel() -> Job {
    Job::Install {
        adopt_as: "otel".to_owned(),
    }
}

#[tokio::test]
async fn an_install_builds_adopts_and_tells_the_sink_in_that_order() {
    let fixture = Fixture::new();
    cache_index(&fixture, "0.2.4");
    let mut reach = fixture.reach(from_the_row());
    let heard = listening(&mut reach);

    let (code, output) = run_with(&fixture.paths, URL, install_otel(), reach, |_| true).await;

    assert_eq!(code, ExitCode::Success, "{output}");
    assert!(fixture.paths.dogs_dir.join("otel/receipt.json").exists());
    assert_eq!(*heard.lock().unwrap(), [Step::Building, Step::Adopting]);
}

#[tokio::test]
async fn a_failing_cargo_returns_the_tail_of_its_stderr() {
    let fixture = Fixture::new();
    cache_index(&fixture, "0.2.4");
    let mut reach = fixture.reach(from_the_row());
    let bin = fixture.dir.path().join("failing-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        "#!/bin/sh\n\
         echo 'the first thing cargo said' >&2\n\
         i=0\n\
         while [ $i -lt 600 ]; do echo \"padding padding padding $i\" >&2; i=$((i+1)); done\n\
         echo 'error: could not compile shep-otel' >&2\n\
         exit 101\n",
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    reach.cargo = Some(cargo);
    let _heard = listening(&mut reach);

    let (code, output) = run_with(&fixture.paths, URL, install_otel(), reach, |_| true).await;

    assert_eq!(code, ExitCode::Failure, "{output}");
    assert!(output.contains("could not compile shep-otel"), "{output}");
    assert!(!output.contains("the first thing cargo said"), "{output}");
    assert!(output.len() < 6000, "{} bytes", output.len());
    assert!(!fixture.paths.dogs_dir.join("otel/receipt.json").exists());
}

#[tokio::test]
async fn an_answer_of_no_leaves_the_dog_adopted_and_disabled() {
    let fixture = Fixture::new();
    cache_index(&fixture, "0.2.4");
    let reach = fixture.reach(from_the_row());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&asked);

    let (code, output) = run_with(&fixture.paths, URL, install_otel(), reach, move |prompt| {
        sink.lock().unwrap().push(prompt.listing().to_owned());
        false
    })
    .await;

    assert_eq!(code, ExitCode::Success, "{output}");
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("flock.read")),
        "the callback is handed the prompt"
    );
    assert!(fixture.paths.dogs_dir.join("otel/receipt.json").exists());
    assert!(config(&fixture).adopted_dog_path("otel").is_some());
    assert!(!enabled(&fixture));
}

/// `output` with the fixture's home folder named `HOME`, so two homes compare.
fn at_home(fixture: &Fixture, output: &str) -> String {
    output.replace(fixture.dir.path().to_str().unwrap(), "HOME")
}

/// A fixture with `otel` installed at 0.2.4 through a session.
async fn installed() -> Fixture {
    let fixture = Fixture::new();
    cache_index(&fixture, "0.2.4");
    let reach = fixture.reach(from_the_row());
    let (code, output) = run_with(&fixture.paths, URL, install_otel(), reach, |_| true).await;
    assert_eq!(code, ExitCode::Success, "{output}");
    fixture
}

fn row_at(version: &str) -> crate::dog_index::AvailableDog {
    row(DogSourceKind::Cargo {
        version: Some(version.to_owned()),
        sha256: BTreeMap::new(),
    })
}

#[tokio::test]
async fn an_upgrade_gives_the_exit_code_and_text_upgrade_dog_gives() {
    let by_verb = installed().await;
    let by_session = installed().await;
    cache_index(&by_session, "0.3.0");

    let capture = Capture::default();
    let (mut out, mut err) = (capture.clone(), capture.clone());
    let rows = [row_at("0.3.0")];
    let verb_code = upgrade_dog(
        &mut Streams::bare(&mut out, &mut err, Format::Table),
        &by_verb.paths,
        &by_verb.reach(from_the_row()),
        &mut typing(None),
        &Catalog::new(URL, &rows),
        "otel",
        &Goal::Follow,
    )
    .await;
    let (code, output) = run_with(
        &by_session.paths,
        URL,
        Job::Upgrade {
            name: "otel".to_owned(),
        },
        by_session.reach(from_the_row()),
        |_| true,
    )
    .await;

    assert_eq!(verb_code, ExitCode::Success, "{}", capture.text());
    assert_eq!(code, verb_code, "{output}");
    assert!(
        output.contains("upgraded otel from 0.2.4 to 0.3.0"),
        "{output}"
    );
    assert_eq!(
        at_home(&by_session, &output),
        at_home(&by_verb, &capture.text())
    );
}

/// `shep upgrade otel`, which loads the index and builds its own `Reach` and
/// stdin input. Nothing needs building when the index is at the held version.
#[tokio::test]
async fn run_gives_the_exit_code_and_text_shep_upgrade_gives_for_a_current_dog() {
    let by_verb = installed().await;
    let by_session = installed().await;
    for fixture in [&by_verb, &by_session] {
        cache_index_at(fixture, &index_url(), "0.2.4");
    }

    let capture = Capture::default();
    let (mut out, mut err) = (capture.clone(), capture.clone());
    let verb_code = upgrade(
        &mut Streams::bare(&mut out, &mut err, Format::Table),
        &by_verb.paths,
        &UpgradeArgs {
            dog: Some("otel".to_owned()),
            all: false,
            git_ref: None,
            release: false,
            rollback: false,
            refresh: false,
        },
    )
    .await;
    let (code, output) = run(
        &by_session.paths,
        Job::Upgrade {
            name: "otel".to_owned(),
        },
        Progress::new(|_| {}),
        |_| true,
    )
    .await;

    assert_eq!(verb_code, ExitCode::Success, "{}", capture.text());
    assert_eq!(code, verb_code, "{output}");
    assert!(output.contains("already at 0.2.4"), "{output}");
    assert_eq!(
        at_home(&by_session, &output),
        at_home(&by_verb, &capture.text())
    );
}

#[tokio::test]
async fn a_delete_gives_the_exit_code_and_text_the_verb_gives() {
    let by_verb = installed().await;
    let by_session = installed().await;

    let capture = Capture::default();
    let (mut out, mut err) = (capture.clone(), capture.clone());
    let verb_code = remove_dog(
        &mut Streams::bare(&mut out, &mut err, Format::Table),
        &by_verb.paths,
        "otel",
        Removal::Delete,
    )
    .await;
    let (code, output) = run(
        &by_session.paths,
        Job::Delete {
            name: "otel".to_owned(),
        },
        Progress::new(|_| {}),
        |_| true,
    )
    .await;

    assert_eq!(verb_code, ExitCode::Success, "{}", capture.text());
    assert_eq!(code, verb_code, "{output}");
    assert!(!output.is_empty());
    assert_eq!(
        at_home(&by_session, &output),
        at_home(&by_verb, &capture.text())
    );
    assert!(config(&by_session).adopted_dog_path("otel").is_none());
    assert!(!by_session.paths.dogs_dir.join("otel").exists());
}

#[tokio::test]
async fn a_missing_cache_fails_with_the_index_message_and_fetches_nothing() {
    let fixture = Fixture::new();
    let progress = Progress::new(|_| {});

    let (code, output) = run(&fixture.paths, install_otel(), progress, |_| true).await;

    assert_eq!(code, ExitCode::Failure, "{output}");
    assert!(output.contains("reading the dog index from"), "{output}");
    assert!(output.contains("no cached copy"), "{output}");
}
