//! A pane through a real shepherd: the `pane_dog` example answers `shep.pane`
//! as the operator triggers it, and what comes back is a pane `Pane::read`
//! accepts.
//!
//! The dog is spawned by the shepherd holding its own channel. The manifest
//! is read off the same binary, so `Probe::pane` and `pane_answer` are shown
//! to agree.
//!
//! Real time throughout: the dog is a real process on a real socket.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use shep_client::Client;
use shep_client::dogs::{Manifest, PANE_ACTION, Pane, Section};
use shep_core::dogs::MANIFEST_FLAG;
use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;
use shep_core::protocol::{ActionOutcome, DogSource, Request, Response, SelectorSpec};
use shep_daemon::boot::{BootOptions, boot};
use shep_daemon::dogs::DogSpec;
use shep_daemon::rpc::RpcContext;
use shep_daemon::tokio_runner::TokioRunner;

/// Slack for a loaded runner, never an expected duration.
const WAIT: Duration = Duration::from_secs(30);

/// The example's binary, next to this test's own under `deps/`.
fn example() -> PathBuf {
    let test_binary = std::env::current_exe().expect("a running test knows its own path");
    let path = test_binary
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives at <profile>/deps/<name>")
        .join("examples")
        .join("pane_dog");
    assert!(
        path.is_file(),
        "{} must exist: run `cargo build -p shep-client --examples`, or a plain \
         `cargo test -p shep-client`; `--test pane` alone does not build it",
        path.display()
    );
    path
}

/// A booted shepherd holding the pane dog, on a home of its own.
struct Shepherd {
    home: PathBuf,
    socket: PathBuf,
    ctx: RpcContext,
    run: Option<tokio::task::JoinHandle<Result<(), shep_daemon::boot::BootError>>>,
    _dir: tempfile::TempDir,
}

impl Shepherd {
    async fn boot() -> Self {
        // `$SHEP_HOME` is the tempdir root: `sun_path` is short on macOS.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let paths = ShepPaths::resolve(
            &|key| (key == "SHEP_HOME").then(|| home.display().to_string()),
            Path::new("/nonexistent"),
        );
        let dogs = vec![DogSpec {
            name: "pane-dog".to_owned(),
            source: DogSource::Adopted {
                path: example().display().to_string(),
                channel: true,
                grants: Some(Grants::parse(Vec::<&str>::new()).expect("no grants")),
                events: Vec::new(),
            },
            env: BTreeMap::new(),
        }];
        let daemon = tokio::time::timeout(
            WAIT,
            boot(
                TokioRunner::new(),
                paths,
                BootOptions {
                    dogs: dogs.clone(),
                    known_dogs: dogs,
                    ..BootOptions::default()
                },
            ),
        )
        .await
        .expect("boot must not hang")
        .expect("the shepherd must boot");
        let socket = daemon.socket().to_path_buf();
        let ctx = daemon.context();
        let run = Some(tokio::spawn(daemon.run()));
        Self {
            home,
            socket,
            ctx,
            run,
            _dir: dir,
        }
    }

    /// Waits until the dog is answering on its channel.
    async fn ready(&self) {
        let path = self.home.join("pane.ready");
        tokio::time::timeout(WAIT, async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{} never appeared", path.display()));
    }

    async fn shutdown(mut self) {
        self.ctx.shutdown();
        let run = self.run.take().expect("taken once");
        tokio::time::timeout(WAIT, run)
            .await
            .expect("teardown must not hang")
            .unwrap()
            .unwrap();
    }
}

impl Drop for Shepherd {
    fn drop(&mut self) {
        if self.run.is_some() {
            self.ctx.shutdown();
        }
    }
}

/// Triggers `action` at the dog as the operator and returns its reply body.
async fn trigger(client: &Client, action: &str, params: Option<&str>) -> String {
    let response = tokio::time::timeout(
        WAIT,
        client.request(Request::Trigger {
            selector: SelectorSpec::Name("pane-dog".to_owned()),
            action: action.to_owned(),
            params: params.map(str::to_owned),
        }),
    )
    .await
    .expect("the trigger must not hang")
    .expect("the shepherd answers");

    let Response::Triggered(rows) = response else {
        panic!("expected Triggered, got {response:?}");
    };
    assert_eq!(rows.len(), 1, "{rows:?}");
    let ActionOutcome::Replied { body } = &rows[0].outcome else {
        panic!("the dog did not reply: {:?}", rows[0].outcome);
    };
    body.clone()
}

async fn connect(shepherd: &Shepherd) -> Client {
    tokio::time::timeout(WAIT, Client::connect(&shepherd.socket))
        .await
        .expect("connect must not hang")
        .expect("the operator connects")
}

#[tokio::test(flavor = "multi_thread")]
async fn triggering_shep_pane_answers_the_examples_pane() {
    let shepherd = Shepherd::boot().await;
    shepherd.ready().await;
    let client = connect(&shepherd).await;

    let body = trigger(&client, PANE_ACTION, None).await;

    let pane = Pane::read(&body).expect("lookout accepts what the dog answered");
    let [Section::Fields(host), Section::Table(models)] = pane.sections.as_slice() else {
        panic!("expected a fields section then a table: {pane:?}");
    };
    assert_eq!(host.id, "host");
    assert_eq!(models.id, "models");
    let keys: Vec<&str> = models.rows.iter().map(|row| row.key.as_str()).collect();
    assert_eq!(keys, ["qwen3-32b", "llama-3-8b"]);
    assert_eq!(models.row_actions[0].action, "unload");
    assert!(models.row_actions[0].destructive);

    client.close().await.ok();
    shepherd.shutdown().await;
}

/// The row action a pane declares reaches a handler, and the next pull shows
/// what it did.
#[tokio::test(flavor = "multi_thread")]
async fn a_row_action_the_pane_declares_is_answered_and_the_next_pane_shows_it() {
    let shepherd = Shepherd::boot().await;
    shepherd.ready().await;
    let client = connect(&shepherd).await;

    let reply = trigger(&client, "unload", Some(r#"{"row":"qwen3-32b"}"#)).await;

    assert_eq!(reply, "unloaded qwen3-32b");
    let pane = Pane::read(&trigger(&client, PANE_ACTION, None).await).unwrap();
    let Section::Table(models) = &pane.sections[1] else {
        panic!("the second section is the table: {pane:?}");
    };
    let keys: Vec<&str> = models.rows.iter().map(|row| row.key.as_str()).collect();
    assert_eq!(keys, ["llama-3-8b"]);

    client.close().await.ok();
    shepherd.shutdown().await;
}

#[test]
fn the_examples_manifest_declares_the_pane_it_answers() {
    let output = Command::new(example())
        .arg(MANIFEST_FLAG)
        .output()
        .expect("the example runs");

    let manifest: Manifest =
        serde_json::from_slice(&output.stdout).expect("the example prints a manifest");
    assert!(manifest.channel);
    assert!(manifest.pane);
    assert!(!manifest.pane_ignored);
}
