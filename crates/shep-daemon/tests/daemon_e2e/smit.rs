//! Smits: a mark a connection paints on a sheep, and what becomes of it
//! when that connection goes.

use super::*;

/// The reference smit, the one `shep-deploy` paints: a mark and a revision,
/// thirteen characters, and nothing shep understands.
const SMIT: &str = "\u{25b2} main@a1b2c3";

/// Starts one long-lived real sheep under `name` and answers with its id.
async fn start_sheep(client: &mut Client, name: &str) -> u32 {
    let app = forever_app(name);
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.expect("the sheep must start") else {
        panic!("expected started")
    };
    infos[0].id
}

/// `name`'s smit as `shep flock` would paint it, read over the socket rather
/// than out of the daemon's memory.
async fn smit_of(client: &mut Client, name: &str) -> Option<String> {
    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.expect("the flock must list") else {
        panic!("expected flock")
    };
    flock
        .into_iter()
        .find(|info| info.name == name)
        .expect("the sheep must still be registered")
        .smit
}

/// Waits for `name`'s smit to clear, answering `false` at [`RECV_TIMEOUT`].
///
/// Polls: the daemon learns of a closed socket asynchronously.
async fn await_smit_cleared(client: &mut Client, name: &str) -> bool {
    tokio::time::timeout(RECV_TIMEOUT, async {
        while smit_of(client, name).await.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

/// What has to hold is that closing a real socket reaches the forget path,
/// through `handle_conn`'s tail, the actor's mailbox and `to_info`.
#[tokio::test]
async fn a_smit_dies_with_the_connection_that_painted_it() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    // The observer must not be the connection whose closing is under test.
    let mut looker = fixture.connect().await;
    start_sheep(&mut looker, "web").await;

    let mut painter = fixture.connect().await;
    let painted = painter
        .request(Request::SetSmit {
            sheep: "web".to_string(),
            smit: Some(SMIT.parse().expect("the reference smit must be valid")),
        })
        .await;
    assert!(
        matches!(painted.result, Ok(Response::SmitPainted(_))),
        "{painted:?}"
    );

    assert_eq!(
        smit_of(&mut looker, "web").await,
        Some(SMIT.to_string()),
        "a smit must be visible to every client, not only its painter"
    );

    drop(painter);

    assert!(
        await_smit_cleared(&mut looker, "web").await,
        "the smit outlived the connection that painted it"
    );

    fixture.shutdown().await;
}

/// Also fails if a dog can clear a smit it did not paint: connection scoping
/// is otherwise indistinguishable from "any disconnect wipes everything".
#[tokio::test]
async fn one_dogs_disconnect_leaves_another_dogs_smit_alone() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut looker = fixture.connect().await;
    start_sheep(&mut looker, "web").await;
    start_sheep(&mut looker, "api").await;

    let mut deployer = fixture.connect().await;
    let mut watcher = fixture.connect().await;
    for (client, sheep) in [(&mut deployer, "web"), (&mut watcher, "api")] {
        let painted = client
            .request(Request::SetSmit {
                sheep: sheep.to_string(),
                smit: Some(SMIT.parse().expect("the reference smit must be valid")),
            })
            .await;
        assert!(
            matches!(painted.result, Ok(Response::SmitPainted(_))),
            "{painted:?}"
        );
    }

    // A clear only takes effect from the connection that painted it.
    let ignored = watcher
        .request(Request::SetSmit {
            sheep: "web".to_string(),
            smit: None,
        })
        .await;
    assert!(
        matches!(ignored.result, Ok(Response::SmitPainted(_))),
        "{ignored:?}"
    );
    assert_eq!(
        smit_of(&mut looker, "web").await,
        Some(SMIT.to_string()),
        "one dog cleared a smit another dog painted"
    );

    drop(deployer);

    assert!(
        await_smit_cleared(&mut looker, "web").await,
        "the smit outlived the connection that painted it"
    );
    assert_eq!(
        smit_of(&mut looker, "api").await,
        Some(SMIT.to_string()),
        "one dog's disconnect cleared another dog's smit"
    );

    fixture.shutdown().await;
}

/// The renderer is not the guard: `output::width::sanitize_cell` keeps a
/// well-formed CSI sequence, since shep's own colouring is made of them.
///
/// The frame is built past the `Smit` parser, so this is the daemon's refusal
/// rather than the client's. A malformed body ends the connection with no
/// reply, so either answer is a refusal and neither is a stored smit.
#[tokio::test]
async fn a_smit_carrying_an_escape_is_refused_at_the_daemon() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut looker = fixture.connect().await;
    start_sheep(&mut looker, "web").await;

    let mut rogue = fixture.connect().await;
    let refused = rogue
        .request_raw(serde_json::json!({
            "kind": "set_smit",
            "sheep": "web",
            "smit": "\u{1b}[2Jgone",
        }))
        .await;
    assert!(
        refused.as_ref().is_none_or(|reply| reply.result.is_err()),
        "the daemon accepted a smit carrying an escape: {refused:?}"
    );
    assert_eq!(smit_of(&mut looker, "web").await, None);

    fixture.shutdown().await;
}
