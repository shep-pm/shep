//! Accept backoff: how long the accept loop waits after a failing accept
//!
//! An accept error that the next call cannot clear (`EMFILE`, `ENFILE`)
//! otherwise spins a core and floods the log. Every error but the ones that
//! consumed a peer waits, doubling to a cap that keeps the log under one
//! line a second. The loop never gives up on the listener: the shepherd
//! still supervises its flock with a dead listener, and a line a second
//! makes that visible.

use core::time::Duration;
use std::io;

/// The first wait after a failing accept, and the one a wait sequence starts
/// at. Short enough to cost an operator nothing to notice, long enough that
/// a hundred failures a second are not a hundred log lines.
const FIRST_WAIT: Duration = Duration::from_millis(5);

/// The longest a failing accept waits. At the cap the loop logs one line a
/// second, which is the rate a daemon with a dead listener should be read
/// at.
const MAX_WAIT: Duration = Duration::from_secs(1);

/// Each wait is the last one doubled, so the count is one `Duration`.
#[derive(Debug)]
pub(super) struct AcceptBackoff {
    /// The wait the last failure chose, `None` while no failure is pending.
    last_wait: Option<Duration>,
}

impl AcceptBackoff {
    #[must_use]
    pub(super) fn new() -> Self {
        Self { last_wait: None }
    }

    /// How long to wait before accepting again: `None` means at once.
    ///
    /// `None` leaves the pending wait alone; only [`Self::on_success`] resets
    /// it. A peer-consumed failure is no evidence the listener got healthier.
    ///
    /// tokio's `accept` retries only `WouldBlock` and mio's `accept4` retries
    /// nothing, so all three kinds below arrive as errors.
    #[must_use]
    pub(super) fn on_error(&mut self, err: &io::Error) -> Option<Duration> {
        if matches!(
            err.kind(),
            io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::Interrupted
        ) {
            // Each consumed a peer, or interrupted nothing at all: retrying
            // at once cannot loop without a stream of peers arriving.
            return None;
        }

        let wait = match self.last_wait {
            None => FIRST_WAIT,
            Some(prev) => prev.saturating_mul(2).min(MAX_WAIT),
        };
        self.last_wait = Some(wait);
        Some(wait)
    }

    /// A good accept: the next failure starts at [`FIRST_WAIT`] again.
    pub(super) fn on_success(&mut self) {
        self.last_wait = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_err(kind: io::ErrorKind) -> io::Error {
        io::Error::new(kind, "accept failed")
    }

    #[test]
    fn an_aborted_connection_is_retried_at_once() {
        let mut backoff = AcceptBackoff::new();
        for kind in [
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::Interrupted,
        ] {
            assert_eq!(
                backoff.on_error(&io_err(kind)),
                None,
                "{kind:?} cannot loop without a stream of peers arriving"
            );
        }
    }

    #[test]
    fn repeated_failures_wait_longer_up_to_the_cap() {
        let mut backoff = AcceptBackoff::new();
        let expected: [Duration; 10] = [
            Duration::from_millis(5),
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(40),
            Duration::from_millis(80),
            Duration::from_millis(160),
            Duration::from_millis(320),
            Duration::from_millis(640),
            Duration::from_secs(1),
            Duration::from_secs(1),
        ];
        for want in expected {
            assert_eq!(
                backoff.on_error(&io::Error::other("accept failed")),
                Some(want),
                "the wait doubles to the cap and holds there"
            );
        }
    }

    #[test]
    fn a_success_resets_the_wait() {
        let mut backoff = AcceptBackoff::new();
        for _ in 0..4 {
            let _ = backoff.on_error(&io::Error::other("accept failed"));
        }
        backoff.on_success();
        assert_eq!(
            backoff.on_error(&io::Error::other("accept failed")),
            Some(Duration::from_millis(5)),
            "a failure after a good accept starts the sequence again"
        );
    }

    #[test]
    fn an_unrecognised_error_waits_rather_than_spinning() {
        let mut backoff = AcceptBackoff::new();
        assert_eq!(
            backoff.on_error(&io::Error::other("unrecognised")),
            Some(Duration::from_millis(5)),
            "an error this type does not know is not evidence it is transient"
        );
    }

    /// `EMFILE` and `ENFILE` reach this type as `Uncategorized`, not as a
    /// kind it knows.
    #[cfg(unix)]
    #[test]
    fn descriptor_exhaustion_waits() {
        for errno in [nix::errno::Errno::EMFILE, nix::errno::Errno::ENFILE] {
            let mut backoff = AcceptBackoff::new();
            let err = io::Error::from_raw_os_error(errno as i32);
            assert_eq!(
                backoff.on_error(&err),
                Some(Duration::from_millis(5)),
                "{errno:?} must not be retried at full speed"
            );
        }
    }

    #[test]
    fn a_peer_consuming_failure_keeps_the_wait_already_running() {
        let mut backoff = AcceptBackoff::new();
        let _ = backoff.on_error(&io::Error::other("accept failed"));
        let _ = backoff.on_error(&io_err(io::ErrorKind::ConnectionAborted));
        assert_eq!(
            backoff.on_error(&io::Error::other("accept failed")),
            Some(Duration::from_millis(10)),
            "only a good accept restarts the sequence"
        );
    }
}
