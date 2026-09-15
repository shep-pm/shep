use std::time::Duration;
// tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
// budget below is measured against a `tokio::time::sleep` that does too.

/// How long the supervisor waits after its FIRST failed reconnect attempt
/// before trying again.
///
/// The first attempt carries no delay: across a handover the listening
/// socket stays bound, so `connect(2)` succeeds into the backlog and only
/// the handshake waits for the successor to start accepting. 50ms is the
/// pause before a second attempt, short enough that a successor a few
/// milliseconds late costs one of these rather than a visible outage.
pub const RECONNECT_MIN_DELAY: Duration = Duration::from_millis(50);

/// The ceiling [`RECONNECT_MIN_DELAY`] doubles up to.
///
/// Same order as [`HANDSHAKE_TIMEOUT`]: past this point one further attempt
/// costs about as much as the wait between attempts, so the loop is
/// bounded noise rather than a spin. A dog whose daemon is genuinely gone
/// sits here indefinitely: the daemon that would have reaped it is the one
/// that vanished.
pub const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(5);

/// The next delay in the reconnect ladder, doubling up to
/// [`RECONNECT_MAX_DELAY`].
///
/// Both reconnect paths walk this one ladder: the supervisor behind
/// [`ReconnectingClient`], and [`Client::reconnect_within`]. Shared so a
/// change to the schedule cannot reach one of them and miss the other.
pub(super) fn next_delay(current: Duration) -> Duration {
    // Saturating: `Duration`'s `Mul` panics on overflow, and a ladder that
    // cannot overflow is one fewer precondition on a caller's own value.
    current.saturating_mul(2).min(RECONNECT_MAX_DELAY)
}

#[cfg(test)]
mod tests {

    use std::time::Duration;

    // tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
    // budget below is measured against a `tokio::time::sleep` that does too.

    use super::*;

    /// fails if the ladder changes shape while its constants stay put.
    ///
    /// Pins the exact sequence both reconnect paths walk, without sleeping
    /// through any of it. The last two entries are the assertion that
    /// matters: doubling stops at the ceiling rather than running past it.
    #[test]
    fn the_reconnect_ladder_doubles_then_holds_at_the_ceiling() {
        let mut delay = RECONNECT_MIN_DELAY;
        let mut walked = vec![delay];
        for _ in 0..8 {
            delay = next_delay(delay);
            walked.push(delay);
        }
        assert_eq!(
            walked,
            [50, 100, 200, 400, 800, 1600, 3200, 5000, 5000].map(Duration::from_millis)
        );

        // A duration no ladder can reach today, so this pins the function
        // as total rather than the loop as correct.
        assert_eq!(next_delay(Duration::MAX), RECONNECT_MAX_DELAY);
    }
}
