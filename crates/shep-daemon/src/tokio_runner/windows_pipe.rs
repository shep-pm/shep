//! The named pipes a Windows spawn hands its child
//!
//! `command-fds` is unix-only and `cmd.exe` has no fd-3 redirection, so where
//! unix gives a child a socketpair, here the shepherd creates a pipe and names
//! it in an environment variable. There are two: the shepherd channel's, and a
//! dog's own connection. Both are created under [`UserOnly`], so no other
//! local account can open or list its way into one (#350).

use core::sync::atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::sync::Arc;

use shep_core::transport::Listener;
use tokio::sync::mpsc;

use crate::runner::{DogLink, RunnerError};
use crate::sys_windows::UserOnly;

/// Distinguishes one spawn's pipe from every other's.
///
/// The pipe namespace is machine-global, so a name must be unique across this
/// daemon's flock and any other daemon on the host: process id plus this
/// counter, never the sheep's name, which two `$SHEP_HOME`s could share.
/// Monotonic, so a restarted sheep never inherits a dying predecessor's name.
static NEXT_PIPE_ID: AtomicU64 = AtomicU64::new(0);

/// Binds a new pipe for `kind`, returning its name and the listener on it.
///
/// `what` names the pipe in an error. The name is unique per spawn. The
/// nonce makes it unguessable, a second layer under the descriptor: another
/// account is refused by the DACL whatever it learns of the name.
///
/// # Errors
///
/// - [`RunnerError::SpawnFailed`] if the nonce, the descriptor or the pipe
///   cannot be made.
pub(super) fn bind(kind: &str, what: &str) -> Result<(PathBuf, Listener), RunnerError> {
    let failed = |step: &str, error: &dyn core::fmt::Display| {
        RunnerError::SpawnFailed(format!("{what}{step}: {error}"))
    };
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|error| failed(" name", &error))?;
    let pipe = PathBuf::from(format!(
        r"\\.\pipe\shep-{kind}-{}-{}-{:032x}",
        std::process::id(),
        NEXT_PIPE_ID.fetch_add(1, Ordering::Relaxed),
        u128::from_ne_bytes(nonce)
    ));
    let security = UserOnly::current_user().map_err(|error| failed(" security", &error))?;
    let listener =
        Listener::bind_with(&pipe, Arc::new(security)).map_err(|error| failed("", &error))?;
    Ok((pipe, listener))
}

/// Hands the dog's side of `listener` to the server once the dog connects.
///
/// On a task, since a spawn is synchronous and the dog cannot connect until it
/// is started. The link carries `name` and `pid` as the unix one does, so the
/// connection is the identity. `gone` bounds the wait: a dog that never opens
/// the pipe would hold the listener for the daemon's life. Whoever opens the
/// pipe first is the dog, which is no new reach.
pub(super) fn serve_dog_link<T: Send + 'static>(
    mut listener: Listener,
    name: String,
    pid: u32,
    links: mpsc::UnboundedSender<DogLink>,
    gone: mpsc::Sender<T>,
) {
    tokio::spawn(async move {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    let _ = links.send(DogLink { name, pid, stream, resumed: None });
                }
                Err(error) => {
                    tracing::warn!(%error, dog = %name, "dog connection accept failed");
                }
            },
            () = gone.closed() => {}
        }
    });
}

#[cfg(test)]
mod tests;
