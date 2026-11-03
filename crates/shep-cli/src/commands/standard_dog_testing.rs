//! A flock with `api` and an adopted dog `jobs` that follows the dog settings
//! standard, for the tests of the verbs that read and write its tables.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value as Json, json};
use shep_client::Client;
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::config::AppConfig;
use shep_core::dogs::manifest::{Manifest, Serves};
use shep_core::protocol::{
    DogSectionToml, DogSource, Envelope, ProcessInfo, Request, Response, SheepConfigView,
};
use shep_core::status::ProcStatus;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::commands::dogs::{ManifestRead, read_manifest};

/// Adopts `jobs` under the home `dir`, a dog whose manifest marks `poll`
/// dog-wide and says it `serves`.
///
/// The manifest is read once with a long budget so the cache holds it: the
/// first run of a fresh script can outlast the one second a lookup gets.
#[cfg(unix)]
pub(crate) fn adopt_jobs(dir: &Path, serves: Serves) {
    use std::os::unix::fs::PermissionsExt;

    let mut manifest = Manifest::new("jobs", "1.0.0", 1);
    manifest.serves = serves;
    manifest.settings = Some(json!({
        "type": "object",
        "properties": {
            "poll": { "type": "string", "x-shep-dog-wide": true },
            "model": { "type": "string" },
            "limit": { "type": "integer" },
        },
    }));
    let script = dir.join("shep-jobs");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat <<'EOF'\n{}\nEOF\n",
            serde_json::to_string(&manifest).unwrap()
        ),
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

/// A shepherd running sheep `api` and the dog `jobs`. `api` carries `own` as
/// its `jobs` table when there is one, and `section` is `jobs`'s section of
/// `dogs.toml`.
pub(crate) async fn shepherd(
    dir: &Path,
    own: Option<Json>,
    section: &str,
) -> (Client, UnboundedReceiver<Envelope>) {
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
    if let Some(own) = own {
        config
            .dogs
            .insert("jobs".into(), own.as_object().cloned().unwrap().into());
    }
    let view = SheepConfigView::new(config, Vec::new(), Vec::new());
    let section = section.to_string();
    fake_client_answering_refusals(&control_address(dir), move |request| match request {
        Request::ListFlock => Ok(Response::Flock(flock.clone())),
        Request::SheepConfig { .. } => Ok(Response::SheepConfig(Box::new(view.clone()))),
        Request::DogConfig { .. } => Ok(Response::DogSection {
            toml: DogSectionToml::from(section.clone()),
        }),
        Request::SetSheepDogSettings { name, dog, .. } => Ok(Response::SheepDogSettingsSet {
            name: name.clone(),
            dog: dog.clone(),
        }),
        _other => Ok(Response::Pong),
    })
    .await
}
