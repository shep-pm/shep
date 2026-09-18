use super::server_lifecycle::HANDSHAKE_TIMEOUT_MS;
use core::fmt;
use shep_core::protocol::{PROTOCOL_VERSION, WireError};

/// Checks that a connected peer runs as the daemon's own user.
///
// `UnixStream::peer_cred()` rather than nix's `PeerCredentials`, which nix
// gates behind `#[cfg(linux_android)]`, so it does not exist on macOS.
// tokio's `UCred` dispatches to `SO_PEERCRED`, `getpeereid` or
// `LOCAL_PEERCRED` per platform.
///
/// # Errors
/// - [`AuthError::NoCredentials`]: the OS would not report peer credentials.
/// - [`AuthError::ForeignUid`]: the peer's uid is not the daemon's.
///
/// # Platform
///
/// Unix only. The Windows pipe's ACL answers this question at open time; see
/// [`shep_core::transport`]'s module doc.
#[cfg(unix)]
pub fn check_peer(stream: &tokio::net::UnixStream, daemon_uid: u32) -> Result<u32, AuthError> {
    let cred = stream
        .peer_cred()
        .map_err(|err| AuthError::NoCredentials(err.to_string()))?;
    let peer = cred.uid();
    if peer == daemon_uid {
        Ok(peer)
    } else {
        Err(AuthError::ForeignUid {
            peer,
            daemon: daemon_uid,
        })
    }
}

/// The connecting peer's pid, when the OS will name one.
///
/// Separate from [`check_peer`], which reads the same
/// [`UCred`](tokio::net::unix::UCred): that answer admits or ends the
/// connection, and this is a diagnostic that must never do either.
///
/// `None` does not mean no process is there, only that the platform has no
/// answer; callers degrade through
/// [`Contact::Unknown`](crate::dogs::Contact::Unknown). Unix only.
#[cfg(unix)]
#[must_use]
pub fn peer_pid(stream: &tokio::net::UnixStream) -> Option<u32> {
    // Every failure is the same answer: an OS that would not say. No platform
    // here produces a pid too wide for `u32`.
    u32::try_from(stream.peer_cred().ok()?.pid()?).ok()
}

/// The daemon's effective uid.
///
/// # Platform
///
/// Unix only, alongside [`check_peer`], its only caller.
#[cfg(unix)]
#[must_use]
pub fn daemon_uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// Why [`check_peer`] refused a connection.
///
/// `#[non_exhaustive]`: a future check, a group membership or a peer
/// certificate, would need its own variant rather than stretching
/// [`Self::ForeignUid`] to mean something it does not.
#[non_exhaustive]
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The OS would not report peer credentials on this socket (carries the
    /// OS error message).
    NoCredentials(String),
    /// The peer runs as another user (carries both uids).
    ForeignUid {
        /// The connecting peer's uid.
        peer: u32,
        /// The daemon's own uid.
        daemon: u32,
    },
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCredentials(msg) => write!(f, "could not read peer credentials: {msg}"),
            Self::ForeignUid { peer, daemon } => {
                write!(f, "peer uid {peer} does not match daemon uid {daemon}")
            }
        }
    }
}

impl core::error::Error for AuthError {}

/// Error type ending one connection.
///
/// Every variant is terminal: the connection layer logs it and closes the
/// socket. A malformed or hostile peer can only cost itself its connection.
///
/// `#[non_exhaustive]`: a future failure point, a TLS handshake or a
/// rate-limit refusal, would add its own variant rather than overloading
/// [`Self::Auth`], which is specifically [`check_peer`]'s verdict.
#[non_exhaustive]
#[derive(Debug)]
pub enum ConnError {
    /// [`check_peer`] refused the connection.
    Auth(AuthError),
    /// The framed transport failed reading or writing a length-delimited frame.
    Frame(std::io::Error),
    /// A frame's payload failed to decode as the expected type.
    Decode(WireError),
    /// A reply or event failed to encode onto the wire.
    Encode(WireError),
    /// The peer's `Hello.protocol` fell below [`MIN_SUPPORTED`](shep_core::protocol::MIN_SUPPORTED) (carries
    /// the client's claimed version; the refusal is written before this is
    /// returned).
    ProtocolMismatch {
        /// The protocol version the client sent.
        client: u32,
    },
    /// The peer did not send `Hello` within [`HANDSHAKE_TIMEOUT_MS`].
    HandshakeTimeout,
    /// The peer closed the connection before sending `Hello`.
    NoHandshake,
    /// The connection's write queue is gone: the writer task exited.
    PeerGone,
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(err) => write!(f, "peer-credential check failed: {err}"),
            Self::Frame(err) => write!(f, "frame transport error: {err}"),
            Self::Decode(err) => write!(f, "frame decode error: {err}"),
            Self::Encode(err) => write!(f, "frame encode error: {err}"),
            Self::ProtocolMismatch { client } => write!(
                f,
                "client sent protocol {client}, daemon speaks {PROTOCOL_VERSION}"
            ),
            Self::HandshakeTimeout => write!(
                f,
                "peer did not send Hello within {HANDSHAKE_TIMEOUT_MS} ms"
            ),
            Self::NoHandshake => f.write_str("peer closed before sending Hello"),
            Self::PeerGone => f.write_str("connection's write queue is gone"),
        }
    }
}

impl core::error::Error for ConnError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Auth(err) => Some(err),
            Self::Frame(err) => Some(err),
            Self::Decode(err) | Self::Encode(err) => Some(err),
            Self::ProtocolMismatch { .. }
            | Self::HandshakeTimeout
            | Self::NoHandshake
            | Self::PeerGone => None,
        }
    }
}

impl From<AuthError> for ConnError {
    fn from(source: AuthError) -> Self {
        Self::Auth(source)
    }
}

// `Decode` and `Encode` both wrap `WireError`, so only one could claim
// `impl From<WireError> for ConnError` and a bare `?` would silently mislabel
// the other direction. Both stay explicit `map_err` calls.
impl From<std::io::Error> for ConnError {
    fn from(source: std::io::Error) -> Self {
        Self::Frame(source)
    }
}

#[cfg(test)]
mod tests {

    // Real time: every test here drives a real socket, and a paused clock
    // auto-advances when the runtime idles, expiring HANDSHAKE_TIMEOUT_MS
    // before the peer's bytes arrive.
    use super::*;

    /// `cfg(unix)`, like [`check_peer`] itself: the Windows pipe's ACL
    /// refuses a foreign user before `handle_conn` is reached at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn peer_credentials_gate_on_uid() {
        // `UnixStream::pair()` reports both ends as this process's own uid,
        // and `UCred` has no synthetic constructor, so only the `daemon_uid`
        // argument can vary: this pins the comparison, not a real cross-uid
        // connection.
        let (a, _b) = tokio::net::UnixStream::pair().unwrap();
        let me = daemon_uid();
        assert_eq!(check_peer(&a, me).unwrap(), me);
        assert_eq!(
            check_peer(&a, me + 1).unwrap_err(),
            AuthError::ForeignUid {
                peer: me,
                daemon: me + 1
            }
        );
    }
}
