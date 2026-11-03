//! `shep set` of a dog-wide key on a sheep is refused before the write.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use shep_client::Client;
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::config::AppConfig;
use shep_core::dogs::manifest::Manifest;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, Envelope, ProcessInfo, Request, Response, SheepConfigView};
use shep_core::status::ProcStatus;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cli::{Format, SetArgs};
use crate::commands::dogs::{ManifestRead, read_manifest};
use crate::exit::ExitCode;
use crate::output::Streams;

/// A flock of sheep `api` and the dog `jobs`, with `api` carrying a table.
async fn shepherd(dir: &Path) -> (Client, UnboundedReceiver<Envelope>) {
    let flock = vec![
        ProcessInfo::builder(1, "api", ProcStatus::Online).build(),
        ProcessInfo::builder(2, "jobs", ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn { grants: None }))
            .build(),
    ];
    let mut config = AppConfig {
        name: "api".into(),
        ..AppConfig::default()
    };
    config.dogs.insert(
        "jobs".into(),
        json!({ "model": "sonnet" }).as_object().cloned().unwrap().into(),
    );
    let view = SheepConfigView::new(config, Vec::new(), Vec::new());
    fake_client_answering_refusals(&control_address(dir), move |request| match request {
        Request::ListFlock => Ok(Response::Flock(flock.clone())),
        Request::SheepConfig { .. } => Ok(Response::SheepConfig(Box::new(view.clone()))),
        Request::SetSheepDogSettings { name, dog, .. } => {
            Ok(Response::SheepDogSettingsSet {
                name: name.clone(),
                dog: dog.clone(),
            })
        }
        _other => Ok(Response::Pong),
    })
    .await
}

/// Adopts `jobs`, a dog whose manifest marks `poll` dog-wide.
///
/// The manifest is read once with a long budget so the cache holds it: the
/// first run of a fresh script can outlast the one second a lookup gets.
#[cfg(unix)]
fn adopt_jobs(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut manifest = Manifest::new("jobs", "1.0.0", 1);
    manifest.settings = Some(json!({
        "type": "object",
        "properties": {
            "poll": { "type": "string", "x-shep-dog-wide": true },
            "model": { "type": "string" },
        },
    }));
    let script = dir.join("shep-jobs");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncat <<'EOF'\n{}\nEOF\n", serde_json::to_string(&manifest).unwrap()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join("shep.toml"),
        format!(
            "[daemon.adopted_dogs]\njobs = {:?}\n",
            script.display().to_string()
        ),
    )
    .unwrap();
    let read = read_manifest(&script, dir, "jobs", Duration::from_secs(30));
    assert!(matches!(read, Ok(ManifestRead::Read(_))), "{read:?}");
}

async fn set(address: &str, value: &str) -> (ExitCode, String, Vec<Request>) {
    let dir = tempfile::tempdir().unwrap();
    adopt_jobs(dir.path());
    let paths = ShepPaths::at(dir.path().to_path_buf());
    let (client, mut envelopes) = shepherd(dir.path()).await;
    let args = SetArgs {
        address: address.to_string(),
        value: value.to_string(),
        dry_run: false,
        yes: true,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        super::set(&client, &mut streams, &paths, &args).await
    };
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        sent.push(envelope.body);
    }
    (code, String::from_utf8_lossy(&err).to_string(), sent)
}

#[cfg(unix)]
#[tokio::test]
async fn a_dog_wide_key_on_a_sheep_is_refused_before_a_write_is_sent() {
    let (code, said, sent) = set("api.dogs.jobs.poll", "5m").await;

    assert_eq!(code, ExitCode::InvalidConfig, "{said} {sent:?}");
    assert!(said.contains("shep set jobs.poll <value>"), "{said}");
    assert!(
        !sent
            .iter()
            .any(|request| matches!(request, Request::SetSheepDogSettings { .. })),
        "a write went out: {sent:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_key_the_sheep_may_override_is_written() {
    let (code, said, sent) = set("api.dogs.jobs.model", "haiku").await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(
        sent.iter()
            .any(|request| matches!(request, Request::SetSheepDogSettings { .. })),
        "{sent:?}"
    );
}
