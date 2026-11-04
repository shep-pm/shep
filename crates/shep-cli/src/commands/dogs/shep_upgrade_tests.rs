//! Bare `shep upgrade` end to end: a stub manager and a stub `shep` on a
//! `PATH` of their own, a stub `cargo` for the dogs, and a fake shepherd.

use std::io::Cursor;
#[cfg(unix)]
use std::path::PathBuf;

use semver::Version;
use shep_core::paths::ShepPaths;

use super::consent::Input;
use super::dog_upgrade::upgrade;
use super::install::Reach;
use super::install_depends::Catalog;
use super::shep_upgrade::{Host, Upgrade, run};
use super::test_support::streams;
use crate::cli::{Format, UpgradeArgs};
#[cfg(unix)]
use crate::dog_index::AvailableDog;
use crate::exit::ExitCode;
use crate::install_channel::Channel;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn host(channel: Channel, windows: bool, path_var: Option<std::ffi::OsString>) -> Host {
    Host {
        channel,
        windows,
        target: "x86_64-unknown-linux-gnu",
        path_var,
        running: version("0.24.0"),
    }
}

fn typed(is_terminal: bool, text: &'static str) -> Input {
    Input::reading(is_terminal, Cursor::new(text.as_bytes()))
}

#[tokio::test]
async fn windows_prints_the_command_and_the_reload_and_says_the_flock_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let host = host(Channel::Winget, true, None);
    let latest = version("0.25.0");

    let code = run(
        &mut streams(&mut out, &mut err),
        &mut typed(false, "y\n"),
        &Upgrade {
            paths: &paths,
            host: &host,
            reach: &Reach::real(),
            catalog: &Catalog::new("https://shep-pm.com/dogs.json", &[]),
            origins: &crate::dog_origin::Origins::over(&paths, None),
            shep: Some(&latest),
            reload: false,
        },
    )
    .await;

    let (out, err) = (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    );
    assert_eq!(code, ExitCode::Success, "{err}");
    let said = format!("{out}{err}");
    assert!(said.contains("winget upgrade shep-pm.shep"), "{said}");
    assert!(said.contains("shep daemon reload"), "{said}");
    assert!(said.contains("flock restarts"), "{said}");
}

#[tokio::test]
async fn bare_upgrade_refuses_format_json_naming_the_dogs_form() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = streams(&mut out, &mut err);
    streams.fmt = Format::Json;

    let code = upgrade(
        &mut streams,
        &paths,
        &UpgradeArgs {
            dog: None,
            all: false,
            git_ref: None,
            release: false,
            rollback: false,
            refresh: false,
            reload: false,
        },
    )
    .await;

    assert_eq!(code, ExitCode::Usage);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("shep upgrade --dogs --format json"), "{err}");
    assert!(out.is_empty());
}

#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

#[cfg(unix)]
use shep_client::testing::{fake_daemon_answering_with_ack, sample_info};
#[cfg(unix)]
use shep_core::protocol::{Envelope, Request, Response};
#[cfg(unix)]
use tokio::sync::mpsc::UnboundedReceiver;

#[cfg(unix)]
use crate::commands::dogs::install_tests::{
    Fixture, args, config, from_the_row, row, run as install, typing,
};
#[cfg(unix)]
use crate::commands::dogs::test_support::this_builds_ack;
#[cfg(unix)]
use crate::dog_index::{DogSourceKind, Index};
#[cfg(unix)]
use crate::dog_origin::Origins;

#[cfg(unix)]
const URL: &str = "https://shep-pm.com/dogs.json";

#[cfg(unix)]
/// The origins of the dogs `rows` lists, read the way `upgrade` reads them.
fn origins_of<'a>(paths: &'a ShepPaths, rows: &[AvailableDog]) -> Origins<'a> {
    Origins::over(
        paths,
        Some(Index {
            dogs: rows.to_vec(),
            skipped: 0,
            sanitised: 0,
            shep: None,
        }),
    )
}

#[cfg(unix)]
/// The manifest of the build an upgrade fetches, asking for one permission more.
const WIDER: &str = r#"{"manifest":1,"name":"otel","version":"0.3.0","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock to report on it"},{"name":"logs.read","required":false,"reason":"reads logs"}]}"#;

#[cfg(unix)]
fn at(version: &str) -> AvailableDog {
    row(DogSourceKind::Cargo {
        version: Some(version.to_owned()),
        sha256: BTreeMap::new(),
    })
}

#[cfg(unix)]
/// A home, a `PATH` of stubs and the log they all append to.
struct Scene {
    fixture: Fixture,
    bin: PathBuf,
}

#[cfg(unix)]
impl Scene {
    fn new() -> Self {
        let fixture = Fixture::new();
        let bin = fixture.dir.path().join("path-bin");
        std::fs::create_dir_all(&bin).unwrap();
        Self { fixture, bin }
    }

    fn log_path(&self) -> PathBuf {
        self.fixture.dir.path().join("log")
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A `brew` that logs its arguments and exits `status`.
    fn manager(&self, status: i32) {
        self.script(
            "brew",
            &format!(
                "printf 'brew %s\\n' \"$*\" >> {}\nexit {status}",
                self.log_path().display()
            ),
        );
    }

    /// A `shep` that reports `version` and logs any other call.
    fn shep_reporting(&self, version: &str) {
        self.script(
            "shep",
            &format!(
                "if [ \"$1\" = --version ]; then echo 'shep {version}'; exit 0; fi\n\
                 printf 'shep %s\\n' \"$*\" >> {}",
                self.log_path().display()
            ),
        );
    }

    /// What the stubs were run with, in order.
    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn host(&self, channel: Channel) -> Host {
        host(channel, false, Some(self.bin.clone().into_os_string()))
    }

    /// A dog installed at `release`, as `shep install` leaves it.
    async fn has_otel_at(&self, release: &str) {
        let reach = self.fixture.reach(from_the_row());
        let (code, _out, err) = install(
            &self.fixture,
            &at(release),
            &args(true, false),
            &reach,
            typing(None),
        )
        .await;
        assert_eq!(code, ExitCode::Success, "{err}");
    }

    fn otel_version(&self) -> String {
        let receipt = self.fixture.paths.dogs_dir.join("otel/receipt.json");
        let held: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
        held["version"].as_str().unwrap().to_owned()
    }

    /// Whether the stub cargo was run since the install.
    fn cargo_runs(&self) -> usize {
        std::fs::read_to_string(self.fixture.attempts())
            .map(|text| text.lines().count())
            .unwrap_or_default()
    }

    /// A shepherd of this build, answering every request.
    async fn shepherd(&self) -> UnboundedReceiver<Envelope> {
        std::fs::create_dir_all(&self.fixture.paths.run).unwrap();
        fake_daemon_answering_with_ack(&self.fixture.paths.socket, this_builds_ack(), |request| {
            match request {
                Request::EnableDog { .. } => Response::DogStarted(sample_info()),
                _ => Response::Pong,
            }
        })
        .await
    }

    /// `shep upgrade` against an index naming `latest` and holding `rows`.
    async fn upgrade(
        &self,
        host: &Host,
        latest: &str,
        rows: &[AvailableDog],
        reload: bool,
        mut input: Input,
    ) -> (ExitCode, String, String) {
        let reach = self.fixture.reach(from_the_row());
        let latest = version(latest);
        let origins = origins_of(&self.fixture.paths, rows);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            &mut streams(&mut out, &mut err),
            &mut input,
            &Upgrade {
                paths: &self.fixture.paths,
                host,
                reach: &reach,
                catalog: &Catalog::new(URL, rows),
                origins: &origins,
                shep: Some(&latest),
                reload,
            },
        )
        .await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_current_shep_and_current_dogs_say_so_and_change_nothing() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    scene.manager(0);
    scene.shep_reporting("0.24.0");
    let cargo_before = scene.cargo_runs();

    let (code, out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.24.0",
            &[at("0.2.4")],
            false,
            typed(true, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let said = format!("{out}{err}");
    assert!(said.contains("shep and every dog are current"), "{said}");
    assert!(scene.log().is_empty(), "{:?}", scene.log());
    assert_eq!(scene.cargo_runs(), cargo_before);
    assert_eq!(scene.otel_version(), "0.2.4");
}

#[cfg(unix)]
#[tokio::test]
async fn the_confirm_lists_shep_with_its_command_and_each_dog_with_both_versions() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;

    let (code, out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[at("0.3.0")],
            false,
            typed(true, "n\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(
        out,
        "shep  0.24.0 \u{2192} 0.25.0   brew upgrade shep\n\
         otel  0.2.4  \u{2192} 0.3.0\n"
    );
    assert_eq!(err, "Upgrade these? [Y/n] ");
}

#[cfg(unix)]
#[tokio::test]
async fn declining_the_confirm_changes_nothing() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    scene.manager(0);
    scene.shep_reporting("0.25.0");
    let cargo_before = scene.cargo_runs();

    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[at("0.3.0")],
            false,
            typed(true, "no\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(scene.log().is_empty(), "{:?}", scene.log());
    assert_eq!(scene.cargo_runs(), cargo_before);
    assert_eq!(scene.otel_version(), "0.2.4");
}

#[cfg(unix)]
#[tokio::test]
async fn end_of_input_counts_as_declining() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    scene.manager(0);
    scene.shep_reporting("0.25.0");

    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[at("0.3.0")],
            false,
            typed(false, ""),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(scene.log().is_empty(), "{:?}", scene.log());
    assert_eq!(scene.otel_version(), "0.2.4");
}

#[cfg(unix)]
#[tokio::test]
async fn enter_alone_goes_on() {
    let scene = Scene::new();
    scene.manager(0);
    scene.shep_reporting("0.25.0");

    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[],
            false,
            typed(false, "\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(scene.log(), ["brew upgrade shep"]);
}

#[cfg(unix)]
#[tokio::test]
async fn the_manager_runs_in_the_foreground_and_its_failure_stops_before_the_dogs() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    scene.manager(3);
    scene.shep_reporting("0.25.0");
    let cargo_before = scene.cargo_runs();

    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[at("0.3.0")],
            true,
            typed(true, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Failure, "{err}");
    assert!(err.contains("brew upgrade shep"), "{err}");
    // The manager finished before the run came back, and nothing after it ran.
    assert_eq!(scene.log(), ["brew upgrade shep"]);
    assert_eq!(scene.cargo_runs(), cargo_before);
    assert_eq!(scene.otel_version(), "0.2.4");
}

#[cfg(unix)]
#[tokio::test]
async fn a_new_binary_that_is_not_newer_says_the_channel_has_not_caught_up_and_skips_the_dogs() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    scene.manager(0);
    scene.shep_reporting("0.24.0");
    let cargo_before = scene.cargo_runs();

    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[at("0.3.0")],
            true,
            typed(true, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Failure, "{err}");
    assert!(err.contains("has not caught up"), "{err}");
    assert_eq!(scene.log(), ["brew upgrade shep"]);
    assert_eq!(scene.cargo_runs(), cargo_before);
    assert_eq!(scene.otel_version(), "0.2.4");
}

#[cfg(unix)]
#[tokio::test]
async fn the_reload_is_asked_for_unless_reload_is_given() {
    let asked = Scene::new();
    asked.manager(0);
    asked.shep_reporting("0.25.0");
    let _requests = asked.shepherd().await;

    let (code, _out, err) = asked
        .upgrade(
            &asked.host(Channel::Homebrew),
            "0.25.0",
            &[],
            false,
            typed(true, "y\ny\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(
        err.contains("Reload the shepherd onto 0.25.0 now? [Y/n]"),
        "{err}"
    );
    assert_eq!(asked.log(), ["brew upgrade shep", "shep daemon reload"]);

    let told = Scene::new();
    told.manager(0);
    told.shep_reporting("0.25.0");
    let _requests = told.shepherd().await;

    let (code, _out, err) = told
        .upgrade(
            &told.host(Channel::Homebrew),
            "0.25.0",
            &[],
            true,
            typed(true, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(!err.contains("Reload the shepherd"), "{err}");
    assert_eq!(told.log(), ["brew upgrade shep", "shep daemon reload"]);

    let declined = Scene::new();
    declined.manager(0);
    declined.shep_reporting("0.25.0");
    let _requests = declined.shepherd().await;

    let (code, _out, err) = declined
        .upgrade(
            &declined.host(Channel::Homebrew),
            "0.25.0",
            &[],
            false,
            typed(true, "y\nn\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(declined.log(), ["brew upgrade shep"]);
}

#[cfg(unix)]
#[tokio::test]
async fn the_reload_names_the_version_the_channel_delivered_and_not_the_index_latest() {
    let scene = Scene::new();
    scene.manager(0);
    scene.shep_reporting("0.25.0");
    let _requests = scene.shepherd().await;

    let (code, out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.26.0",
            &[],
            false,
            typed(true, "y\nn\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(
        err.contains("Reload the shepherd onto 0.25.0 now? [Y/n]"),
        "{err}"
    );
    assert!(!err.contains("0.26.0"), "{err}");
    assert!(out.contains("moves it onto 0.25.0"), "{out}");
}

#[cfg(unix)]
#[tokio::test]
async fn no_terminal_and_no_reload_skips_the_reload_and_prints_the_command() {
    let scene = Scene::new();
    scene.manager(0);
    scene.shep_reporting("0.25.0");
    let _requests = scene.shepherd().await;

    let (code, out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[],
            false,
            typed(false, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(scene.log(), ["brew upgrade shep"]);
    let said = format!("{out}{err}");
    assert!(said.contains("shep daemon reload"), "{said}");
    assert!(!said.contains("Reload the shepherd"), "{said}");
}

#[cfg(unix)]
#[tokio::test]
async fn without_a_shepherd_there_is_nothing_to_reload() {
    let scene = Scene::new();
    scene.manager(0);
    scene.shep_reporting("0.25.0");

    let (code, out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[],
            true,
            typed(true, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(scene.log(), ["brew upgrade shep"]);
    assert!(!format!("{out}{err}").contains("daemon reload"));
}

#[cfg(unix)]
#[tokio::test]
async fn the_reload_runs_the_new_binary_and_not_this_one() {
    let scene = Scene::new();
    scene.manager(0);
    scene.shep_reporting("0.25.0");
    let _requests = scene.shepherd().await;
    // The shep on PATH is the only one that records a call; this test
    // binary would answer `daemon reload` by failing to parse it.
    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[],
            true,
            typed(false, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(
        scene.log().last().map(String::as_str),
        Some("shep daemon reload")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_deb_and_an_archive_print_their_steps_and_go_on_to_the_dogs() {
    for (channel, wanted) in [
        (Channel::Deb, "sudo dpkg -i"),
        (
            Channel::Archive,
            "https://github.com/shep-pm/shep/releases/latest/download/shep-x86_64-unknown-linux-gnu.tar.gz",
        ),
    ] {
        let scene = Scene::new();
        scene.has_otel_at("0.2.4").await;
        scene.manager(0);

        let (code, out, err) = scene
            .upgrade(
                &scene.host(channel),
                "0.25.0",
                &[at("0.3.0")],
                false,
                typed(false, "y\n"),
            )
            .await;

        assert_eq!(code, ExitCode::Success, "{channel:?}: {err}");
        let said = format!("{out}{err}");
        assert!(said.contains(wanted), "{channel:?}: {said}");
        assert!(said.contains("SHA256SUMS"), "{channel:?}: {said}");
        assert!(scene.log().is_empty(), "no manager runs for {channel:?}");
        assert_eq!(scene.otel_version(), "0.3.0", "{channel:?}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_dogs_upgrade_after_shep_through_the_step_three_path() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    scene.manager(0);
    scene.shep_reporting("0.25.0");

    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[at("0.3.0")],
            false,
            typed(false, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(scene.otel_version(), "0.3.0");
    assert!(
        scene
            .fixture
            .paths
            .dogs_dir
            .join("otel/0.3.0/bin/shep-otel")
            .is_file()
    );
    assert!(err.contains("upgraded otel from 0.2.4 to 0.3.0"), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_dog_that_fails_does_not_stop_the_rest_and_is_the_exit_code() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    // The row for the dog is gone from the catalog the upgrade reads.
    let (code, err) = {
        let reach = scene.fixture.reach(from_the_row());
        let rows = [at("0.3.0")];
        let origins = origins_of(&scene.fixture.paths, &rows);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            &mut streams(&mut out, &mut err),
            &mut typed(false, "y\n"),
            &Upgrade {
                paths: &scene.fixture.paths,
                host: &scene.host(Channel::Archive),
                reach: &reach,
                catalog: &Catalog::new(URL, &[]),
                origins: &origins,
                shep: None,
                reload: false,
            },
        )
        .await;
        (code, String::from_utf8(err).unwrap())
    };

    assert_eq!(code, ExitCode::NotFound, "{err}");
    assert_eq!(scene.otel_version(), "0.2.4");
}

#[cfg(unix)]
#[tokio::test]
async fn a_newly_declared_permission_still_waits_for_review() {
    let scene = Scene::new();
    scene.has_otel_at("0.2.4").await;
    scene.manager(0);
    scene.shep_reporting("0.25.0");
    scene.fixture.builds_manifest(WIDER);

    let (code, _out, err) = scene
        .upgrade(
            &scene.host(Channel::Homebrew),
            "0.25.0",
            &[at("0.3.0")],
            false,
            typed(false, "y\n"),
        )
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let record = config(&scene.fixture)
        .grant_record("otel")
        .unwrap()
        .unwrap();
    assert_eq!(record.granted.names().collect::<Vec<_>>(), ["flock.read"]);
    assert_eq!(record.waiting.names().collect::<Vec<_>>(), ["logs.read"]);
}
