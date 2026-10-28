use super::*;
use crate::spawn::SpawnOptions;
use std::path::Path;
use std::time::Duration;

/// [`SpawnOptions`] tuned so `spawn.rs`'s tests finish in under a second on
/// a real clock, since none of them may pause tokio's clock.
///
/// `a_child_that_dies_fails_fast_instead_of_waiting_out_the_deadline` uses
/// the production defaults instead: it asserts on the 30s deadline itself.
#[must_use]
pub fn fast_opts() -> SpawnOptions {
    SpawnOptions {
        deadline: Duration::from_millis(600),
        backoff_start: Duration::from_millis(10),
        backoff_cap: Duration::from_millis(50),
        handshake_timeout: Duration::from_millis(100),
    }
}

/// Binds `path`, accepts one connection, answers its handshake with
/// [`sample_ack`], then parks as a daemon a launcher must create synchronously.
///
/// Synchronous: a `connect_or_spawn` launcher is a plain
/// `FnOnce() -> io::Result<Child>`. The returned task is detached.
///
/// Panics if `path` cannot be bound.
pub fn start_fake_daemon_answering_on(path: &Path) {
    let mut listener = bind(path);
    tokio::spawn(async move {
        let _frames = accept_handshaked(&mut listener, sample_ack()).await;
        core::future::pending::<()>().await;
    });
}

/// A launcher-ready child that is already exiting with `code`: spawns
/// `sh -c "exit <code>"` and returns the `Child` immediately.
///
/// # Errors
///
/// Whatever `std::process::Command::spawn` can return, propagated rather
/// than unwrapped so this fits `connect_or_spawn`'s launcher signature.
pub fn child_exiting_with(code: i32) -> std::io::Result<std::process::Child> {
    std::process::Command::new("sh")
        .args(["-c", &format!("exit {code}")])
        .spawn()
}
