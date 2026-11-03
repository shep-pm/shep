//! A dog's connection across a handover: the successor serves it without a
//! `Hello`, attributes it, and tells the dog its subscription was re-armed.

use core::time::Duration;

use super::dog_links::serve_dog_links;
use super::link_table::DogLinks;
use super::testing::*;
use crate::dogs::Contact;
use crate::fake::{FIRST_SCRIPTED_PID, ProcScript};
use crate::runner::DogLink;
use crate::testing::harness;
use shep_core::protocol::{BusEvent, Envelope, Request, Response, RpcErrorCode, ServerFrame};
use tokio::sync::{mpsc, watch};
use tokio_util::codec::Framed;

/// What a client reads off a link that is being served: the daemon's own side
/// of a socketpair, with the table and the shutdown switch its server runs on.
pub(super) struct Served {
    pub(super) client: Client,
    pub(super) table: DogLinks,
    _shutdown: watch::Sender<bool>,
}

/// Hands one `resumed` link for `name` to a running [`serve_dog_links`], as a
/// successor's runner does for a connection it adopted.
pub(super) async fn serve(
    ctx: &crate::rpc::RpcContext,
    name: &str,
    resumed: Option<Vec<String>>,
) -> Served {
    serve_link(ctx, name, None, resumed).await
}

/// [`serve`], for the link `name` launched `launch_for` when that is set.
async fn serve_link(
    ctx: &crate::rpc::RpcContext,
    name: &str,
    launch_for: Option<&str>,
    resumed: Option<Vec<String>>,
) -> Served {
    let (server, client) = shep_core::transport::connected_pair().await.unwrap();
    let (links, links_rx) = mpsc::unbounded_channel();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let table = DogLinks::default();
    tokio::spawn(serve_dog_links(
        links_rx,
        ctx.clone(),
        table.clone(),
        shutdown_rx,
    ));
    links
        .send(DogLink {
            name: name.to_string(),
            launch_for: launch_for.map(str::to_string),
            pid: FIRST_SCRIPTED_PID,
            stream: server,
            resumed,
        })
        .unwrap();
    // The server registers the link before it serves it, but on its own task.
    tokio::time::timeout(Duration::from_secs(5), async {
        while table.snapshot().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the link must be entered into the table");
    Served {
        client: Client {
            frames: Framed::new(client, shep_core::protocol::codec()),
        },
        table,
        _shutdown: shutdown,
    }
}

pub(super) async fn ping(client: &mut Client, id: u64) -> ServerFrame {
    client
        .send(&Envelope {
            id,
            deadline_ms: None,
            body: Request::Ping,
        })
        .await;
    client.recv().await
}

#[tokio::test]
async fn a_resumed_link_answers_its_first_request_without_a_hello() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut served = serve(&h.ctx, "metrics", Some(vec![])).await;

    let frame = ping(&mut served.client, 5).await;

    assert!(
        matches!(&frame, ServerFrame::Reply(reply) if reply.id == 5 && reply.result == Ok(Response::Pong)),
        "{frame:?}"
    );
}

#[tokio::test]
async fn a_resumed_link_is_still_attributed_to_its_dog() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    h.ctx.peer_contacts.force_warm();
    let mut served = serve(&h.ctx, "metrics", Some(vec![])).await;

    ping(&mut served.client, 1).await;

    assert!(
        h.ctx.dog_refusals.has_handshook("metrics"),
        "the successor's refusal ledger starts empty and must learn the dog"
    );
    assert_eq!(
        h.ctx.peer_contacts.from_pid(Some(FIRST_SCRIPTED_PID)),
        Contact::Named,
        "a talking dog must not read as silent to the successor's watch"
    );
}

#[tokio::test]
async fn a_resumed_link_re_arms_its_subscription_after_a_drop_notice() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut served = serve(&h.ctx, "metrics", Some(vec!["log.out".to_string()])).await;

    let notice: BusEvent = served.client.recv().await;
    assert!(
        matches!(notice, BusEvent::Dropped { count } if count >= 1),
        "{notice:?}"
    );
    h.ctx
        .events
        .send(
            BusEvent::LogOut {
                id: 0,
                line: "after the exec".to_string(),
            }
            .into(),
        )
        .unwrap();
    let event: BusEvent = served.client.recv().await;
    assert_eq!(
        event,
        BusEvent::LogOut {
            id: 0,
            line: "after the exec".to_string()
        }
    );
}

#[tokio::test]
async fn a_resumed_link_that_had_subscribed_to_nothing_hears_nothing_unasked() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut served = serve(&h.ctx, "metrics", Some(vec![])).await;

    // The first frame the dog reads is the reply to its own request, not a
    // notice ahead of it.
    let frame = ping(&mut served.client, 9).await;

    assert!(matches!(frame, ServerFrame::Reply(reply) if reply.id == 9));
}

#[tokio::test]
async fn a_link_the_dog_has_not_greeted_still_greets() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut served = serve(&h.ctx, "metrics", None).await;
    assert_eq!(served.table.snapshot()[0].topics, None);

    served.client.handshake().await;

    assert_eq!(served.table.snapshot()[0].topics, Some(vec![]));
}

#[tokio::test]
async fn a_subscribe_on_a_link_is_what_the_next_handover_carries() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut served = serve(&h.ctx, "metrics", None).await;
    served.client.handshake().await;

    served.client.subscribe(&["process.*", "log.err"]).await;

    assert_eq!(
        served.table.snapshot()[0].topics,
        Some(vec!["process.*".to_string(), "log.err".to_string()])
    );
}

#[tokio::test]
async fn a_link_leaves_the_table_when_the_dog_hangs_up() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let served = serve(&h.ctx, "metrics", Some(vec![])).await;

    drop(served.client);

    tokio::time::timeout(Duration::from_secs(5), async {
        while !served.table.snapshot().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a closed connection must not be carried");
}

/// The sheep's own table, then another sheep's, over a link with no `Hello`.
async fn read_table(client: &mut Client, id: u64, sheep: &str) -> ServerFrame {
    client
        .send(&Envelope {
            id,
            deadline_ms: None,
            body: Request::SheepDogSettingsResolved {
                name: sheep.to_string(),
                dog: "jobs".to_string(),
            },
        })
        .await;
    client.recv().await
}

#[tokio::test]
async fn a_launch_link_survives_a_handover() {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    let table = shep_core::config::DogTable::from(serde_json::Map::from_iter([(
        "concurrency".to_string(),
        serde_json::json!(2),
    )]));
    let web = crate::testing::app_with("web", |app| {
        app.dogs.insert("jobs".to_string(), table);
    });
    h.ctx.supervisor.start(vec![web]).await.unwrap();
    let topics = vec!["config.sheep.jobs".to_string()];
    let mut served = serve_link(&h.ctx, "jobs", Some("web"), Some(topics.clone())).await;

    let carried = served.table.snapshot();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].name, "jobs");
    assert_eq!(carried[0].launch_for.as_deref(), Some("web"));
    assert_eq!(
        carried[0].topics,
        Some(topics),
        "what the next handover carries"
    );

    let notice: BusEvent = served.client.recv().await;
    assert!(
        matches!(notice, BusEvent::Dropped { count } if count >= 1),
        "{notice:?}"
    );
    let own = read_table(&mut served.client, 1, "web").await;
    assert!(
        matches!(&own, ServerFrame::Reply(reply) if reply.id == 1 && reply.result.is_ok()),
        "{own:?}"
    );
    let other = read_table(&mut served.client, 2, "api").await;
    assert!(
        matches!(&other, ServerFrame::Reply(reply)
            if reply.id == 2 && matches!(&reply.result, Err(err) if err.code == RpcErrorCode::Forbidden)),
        "{other:?}"
    );

    let changed = |sheep: &str| {
        BusEvent::DogSheepSettingsChanged {
            dog: "jobs".to_string(),
            sheep: sheep.to_string(),
        }
        .into()
    };
    h.ctx.events.send(changed("api")).unwrap();
    h.ctx.events.send(changed("web")).unwrap();
    let heard: BusEvent = served.client.recv().await;
    assert_eq!(
        heard,
        BusEvent::DogSheepSettingsChanged {
            dog: "jobs".to_string(),
            sheep: "web".to_string()
        },
        "the re-armed subscription is still scoped to the sheep"
    );
}
