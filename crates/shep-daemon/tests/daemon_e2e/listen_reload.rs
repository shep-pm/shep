//! A reload of a sheep whose port the shepherd holds: the socket never
//! closes, so a client connecting throughout is never refused.

use super::reload::{Sharing, reload_under_load_sharing, tally};

/// Both instances accept on one socket the shepherd keeps open, so a
/// connection queued while the drainee stops is accepted by the replacement.
/// Unlike `SO_REUSEPORT`, which resets a closing listener's backlog, nothing
/// is lost, on Linux and macOS alike.
#[tokio::test]
async fn a_reload_of_a_listen_sheep_refuses_no_connection() {
    let during = reload_under_load_sharing("lamb", false, Sharing::Listen).await;
    assert!(
        during.len() > 20,
        "the reload must last long enough to be measured: {}",
        tally(&during)
    );
    let failures = during.iter().filter(|attempt| attempt.failed()).count();
    assert_eq!(
        failures,
        0,
        "the shepherd's socket stays open across the swap: {}",
        tally(&during)
    );
}
