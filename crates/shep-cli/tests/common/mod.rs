//! Shared by `init.rs` and `term_panic_order.rs`, the two targets that run
//! the real `shep` binary as a child process and wait for it to exit.

#![cfg(unix)]

use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

/// Polls `child.try_wait()` until it exits, or `timeout` elapses.
///
/// Polls rather than blocking in `Child::wait`, so a hung child fails with
/// a named panic. The harness's own process timeout would instead fail the
/// whole binary and name nothing.
///
/// `what` names the child, since both callers run a different one.
///
/// # Panics
///
/// If the child cannot be polled, or does not exit within `timeout`. A
/// child that outlasts the timeout is killed and reaped first, so a failing
/// assertion never leaves a process behind.
pub fn wait_bounded(child: &mut Child, timeout: Duration, what: &str) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|error| panic!("poll {what}: {error}"))
        {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
