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

#[cfg(test)]
mod tests {

    use core::time::Duration;

    use crate::fake::{FIRST_SCRIPTED_PID, ProcScript};
    use crate::testing::harness;

    use shep_core::status::ProcStatus;

    use super::super::testing::*;

    /// fails if a refused dog is left mute. The daemon is the only party that
    /// can restart it: the dog's own client has stopped rather than spinning.
    ///
    /// The pid moving is the assertion, not the restart count: a restart that
    /// re-registered the row without re-spawning leaves the dog as mute.
    #[tokio::test]
    async fn a_refused_dog_is_restarted_once_from_the_binary_on_disk() {
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let started = start_dog(&h.ctx, "metrics").await;
        assert_eq!(started.pid, Some(FIRST_SCRIPTED_PID));

        refuse_as(&h.ctx, Some("metrics")).await;

        await_dog(&h.ctx, "metrics", FIRST_SCRIPTED_PID + 1).await;
        assert!(
            h.ctx.dog_refusals.stale().is_empty(),
            "one refusal buys a restart; it does not condemn the dog"
        );
    }

    /// fails if the daemon restarts a dog it has already restarted, the spin
    /// G8 forbids. A second refusal proves the binary on disk cannot satisfy
    /// this daemon either, since the restart already ran it.
    ///
    /// The pid must not move inside a window sized against the restart that
    /// really happened earlier in this test, and the harness is scripted with
    /// exactly the two spawns G8 permits.
    #[tokio::test]
    async fn a_twice_refused_dog_is_reported_stale_and_never_restarted_again() {
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        start_dog(&h.ctx, "metrics").await;

        refuse_as(&h.ctx, Some("metrics")).await;
        let restart_took = await_dog(&h.ctx, "metrics", FIRST_SCRIPTED_PID + 1).await;

        refuse_as(&h.ctx, Some("metrics")).await;
        assert_eq!(
            h.ctx.dog_refusals.stale(),
            vec!["metrics".to_string()],
            "the second refusal must be reported, not swallowed"
        );

        // Ten times the restart that did happen, floored so a fast machine
        // still watches for a real interval.
        let window = (restart_took * 10).max(Duration::from_millis(200));
        tokio::time::sleep(window).await;

        let after = dog_row(&h.ctx, "metrics").await;
        assert_eq!(
            after.pid,
            Some(FIRST_SCRIPTED_PID + 1),
            "a second restart within {window:?} is the spin G8 forbids"
        );
        assert_eq!(
            after.status,
            ProcStatus::Online,
            "a third spawn would exhaust the script and error the dog"
        );
    }

    /// fails if an operator running an older `shep` has a dog restarted under
    /// them. The CLI cannot name a dog, so a refusal carrying no name must
    /// leave the flock exactly as it was.
    #[tokio::test]
    async fn a_refused_client_that_is_not_a_dog_touches_nothing() {
        let h = harness(vec![ProcScript::never_exits()]);
        let started = start_dog(&h.ctx, "metrics").await;

        for _ in 0..3 {
            refuse_as(&h.ctx, None).await;
        }

        assert!(h.ctx.dog_refusals.stale().is_empty());
        let after = dog_row(&h.ctx, "metrics").await;
        assert_eq!(
            after.pid, started.pid,
            "a nameless refusal must not restart anything"
        );
        assert_eq!(after.status, ProcStatus::Online);
    }
}
