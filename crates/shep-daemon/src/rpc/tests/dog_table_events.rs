//! `config.sheep.<dog>`: announced once per dog and sheep whenever the
//! answer to `DogSheepSettings` moves, and never when it does not.

use super::dog_tables::{apply, carrying, declared_jobs, set_table, start, table};
use super::*;
use crate::bus::{SharedEvent, TopicFilter};
use shep_core::config::DogTable;
use shep_core::protocol::BusEvent;
use tokio::sync::broadcast;

/// Every `DogSheepSettingsChanged` queued on `rx`.
///
/// A `ListFlock` goes through the actor first. It reads its mailbox in
/// order and announces after each message, so once that reply is back
/// every earlier request's announcement is queued.
pub(super) async fn heard(
    ctx: &RpcContext,
    rx: &mut broadcast::Receiver<SharedEvent>,
) -> Vec<BusEvent> {
    list_flock(ctx, 0).await;
    let mut heard = Vec::new();
    while let Ok(event) = rx.try_recv() {
        let event = event.to_event();
        if matches!(event, BusEvent::DogSheepSettingsChanged { .. }) {
            heard.push(event);
        }
    }
    heard
}

/// `(dog, sheep)` of each event in `events` a subscriber to `topic` gets.
pub(super) fn on(topic: &str, events: &[BusEvent]) -> Vec<(String, String)> {
    let filter = TopicFilter::new(&[topic.to_string()]).expect("a valid topic pattern");
    events
        .iter()
        .filter(|event| filter.matches(event))
        .filter_map(|event| match event {
            BusEvent::DogSheepSettingsChanged { dog, sheep } => Some((dog.clone(), sheep.clone())),
            _ => None,
        })
        .collect()
}

/// The one announcement `sheep`'s `jobs` table moving makes.
fn jobs_on(sheep: &str) -> Vec<(String, String)> {
    vec![("jobs".to_string(), sheep.to_string())]
}

/// Sends one `SetSheepDogSettings` and asserts it landed.
async fn set(ctx: &RpcContext, id: u64, dog: &str, table: Option<DogTable>) {
    let reply = set_table(ctx, id, "web", dog, table).await;
    assert!(reply.is_ok(), "{reply:?}");
}

/// Every trigger a dog's own sheep can pull, in one lifetime of `web`:
/// registered carrying a table, a write that changes nothing, an edit,
/// another dog's removal, this dog's removal and return, and a delete.
#[tokio::test(start_paused = true)]
async fn a_dog_hears_each_change_to_its_table_on_a_sheep_once() {
    let h = harness(vec![ProcScript::never_exits()]);
    let mut rx = h.ctx.events.subscribe();
    let jobs =
        |n: u64| table(serde_json::json!({ "concurrency": n, "hours": { "start": "09:00" } }));
    let deploy = table(serde_json::json!({ "branch": "main" }));
    start(
        &h.ctx,
        1,
        vec![carrying("web", vec![("jobs", jobs(2)), ("deploy", deploy)])],
    )
    .await;
    let events = heard(&h.ctx, &mut rx).await;
    assert_eq!(on("config.sheep.jobs", &events), jobs_on("web"));
    assert_eq!(on("config.sheep.*", &events).len(), 2, "{events:?}");

    set(&h.ctx, 2, "jobs", Some(jobs(2))).await;
    assert_eq!(
        heard(&h.ctx, &mut rx).await,
        [],
        "the same table again moved no answer"
    );

    set(&h.ctx, 3, "jobs", Some(jobs(4))).await;
    assert_eq!(
        on("config.sheep.jobs", &heard(&h.ctx, &mut rx).await),
        jobs_on("web")
    );

    set(&h.ctx, 4, "deploy", None).await;
    let events = heard(&h.ctx, &mut rx).await;
    assert_eq!(on("config.sheep.jobs", &events), []);
    assert_eq!(
        on("config.sheep.deploy", &events),
        [("deploy".to_string(), "web".to_string())]
    );

    set(&h.ctx, 5, "jobs", None).await;
    assert_eq!(
        on("config.sheep.jobs", &heard(&h.ctx, &mut rx).await),
        jobs_on("web")
    );
    set(&h.ctx, 6, "jobs", Some(jobs(8))).await;
    assert_eq!(
        on("config.sheep.jobs", &heard(&h.ctx, &mut rx).await),
        jobs_on("web")
    );

    let deleted = reply_of(
        dispatch(
            envelope(
                7,
                Request::Delete {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(deleted.result.is_ok(), "{:?}", deleted.result);
    assert_eq!(
        on("config.sheep.*", &heard(&h.ctx, &mut rx).await),
        jobs_on("web")
    );
}

/// `Add` registers through `RegisterAtRest`, not `Start`, and is
/// announced all the same, on the carrying dog's topic alone.
#[tokio::test(start_paused = true)]
async fn another_dogs_table_is_silent_on_this_dogs_topic() {
    let h = harness(vec![]);
    let mut rx = h.ctx.events.subscribe();
    let added = reply_of(
        dispatch(
            envelope(
                1,
                Request::Add {
                    apps: vec![carrying(
                        "api",
                        vec![("deploy", table(serde_json::json!({ "branch": "main" })))],
                    )],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(added.result.is_ok(), "{:?}", added.result);

    let events = heard(&h.ctx, &mut rx).await;
    assert_eq!(on("config.sheep.jobs", &events), []);
    assert_eq!(
        on("config.sheep.deploy", &events),
        [("deploy".to_string(), "api".to_string())]
    );
}

/// The name already carried the table, so a second instance, and its
/// removal, change nobody's answer.
#[tokio::test(start_paused = true)]
async fn scaling_a_sheep_carrying_a_table_announces_nothing() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    let mut rx = h.ctx.events.subscribe();
    start(
        &h.ctx,
        1,
        vec![carrying(
            "web",
            vec![("jobs", table(serde_json::json!({ "concurrency": 2 })))],
        )],
    )
    .await;
    assert_eq!(
        on("config.sheep.jobs", &heard(&h.ctx, &mut rx).await),
        jobs_on("web")
    );

    for (id, count) in [(2, 2), (3, 1)] {
        let scaled = reply_of(
            dispatch(
                envelope(
                    id,
                    Request::Scale {
                        name: "web".to_string(),
                        count,
                    },
                ),
                &h.ctx,
            )
            .await,
        );
        assert!(scaled.result.is_ok(), "{:?}", scaled.result);
        assert_eq!(heard(&h.ctx, &mut rx).await, [], "scaled to {count}");
    }
}

/// A load is announced when it moves a table and silent when it does
/// not, including a plain load an established table holds off.
#[tokio::test(start_paused = true)]
async fn a_load_is_heard_only_when_it_moves_a_table() {
    let h = harness(vec![ProcScript::never_exits()]);
    let mut rx = h.ctx.events.subscribe();
    start(&h.ctx, 1, vec![AppConfig::minimal("web", "./srv")]).await;
    assert_eq!(heard(&h.ctx, &mut rx).await, []);

    for (id, n, reset, expected) in [
        (2, 2, ResetDepth::None, jobs_on("web")),
        (3, 2, ResetDepth::File, Vec::new()),
        (4, 4, ResetDepth::None, Vec::new()),
        (5, 4, ResetDepth::File, jobs_on("web")),
    ] {
        apply(&h.ctx, id, declared_jobs(n), reset).await;
        assert_eq!(
            on("config.sheep.jobs", &heard(&h.ctx, &mut rx).await),
            expected,
            "concurrency {n} at {reset:?}"
        );
    }
}
