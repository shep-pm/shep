use std::path::Path;
use std::time::Duration;

use serde_json::json;
use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

use super::*;
use crate::dogs::{DogIdentity, dog_config};
use crate::testing::sample_ack;
use crate::testing::{Handovers, Handshake, control_address, fake_daemon_across_handovers};

#[dog_config]
#[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
struct Jobs {
    #[shep(dog_wide)]
    poll: String,
    model: String,
    concurrency: u32,
}

fn table(value: serde_json::Value) -> DogTable {
    match value {
        serde_json::Value::Object(map) => DogTable::from(map),
        other => panic!("not a table: {other}"),
    }
}

fn sheep(id: u32, name: &str) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online).build()
}

/// A runtime for the dog `jobs` against a fake shepherd that serves
/// `section`, a flock of `flock`, and `tables`.
async fn started(
    dir: &Path,
    section: &str,
    flock: &[&str],
    tables: &[(&str, serde_json::Value)],
) -> (DogRuntime, Handovers) {
    started_under(dir, section, None, flock, tables).await
}

/// [`started`], with the operator's `jobs.defaults` set to `default`. The
/// shepherd lays it under every table it sends in `DogSheepSettings` and
/// keeps each sheep's own table apart in its config.
async fn started_under(
    dir: &Path,
    section: &str,
    default: Option<serde_json::Value>,
    flock: &[&str],
    tables: &[(&str, serde_json::Value)],
) -> (DogRuntime, Handovers) {
    let socket = control_address(dir);
    let shepherd = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
    shepherd.reply_to_dog_config(section);
    shepherd.reply_to_list(
        flock
            .iter()
            .zip(1..)
            .map(|(name, id)| sheep(id, name))
            .collect(),
    );
    let default = default.map_or_else(|| table(json!({})), table);
    shepherd.reply_to_dog_sheep_settings(
        tables
            .iter()
            .map(|(name, value)| {
                (
                    (*name).to_owned(),
                    layered(&[&default, &table(value.clone())]),
                )
            })
            .collect(),
    );
    shepherd.reply_to_sheep_config(
        tables
            .iter()
            .map(|(name, value)| {
                let own = BTreeMap::from([("jobs".to_owned(), table(value.clone()))]);
                ((*name).to_owned(), own)
            })
            .collect(),
    );
    let mut paths = shep_core::paths::ShepPaths::resolve(&|_| None, dir);
    paths.socket = socket;
    let runtime = tokio::time::timeout(
        Duration::from_secs(5),
        DogRuntime::start(DogIdentity::named("jobs"), paths),
    )
    .await
    .expect("DogRuntime::start hung instead of connecting")
    .unwrap();
    (runtime, shepherd)
}

async fn read(
    runtime: &DogRuntime,
    serves: Serves,
) -> BTreeMap<String, Result<Jobs, ServedSheepError>> {
    tokio::time::timeout(
        Duration::from_secs(5),
        runtime.sheep_settings::<Jobs>(serves),
    )
    .await
    .expect("sheep_settings hung instead of answering")
    .unwrap()
}

fn jobs(model: &str, concurrency: u32) -> Jobs {
    Jobs {
        poll: String::new(),
        model: model.to_owned(),
        concurrency,
    }
}

#[tokio::test]
async fn a_sheeps_table_overrides_one_key_and_the_rest_come_from_the_section() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started(
        dir.path(),
        "model = \"opus\"\nconcurrency = 2\npoll = \"1m\"\n",
        &["web", "api"],
        &[("web", json!({ "model": "sonnet" }))],
    )
    .await;

    let settings = read(&runtime, Serves::OptedIn).await;

    assert_eq!(
        settings.keys().collect::<Vec<_>>(),
        ["web"],
        "a dog served opted-in sheep only hears of the ones with a table"
    );
    assert_eq!(settings["web"], Ok(jobs("sonnet", 2)));
}

#[tokio::test]
async fn a_dog_wide_key_in_a_sheeps_table_is_that_sheeps_error_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started(
        dir.path(),
        "model = \"opus\"\n",
        &["web", "api", "db"],
        &[
            ("web", json!({ "concurrency": 3 })),
            ("api", json!({ "poll": "5s", "model": "haiku" })),
            ("db", json!({ "model": "sonnet" })),
        ],
    )
    .await;

    let settings = read(&runtime, Serves::OptedIn).await;

    assert_eq!(settings["web"], Ok(jobs("opus", 3)));
    assert_eq!(settings["db"], Ok(jobs("sonnet", 0)));
    let refused = settings["api"].as_ref().unwrap_err();
    assert_eq!(
        refused,
        &ServedSheepError::DogWide {
            sheep: "api".to_owned(),
            keys: vec!["poll".to_owned()],
        }
    );
    assert!(refused.to_string().contains("poll"), "{refused}");
}

#[tokio::test]
async fn a_dog_serving_every_sheep_gets_the_section_for_a_sheep_with_no_table() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started(
        dir.path(),
        "model = \"opus\"\nconcurrency = 2\npoll = \"1m\"\n",
        &["web", "api"],
        &[("web", json!({ "concurrency": 4 }))],
    )
    .await;

    let settings = read(&runtime, Serves::EverySheep).await;

    assert_eq!(settings.keys().collect::<Vec<_>>(), ["api", "web"]);
    assert_eq!(
        settings["api"],
        Ok(jobs("opus", 2)),
        "the section without `poll`, which is dog-wide"
    );
    assert_eq!(settings["web"], Ok(jobs("opus", 4)));
}

#[tokio::test]
async fn a_dog_in_the_flock_is_not_a_sheep_it_serves() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, shepherd) = started(dir.path(), "", &["web"], &[]).await;
    let mut dog = sheep(2, "bark");
    dog.dog = Some(shep_core::protocol::DogSource::BuiltIn {
        grants: None,
        events: Vec::new(),
    });
    shepherd.reply_to_list(vec![sheep(1, "web"), dog]);

    let settings = read(&runtime, Serves::EverySheep).await;

    assert_eq!(settings.keys().collect::<Vec<_>>(), ["web"]);
}

#[tokio::test]
async fn a_dog_with_no_section_and_no_dog_wide_key_gets_the_tables_as_they_are() {
    #[dog_config]
    #[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields, default)]
    struct Plain {
        model: String,
        concurrency: u32,
    }
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started(
        dir.path(),
        "",
        &["web", "api"],
        &[
            ("web", json!({ "model": "sonnet", "concurrency": 3 })),
            ("api", json!({})),
        ],
    )
    .await;

    let settings = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.sheep_settings::<Plain>(Serves::OptedIn),
    )
    .await
    .expect("sheep_settings hung instead of answering")
    .unwrap();

    assert_eq!(
        settings["web"],
        Ok(Plain {
            model: "sonnet".to_owned(),
            concurrency: 3
        })
    );
    assert_eq!(settings["api"], Ok(Plain::default()));
    assert_eq!(settings.len(), 2);
}

#[tokio::test]
async fn a_table_that_does_not_fit_is_that_sheeps_error_and_quotes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started(
        dir.path(),
        "",
        &["web", "api"],
        &[
            ("web", json!({ "concurrency": "hunter2" })),
            ("api", json!({ "concurrency": 1 })),
        ],
    )
    .await;

    let settings = read(&runtime, Serves::OptedIn).await;

    let refused = settings["web"].as_ref().unwrap_err();
    assert_eq!(
        refused.to_string(),
        "the settings for web, its [app.dogs.jobs] table over [jobs], do not fit this dog's settings"
    );
    assert!(!format!("{refused:?}").contains("hunter2"));
    assert_eq!(settings["api"], Ok(jobs("", 1)));
}

#[tokio::test]
async fn a_section_that_is_not_toml_fails_the_whole_read() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started(dir.path(), "model = = \n", &["web"], &[]).await;

    let err = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.sheep_settings::<Jobs>(Serves::EverySheep),
    )
    .await
    .expect("sheep_settings hung instead of answering")
    .unwrap_err();

    assert!(matches!(err, SheepSettingsReadError::Section(_)), "{err}");
}

#[test]
fn a_dog_wide_error_names_every_key_and_no_value() {
    let err = ServedSheepError::DogWide {
        sheep: "api".to_owned(),
        keys: vec!["poll".to_owned(), "region".to_owned()],
    };

    assert_eq!(
        err.to_string(),
        "the table on api sets poll, region, which is dog-wide: set it in the dog's own section"
    );
    assert_eq!(err.sheep(), "api");
}

#[tokio::test]
async fn a_secret_the_shepherd_cannot_resolve_is_that_sheeps_error_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, shepherd) = started(
        dir.path(),
        "",
        &["web", "api"],
        &[
            ("web", json!({ "model": "{{secret:api_key}}" })),
            ("api", json!({ "model": "haiku" })),
        ],
    )
    .await;
    shepherd.refuse_dog_config_resolved(shep_core::protocol::RpcError {
        code: shep_core::protocol::RpcErrorCode::Unsupported,
        message: "this shepherd does not implement the request".to_owned(),
        daemon_version: None,
    });

    let settings = read(&runtime, Serves::OptedIn).await;

    assert!(
        matches!(&settings["web"], Err(ServedSheepError::Unresolved { sheep, .. }) if sheep == "web"),
        "{:?}",
        settings["web"]
    );
    assert_eq!(settings["api"], Ok(jobs("haiku", 0)));
}

#[tokio::test]
async fn an_operator_default_for_the_dog_reaches_no_sheep_with_or_without_a_table() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started_under(
        dir.path(),
        "model = \"opus\"\n",
        Some(json!({ "model": "haiku" })),
        &["web", "api"],
        &[("web", json!({ "concurrency": 3 }))],
    )
    .await;

    let settings = read(&runtime, Serves::EverySheep).await;

    assert_eq!(settings["web"], Ok(jobs("opus", 3)));
    assert_eq!(
        settings["api"],
        Ok(jobs("opus", 0)),
        "both sheep read the section, whether or not they carry a table"
    );
}

#[tokio::test]
async fn a_dog_wide_key_only_in_the_operator_default_is_no_sheeps_error() {
    let dir = tempfile::tempdir().unwrap();
    let (runtime, _shepherd) = started_under(
        dir.path(),
        "poll = \"1m\"\n",
        Some(json!({ "poll": "5s" })),
        &["web", "api"],
        &[
            ("web", json!({ "concurrency": 3 })),
            ("api", json!({ "model": "sonnet" })),
        ],
    )
    .await;

    let settings = read(&runtime, Serves::OptedIn).await;

    assert_eq!(settings["web"], Ok(jobs("", 3)));
    assert_eq!(settings["api"], Ok(jobs("sonnet", 0)));
}

#[test]
fn a_resolved_table_keeps_the_sheeps_own_keys_and_none_of_the_defaults() {
    let own = table(json!({
        "model": "{{secret:model}}",
        "limits": { "token": "{{secret:token}}" },
        "concurrency": 2,
    }));
    let resolved = table(json!({
        "model": "opus",
        "limits": { "token": "t0k3n", "burst": 9 },
        "concurrency": 2,
        "poll": "5s",
    }));

    assert_eq!(
        resolved_like(&own, Some(&resolved)),
        table(json!({
            "model": "opus",
            "limits": { "token": "t0k3n" },
            "concurrency": 2,
        }))
    );
}
