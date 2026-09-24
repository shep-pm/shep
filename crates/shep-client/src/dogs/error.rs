//! [`ShepherdError`], every way a dog's shepherd can fail it, as one
//! variant a dog's own error wraps.

use core::fmt;

use crate::{ConnectError, LinkLost, RequestError};

/// Why a dog could not do what it asked its shepherd.
///
/// Non-exhaustive: the client may grow a way to fail.
#[derive(Debug)]
#[non_exhaustive]
pub enum ShepherdError {
    /// No shepherd answered at the socket, or one refused the handshake.
    Connect(ConnectError),
    /// A request failed, or the shepherd answered it out of turn.
    Request(RequestError),
    /// The link dropped and no shepherd came back inside the dog's budget,
    /// or a successor refused it.
    Lost(LinkLost),
}

impl ShepherdError {
    /// The [`shep_core::exit`] code a dog stopping on this error exits
    /// with, the same one `shep` would for the same cause.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Connect(err) => err.exit_code(),
            Self::Request(err) => err.exit_code(),
            Self::Lost(lost) => lost.exit_code(),
        }
    }
}

impl fmt::Display for ShepherdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "cannot reach the shepherd: {err}"),
            Self::Request(err) => write!(f, "a request to the shepherd failed: {err}"),
            Self::Lost(lost) => write!(f, "lost the shepherd: {lost}"),
        }
    }
}

impl core::error::Error for ShepherdError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Request(err) => Some(err),
            Self::Lost(lost) => Some(lost),
        }
    }
}

impl From<ConnectError> for ShepherdError {
    fn from(err: ConnectError) -> Self {
        Self::Connect(err)
    }
}

impl From<RequestError> for ShepherdError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

impl From<LinkLost> for ShepherdError {
    fn from(lost: LinkLost) -> Self {
        Self::Lost(lost)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_core::exit;

    use super::*;

    #[test]
    fn each_cause_exits_on_its_own_code() {
        let cases = [
            (
                ShepherdError::from(ConnectError::HandshakeClosed),
                exit::DAEMON_UNREACHABLE,
            ),
            (
                ShepherdError::from(RequestError::Closed),
                exit::DAEMON_UNREACHABLE,
            ),
            (
                ShepherdError::from(LinkLost::Refused {
                    daemon_version: None,
                    message: "too old".to_owned(),
                }),
                exit::PROTOCOL_MISMATCH,
            ),
            (
                ShepherdError::from(LinkLost::Budget {
                    waited: Duration::from_secs(5),
                }),
                exit::DAEMON_UNREACHABLE,
            ),
        ];
        for (err, code) in cases {
            assert_eq!(err.exit_code(), code, "{err}");
        }
    }

    #[test]
    fn the_cause_is_reachable_as_the_source() {
        use core::error::Error as _;
        let err = ShepherdError::from(RequestError::Closed);
        let source = err.source().expect("a wrapped error has a source");
        assert_eq!(source.to_string(), RequestError::Closed.to_string());
    }
}
