//! Fixtures and helpers shared by this module's tests.

use std::time::Duration;
// tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
// budget below is measured against a `tokio::time::sleep` that does too.
use super::*;
use crate::client::Client;
use crate::testing::Handovers;
use shep_core::protocol::HelloAck;
use shep_core::protocol::PROTOCOL_VERSION;

/// Every bounded wait in this module uses one budget: generous against
/// a loaded CI runner, small enough that a genuinely stuck test fails
/// rather than hanging the suite.
pub(super) const BOUND: Duration = Duration::from_secs(5);

/// The window a test watches for something that must not happen.
///
/// Eight times [`RECONNECT_MIN_DELAY`], so a supervisor still looping
/// would have made several further attempts inside it, the first after
/// 50ms. Stated against the delay it has to outrun, not a round number.
pub(super) const NEGATIVE_WINDOW: Duration = Duration::from_millis(400);

/// Polls `condition` until it holds, yielding between polls, and
/// reports whether it held before `bound` ran out.
///
/// Positive waits assert `true`; a window watching for something that
/// must not happen asserts `false`. Under a paused clock the bound
/// elapses once every task is idle, as it would with a real one.
pub(super) async fn wait_until(bound: Duration, mut condition: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(bound, async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

/// An ack distinguishable per generation, so a test can tell which
/// daemon answered rather than only that one did.
pub(super) fn ack_from(pid: u32) -> HelloAck {
    HelloAck {
        daemon_version: format!("0.0.{pid}"),
        protocol: PROTOCOL_VERSION,
        pid,
        min_supported: None,
    }
}

/// Waits until the fake has accepted `accepts` connections and the
/// client has installed the newest of them, or fails inside [`BOUND`].
///
/// Both halves are needed: the accept count alone rises before the
/// handshake completes, and the link alone still reads `Connected` in
/// the instant after a cut. The supervisor sets `Reconnecting` before
/// it dials, so together they are unambiguous.
///
/// Does not observe [`ReconnectingClient::daemon`]: some tests assert
/// on the ack, and a helper that waited on it would make those fail
/// for an unrelated reason.
pub(super) async fn await_reconnect(
    client: &ReconnectingClient,
    shepherds: &Handovers,
    accepts: u32,
) {
    let seen = wait_until(BOUND, || {
        shepherds.accepted() >= accepts && client.link() == LinkState::Connected
    })
    .await;
    assert!(
        seen,
        "no reconnect within {BOUND:?}: {} accepts, link {:?}",
        shepherds.accepted(),
        client.link()
    );
}

/// Subscribes, cuts the connection under the subscription, and drains
/// the stream to its end.
///
/// The sequence a dog actually meets: it learns its connection died by
/// its own event stream ending, never by reading a link state. A test
/// that cuts and asks straight away would be asking before the client
/// itself has noticed.
pub(super) async fn subscribe_then_lose_it(client: &ReconnectingClient, shepherds: &Handovers) {
    let mut events = client
        .subscribe(vec!["process.*".to_owned()])
        .await
        .expect("the first subscription must be answered");
    shepherds.cut().await;
    let ended = tokio::time::timeout(BOUND, async { while events.next().await.is_some() {} }).await;
    assert!(ended.is_ok(), "the stream must end within {BOUND:?}");
}

/// Waits until `client`'s link reaches a refusal, or fails inside
/// [`BOUND`].
pub(super) async fn await_refusal(client: &ReconnectingClient) -> LinkState {
    let mut refusal = None;
    let refused = wait_until(BOUND, || {
        let link = client.link();
        let is_refusal = matches!(link, LinkState::Refused { .. });
        if is_refusal {
            refusal = Some(link);
        }
        is_refusal
    })
    .await;
    assert!(refused, "the link never reached a refusal within {BOUND:?}");
    refusal.expect("a held condition recorded the refusal it saw")
}

/// Reconnects `client` inside [`BOUND`], failing the test rather than
/// hanging, and hands back the verdict.
///
/// Most cases want exactly this and differ only in what they assert
/// afterwards. The refusal and spent-budget cases spell it out instead,
/// because they assert on the error this unwraps.
pub(super) async fn reconnect_ok(client: &mut Client) -> Reconnected {
    tokio::time::timeout(BOUND, client.reconnect())
        .await
        .expect("the reconnect must not hang")
        .expect("the successor accepted this handshake")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn wait_until_reports_a_condition_that_never_holds() {
        // The poll loop yields forever, so the runtime is never idle and the
        // paused clock never auto-advances: move it past the bound by hand.
        let (held, ()) = tokio::join!(
            wait_until(NEGATIVE_WINDOW, || false),
            tokio::time::advance(NEGATIVE_WINDOW * 2)
        );

        assert!(!held, "a condition that never holds must report false");
    }
}
