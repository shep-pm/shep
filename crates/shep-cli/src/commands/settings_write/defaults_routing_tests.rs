//! Where a defaults address goes when a sheep or an unrun dog shares its name:
//! to the dog's defaults, by `set`, `unset` and `copy` alike.

use std::path::Path;

use shep_client::Client;
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, Envelope, ProcessInfo, Request, Response};
use shep_core::status::ProcStatus;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cli::{CopyArgs, Format, SetArgs, UnsetArgs};
use crate::commands::copy;
use crate::exit::ExitCode;
use crate::output::Streams;

/// A shepherd running `names` as sheep and `dogs` as dogs, which answers a
/// sheep's config with a failure the verbs cannot carry on past.
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
                .dog(Some(DogSource::BuiltIn { grants: None }))
                .build()
        }))
        .collect();
    fake_client_answering_refusals(&control_address(dir), move |request| match request {
        Request::ListFlock => Ok(Response::Flock(flock.clone())),
        Request::DogConfig { .. } => Ok(Response::DogSection {
            toml: String::new().into(),
        }),
        Request::DogDefaults { .. } => Ok(Response::DogDefaults { table: None }),
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

/// A sheep named after a dog does not take the dog's defaults address: the
/// write is the dog's.
#[tokio::test]
async fn set_writes_a_dogs_default_when_a_sheep_has_the_dogs_name() {
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

    assert_eq!(ran.code, ExitCode::Success, "{}", ran.said);
    assert!(
        ran.sent
            .iter()
            .any(|request| matches!(request, Request::DogDefaults { dog } if dog == "metrics")),
        "the dog's default was read: {:?}",
        ran.sent
    );
    assert!(
        !ran.sent
            .iter()
            .any(|request| matches!(request, Request::SheepConfig { .. })),
        "the sheep was not read: {:?}",
        ran.sent
    );
}

/// `unset` answers a defaults address with the defaults refusal, not a sheep's
/// invalid path, when a sheep has the dog's name.
#[tokio::test]
async fn unset_refuses_a_defaults_key_as_the_dogs_when_a_sheep_has_the_dogs_name() {
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

    assert_ne!(ran.code, ExitCode::Success);
    assert!(
        ran.said
            .contains("does not remove a key of `metrics`'s defaults"),
        "{}",
        ran.said
    );
}

/// `copy` answers a defaults address with the defaults refusal when a sheep
/// has the dog's name.
#[tokio::test]
async fn copy_refuses_a_defaults_address_as_the_dogs_when_a_sheep_has_the_dogs_name() {
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

    assert_ne!(ran.code, ExitCode::Success);
    assert!(
        ran.said.contains("shep set metrics.defaults"),
        "{}",
        ran.said
    );
}

/// A dog this shepherd knows and no sheep runs still owns its defaults
/// address, so `copy` refuses it as a defaults address and not as a missing
/// sheep.
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
    assert!(
        ran.said.contains("shep set metrics.defaults"),
        "{}",
        ran.said
    );
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
