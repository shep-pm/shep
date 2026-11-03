//! Serving the connection each dog, and each sheep a dog launches, is spawned holding.

#[cfg(unix)]
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use super::conn_protocol::serve_conn;
#[cfg(unix)]
use super::link_table::DogLinks;
use super::origin::Origin;
use crate::rpc::RpcContext;
use crate::runner::DogLink;

/// Serves every [`DogLink`] a runner opens until `shutdown` flips or the runner is gone
///
/// Independent of the control socket's listener: the links arrive at spawn, so
/// a dog started ahead of the muster restore is served during it. Each runs
/// the same handshake and request loop as a socket connection, with the
/// runner's own identity in place of the peer's claim. A link with a `launch_for`
/// is served as a launch link, bounded to that sheep's table. One a predecessor
/// carried skips the handshake it already did. `table` holds each one.
pub(crate) async fn serve_dog_links(
    mut links: mpsc::UnboundedReceiver<DogLink>,
    ctx: RpcContext,
    #[cfg(unix)] table: DogLinks,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            link = links.recv() => {
                let Some(DogLink { name, launch_for, pid, stream, resumed }) = link else { return };
                let ctx = ctx.clone();
                #[cfg(unix)]
                let lease = Arc::new(table.register(
                    &name,
                    launch_for.as_deref(),
                    pid,
                    &stream,
                    resumed.clone(),
                ));
                tokio::spawn(async move {
                    let origin = match launch_for {
                        Some(sheep) => Origin::Launch {
                            dog: name,
                            sheep,
                            #[cfg(unix)]
                            lease,
                            resumed,
                        },
                        None => Origin::Dog {
                            name,
                            pid,
                            #[cfg(unix)]
                            lease,
                            resumed,
                        },
                    };
                    if let Err(err) = serve_conn(stream, ctx, origin).await {
                        tracing::debug!(%err, "dog connection ended");
                    }
                });
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}
