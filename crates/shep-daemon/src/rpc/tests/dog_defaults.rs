//! A dog's operator-set default: layered under every table a sheep carries
//! for that dog, at read time, and held in no sheep's `dogs`.

use super::dog_tables::{apply, carrying, declared_jobs, set_table, start, table};
use super::*;
use shep_core::config::DogTable;

/// Sends one `SetDogDefaults` and asserts it landed.
pub(super) async fn set_default(ctx: &RpcContext, id: u64, dog: &str, table: Option<DogTable>) {
    let reply = reply_of(
        dispatch(
            envelope(
                id,
                Request::SetDogDefaults {
                    dog: dog.to_string(),
                    table,
                },
            ),
            ctx,
        )
        .await,
    );
    assert_eq!(
        reply.result,
        Ok(Response::DogDefaultsSet {
            dog: dog.to_string()
        })
    );
}

async fn tables_for(ctx: &RpcContext, id: u64, dog: &str) -> BTreeMap<String, DogTable> {
    let body = Request::DogSheepSettings {
        dog: dog.to_string(),
    };
    match reply_of(dispatch(envelope(id, body), ctx).await).result {
        Ok(Response::DogSheepSettings { tables }) => tables,
        other => panic!("expected DogSheepSettings, got {other:?}"),
    }
}

async fn resolved(ctx: &RpcContext, id: u64, name: &str, dog: &str) -> Option<DogTable> {
    let body = Request::SheepDogSettingsResolved {
        name: name.to_string(),
        dog: dog.to_string(),
    };
    match reply_of(dispatch(envelope(id, body), ctx).await).result {
        Ok(Response::SheepDogTable { table }) => table,
        other => panic!("expected SheepDogTable, got {other:?}"),
    }
}

async fn default_of(ctx: &RpcContext, id: u64, dog: &str) -> Option<DogTable> {
    let body = Request::DogDefaults {
        dog: dog.to_string(),
    };
    match reply_of(dispatch(envelope(id, body), ctx).await).result {
        Ok(Response::DogDefaults { table }) => table,
        other => panic!("expected DogDefaults, got {other:?}"),
    }
}

fn operator_jobs() -> DogTable {
    table(serde_json::json!({
        "model": "opus",
        "hours": { "start": "08:00", "end": "17:00" },
        "labels": ["a", "b"],
    }))
}

#[tokio::test(start_paused = true)]
async fn a_dog_reads_every_layered_table_and_nobody_elses() {
    let h = harness(vec![ProcScript::never_exits(); 3]);
    let deploy = table(serde_json::json!({ "branch": "main" }));
    start(
        &h.ctx,
        1,
        vec![
            carrying(
                "web",
                vec![(
                    "jobs",
                    table(serde_json::json!({
                        "model": "sonnet",
                        "hours": { "start": "09:00" },
                        "labels": ["c"],
                    })),
                )],
            ),
            carrying("api", vec![("jobs", table(serde_json::json!({})))]),
            carrying("worker", vec![("deploy", deploy.clone())]),
        ],
    )
    .await;
    set_default(&h.ctx, 2, "jobs", Some(operator_jobs())).await;

    assert_eq!(
        tables_for(&h.ctx, 3, "jobs").await,
        BTreeMap::from([
            (
                "api".to_string(),
                table(serde_json::json!({
                    "model": "opus",
                    "hours": { "start": "08:00", "end": "17:00" },
                    "labels": ["a", "b"],
                }))
            ),
            (
                "web".to_string(),
                table(serde_json::json!({
                    "model": "sonnet",
                    "hours": { "start": "09:00", "end": "17:00" },
                    "labels": ["c"],
                }))
            ),
        ]),
        "worker carries no jobs table and takes no default"
    );
    assert_eq!(
        tables_for(&h.ctx, 4, "deploy").await,
        BTreeMap::from([("worker".to_string(), deploy)]),
        "a default for one dog reaches no other dog's table"
    );
    assert_eq!(
        resolved(&h.ctx, 5, "api", "jobs").await,
        Some(operator_jobs()),
        "the resolved read agrees with the plain one"
    );
}

#[tokio::test(start_paused = true)]
async fn a_default_reaches_every_sheep_carrying_one_and_stops_there() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    start(
        &h.ctx,
        1,
        vec![
            carrying("web", vec![("jobs", table(serde_json::json!({})))]),
            carrying("worker", Vec::new()),
        ],
    )
    .await;
    assert_eq!(default_of(&h.ctx, 2, "jobs").await, None);
    set_default(&h.ctx, 3, "jobs", Some(operator_jobs())).await;
    assert_eq!(default_of(&h.ctx, 4, "jobs").await, Some(operator_jobs()));

    assert_eq!(
        resolved(&h.ctx, 5, "web", "jobs").await,
        Some(operator_jobs())
    );
    assert_eq!(resolved(&h.ctx, 6, "worker", "jobs").await, None);
    assert_eq!(
        tables_for(&h.ctx, 7, "jobs")
            .await
            .keys()
            .collect::<Vec<_>>(),
        ["web"]
    );

    let view = sheep_config_view(&h.ctx, 8, "web").await;
    assert_eq!(
        view.config.dogs,
        BTreeMap::from([("jobs".to_string(), table(serde_json::json!({})))]),
        "nothing is baked into the sheep's own `dogs`"
    );
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "a default is no sheep's override"
    );

    set_default(&h.ctx, 9, "jobs", None).await;
    assert_eq!(default_of(&h.ctx, 10, "jobs").await, None);
    assert_eq!(
        resolved(&h.ctx, 11, "web", "jobs").await,
        Some(table(serde_json::json!({})))
    );
}

#[tokio::test(start_paused = true)]
async fn a_sheep_reset_picks_up_the_current_default() {
    let h = harness(vec![ProcScript::never_exits()]);
    start(
        &h.ctx,
        1,
        vec![carrying(
            "web",
            vec![("jobs", table(serde_json::json!({ "concurrency": 2 })))],
        )],
    )
    .await;
    let own = table(serde_json::json!({ "concurrency": 9 }));
    set_table(&h.ctx, 2, "web", "jobs", Some(own))
        .await
        .unwrap();
    set_default(
        &h.ctx,
        3,
        "jobs",
        Some(table(serde_json::json!({ "model": "opus" }))),
    )
    .await;
    assert_eq!(
        resolved(&h.ctx, 4, "web", "jobs").await,
        Some(table(
            serde_json::json!({ "concurrency": 9, "model": "opus" })
        ))
    );

    set_default(
        &h.ctx,
        5,
        "jobs",
        Some(table(serde_json::json!({ "model": "sonnet" }))),
    )
    .await;
    let report = apply(&h.ctx, 6, declared_jobs(2), ResetDepth::File).await;
    assert_eq!(report.applied, ["dogs"]);
    assert_eq!(
        resolved(&h.ctx, 7, "web", "jobs").await,
        Some(table(
            serde_json::json!({ "concurrency": 2, "model": "sonnet" })
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn an_empty_dog_name_is_refused_and_writes_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::SetDogDefaults {
                    dog: String::new(),
                    table: Some(operator_jobs()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("an empty dog name was accepted")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(!h.ctx.paths.dog_defaults.exists());
}
