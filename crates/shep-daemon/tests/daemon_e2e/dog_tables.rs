//! A dog's per-sheep table over a real socket: a Flockfile's
//! `[app.dogs.jobs]` reaches the dog that reads it, and a write reaches
//! the dog that subscribed to `config.sheep.jobs`.

use shep_core::config::{DogTable, FlockFormat, Flockfile};

use super::*;

/// One sheep's entry as an operator writes it: a nested table and an
/// unquoted TOML time, the case the conversion to a plain string is for.
const FLOCKFILE: &str = r#"
[[app]]
name = "shop-api"
script = "./server"

[app.dogs.jobs]
concurrency = 2
merge = "ask"
hours = { start = 09:00:00, end = 17:00:00 }
"#;

/// A table from a JSON object literal.
fn table(value: serde_json::Value) -> DogTable {
    let serde_json::Value::Object(map) = value else {
        panic!("a table fixture is an object")
    };
    DogTable::from(map)
}

/// The next `config.sheep.*` event this connection receives, re-queueing
/// every other frame, bounded by [`RECV_TIMEOUT`] overall.
async fn next_table_change(client: &mut Client) -> (String, String) {
    tokio::time::timeout(RECV_TIMEOUT, async {
        let mut skipped = Vec::new();
        let found = loop {
            let frame = client.next_frame().await;
            if let ServerFrame::Event(BusEvent::DogSheepSettingsChanged { dog, sheep }) = &frame {
                break (dog.clone(), sheep.clone());
            }
            skipped.push(frame);
        };
        requeue(&mut client.pending, skipped);
        found
    })
    .await
    .expect("timed out waiting for a config.sheep.* event")
}

/// What `DogSheepSettings { dog: "jobs" }` answers on `client`.
async fn jobs_tables(client: &mut Client) -> std::collections::BTreeMap<String, DogTable> {
    let reply = client
        .request(Request::DogSheepSettings {
            dog: "jobs".to_string(),
        })
        .await;
    match reply.result {
        Ok(Response::DogSheepSettings { tables }) => tables,
        other => panic!("expected DogSheepSettings, got {other:?}"),
    }
}

#[tokio::test]
async fn a_dog_hears_its_per_sheep_table_change_end_to_end() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    // The dog and the operator are two connections, as they are in life.
    let mut dog = fixture.connect().await;
    dog.subscribe("config.sheep.jobs").await;
    let mut operator = fixture.connect().await;

    let parsed = Flockfile::parse(FLOCKFILE, FlockFormat::Toml).expect("the Flockfile parses");
    let mut app = forever_app("shop-api");
    app.dogs = parsed.apps[0].dogs.clone();
    // Only another dog's table: nothing here may reach `config.sheep.jobs`.
    let mut other = forever_app("worker");
    other.dogs.insert(
        "deploy".to_string(),
        table(serde_json::json!({ "branch": "main" })),
    );
    let started = operator
        .request(Request::Start {
            apps: vec![app, other],
        })
        .await;
    assert!(started.result.is_ok(), "{:?}", started.result);

    assert_eq!(
        next_table_change(&mut dog).await,
        ("jobs".to_string(), "shop-api".to_string())
    );
    let loaded = table(serde_json::json!({
        "concurrency": 2,
        "merge": "ask",
        "hours": { "start": "09:00:00", "end": "17:00:00" },
    }));
    assert_eq!(
        jobs_tables(&mut dog).await,
        std::collections::BTreeMap::from([("shop-api".to_string(), loaded)])
    );

    let edited = table(serde_json::json!({
        "concurrency": 4,
        "hours": { "start": "08:00:00", "end": "18:00:00" },
    }));
    let set = operator
        .request(Request::SetSheepDogSettings {
            name: "shop-api".to_string(),
            dog: "jobs".to_string(),
            table: Some(edited.clone()),
        })
        .await;
    assert_eq!(
        set.result.unwrap(),
        Response::SheepDogSettingsSet {
            name: "shop-api".to_string(),
            dog: "jobs".to_string(),
        }
    );

    assert_eq!(
        next_table_change(&mut dog).await,
        ("jobs".to_string(), "shop-api".to_string()),
        "the write is the second announcement, and worker's deploy table never was one"
    );
    assert_eq!(
        jobs_tables(&mut dog).await,
        std::collections::BTreeMap::from([("shop-api".to_string(), edited)])
    );

    fixture.shutdown().await;
}

/// A default written by the operator reaches the dog that reads `jobs`
/// tables, layered under what each sheep holds, and announces itself once per
/// sheep carrying a table.
#[tokio::test]
async fn a_dog_reads_the_layered_table_after_the_operator_writes_a_default() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut dog = fixture.connect().await;
    let mut operator = fixture.connect().await;

    let mut shop = forever_app("shop-api");
    shop.dogs.insert(
        "jobs".to_string(),
        table(serde_json::json!({ "concurrency": 2, "hours": { "start": "09:00:00" } })),
    );
    let mut bare = forever_app("bare");
    bare.dogs
        .insert("jobs".to_string(), table(serde_json::json!({})));
    // No table for the dog: a default does not reach this sheep.
    let plain = forever_app("plain");
    let started = operator
        .request(Request::Start {
            apps: vec![shop, bare, plain],
        })
        .await;
    assert!(started.result.is_ok(), "{:?}", started.result);
    dog.subscribe("config.sheep.jobs").await;

    let default = table(serde_json::json!({
        "concurrency": 8,
        "merge": "ask",
        "hours": { "start": "08:00:00", "end": "17:00:00" },
    }));
    let set = operator
        .request(Request::SetDogDefaults {
            dog: "jobs".to_string(),
            table: Some(default),
        })
        .await;
    assert_eq!(
        set.result.unwrap(),
        Response::DogDefaultsSet {
            dog: "jobs".to_string()
        }
    );

    let mut heard = vec![
        next_table_change(&mut dog).await,
        next_table_change(&mut dog).await,
    ];
    heard.sort();
    assert_eq!(
        heard,
        [
            ("jobs".to_string(), "bare".to_string()),
            ("jobs".to_string(), "shop-api".to_string()),
        ],
        "one announcement per sheep whose layered table moved, and none for `plain`"
    );

    let tables = jobs_tables(&mut dog).await;
    assert_eq!(
        tables.keys().map(String::as_str).collect::<Vec<_>>(),
        ["bare", "shop-api"]
    );
    assert_eq!(
        tables["bare"],
        table(serde_json::json!({
            "concurrency": 8,
            "merge": "ask",
            "hours": { "start": "08:00:00", "end": "17:00:00" },
        })),
        "an empty table takes every default"
    );
    assert_eq!(
        tables["shop-api"],
        table(serde_json::json!({
            "concurrency": 2,
            "merge": "ask",
            "hours": { "start": "09:00:00", "end": "17:00:00" },
        })),
        "the sheep's own values win and a nested table merges key by key"
    );

    fixture.shutdown().await;
}
