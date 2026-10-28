use super::accept_backoff::AcceptBackoff;
use super::conn_protocol::handle_conn;
use crate::rpc::RpcContext;
use core::time::Duration;
use shep_core::transport::Listener;
use tokio::sync::watch;

/// Frames queued toward one client before the connection back-pressures.
pub const CONN_QUEUE: usize = 64;

/// How long a connected peer has to send its `Hello` before the daemon closes.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;

/// The control socket: shep's privilege boundary
///
/// # Security
/// On unix, [`check_peer`](crate::server::peer_auth::check_peer) refuses a peer of another
/// uid, and refuses when the OS will not answer. Windows refuses at open time through the
/// pipe's ACL. Skew, frame size, glob count and regex size are capped, and every call
/// carries a clamped deadline. A same-uid peer is fully trusted: no idle timeout, no cap.
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
    /// A failing accept never ends the loop and never retries flat out.
    /// [`AcceptBackoff`] answers how long to wait. A `shutdown` that arrives
    /// mid-wait ends the wait. Connection tasks are detached, so `serve`
    /// returning does not mean every connection has finished.
    pub async fn serve(self, mut shutdown: watch::Receiver<bool>) {
        // `mut` because `Listener::accept` needs `&mut self` on both
        // platforms: a Windows named pipe server instance is consumed by
        // whoever connects to it, so accepting means handing that instance
        // out and creating the next one. See `shep_core::transport::Listener`.
        let Self { mut listener, ctx } = self;
        let mut backoff = AcceptBackoff::new();
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
                            backoff.on_success();
                            let ctx = ctx.clone();
                            tokio::spawn(async move {
                                if let Err(err) = handle_conn(stream, ctx).await {
                                    tracing::debug!(%err, "connection ended");
                                }
                            });
                        }
                        Err(err) => {
                            let wait = backoff.on_error(&err);
                            tracing::warn!(
                                %err,
                                retry_in_ms = wait.map_or(0, |d| d.as_millis() as u64),
                                "accept failed; continuing"
                            );
                            if wait_out_backoff(wait, &mut shutdown).await {
                                break;
                            }
                        }
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

/// Sleeps out `delay` unless `shutdown` flips first, so a stop lands inside a
/// backoff instead of after it. `None` sleeps nothing. `true` means
/// `shutdown` won and [`RpcServer::serve`] must stop.
///
/// A `changed()` that resolves on a `false` drops the rest of the wait and
/// the loop accepts again. Both branches are cancel-safe.
async fn wait_out_backoff(delay: Option<Duration>, shutdown: &mut watch::Receiver<bool>) -> bool {
    let Some(delay) = delay else {
        return false;
    };
    tokio::select! {
        () = tokio::time::sleep(delay) => false,
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paused clock: the 30 s wait below cannot elapse on its own, so the
    /// shutdown is the only thing that can end the wait. The test's own
    /// timeout is the bound.
    #[tokio::test(start_paused = true)]
    async fn a_shutdown_during_a_backoff_wait_stops_the_loop() {
        let (tx, mut rx) = watch::channel(false);
        let flip = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            tx.send(true).expect("the receiver is alive");
        });

        let stopped = tokio::time::timeout(
            Duration::from_millis(100),
            wait_out_backoff(Some(Duration::from_secs(30)), &mut rx),
        )
        .await
        .expect("the shutdown must end the wait, not the 30 s delay elapsing");

        assert!(stopped, "a stop signal mid-backoff must end the wait");
        flip.await.expect("the flip task must finish");
    }

    /// The other edge: with no signal the wait does end on its own, so a
    /// backoff cannot hold the loop open forever.
    #[tokio::test(start_paused = true)]
    async fn a_backoff_wait_ends_on_its_own_clock() {
        let (_tx, mut rx) = watch::channel(false);
        let stopped = tokio::time::timeout(
            Duration::from_secs(1),
            wait_out_backoff(Some(Duration::from_millis(5)), &mut rx),
        )
        .await
        .expect("a 5 ms wait must elapse with no signal");
        assert!(!stopped, "a wait that elapsed is not a shutdown");
    }

    /// `None` means retry at once: the loop must not spend any clock time.
    #[tokio::test(start_paused = true)]
    async fn no_wait_means_no_wait() {
        let (_tx, mut rx) = watch::channel(false);
        let stopped =
            tokio::time::timeout(Duration::from_millis(100), wait_out_backoff(None, &mut rx))
                .await
                .expect("an at-once verdict must return without touching the clock");
        assert!(!stopped, "an at-once verdict must not read as a shutdown");
    }
}
