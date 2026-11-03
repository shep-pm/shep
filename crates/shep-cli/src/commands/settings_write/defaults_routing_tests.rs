//! The retired `<dog>.defaults` address: `get`, `set`, `unset` and `copy` all
//! refuse it, name the dog's own section, and send nothing that writes, also
//! when a sheep or an unrun dog shares the dog's name.

use std::path::Path;

use shep_client::Client;
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, Envelope, ProcessInfo, Request, Response};
use shep_core::status::ProcStatus;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cli::{CopyArgs, Format, GetArgs, SetArgs, UnsetArgs};
use crate::commands::{copy, settings_read};
use crate::exit::ExitCode;
use crate::output::Streams;

/// A shepherd running `sheep` as sheep and `dogs` as dogs.
async fn shepherd(
    dir: &Path,
    sheep: &[&str],
    dogs: &[&str],
) -> (Client, UnboundedReceiver<Envelope>) {
    let flock: Vec<ProcessInfo> = sheep
        .iter()
        .map(|name| ProcessInfo::builder(1, *name, ProcStatus::Online).build())
        .chain(dogs.iter().map(|name| {
            ProcessInfo::builder(2, *name, ProcStatus::Online)
                .dog(Some(DogSource::BuiltIn {
                    grants: None,
                    events: Vec::new(),
                }))
                .build()
        }))
        .collect();
    fake_client_answering_refusals(&control_address(dir), move |request| match request {
        Request::ListFlock => Ok(Response::Flock(flock.clone())),
        Request::DogConfig { .. } => Ok(Response::DogSection {
            toml: String::new().into(),
        }),
        Request::DogSheepSettings { .. } => Ok(Response::DogSheepSettings {
            tables: Default::default(),
        }),
        _other => Ok(Response::Pong),
    })
    .await
}

/// What a verb sent, and what it wrote to stderr and exited with.
struct Outcome {
    code: ExitCode,
    said: String,
    sent: Vec<Request>,
}

async fn outcome(
    paths: &ShepPaths,
    client: &Client,
    mut envelopes: UnboundedReceiver<Envelope>,
    verb: impl AsyncFnOnce(&Client, &mut Streams<'_>, &ShepPaths) -> ExitCode,
) -> Outcome {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        verb(client, &mut streams, paths).await
    };
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        sent.push(envelope.body);
    }
    Outcome {
        code,
        said: String::from_utf8_lossy(&err).to_string(),
        sent,
    }
}

/// The text every verb says of the retired address.
const SECTION_ADDRESS: &str = "`metrics.otlp_timeout`";

/// `set` refuses the address and names the dog's own section, though a sheep
/// has the dog's name: the sheep's config is not read, and nothing is written.
#[tokio::test]
async fn set_refuses_a_defaults_address_and_names_the_dogs_section() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, envelopes) = shepherd(dir.path(), &["metrics"], &["metrics"]).await;
    let args = SetArgs {
        address: "metrics.defaults.otlp_timeout".to_string(),
        value: "12s".to_string(),
        dry_run: true,
        yes: true,
    };

    let ran = outcome(
        &paths,
        &client,
        envelopes,
        async |client, streams, paths| super::set(client, streams, paths, &args).await,
    )
    .await;

    assert_eq!(ran.code, ExitCode::InvalidConfig, "{}", ran.said);
    assert!(ran.said.contains("is retired"), "{}", ran.said);
    assert!(ran.said.contains(SECTION_ADDRESS), "{}", ran.said);
    assert!(
        ran.sent
            .iter()
            .all(|request| matches!(request, Request::ListFlock)),
        "only the flock was read: {:?}",
        ran.sent
    );
}

/// `unset` refuses it the same way, not as a sheep's invalid path.
#[tokio::test]
async fn unset_refuses_a_defaults_address_and_names_the_dogs_section() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, envelopes) = shepherd(dir.path(), &["metrics"], &["metrics"]).await;
    let args = UnsetArgs {
        address: "metrics.defaults.otlp_timeout".to_string(),
        dry_run: true,
        yes: true,
    };

    let ran = outcome(
        &paths,
        &client,
        envelopes,
        async |client, streams, paths| super::unset(client, streams, paths, &args).await,
    )
    .await;

    assert_eq!(ran.code, ExitCode::InvalidConfig, "{}", ran.said);
    assert!(ran.said.contains(SECTION_ADDRESS), "{}", ran.said);
}

/// `get` refuses the whole address too, and names the key placeholder when no
/// key follows.
#[tokio::test]
async fn get_refuses_a_defaults_address_and_names_the_dogs_section() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, envelopes) = shepherd(dir.path(), &["metrics"], &["metrics"]).await;
    let args = GetArgs {
        address: "metrics.defaults".to_string(),
    };

    let ran = outcome(
        &paths,
        &client,
        envelopes,
        async |client, streams, paths| settings_read::get(client, streams, paths, &args).await,
    )
    .await;

    assert_eq!(ran.code, ExitCode::InvalidConfig, "{}", ran.said);
    assert!(ran.said.contains("`metrics.<key>`"), "{}", ran.said);
}

/// `copy` refuses it when a sheep has the dog's name.
#[tokio::test]
async fn copy_refuses_a_defaults_address_when_a_sheep_has_the_dogs_name() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, envelopes) = shepherd(dir.path(), &["metrics", "web"], &["metrics"]).await;
    let args = copy_args(
        "metrics.defaults.otlp_timeout",
        "web.dogs.metrics.otlp_timeout",
    );

    let ran = outcome(
        &paths,
        &client,
        envelopes,
        async |client, streams, paths| copy::copy(client, streams, paths, &args).await,
    )
    .await;

    assert_eq!(ran.code, ExitCode::InvalidConfig, "{}", ran.said);
    assert!(ran.said.contains(SECTION_ADDRESS), "{}", ran.said);
}

/// A dog this shepherd knows and no sheep runs still owns the address, so
/// `copy` refuses it as the retired address and not as a missing sheep.
#[tokio::test]
async fn copy_refuses_a_defaults_address_of_a_known_dog_no_flock_row_names() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, envelopes) = shepherd(dir.path(), &["web"], &[]).await;
    let args = copy_args(
        "metrics.defaults.otlp_timeout",
        "web.dogs.metrics.otlp_timeout",
    );

    let ran = outcome(
        &paths,
        &client,
        envelopes,
        async |client, streams, paths| copy::copy(client, streams, paths, &args).await,
    )
    .await;

    assert_ne!(ran.code, ExitCode::Success);
    assert!(ran.said.contains(SECTION_ADDRESS), "{}", ran.said);
    assert!(!ran.said.contains("no sheep named"), "{}", ran.said);
}

fn copy_args(from: &str, to: &str) -> CopyArgs {
    CopyArgs {
        from: from.to_string(),
        to: to.to_string(),
        replace: false,
        except: Vec::new(),
        dry_run: true,
        yes: true,
    }
}

/// A defaults address with an empty segment is a malformed address, and `copy`
/// reports it as one rather than as a defaults address.
#[tokio::test]
async fn copy_reports_a_defaults_address_with_an_empty_key_as_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, envelopes) = shepherd(dir.path(), &["web"], &["metrics"]).await;
    let args = copy_args("metrics.defaults.", "web.dogs.metrics.otlp_timeout");

    let ran = outcome(
        &paths,
        &client,
        envelopes,
        async |client, streams, paths| copy::copy(client, streams, paths, &args).await,
    )
    .await;

    assert_ne!(ran.code, ExitCode::Success);
    assert!(!ran.said.contains("is retired"), "{}", ran.said);
}
