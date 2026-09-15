//! Fixtures and helpers shared by this module's tests.

use super::conn_protocol::handle_conn;
use crate::rpc::RpcContext;
use core::time::Duration;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use shep_core::protocol::{Hello, HelloReply, MIN_SUPPORTED, codec, decode_frame, encode_frame};

use serde::de::DeserializeOwned;
use shep_core::protocol::{DogSource, ProcessInfo};
use tokio_util::codec::Framed;

pub(super) const RECV_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Client {
    pub(super) frames:
        Framed<shep_core::transport::ClientStream, tokio_util::codec::LengthDelimitedCodec>,
}

impl Client {
    pub(super) async fn send<T: Serialize>(&mut self, value: &T) {
        self.frames
            .send(encode_frame(value).unwrap())
            .await
            .unwrap();
    }

    pub(super) async fn recv<T: DeserializeOwned>(&mut self) -> T {
        let frame = tokio::time::timeout(RECV_TIMEOUT, self.frames.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("connection closed early")
            .unwrap();
        decode_frame(&frame).unwrap()
    }

    pub(super) async fn closed(&mut self) -> bool {
        tokio::time::timeout(RECV_TIMEOUT, self.frames.next())
            .await
            .expect("timed out waiting for close")
            .is_none()
    }
}

/// Spawns `handle_conn` over a real connected pair and hands back the
/// client end.
///
/// A real transport on both platforms, a socketpair on unix and a named
/// pipe on Windows, rather than an in-memory duplex: several tests below
/// turn on what a peer sees when the other side closes.
pub(super) async fn connected(ctx: RpcContext) -> Client {
    let (server, client) = shep_core::transport::connected_pair().await.unwrap();
    tokio::spawn(async move {
        let _ = handle_conn(server, ctx).await;
    });
    Client {
        frames: Framed::new(client, codec()),
    }
}

// --- G8: what a refused DOG's handshake costs it ------------------
//
// A refused handshake never reaches a request, so `Hello.dog_name` is the
// only place the daemon learns which dog it just refused.
/// Registers `name` as a built-in dog and returns the row it produced.
///
/// Straight through [`crate::supervisor::SupervisorHandle::start_dog`]:
/// `Request::EnableDog` would need a handshaken connection of its own.
pub(super) async fn start_dog(ctx: &RpcContext, name: &str) -> ProcessInfo {
    let spec = crate::dogs::DogSpec {
        name: name.to_string(),
        source: DogSource::BuiltIn,
    };
    let app = crate::dogs::dog_app(&spec, &ctx.paths).expect("the dog fixture must assemble");
    ctx.supervisor
        .start_dog(app, DogSource::BuiltIn)
        .await
        .expect("the dog fixture must start")
}

/// One refused handshake, announcing `dog` (or nothing, for a client
/// that is not a dog), returning once the daemon has closed on it.
///
/// The daemon records the refusal and decides what it owes the dog before
/// it returns the error that closes the socket, so a caller that has seen
/// the close can read the verdict without racing it. The restart itself
/// runs on its own task and needs [`await_dog`].
pub(super) async fn refuse_as(ctx: &RpcContext, dog: Option<&str>) {
    let mut client = connected(ctx.clone()).await;
    client
        .send(&Hello {
            client_version: "0.1.14".to_string(),
            protocol: MIN_SUPPORTED - 1,
            dog_name: dog.map(str::to_owned),
        })
        .await;
    let refusal: HelloReply = client.recv().await;
    refusal.expect_err("a protocol below the floor must be refused");
    assert!(
        client.closed().await,
        "the daemon must close after refusing"
    );
}

/// The flock row named `name`, or a panic naming what was there.
pub(super) async fn dog_row(ctx: &RpcContext, name: &str) -> ProcessInfo {
    ctx.supervisor
        .list()
        .await
        .into_iter()
        .find(|info| info.name == name)
        .unwrap_or_else(|| panic!("no row named {name}"))
}

/// Waits until `name` is running as `pid`, or fails inside
/// [`RECV_TIMEOUT`], returning how long it took.
///
/// The restart a refusal triggers runs on its own task, so there is no
/// handle to await. The elapsed time is returned because the
/// never-restart-twice test below sizes its negative window against it.
pub(super) async fn await_dog(ctx: &RpcContext, name: &str, pid: u32) -> Duration {
    let began = tokio::time::Instant::now();
    let seen = tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            if dog_row(ctx, name).await.pid == Some(pid) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        seen.is_ok(),
        "{name} never reached pid {pid} within {RECV_TIMEOUT:?}: {:?}",
        ctx.supervisor.list().await
    );
    began.elapsed()
}
