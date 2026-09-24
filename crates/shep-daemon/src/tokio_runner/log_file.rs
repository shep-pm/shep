//! Buffered per-stream log I/O: [`LogFile`] and [`LogFiles`], the
//! path-keyed write lock two writers on one log path must share, and
//! [`open_append`], the low-level open both a fresh spawn and a reopen go
//! through.

use std::collections::HashMap;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shep_core::logstamp::Stamper;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, BufWriter};
use tokio::time::Instant;

#[cfg(unix)]
use crate::boot::DIR_MODE;
use crate::runner::{FlushError, LogCtl, ReopenError, check_log_ancestry, open_log_path};

use super::pump::Streams;
#[cfg(unix)]
use super::pump::drain_ready;
use super::{IDLE_FLUSH, LOG_BUFFER};

/// The sink for one carried stream: its handle when the blob had one, and
/// its path when it did not.
///
/// A sheep whose log open had failed before the handover carries no handle
/// for that stream, which is a `None` rather than a refusal (see
/// `handover::adopt`). Opening the path here is the right recovery: it is
/// what the predecessor's pump would have done at its next reopen, and it
/// costs the successor nothing when the open fails again.
#[cfg(unix)]
pub(super) fn carried_sink(path: PathBuf, handle: Option<tokio::fs::File>) -> LogSink {
    match handle {
        Some(file) => LogSink::Carried(path, file),
        None => LogSink::Path(path),
    }
}

/// One stream's log file: the path the spec named, plus the buffered handle
/// open on it, `None` when the open failed.
///
/// Generic over the sink only so a test can count the writes that reach it;
/// production only ever builds the default.
#[derive(Debug)]
pub(super) struct LogFile<W = tokio::fs::File> {
    pub(super) path: PathBuf,
    pub(super) handle: Option<BufWriter<W>>,
    /// Serializes a whole record against the other writer on this path.
    ///
    /// From [`record_lock`]. Taken at open and kept rather than looked up
    /// per line: the path never changes and a reopen keeps it.
    pub(super) record: Arc<tokio::sync::Mutex<()>>,
    /// When the oldest line the pump has not tried to flush yet was
    /// appended; the pump reads it as an [`IDLE_FLUSH`] deadline.
    ///
    /// Cleared by every flush attempt, successful or not: a file that cannot
    /// be written must not turn the idle flush into a retry loop.
    pub(super) buffered_since: Option<Instant>,
    /// Scratch the line's timestamp is formatted into, cleared and refilled
    /// per line rather than reallocated.
    ///
    /// Nothing outside [`LogFile::append`] may read it.
    pub(super) stamp: String,
    /// Renders each line's timestamp into `stamp`.
    pub(super) stamper: Stamper,
}

/// Where one stream's log handle comes from when a pump starts.
///
/// A successor's pump is handed a handle already open on the file; opening
/// the path again would lose `O_APPEND` (see [`open_append`]). The path
/// travels with the handle either way, so a later [`LogCtl::Reopen`] behaves
/// identically for both.
pub(super) enum LogSink {
    /// Open this path for appending, which is what every spawn does.
    Path(PathBuf),
    /// Write through this already-open appending handle, on this path.
    #[cfg(unix)]
    Carried(PathBuf, tokio::fs::File),
}

impl LogFile<tokio::fs::File> {
    /// Builds the log file a [`LogSink`] describes.
    pub(super) async fn from_sink(sink: LogSink) -> Self {
        match sink {
            LogSink::Path(path) => Self::open(path).await,
            #[cfg(unix)]
            LogSink::Carried(path, file) => Self::from_file(path, file),
        }
    }

    /// Takes an already-open appending handle on `path`, opening nothing.
    ///
    /// `O_APPEND` is a file status flag on the open file description, so it
    /// crossed the `execve` with the descriptor; reopening `path` here would
    /// give a handle that writes at its own tracked offset, the sparse hole
    /// [`open_append`] documents. [`Self::reopen`] still goes by path, so a
    /// rotation works on a carried handle as on an opened one.
    #[cfg(unix)]
    pub(super) fn from_file(path: PathBuf, file: tokio::fs::File) -> Self {
        Self {
            // Same path, therefore the same lock as any other handle on it.
            // A carried descriptor is still one of two writers.
            record: record_lock(&path),
            path,
            handle: Some(BufWriter::with_capacity(LOG_BUFFER, file)),
            buffered_since: None,
            stamp: String::new(),
            stamper: Stamper::default(),
        }
    }

    /// Opens `path` for appending, keeping the path for later reopens.
    ///
    /// A failed open is not fatal: the pump must still drain the child's
    /// streams whether or not it can write them anywhere.
    /// [`LogFile::reopen`] is the one that reports, since there a caller is
    /// waiting.
    pub(super) async fn open(path: PathBuf) -> Self {
        let handle = open_append(&path)
            .await
            .ok()
            .map(|file| BufWriter::with_capacity(LOG_BUFFER, file));
        Self {
            record: record_lock(&path),
            path,
            handle,
            buffered_since: None,
            stamp: String::new(),
            stamper: Stamper::default(),
        }
    }

    /// Flushes and closes the current handle, then opens the path again.
    ///
    /// Flushing first is what makes [`LogCtl::Reopen`]'s acknowledgement
    /// worth having: the buffer travels with the handle being dropped, so a
    /// reopen that skipped it would discard those lines rather than delay
    /// them. Reopening goes through [`open_append`].
    ///
    /// # Errors
    ///
    /// The path could not be opened again. The old handle is closed
    /// regardless, so the rotator's rename is safe to act on. A failed flush
    /// is logged rather than returned.
    async fn reopen(&mut self) -> Result<(), ReopenError> {
        self.buffered_since = None;
        if let Some(handle) = self.handle.as_mut() {
            // A flush of a full buffer is the at-or-over-capacity case
            // `record_lock` documents as spanning several `poll_write`
            // calls.
            let _record = self.record.lock().await;
            if let Err(error) = handle.flush().await {
                tracing::error!(path = ?self.path, %error, "log file flush failed");
            }
        }
        // Closed before the reopen, so the pump never holds two descriptors
        // on one log at the same time.
        drop(self.handle.take());
        match open_append(&self.path).await {
            Ok(handle) => {
                self.handle = Some(BufWriter::with_capacity(LOG_BUFFER, handle));
                Ok(())
            }
            Err(error) => Err(ReopenError {
                message: format!("{}: {error}", self.path.display()),
            }),
        }
    }

    /// This stream's open log-file descriptor, `None` when there is no
    /// handle.
    ///
    /// Read off the live handle rather than remembered, so a
    /// [`Self::reopen`] cannot leave a stale number for a handover to carry.
    #[cfg(unix)]
    fn raw_fd(&self) -> Option<RawFd> {
        self.handle
            .as_ref()
            .map(|handle| handle.get_ref().as_raw_fd())
    }
}

/// Opens `path` for appending, creating its parent directory at
/// [`DIR_MODE`], asked for at `mkdir` time so it is never wider.
///
/// `.append(true)` is load-bearing: `O_APPEND` makes every write seek to end
/// atomically, so a `copytruncate` rotator can truncate under a live handle
/// and the next line still lands at offset 0. [`check_log_ancestry`] runs
/// before the `mkdir`, since `create_dir_all` walks straight through a
/// symlink to a directory, and [`open_log_path`] adds `O_NOFOLLOW`.
///
/// # Errors
///
/// The parent could not be created, or the file could not be opened.
pub(crate) async fn open_append(path: &Path) -> io::Result<tokio::fs::File> {
    check_log_ancestry(path)
        .inspect_err(|error| tracing::error!(?path, %error, "log ancestry check failed"))?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        // No `DIR_MODE` on Windows: no scalar mode to set. See
        // `boot::create_dir_at_dir_mode`'s Windows arm.
        #[cfg(unix)]
        builder.mode(DIR_MODE);
        if let Err(error) = builder.create(parent).await {
            tracing::error!(?path, %error, "log directory create failed");
            return Err(error);
        }
    }

    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    open_log_path(&mut options, path)
        .await
        .inspect_err(|error| tracing::error!(?path, %error, "log file open failed"))
}

/// The lock that serializes one whole record written to one log path.
///
/// Two things write a sheep's log file: [`LogFile::append`] through a
/// [`BufWriter`] owned by the pump, and [`crate::dogs::narrate`] through its
/// own handle. One `write_all` is not enough: a `BufWriter` writes through
/// directly at or over capacity, and a short write is several `poll_write`
/// calls the other handle can land between, tearing a line into one half
/// with two stamps and one with none.
///
/// Keyed by path, since narration reaches a log through a path and never
/// sees the `LogFile`. The map grows one entry per log path opened and is
/// never freed.
pub(crate) fn record_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(std::sync::Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(locks.entry(path.to_path_buf()).or_default())
}

impl<W: AsyncWrite + Unpin> LogFile<W> {
    /// Appends one timestamped line and its newline to the buffer, logging
    /// rather than propagating a write failure: a log we cannot write to
    /// must not stop the pump draining the child's pipes.
    ///
    /// The stamp, the line and the newline are joined in `self.stamp` and
    /// handed to one `write_all`. [`crate::dogs::narrate`] writes through a
    /// second handle on this same path, so three separate writes let a
    /// narration line land between a stamp and the body it belongs to,
    /// breaking the one-stamp-per-line contract
    /// [`shep_core::logstamp::strip`] reads on.
    ///
    /// The line forwarded on `logs_tx` is the sheep's own bytes, unstamped.
    pub(super) async fn append(&mut self, line: &str) {
        let Some(handle) = self.handle.as_mut() else {
            return;
        };
        self.stamp.clear();
        self.stamper.stamp_into(&mut self.stamp);
        self.stamp.push_str(line);
        self.stamp.push('\n');
        // Held across the write, not merely around the buffer copy: one
        // `write_all` is still several syscalls when the line is at or over
        // the buffer's capacity. See `record_lock`.
        let written = {
            let _record = self.record.lock().await;
            handle.write_all(self.stamp.as_bytes()).await
        };
        self.buffered_since.get_or_insert_with(Instant::now);
        if let Err(error) = written {
            tracing::error!(path = ?self.path, %error, "log file append failed");
        }
    }

    /// Writes out the buffer and waits for it to reach the file, keeping the
    /// handle open.
    ///
    /// [`Self::append`] only reaches the buffer, so truncating this path
    /// without waiting here can empty the file before already-accepted lines
    /// land at offset 0. A stream with no handle answers `Ok`.
    ///
    /// # Errors
    ///
    /// A buffered or already-dispatched write failed.
    pub(super) async fn flush(&mut self) -> Result<(), FlushError> {
        self.buffered_since = None;
        let Some(handle) = self.handle.as_mut() else {
            return Ok(());
        };
        // A flush can land inside the other writer's record as readily as an
        // append; see `record_lock`.
        let _record = self.record.lock().await;
        handle.flush().await.map_err(|error| FlushError {
            message: format!("{}: {error}", self.path.display()),
        })
    }
}

/// The descriptor numbers a handover carries for one sheep's pipes.
///
/// Told to the pump rather than read off its readers: the pump is generic
/// over them, and an in-memory stream has no descriptor at all.
///
/// Empty on Windows, which has no handover to carry them: `Arm::for_daemon`
/// returns the stop-and-start arm there.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PipeFds {
    /// The read end of the child's stdout, while the pump still holds it.
    pub(super) out: Option<RawFd>,
    /// The read end of the child's stderr, while the pump still holds it.
    pub(super) err: Option<RawFd>,
    /// The write end of the child's stdin, held by the stdin pump rather
    /// than by this one.
    ///
    /// Reported here because this pump is the only party a snapshot asks.
    /// The stdin pump ends when the last `to_stdin` sender drops, and the
    /// supervisor's slot holds one while the sheep is registered, so the
    /// write end cannot be closed and its number reissued between the report
    /// and the exec.
    ///
    /// Never cleared as a stream ends: it has no EOF to reach.
    pub(super) stdin: Option<RawFd>,
    /// The daemon's end of the child's shepherd-channel socketpair, held by
    /// that channel's two pump tasks rather than by this one.
    ///
    /// Reported here for the reason [`Self::stdin`] gives, but the ownership
    /// argument is weaker: the writer task also ends on a write that fails,
    /// which a child that has closed its fd 3 produces, and this pump never
    /// hears about it. `Actor::handle_handover_snapshot` masks this field
    /// when `SheepSlot::open_channel` says the channel is gone.
    pub(super) channel: Option<RawFd>,
}

/// See the unix definition above; there is nothing to carry here.
#[cfg(not(unix))]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PipeFds;

/// A sheep's two log files and the two stream descriptors alongside them,
/// which is the set a handover carries.
#[derive(Debug)]
pub(super) struct LogFiles {
    pub(super) out: LogFile,
    pub(super) err: LogFile,
    /// The descriptors a blob names for this sheep. Each stream's read end
    /// is cleared as that stream ends: the reader is dropped at the same
    /// moment, and a closed number must never reach a handover blob.
    pub(super) pipes: PipeFds,
    /// Whether this pump has answered a [`LogCtl::ReportFds`] that no
    /// [`LogCtl::Resume`] has ended, and so has stopped reading its
    /// sheep's streams.
    ///
    /// See [`LogFiles::reading`] for why a report parks a pump at all.
    #[cfg(unix)]
    pub(super) parked: bool,
}

impl LogFiles {
    /// The file a line from this stream is appended to (`err` picks stderr).
    pub(super) fn stream(&mut self, err: bool) -> &mut LogFile {
        if err { &mut self.err } else { &mut self.out }
    }

    /// Whether the pump should still be reading its sheep's streams.
    ///
    /// False between a [`LogCtl::ReportFds`] and the exec that consumes it:
    /// the report hands the successor descriptor numbers and a flush behind
    /// them, so a pump still reading would append lines the `execve` erases
    /// and strand pipe bytes the successor cannot see.
    ///
    /// Also pins both stream numbers: a paused pump cannot reach EOF, drop a
    /// reader, or free its number. The residual is one line per reload, the
    /// one the sheep was mid-write on at the exec.
    pub(super) fn reading(&self) -> bool {
        #[cfg(unix)]
        {
            !self.parked
        }
        #[cfg(not(unix))]
        {
            true
        }
    }

    /// When the pump owes the older of the two buffers a flush, or `None`
    /// when neither holds anything.
    ///
    /// Derived rather than stored, so an explicit [`LogCtl::Flush`] or
    /// [`LogCtl::Reopen`] retires the deadline by the same act that empties
    /// the buffer; nothing here can be left armed for a buffer that is
    /// already on disk.
    pub(super) fn flush_deadline(&self) -> Option<Instant> {
        let oldest = match (self.out.buffered_since, self.err.buffered_since) {
            (Some(out), Some(err)) => out.min(err),
            (Some(only), None) | (None, Some(only)) => only,
            (None, None) => return None,
        };
        Some(oldest + IDLE_FLUSH)
    }

    /// Flushes both buffers, logging rather than reporting a failure: no
    /// caller is waiting on an idle flush.
    pub(super) async fn flush_idle(&mut self) {
        for file in [&mut self.out, &mut self.err] {
            if let Err(error) = file.flush().await {
                tracing::error!(%error, "log file idle flush failed");
            }
        }
    }

    /// Carries out one control request and then answers it.
    ///
    /// The acknowledgement is the last statement, after both streams have
    /// been dealt with: a caller that has heard back knows both handles were
    /// swapped, or that neither has a write left in flight.
    ///
    /// stderr is served even when stdout's turn just failed, since
    /// short-circuiting would take a sheep's working half offline over the
    /// broken one. Both failures then travel joined by `", "`, where the
    /// supervisor joins one of these per sheep with `"; "`.
    #[cfg_attr(
        not(unix),
        allow(
            unused_variables,
            reason = "`streams` is read only by `ReportFds`, and that variant is unix-only"
        )
    )]
    pub(super) async fn serve<O, E>(&mut self, ctl: LogCtl, streams: &mut Streams<O, E>)
    where
        O: AsyncRead + Unpin,
        E: AsyncRead + Unpin,
    {
        match ctl {
            LogCtl::Reopen { done } => {
                let mut failures = Vec::new();
                if let Err(error) = self.out.reopen().await {
                    failures.push(error.message);
                }
                if let Err(error) = self.err.reopen().await {
                    failures.push(error.message);
                }
                let result = if failures.is_empty() {
                    Ok(())
                } else {
                    Err(ReopenError {
                        message: failures.join(", "),
                    })
                };
                // A caller that stopped waiting is not a failure: the reopen
                // happened either way.
                let _ = done.send(result);
            }
            LogCtl::Flush { done } => {
                let mut failures = Vec::new();
                if let Err(error) = self.out.flush().await {
                    failures.push(error.message);
                }
                if let Err(error) = self.err.flush().await {
                    failures.push(error.message);
                }
                let result = if failures.is_empty() {
                    Ok(())
                } else {
                    Err(FlushError {
                        message: failures.join(", "),
                    })
                };
                // Same as above: the flush happened either way. A caller that
                // stopped waiting is one whose deadline expired, and it will
                // not truncate anything on the strength of an answer it never
                // read.
                let _ = done.send(result);
            }
            // Park, drain, flush, then answer. What a reader has taken off
            // its pipe is on the far side of the descriptor the blob
            // carries, so those bytes reach a file only if this image writes
            // them. A failed flush is logged; the answer has no room for it.
            #[cfg(unix)]
            LogCtl::ReportFds { done } => {
                // Nothing between here and the answer could read a stream
                // anyway: `serve` runs to completion inside the pump's own
                // task, the only reader either stream has.
                self.parked = true;
                drain_ready(&mut streams.out, &mut self.out).await;
                drain_ready(&mut streams.err, &mut self.err).await;
                for file in [&mut self.out, &mut self.err] {
                    if let Err(error) = file.flush().await {
                        tracing::error!(%error, "log flush before a handover report failed");
                    }
                }
                let _ = done.send(crate::handover::CarriedFds {
                    out_pipe: self.pipes.out,
                    err_pipe: self.pipes.err,
                    out_log: self.out.raw_fd(),
                    err_log: self.err.raw_fd(),
                    stdin: self.pipes.stdin,
                    channel: self.pipes.channel,
                });
            }
            // No acknowledgement to send, and nothing to undo but the flag:
            // the drain above emptied the reader rather than copying it, so
            // a resumed pump picks its sheep up wherever the pipe left off.
            #[cfg(unix)]
            LogCtl::Resume => self.parked = false,
        }
    }
}
