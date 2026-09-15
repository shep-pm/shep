use core::fmt;
use tokio::sync::oneshot;

/// One exit observation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitOutcome {
    /// Exit code on normal exit
    pub code: Option<i32>,
    /// Raw unix signal number when killed (`SIGTERM`=15, `SIGKILL`=9, ...)
    pub signal: Option<i32>,
}

/// One stdout/stderr line from a child
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// True = stderr, false = stdout
    pub err: bool,
    /// The line, no trailing newline
    pub line: String,
}

/// What the supervisor can ask a log pump to do mid-flight.
///
/// The [`oneshot`] on each variant is what makes it usable as a barrier:
/// once it resolves, every live pump has done the thing. A logrotate
/// `postrotate` stanza needs that of [`Self::Reopen`] before it compresses
/// what it renamed; `shep flush` needs it of [`Self::Flush`] before it
/// truncates.
///
/// `#[non_exhaustive]`: this crate is published, and an out-of-tree
/// [`ProcessRunner`](crate::runner::ProcessRunner) matching exhaustively would
/// break on the next variant.
#[derive(Debug)]
#[non_exhaustive]
pub enum LogCtl {
    /// Drop the current handle and open the path again, then acknowledge.
    /// Sent when an external rotator has renamed the file.
    Reopen {
        /// Fires once the pump has finished acting on this request.
        ///
        /// `Ok` says both old handles were flushed and closed and both paths
        /// were opened again. [`ReopenError`] says at least one path could
        /// not be opened: the old handle is closed either way, so the rename
        /// is safe to act on, but that stream's lines are dropped until
        /// something reopens it.
        ///
        /// The channel buffers, so a request that was accepted is not one
        /// that will be served: a pump that ends first drops this sender and
        /// the caller's `await` resolves [`Err`](oneshot::error::RecvError).
        /// Treat that as the stopped-sheep no-op a failed send means.
        done: oneshot::Sender<Result<(), ReopenError>>,
    },
    /// Write out whatever the pump has buffered, wait for it to reach the
    /// file, keep the handle, then acknowledge. Sent as the first half of
    /// `shep flush`, immediately before the recorded paths are truncated.
    Flush {
        /// Fires once both handles have nothing buffered and no write in
        /// flight.
        ///
        /// The barrier the truncate that follows is ordered against:
        /// `write_all` on a [`tokio::fs::File`] returns as soon as the real
        /// `write(2)` is queued, so a line already dispatched could otherwise
        /// land at offset 0 after the file was emptied.
        ///
        /// [`FlushError`] says at least one stream's owed bytes never reached
        /// its file. It does not hold up the truncate: `poll_flush` drives
        /// the in-flight write to completion either way. A pump that ends
        /// first drops this sender, as [`Self::Reopen`]'s does.
        done: oneshot::Sender<Result<(), FlushError>>,
    },
    /// Write out whatever the pump has buffered, wait for it to reach the
    /// files, then acknowledge with the descriptor numbers a blob is to
    /// name for this sheep. Sent while assembling a daemon handover.
    ///
    /// Flush and report are one request: bytes still buffered behind a
    /// descriptor the blob already claims die with the image at the `execve`,
    /// and the successor cannot repair a gap it never saw.
    ///
    /// Unix only: it answers with raw descriptor numbers, and Windows has no
    /// `execve` and no handover.
    #[cfg(unix)]
    ReportFds {
        /// Fires once both handles have nothing buffered and no write in
        /// flight, carrying the descriptors the blob is to name.
        ///
        /// `CarriedFds::none` is the honest answer for a pump holding
        /// nothing, never an error: a number that names nothing is what the
        /// successor must not be handed. A pump that ends first drops this
        /// sender, as [`Self::Reopen`]'s does.
        ///
        /// `CarriedFds` is crate-private, so an out-of-tree runner can drop
        /// this sender but cannot answer it.
        done: oneshot::Sender<crate::handover::CarriedFds>,
    },
    /// Read the sheep's streams again after a [`Self::ReportFds`] that no
    /// exec followed. Sent when a handover is abandoned.
    ///
    /// A report parks the pump, since a snapshot only stays true while
    /// nothing moves behind it, and normally the exec ends the park by
    /// replacing the image. Every other way out leaves this daemon with a
    /// pump that has stopped reading for the rest of its life, so an
    /// abandoned handover owes every pump it reported one of these.
    ///
    /// No acknowledgement: nobody is ordered against it, and sending one to
    /// a pump that was never parked is a no-op, so it is safe to send widely
    /// rather than exactly. Unix only, as [`Self::ReportFds`] is.
    #[cfg(unix)]
    Resume,
}

/// A [`LogCtl::Reopen`] that could not open one or both of a sheep's log
/// files again.
///
/// Carries a rendered message rather than the `io::Error`s behind it, as
/// [`RunnerError`](crate::runner::RunnerError) does: it crosses a channel and every layer between only
/// prints it, and a `String` is what keeps this `Clone`/`Eq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReopenError {
    /// Every log file the reopen could not open again, as
    /// `"<path>: <what the open reported>"`, joined by `", "` when both
    /// streams failed. Never empty: a reopen that opened both answers `Ok`.
    ///
    /// `", "` and not `"; "` because this list nests:
    /// [`SupervisorError::ReopenFailed`] joins one of these per sheep with
    /// `"; "`, and one separator at both levels would punctuate one sheep
    /// that failed on both streams like two sheep that failed on one each.
    ///
    /// [`SupervisorError::ReopenFailed`]: crate::supervisor::SupervisorError::ReopenFailed
    pub message: String,
}

impl fmt::Display for ReopenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not reopen {}", self.message)
    }
}

impl core::error::Error for ReopenError {}

/// A `shep flush` that could not empty a log file, from either half of that
/// verb: a pump whose pending writes would not reach the file
/// ([`LogCtl::Flush`]), or a path that could not be truncated once they had.
///
/// What the two leave behind differs: a truncate that failed leaves its file
/// as it was, while a failed flush does not hold up the truncate, so that
/// file ends up empty with the lines it held gone unwritten. Carries a
/// rendered message for the reasons [`ReopenError`] gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushError {
    /// Every log file the flush could not empty, as
    /// `"<path>: <what the failing call reported>"`, joined by `", "` when
    /// both of a pump's streams did. Never empty: a flush that emptied every
    /// file answers `Ok`.
    ///
    /// `", "` for the reason [`ReopenError::message`] gives about its own
    /// separator.
    ///
    /// Keyed by path and never by sheep: several sheep can share one log
    /// path (`merge_logs`, or an explicit `out_file` on a multi-instance
    /// app), so naming one of them would be arbitrary.
    pub message: String,
}

impl fmt::Display for FlushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not flush {}", self.message)
    }
}

impl core::error::Error for FlushError {}
