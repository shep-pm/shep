use super::conn_protocol::handle_conn;
use crate::rpc::RpcContext;
use shep_core::transport::Listener;
use tokio::sync::watch;

/// Frames queued toward one client before the connection back-pressures.
pub const CONN_QUEUE: usize = 64;

/// How long a connected peer has to send its `Hello` before the daemon closes.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;

/// The control socket: shep's privilege boundary
///
/// # Security
///
/// The daemon's canonical writeup; other modules link here. On unix a
/// connection is refused unless `SO_PEERCRED`/`getpeereid` ([`check_peer`](crate::server::peer_auth::check_peer))
/// names the daemon's own uid, and refused too when the OS will not answer.
/// `$SHEP_HOME/run`'s `0700` is [`crate::boot::init_dirs`]'s job; Windows has
/// neither, and refuses at open time through the pipe's ACL. Skew, frame size,
/// a `Subscribe`'s glob count and a `/regex/` selector's compiled size are all
/// capped, and every call carries a clamped deadline. A same-uid peer is fully
/// trusted; there is no idle timeout and no per-uid connection cap.
#[derive(Debug)]
pub struct RpcServer {
    listener: Listener,
    ctx: RpcContext,
}

impl RpcServer {
    /// Wraps an already-bound listener with the request-handling context.
    #[must_use]
    pub fn new(listener: Listener, ctx: RpcContext) -> Self {
        Self { listener, ctx }
    }

    /// Accepts connections, each on its own task, until `shutdown` flips to
    /// `true` or its sender drops.
    ///
    /// Both `select!` branches are cancel-safe. A transient accept error such
    /// as `EMFILE` is logged and the loop continues.
    ///
    /// Connection tasks are spawned and detached, so `serve` returning does
    /// not mean every in-flight connection has finished. Draining them would
    /// need a `tokio::task::JoinSet` here.
    pub async fn serve(self, mut shutdown: watch::Receiver<bool>) {
        // `mut` because `Listener::accept` needs `&mut self` on both
        // platforms: a Windows named pipe server instance is consumed by
        // whoever connects to it, so accepting means handing that instance
        // out and creating the next one. See `shep_core::transport::Listener`.
        let Self { mut listener, ctx } = self;
        // A shutdown signal already `true` before the first `changed()` would
        // otherwise never be observed: `changed()` only resolves on a value
        // newer than the one this receiver has seen.
        if *shutdown.borrow() {
            return;
        }
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok(stream) => {
                            let ctx = ctx.clone();
                            tokio::spawn(async move {
                                if let Err(err) = handle_conn(stream, ctx).await {
                                    tracing::debug!(%err, "connection ended");
                                }
                            });
                        }
                        Err(err) => tracing::warn!(%err, "accept failed; continuing"),
                    }
                }
                changed = shutdown.changed() => {
                    // An `Err` means the sender dropped: stop serving either
                    // way.
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    }
}
