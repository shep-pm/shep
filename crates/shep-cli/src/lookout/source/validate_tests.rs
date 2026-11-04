//! The gate in [`ClientFlock::send`], against a scripted shepherd: a
//! dashboard write the dog turns down is refused here, and the request that
//! would have written it never goes out.

use std::fmt::Write as _;
use std::path::Path;

use crate::commands::shep_toml::ShepToml;
use serde_json::{Map, Value as Json, json};
use shep_client::RequestError;
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::config::DogTable;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    ActionOutcome, ActionReply, Envelope, Request, Response, RpcError, RpcErrorCode,
};

use super::*;

/// The reason the fixture dog gives.
const REASON: &str = "checkout may not point outside the project";

/// This machine's `$SHEP_HOME`, with `kelpie` adopted at a file of its own
/// and `manifest` cached for it.
fn home_with(dir: &Path, manifest: Option<&str>) -> ShepPaths {
    let paths = ShepPaths::resolve(&|_| None, dir);
    std::fs::create_dir_all(&paths.home).unwrap();
    let binary = dir.join("shep-kelpie");
    std::fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
    // Written by the same writer `shep adopt` uses, so the adopted path is
    // quoted the way TOML quotes a path. A windows runner spells every temp
    // path with backslashes, and a raw backslash in a TOML basic string is an
    // escape sequence, not a separator.
    ShepToml::edit(&paths.daemon_config, |doc| {
        doc.adopt_dog("kelpie", &binary, true).unwrap();
    })
    .unwrap();
    if let Some(manifest) = manifest {
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        context.update(&std::fs::read(&binary).unwrap());
        let hex = context
            .finish()
            .as_ref()
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        std::fs::create_dir_all(paths.home.join("manifests")).unwrap();
        std::fs::write(
            paths.home.join("manifests").join(format!("{hex}.json")),
            manifest,
        )
        .unwrap();
    }
    paths
}

fn validating() -> &'static str {
    r#"{"manifest":1,"name":"kelpie","version":"0.2.0","protocol":12,"channel":true,"validate":true}"#
}

/// A cached manifest with `validate` left out, which is what every dog adopted
/// before this field did anything has.
fn declares_nothing() -> &'static str {
    r#"{"manifest":1,"name":"kelpie","version":"0.2.0","protocol":12,"channel":true}"#
}

/// A shepherd that refuses the ask in the dog's words, and answers anything
/// else with `Pong`.
async fn a_shepherd_that_refuses(
    dir: &Path,
) -> (ClientFlock, tokio::sync::mpsc::UnboundedReceiver<Envelope>) {
    let paths = home_with(dir, Some(validating()));
    let (client, envelopes) =
        fake_client_answering_refusals(&control_address(dir), |request| match request {
            Request::Trigger { .. } => Ok(Response::Triggered(vec![ActionReply {
                id: 4,
                name: "kelpie".to_string(),
                outcome: ActionOutcome::Replied {
                    body: format!(r#"{{"accept":false,"reason":"{REASON}"}}"#),
                },
            }])),
            _other => Ok(Response::Pong),
        })
        .await;
    (
        ClientFlock {
            client,
            socket: None,
            home: Some(paths),
        },
        envelopes,
    )
}

fn a_write_to_kelpies_table_on_api() -> Request {
    Request::SetSheepDogSettings {
        name: "api".to_string(),
        dog: "kelpie".to_string(),
        table: Some(DogTable::from(
            serde_json::from_value::<Map<String, Json>>(json!({ "checkout": "/outside" })).unwrap(),
        )),
    }
}

/// The acceptance criterion for the dashboard, in the two halves that matter:
/// the pane's error carries the dog's sentence, and the shepherd was never
/// asked to write.
#[tokio::test]
async fn a_dashboard_write_the_dog_refuses_never_reaches_the_shepherd() {
    let dir = tempfile::tempdir().unwrap();
    let (flock, mut envelopes) = a_shepherd_that_refuses(dir.path()).await;

    let err = flock
        .send(a_write_to_kelpies_table_on_api())
        .await
        .expect_err("the dog refused");

    assert_eq!(
        err,
        RequestError::Rpc(RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: REASON.to_string(),
            daemon_version: None,
        }),
        "the pane reports a dog's refusal as a refused write: {err:?}"
    );

    let asked = envelopes.recv().await.expect("the ask went out");
    assert!(
        matches!(asked.body, Request::Trigger { .. }),
        "the dog is asked first: {:?}",
        asked.body
    );
    assert_eq!(
        envelopes.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty),
        "the write itself was never sent"
    );
}

/// A dog's own section is its own to validate, and the dashboard writes those
/// too.
#[tokio::test]
async fn a_dashboard_write_to_a_dogs_section_is_asked_about_too() {
    let dir = tempfile::tempdir().unwrap();
    let (flock, mut envelopes) = a_shepherd_that_refuses(dir.path()).await;

    let err = flock
        .send(Request::SetDogConfig {
            name: "kelpie".to_string(),
            toml: "keep = 2\n".to_string().into(),
        })
        .await
        .expect_err("the dog refused");

    let RequestError::Rpc(rpc) = &err else {
        panic!("a dog's refusal reaches the pane as a refused write: {err:?}");
    };
    assert_eq!(rpc.message, REASON);
    let asked = envelopes.recv().await.expect("the ask went out");
    let Request::Trigger { params, .. } = asked.body else {
        panic!("{:?}", asked.body);
    };
    assert_eq!(
        serde_json::from_str::<Json>(&params.expect("a table is sent")).unwrap(),
        json!({ "table": { "keep": 2 } }),
        "the section is proposed as the table the shepherd would parse out of it"
    );
}

/// A dog whose cached manifest declares nothing is not asked, and the
/// dashboard writes it as it always did.
#[tokio::test]
async fn a_dashboard_write_about_a_dog_that_declared_nothing_goes_straight_through() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(declares_nothing()));
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |_| Ok(Response::Pong)).await;
    let flock = ClientFlock {
        client,
        socket: None,
        home: Some(paths),
    };

    assert_eq!(
        flock.send(a_write_to_kelpies_table_on_api()).await,
        Ok(Response::Pong)
    );
    let envelope = envelopes.recv().await.expect("the write went out");
    assert!(
        matches!(envelope.body, Request::SetSheepDogSettings { .. }),
        "{:?}",
        envelope.body
    );
}

/// A shepherd on another machine has no manifests here to read, so its writes
/// are its own machine's `shep set` to be asked about.
#[tokio::test]
async fn a_watched_host_is_never_asked_from_here() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |_| Ok(Response::Pong)).await;
    let flock = ClientFlock {
        client,
        socket: None,
        home: None,
    };

    assert_eq!(
        flock.send(a_write_to_kelpies_table_on_api()).await,
        Ok(Response::Pong)
    );
    let envelope = envelopes.recv().await.expect("the write went out");
    assert!(
        matches!(envelope.body, Request::SetSheepDogSettings { .. }),
        "{:?}",
        envelope.body
    );
}

fn a_pane_pull() -> Request {
    Request::Trigger {
        selector: shep_core::protocol::SelectorSpec::Name("kelpie".to_string()),
        action: "shep.pane".to_string(),
        params: None,
    }
}

/// A pull dials a connection of its own, so one that cannot be dialled is
/// the pull's failure and nothing the shared connection is asked.
#[tokio::test]
async fn a_pull_dials_its_own_connection_and_answers_closed_when_none_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |_| Ok(Response::Pong)).await;
    let flock = ClientFlock {
        client,
        socket: Some(dir.path().join("nobody-listens.sock")),
        home: None,
    };

    assert_eq!(flock.pull(a_pane_pull()).await, Err(RequestError::Closed));
    assert!(
        envelopes.try_recv().is_err(),
        "the shared connection was not used"
    );
}

/// Over `--host` there is no socket to dial, so the pull rides the one
/// connection there is.
#[tokio::test]
async fn a_pull_over_a_watched_host_rides_the_shared_connection() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |_| Ok(Response::Pong)).await;
    let flock = ClientFlock {
        client,
        socket: None,
        home: None,
    };

    assert_eq!(flock.pull(a_pane_pull()).await, Ok(Response::Pong));
    assert!(matches!(
        envelopes.recv().await.expect("it went out").body,
        Request::Trigger { .. }
    ));
}
