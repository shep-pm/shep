//! Real-daemon integration tier: boots shep-daemon on a temp `$SHEP_HOME`,
//! talks to it over the control socket with shep-core's own codec, and
//! drives real child processes.
//!
//! Real time throughout: a paused clock's auto-advance would expire timeouts
//! before IO wakeups arrive.

// Many cases here are `#[cfg(unix)]`, so on Windows those items are unreached.
#![cfg_attr(windows, allow(dead_code))]
// And so are the imports only those cases use.
#![cfg_attr(windows, allow(unused_imports))]

use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use shep_core::transport::{self, ClientStream};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    ActionOutcome, BusEvent, ChildMessage, Envelope, Hello, HelloAck, HelloReply, LineOutcome,
    MIN_SUPPORTED, PROTOCOL_VERSION, ProcessEventKind, ProcessInfo, Reply, Request, Response,
    RpcError, RpcErrorCode, SelectorSpec, ServerFrame, codec, decode_frame, encode_frame,
};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

use shep_daemon::boot::{BootError, BootOptions, DIR_MODE, boot};
use shep_daemon::rpc::RpcContext;
use shep_daemon::tokio_runner::TokioRunner;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);

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

// --- Reload: a probed app's replacement answers for itself ---

/// The `AwaitReady` window a probed reload gets, as `listen_timeout`.
///
/// Both directions matter: a replacement that will serve has to bind inside
/// it, and one that will not costs the case its whole length before the
/// abandonment lands.
///
/// `cfg(unix)`: `reuse_port_sheep` has no Windows build under that name.
#[cfg(unix)]
const PROBED_READY_WINDOW: UpDuration = UpDuration::from_millis(800);

/// One `reuse_port_sheep` on `port`, gated on a TCP probe against the port it
/// binds: the arrangement in which "is the new instance ready" and "is
/// something listening" are the same question.
#[cfg(unix)]
fn probed_sheep(name: &str, port: u16, mute_file: &std::path::Path) -> AppConfig {
    let mut app = AppConfig::minimal(name, &reuse_port_sheep().display().to_string());
    app.interpreter = Some("none".to_string());
    app.env
        .insert("SHEEP_PORT_BASE".to_string(), port.to_string());
    app.env.insert("SHEEP_HOLD_MS".to_string(), "0".to_string());
    app.env.insert(
        "SHEEP_MUTE_FILE".to_string(),
        mute_file.display().to_string(),
    );
    app.readiness_probe = Some(ProbeConfig {
        kind: ProbeKind::Tcp,
        target: format!("127.0.0.1:{port}"),
        interval: UpDuration::from_millis(50),
        timeout: UpDuration::from_millis(200),
        failure_threshold: 3,
    });
    app.listen_timeout = PROBED_READY_WINDOW;
    app.graceful_timeout = DRAIN_WINDOW;
    app.kill_timeout = DRAIN_WINDOW;
    // Nothing may respawn behind the case: a restart would put a third process
    // on this port.
    app.autorestart = false;
    app
}

/// The control for the case below: without it, an implementation that
/// abandoned every probed reload would pass the failure case and look correct.
///
/// The replacement must land in the drainee's instance slot, since the fixture
/// derives its port from `SHEP_INSTANCE`.
#[cfg(unix)]
#[tokio::test]
async fn a_probed_reload_of_a_working_release_still_finishes() {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let mute = dir.path().join("mute");

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let started = client
        .request(Request::Start {
            apps: vec![probed_sheep("web", port, &mute)],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    await_serving(port, drainee_pid).await;
    await_online(&mut client, drainee_id).await;

    // Subscribed after the app is up: this case reads the event stream in
    // emission order, and an earlier subscription would put `Start` in front.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name("web".to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // In order: the question is which of the two endings the reload reached,
    // and a search would find one whatever else had already been said.
    let (ending, replacement) = client
        .next_process_event_of(&[
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
        ])
        .await;
    assert_eq!(
        ending,
        ProcessEventKind::Reloaded,
        "a serial reload is slower than an overlapping one, not broken"
    );
    let replacement_pid = replacement.pid.expect("a replacement has a pid");
    assert_ne!(replacement_pid, drainee_pid);
    assert_eq!(replacement.status, ProcStatus::Online);
    assert_eq!(
        tally(&burst(port).await),
        format!("{BURST}x served by {replacement_pid}"),
        "the replacement owns the port once the swap is done"
    );

    let held = fixture.shutdown().await;
    drop(held);
    drop(dir);
}

/// Both instances run the same command; the second finds `SHEEP_MUTE_FILE` in
/// place and binds nothing, which is what a release whose listener moved to
/// the wrong port does. Its first probe is otherwise answered by the
/// instance still bound to that address.
///
/// Two independent mechanisms cover a single-instance probed app and each is
/// enough alone: the serial reload mode, and the post-drain probe. The flock
/// row's status is what a deploy tool reads, so it is asserted too.
#[cfg(unix)]
#[tokio::test]
async fn a_replacement_that_serves_nothing_is_refused_not_reported_reloaded() {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let mute = dir.path().join("mute");

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let started = client
        .request(Request::Start {
            apps: vec![probed_sheep("web", port, &mute)],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    await_serving(port, drainee_pid).await;
    await_online(&mut client, drainee_id).await;

    // Subscribed after the app is up: this case reads the event stream in
    // emission order, and an earlier subscription would put `Start` in front.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    // The bad release, staged between the two spawns of one unchanged app.
    std::fs::write(&mute, b"").expect("the marker must be writable");

    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name("web".to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // In order again, for the reason the control case gives.
    let (ending, info) = client
        .next_process_event_of(&[
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
        ])
        .await;
    assert_eq!(
        ending,
        ProcessEventKind::ReloadAbandoned,
        "a replacement that binds nothing has not proved it can take over"
    );
    assert_ne!(info.id, drainee_id, "the abandonment names the replacement");

    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1, "the app keeps the one instance it has left");
    assert_eq!(
        flock[0].status,
        ProcStatus::Starting,
        "a replacement that never answered its probe is never called online"
    );

    let held = fixture.shutdown().await;
    drop(held);
    drop(dir);
}

mod app_configs;
mod channel;
mod daemon_lifetime;
mod harness;
mod lifecycle;
mod protocol;
mod reload;

pub(crate) use app_configs::*;
pub(crate) use harness::*;
pub(crate) use reload::*;
