//! A dog's own connection: the identity it carries, and that a claim on it
//! changes nothing. A launch link is here too, for what it hears.

use super::conn_protocol::serve_conn;
use super::origin::Origin;
use super::testing::*;
use crate::dogs::Contact;
use crate::fake::{FIRST_SCRIPTED_PID, ProcScript};
use crate::rpc::RpcContext;
use crate::testing::harness;
use futures_util::StreamExt;
use shep_core::protocol::{
    BusEvent, Envelope, Hello, HelloReply, MIN_SUPPORTED, PROTOCOL_VERSION, Request, Response,
    ServerFrame,
};
use tokio_util::codec::Framed;

/// Serves a fresh connection as the link `name` was spawned holding, under
/// `pid`, and hands back the dog's end.
async fn linked(ctx: &RpcContext, name: &str, pid: u32) -> Client {
    let (server, client) = shep_core::transport::connected_pair().await.unwrap();
    let ctx = ctx.clone();
    let origin = Origin::untracked_dog(name, pid);
    tokio::spawn(async move {
        let _ = serve_conn(server, ctx, origin).await;
    });
    Client {
        frames: Framed::new(client, shep_core::protocol::codec()),
    }
}

#[tokio::test]
async fn a_link_handshakes_as_the_dog_it_was_opened_for() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut client = linked(&h.ctx, "metrics", FIRST_SCRIPTED_PID).await;

    client.handshake().await;

    assert!(h.ctx.dog_refusals.has_handshook("metrics"));
    assert_eq!(
        h.ctx.peer_contacts.from_pid(Some(FIRST_SCRIPTED_PID)),
        Contact::Named,
        "the silence watch must see the dog's own pid, not the shepherd's"
    );
}

#[tokio::test]
async fn a_link_answers_requests() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut client = linked(&h.ctx, "metrics", FIRST_SCRIPTED_PID).await;
    client.handshake().await;

    client
        .send(&Envelope {
            id: 1,
            deadline_ms: None,
            body: Request::Ping,
        })
        .await;
    let frame: ServerFrame = client.recv().await;

    assert!(
        matches!(&frame, ServerFrame::Reply(reply) if reply.result == Ok(Response::Pong)),
        "{frame:?}"
    );
}

#[tokio::test]
async fn a_name_claimed_on_a_link_is_ignored() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut client = linked(&h.ctx, "metrics", FIRST_SCRIPTED_PID).await;

    client
        .send(&Hello {
            client_version: "0.1.0".to_string(),
            protocol: PROTOCOL_VERSION,
            dog_name: Some("impostor".to_string()),
        })
        .await;
    let ack: HelloReply = client.recv().await;
    ack.expect("the handshake is on this daemon's own protocol");

    assert!(h.ctx.dog_refusals.has_handshook("metrics"));
    assert!(
        !h.ctx.dog_refusals.has_handshook("impostor"),
        "the name on the wire must not become an identity"
    );
}

#[tokio::test]
async fn a_refusal_on_a_link_lands_on_the_dog_it_was_opened_for() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    let mut client = linked(&h.ctx, "metrics", FIRST_SCRIPTED_PID).await;

    client
        .send(&Hello {
            client_version: "0.1.0".to_string(),
            protocol: MIN_SUPPORTED - 1,
            dog_name: Some("impostor".to_string()),
        })
        .await;
    let refusal: HelloReply = client.recv().await;
    refusal.expect_err("a protocol below the floor must be refused");
    assert!(client.frames.next().await.is_none());

    assert_eq!(h.ctx.dog_refusals.restarting(), vec!["metrics".to_string()]);
}

#[tokio::test]
async fn a_link_the_dog_never_wrote_to_is_not_contact() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    h.ctx.peer_contacts.force_warm();
    let (server, client) = shep_core::transport::connected_pair().await.unwrap();
    let origin = Origin::untracked_dog("metrics", FIRST_SCRIPTED_PID);
    let served = tokio::spawn(serve_conn(server, h.ctx.clone(), origin));

    drop(client);
    let _ = served.await.unwrap();

    assert_eq!(
        h.ctx.peer_contacts.from_pid(Some(FIRST_SCRIPTED_PID)),
        Contact::None,
        "a dog that never reached the shepherd must not read as having connected"
    );
}

#[tokio::test]
async fn a_link_is_contact_once_the_dog_sends_a_frame() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_dog(&h.ctx, "metrics").await;
    h.ctx.peer_contacts.force_warm();
    let mut client = linked(&h.ctx, "metrics", FIRST_SCRIPTED_PID).await;

    client
        .send(&Hello {
            client_version: "0.1.0".to_string(),
            protocol: PROTOCOL_VERSION,
            dog_name: None,
        })
        .await;
    let ack: HelloReply = client.recv().await;
    ack.expect("the handshake is on this daemon's own protocol");

    assert_eq!(
        h.ctx.peer_contacts.from_pid(Some(FIRST_SCRIPTED_PID)),
        Contact::Named
    );
}

#[tokio::test]
async fn a_launch_link_hears_only_its_own_sheeps_table_changes() {
    let h = harness(vec![]);
    let (server, client) = shep_core::transport::connected_pair().await.unwrap();
    let origin = Origin::untracked_launch("sandbox", "web");
    let ctx = h.ctx.clone();
    tokio::spawn(async move {
        let _ = serve_conn(server, ctx, origin).await;
    });
    let mut client = Client {
        frames: Framed::new(client, shep_core::protocol::codec()),
    };
    client.handshake().await;
    client.subscribe(&["config.sheep.sandbox"]).await;

    let changed = |dog: &str, sheep: &str| {
        BusEvent::DogSheepSettingsChanged {
            dog: dog.to_string(),
            sheep: sheep.to_string(),
        }
        .into()
    };
    h.ctx.events.send(changed("sandbox", "api")).unwrap();
    h.ctx.events.send(changed("jobs", "web")).unwrap();
    h.ctx.events.send(changed("sandbox", "web")).unwrap();

    // Back-to-back arrival is the filtering assertion: no negative wait.
    let heard: ServerFrame = client.recv().await;
    assert!(
        matches!(
            &heard,
            ServerFrame::Event(BusEvent::DogSheepSettingsChanged { dog, sheep })
                if dog == "sandbox" && sheep == "web"
        ),
        "{heard:?}"
    );
}
