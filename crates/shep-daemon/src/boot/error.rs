//! The one error every boot step reports through
//!
//! Layout, pidfile lock, socket bind, readiness write, muster restore and
//! handover adoption are steps in a single sequence rather than six
//! independent operations, so they share [`BootError`] and the caller that
//! starts the sequence writes one `?`.

use core::fmt;
use std::path::PathBuf;

use crate::snapshot::SnapshotError;

/// Error type returned from this module's boot steps
///
/// Wraps `io::Error` directly rather than stringifying it, so callers keep the
/// OS diagnostic via [`core::error::Error::source`]; that costs this enum
/// `Clone`/`PartialEq`/`Eq`.
///
/// `#[non_exhaustive]`: a future boot step adds a variant rather than
/// overloading [`Self::Io`], whose `path`/`source` shape is specific to the
/// steps that already exist.
#[non_exhaustive]
#[derive(Debug)]
pub enum BootError {
    /// A flock this image inherited across a handover could not be installed.
    ///
    /// A `String` because the two underlying sources are private types in
    /// different modules; what a caller needs is the sentence naming which
    /// sheep is now unsupervised.
    Adopt(String),
    /// A filesystem step failed (carries the path and the OS error)
    ///
    /// No `From<std::io::Error>`, here or on any sibling: `ReadyWrite` wraps
    /// the same type, so one would make a bare `?` in this module pick a
    /// variant rather than report one.
    Io {
        /// The path the failing step operated on
        path: PathBuf,
        /// The underlying OS error
        source: std::io::Error,
    },
    /// Another daemon already answers on this socket (carries its pid if recorded)
    AlreadyRunning {
        /// The pid recorded in the pidfile, if one was readable
        pid: Option<u32>,
    },
    /// The muster roll exists but could not be read or parsed on restore
    Snapshot(SnapshotError),
    /// `$SHEP_HOME` puts the control socket past the platform's `sun_path`
    /// limit, so no bind could ever succeed (carries the path and the limit)
    ///
    /// Checked before the bind rather than translated after it: the kernel's
    /// `ENAMETOOLONG` names neither the limit nor `$SHEP_HOME`.
    SocketPathTooLong {
        /// The socket path that would not fit
        path: PathBuf,
        /// Its length in bytes
        len: usize,
        /// This platform's `sun_path` capacity in bytes
        limit: usize,
    },
    /// Writing the readiness line to the caller-adopted readiness pipe
    /// failed (carries the OS error)
    ///
    /// Only the write. Adoption is the caller's job (see
    /// [`BootOptions::ready_fd`](super::BootOptions::ready_fd)), so `boot` has no
    /// variant for a failed one.
    ReadyWrite(std::io::Error),
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "boot step failed for `{}`: {source}", path.display())
            }
            Self::AlreadyRunning { pid: Some(pid) } => {
                write!(f, "a shep daemon is already running (pid {pid})")
            }
            Self::AlreadyRunning { pid: None } => write!(f, "a shep daemon is already running"),
            Self::Snapshot(err) => write!(f, "muster roll restore failed: {err}"),
            Self::SocketPathTooLong { path, len, limit } => write!(
                f,
                "the control socket path is {len} bytes and this platform allows {limit}: `{}`. \
                 A unix socket path is bounded by the kernel, not by shep, so a shorter \
                 $SHEP_HOME is the only fix.",
                path.display()
            ),
            Self::ReadyWrite(err) => write!(f, "writing the readiness line failed: {err}"),
            Self::Adopt(reason) => write!(
                f,
                "this shepherd was handed a flock it could not take over: {reason}. The flock is \
                 still running and nothing is supervising it. `shep daemon reload` is not the \
                 way back: it needs a live shepherd to ask and to signal, and this process is \
                 about to exit without ever serving. It holds the pidfile until it does, so the \
                 home is claimable straight afterwards and `shep muster` starts a shepherd from \
                 the roll"
            ),
        }
    }
}

impl core::error::Error for BootError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::AlreadyRunning { .. } => None,
            // Refused before any syscall was attempted.
            Self::SocketPathTooLong { .. } => None,
            Self::Snapshot(err) => Some(err),
            Self::ReadyWrite(err) => Some(err),
            // Both underlying types are module-private.
            Self::Adopt(_) => None,
        }
    }
}

impl From<SnapshotError> for BootError {
    fn from(source: SnapshotError) -> Self {
        Self::Snapshot(source)
    }
}
