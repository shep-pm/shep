use core::fmt;
use std::time::Duration;
// tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
// budget below is measured against a `tokio::time::sleep` that does too.

/// What a [`ReconnectingClient`]'s supervisor is currently doing.
///
/// Non-exhaustive: expect more variants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkState {
    /// Connected. Requests go out on this generation of the connection.
    Connected,
    /// The connection dropped and the supervisor is re-establishing it.
    /// Requests issued now fail with [`RequestError::Closed`]; they are not
    /// queued and not retried.
    Reconnecting,
    /// A successor refused the handshake on protocol-version skew. The
    /// supervisor has stopped, and every later request fails with
    /// [`RequestError::Closed`]: the daemon that refused is the party
    /// that can fix it, not a retry.
    Refused {
        /// The daemon's own crate version, when it named one. `None` from a
        /// daemon built before the refusal carried it.
        daemon_version: Option<String>,
        /// The daemon's refusal message, verbatim.
        message: String,
    },
}

/// Why a bounded wait on the link ended with the link still down.
///
/// Non-exhaustive: expect more variants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "which of the two it was decides whether a dog exits unreachable or refused"]
#[non_exhaustive]
pub enum LinkLost {
    /// The wait ran out with the supervisor still dialling.
    Budget {
        /// How long the caller waited before giving up.
        waited: Duration,
    },
    /// A successor refused the handshake on protocol-version skew, so the
    /// supervisor has stopped and no further wait could succeed.
    Refused {
        /// The daemon's own crate version, when it named one. `None` from a
        /// daemon built before the refusal carried it.
        daemon_version: Option<String>,
        /// The daemon's refusal message, verbatim.
        message: String,
    },
}

impl fmt::Display for LinkLost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Budget { waited } => write!(f, "no shepherd answered within {waited:?}"),
            Self::Refused { message, .. } => {
                write!(f, "the shepherd refused this connection: {message}")
            }
        }
    }
}

impl core::error::Error for LinkLost {}

// Exhaustive on purpose, unlike `LinkState`: the question is binary, and a
// caller branching on it is better served by a match a third variant would
// break than by a wildcard arm that goes on compiling.
/// Whether a [`Client::reconnect`] reached the daemon it was talking to
/// before, or a different one.
///
/// See this module's own docs for how the two are told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the verdict says whether ids held from before the reconnect still mean anything"]
pub enum Reconnected {
    /// The daemon now answering is the one that answered before, so an id
    /// minted before the connection dropped still names the same sheep.
    ///
    /// It says nothing about the connection. The old generation and the
    /// subscription on it died under either verdict, so a caller wanting
    /// events subscribes again whichever one it gets.
    SameDaemon,
    /// A different daemon is answering, minting ids from its own fresh
    /// space. Every id the caller still holds names nothing here.
    NewDaemon,
}

#[cfg(test)]
mod tests {

    use std::time::Duration;

    // tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
    // budget below is measured against a `tokio::time::sleep` that does too.

    use super::*;

    /// fails if a refusal reaches an operator as a timeout, which would
    /// send them looking for a shepherd that is running and answering.
    #[test]
    fn a_lost_link_says_which_of_the_two_it_was() {
        let budget = LinkLost::Budget {
            waited: Duration::from_secs(5),
        };
        assert_eq!(budget.to_string(), "no shepherd answered within 5s");

        let refused = LinkLost::Refused {
            daemon_version: Some("0.9.9".into()),
            message: "daemon speaks protocol 3, client speaks 2".into(),
        };
        assert_eq!(
            refused.to_string(),
            "the shepherd refused this connection: daemon speaks protocol 3, client speaks 2"
        );
    }

    /// fails if `Reconnected` starts printing anything but its verdict. It
    /// carries no payload, and a caller logging one must never begin
    /// emitting a daemon's own details alongside it.
    #[test]
    fn reconnected_debug_is_the_verdict_and_nothing_else() {
        assert_eq!(format!("{:?}", Reconnected::SameDaemon), "SameDaemon");
        assert_eq!(format!("{:?}", Reconnected::NewDaemon), "NewDaemon");
    }
}
