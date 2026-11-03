//! A dog's per-sheep tables: `DogSheepSettings` reads one dog's tables,
//! `SetSheepDogSettings` writes one, and a load merges `dogs` as one field.

use super::*;
use shep_core::config::DogTable;

/// A table from a JSON object literal.
///
/// # Panics
///
/// If `value` is not an object, which is a fixture bug.
pub(super) fn table(value: serde_json::Value) -> DogTable {
    let serde_json::Value::Object(map) = value else {
        panic!("a table fixture is an object")
    };
    DogTable::from(map)
}

/// `name` carrying one table per `(dog, table)` pair.
pub(super) fn carrying(name: &str, tables: Vec<(&str, DogTable)>) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.dogs = tables
        .into_iter()
        .map(|(dog, table)| (dog.to_string(), table))
        .collect();
    app
}

/// Starts `apps` and asserts the start landed.
pub(super) async fn start(ctx: &RpcContext, id: u64, apps: Vec<AppConfig>) {
    let started = reply_of(dispatch(envelope(id, Request::Start { apps }), ctx).await);
    assert!(started.result.is_ok(), "{:?}", started.result);
}

/// What `DogSheepSettings { dog }` answers.
async fn tables_for(ctx: &RpcContext, id: u64, dog: &str) -> BTreeMap<String, DogTable> {
    let reply = reply_of(
        dispatch(
            envelope(
                id,
                Request::DogSheepSettings {
                    dog: dog.to_string(),
                },
            ),
            ctx,
        )
        .await,
    );
    match reply.result {
        Ok(Response::DogSheepSettings { tables }) => tables,
        other => panic!("expected DogSheepSettings, got {other:?}"),
    }
}

/// Sends one `SetSheepDogSettings` and hands back the reply.
pub(super) async fn set_table(
    ctx: &RpcContext,
    id: u64,
    name: &str,
    dog: &str,
    table: Option<DogTable>,
) -> Result<Response, RpcError> {
    reply_of(
        dispatch(
            envelope(
                id,
                Request::SetSheepDogSettings {
                    name: name.to_string(),
                    dog: dog.to_string(),
                    table,
                },
            ),
            ctx,
        )
        .await,
    )
    .result
}

/// Asserts `reply` is the success answer for `name` and `dog`.
#[track_caller]
fn assert_set(reply: &Result<Response, RpcError>, name: &str, dog: &str) {
    assert_eq!(
        reply,
        &Ok(Response::SheepDogSettingsSet {
            name: name.to_string(),
            dog: dog.to_string(),
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_dog_reads_its_own_table_from_every_sheep_and_nobody_elses() {
    let h = harness(vec![ProcScript::never_exits(); 4]);
    let jobs_on_web = table(serde_json::json!({ "concurrency": 2, "hours": { "start": "09:00" } }));
    let deploy_on_web = table(serde_json::json!({ "branch": "main" }));
    let deploy_on_api = table(serde_json::json!({ "branch": "release" }));
    start(
        &h.ctx,
        1,
        vec![
            carrying(
                "web",
                vec![
                    ("jobs", jobs_on_web.clone()),
                    ("deploy", deploy_on_web.clone()),
                ],
            ),
            carrying("api", vec![("deploy", deploy_on_api.clone())]),
            carrying("worker", Vec::new()),
        ],
    )
    .await;
    enable_dog(&h.ctx, 2, "bark").await;

    assert_eq!(
        tables_for(&h.ctx, 3, "jobs").await,
        BTreeMap::from([("web".to_string(), jobs_on_web)]),
        "api carries only another dog's table and worker carries none"
    );
    assert_eq!(
        tables_for(&h.ctx, 4, "deploy").await,
        BTreeMap::from([
            ("api".to_string(), deploy_on_api),
            ("web".to_string(), deploy_on_web),
        ])
    );
    assert_eq!(
        tables_for(&h.ctx, 5, "nobody").await,
        BTreeMap::new(),
        "a dog nobody names is an empty map, never NotFound"
    );
    assert_eq!(tables_for(&h.ctx, 6, "bark").await, BTreeMap::new());
}

/// Set, replace and remove, each asserted against the second dog's
/// table on the same sheep: the write builds a whole `dogs` map, and a
/// map built from the wrong base would drop it.
#[tokio::test(start_paused = true)]
async fn a_write_sets_replaces_and_removes_one_table_and_leaves_the_other_dogs() {
    let h = harness(vec![ProcScript::never_exits()]);
    let deploy = table(serde_json::json!({ "branch": "main" }));
    start(
        &h.ctx,
        1,
        vec![carrying("web", vec![("deploy", deploy.clone())])],
    )
    .await;
    let first = table(serde_json::json!({ "concurrency": 2 }));
    let second = table(serde_json::json!({ "concurrency": 4, "merge": { "mode": "ask" } }));

    for (id, next) in [(2, Some(first)), (4, Some(second)), (6, None)] {
        let reply = set_table(&h.ctx, id, "web", "jobs", next.clone()).await;
        assert_set(&reply, "web", "jobs");
        let expected = next.map_or_else(BTreeMap::new, |next| {
            BTreeMap::from([("web".to_string(), next)])
        });
        assert_eq!(tables_for(&h.ctx, id + 1, "jobs").await, expected);
        assert_eq!(
            tables_for(&h.ctx, id + 1, "deploy").await,
            BTreeMap::from([("web".to_string(), deploy.clone())]),
            "the write to jobs moved deploy's table"
        );
    }
}

/// Recorded as an override of the whole field, so both the pane's `*`
/// and the CFG column name it, and recorded to the roll, because nothing
/// on the restore path reads the override store.
#[tokio::test(start_paused = true)]
async fn a_write_is_an_operator_override_of_dogs_and_reaches_the_muster_roll() {
    let h = harness(vec![ProcScript::never_exits()]);
    start(&h.ctx, 1, vec![carrying("web", Vec::new())]).await;
    // An unrelated override the write must leave standing.
    let restarts = reply_of(
        dispatch(
            envelope(
                3,
                Request::SetSheepField {
                    name: "web".to_string(),
                    key: "max_restarts".to_string(),
                    value: serde_json::json!(40),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(restarts.result.is_ok(), "{:?}", restarts.result);

    let jobs = table(serde_json::json!({ "concurrency": 2, "token": "hunter2" }));
    let reply = set_table(&h.ctx, 4, "web", "jobs", Some(jobs.clone())).await;
    assert_set(&reply, "web", "jobs");

    let stored = shep_core::overrides::get(&h.ctx.paths.overrides, "web")
        .unwrap()
        .expect("the write is recorded");
    assert_eq!(
        stored.fields["dogs"],
        serde_json::json!({ "jobs": { "concurrency": 2, "token": "hunter2" } })
    );
    assert_eq!(stored.fields["max_restarts"], 40, "an unrelated override");

    let view = sheep_config_view(&h.ctx, 5, "web").await;
    assert_eq!(view.overridden, ["dogs", "max_restarts"]);
    let infos = list_flock(&h.ctx, 6).await;
    let web = infos.iter().find(|info| info.name == "web").unwrap();
    assert_eq!(
        web.overridden.as_deref(),
        Some(&["dogs".to_string(), "max_restarts".to_string()][..])
    );

    let roll = h.ctx.registry.roll(&infos, 0);
    let entry = roll
        .apps
        .iter()
        .find(|entry| entry.app.name == "web")
        .expect("web is in the roll");
    assert_eq!(entry.app.dogs, BTreeMap::from([("jobs".to_string(), jobs)]));
    assert_eq!(entry.app.max_restarts, 40);
}

/// A dog runs at the daemon's own trust level and is never in the
/// override store; the store is asserted as well as the code, since a
/// refusal that still wrote would be the same hole with a better error.
#[tokio::test(start_paused = true)]
async fn a_dogs_own_name_is_refused_and_an_unknown_sheep_is_not_found() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_dog(&h.ctx, 1, "bark").await;
    let jobs = table(serde_json::json!({ "concurrency": 2 }));

    let Err(err) = set_table(&h.ctx, 2, "bark", "jobs", Some(jobs.clone())).await else {
        panic!("a dog was given a table")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("bark is a dog"), "{}", err.message);
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "bark")
            .unwrap()
            .is_none(),
        "the refusal still wrote the store"
    );

    let Err(err) = set_table(&h.ctx, 3, "ghost", "jobs", Some(jobs)).await else {
        panic!("a sheep nobody registered took a table")
    };
    assert_eq!(err.code, RpcErrorCode::NotFound);
}

/// `normalize` refuses an empty dog name the way it refuses one in a
/// Flockfile; the write is checked before the store is touched, so the
/// refusal leaves it untouched too.
#[tokio::test(start_paused = true)]
async fn an_empty_dog_name_is_refused_and_writes_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    start(&h.ctx, 1, vec![carrying("web", Vec::new())]).await;
    let jobs = table(serde_json::json!({ "concurrency": 2 }));

    let Err(err) = set_table(&h.ctx, 2, "web", "", Some(jobs)).await else {
        panic!("an empty dog name was accepted")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "the refusal still wrote the store"
    );
}

/// A whole-map write from a pane editing one dog would overwrite a
/// concurrent edit to another dog's table.
#[tokio::test(start_paused = true)]
async fn set_sheep_field_refuses_dogs_and_names_the_request_that_owns_it() {
    let h = harness(vec![ProcScript::never_exits()]);
    start(&h.ctx, 1, vec![carrying("web", Vec::new())]).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::SetSheepField {
                    name: "web".to_string(),
                    key: "dogs".to_string(),
                    value: serde_json::json!({ "jobs": { "concurrency": 2 } }),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("a whole dogs map was accepted")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(
        err.message.contains("SetSheepDogSettings"),
        "{}",
        err.message
    );
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "the refusal still wrote the store"
    );
}

/// `web` declaring `dogs` with `concurrency` at `n`, as a Flockfile would.
pub(super) fn declared_jobs(n: u64) -> DeclaredApp {
    DeclaredApp {
        config: carrying(
            "web",
            vec![("jobs", table(serde_json::json!({ "concurrency": n })))],
        ),
        declared: ["dogs".to_string()].into_iter().collect(),
        declared_env: BTreeSet::new(),
    }
}

/// Sends one `ApplyConfig` of `app` at `reset` and hands back its report.
pub(super) async fn apply(
    ctx: &RpcContext,
    id: u64,
    app: DeclaredApp,
    reset: ResetDepth,
) -> shep_core::protocol::SheepApplied {
    let reply = reply_of(
        dispatch(
            envelope(
                id,
                Request::ApplyConfig {
                    apps: vec![app],
                    reset,
                },
            ),
            ctx,
        )
        .await,
    );
    let Ok(Response::Applied(mut report)) = reply.result else {
        panic!("expected Applied, got {:?}", reply.result)
    };
    report.remove(0)
}

/// `dogs` merges as one field, the way `level_rules` does: once a load has
/// established it, a plain load of a changed table leaves the stored one
/// and the drift report names it, `--reset=env` leaves it too, and
/// `--reset=file` applies it.
#[tokio::test(start_paused = true)]
async fn a_plain_load_leaves_an_established_table_and_reset_file_applies_it() {
    let h = harness(vec![ProcScript::never_exits()]);
    start(&h.ctx, 1, vec![AppConfig::minimal("web", "./srv")]).await;
    let first = apply(&h.ctx, 2, declared_jobs(2), ResetDepth::None).await;
    assert_eq!(first.applied, ["dogs"], "nobody had established it yet");
    let in_force = tables_for(&h.ctx, 3, "jobs").await;

    for (id, reset) in [(4, ResetDepth::None), (6, ResetDepth::Env)] {
        let report = apply(&h.ctx, id, declared_jobs(4), reset).await;
        assert!(report.applied.is_empty(), "{reset:?}: {report:?}");
        assert_eq!(
            tables_for(&h.ctx, id + 1, "jobs").await,
            in_force,
            "{reset:?} moved an established table"
        );
    }
    let drift = reply_of(
        dispatch(
            envelope(
                8,
                Request::ConfigDrift {
                    apps: vec![declared_jobs(4).config],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Drifted(drifted)) = drift.result else {
        panic!("expected Drifted, got {:?}", drift.result)
    };
    assert_eq!(drifted.len(), 1);
    assert_eq!(drifted[0].fields, ["dogs"]);

    let report = apply(&h.ctx, 9, declared_jobs(4), ResetDepth::File).await;
    assert_eq!(report.applied, ["dogs"]);
    assert_eq!(
        tables_for(&h.ctx, 10, "jobs").await,
        BTreeMap::from([(
            "web".to_string(),
            table(serde_json::json!({ "concurrency": 4 }))
        )])
    );
}
