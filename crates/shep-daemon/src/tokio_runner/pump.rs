//! The tasks [`spawn_log_pump`], [`spawn_stdin_pump`] and
//! [`spawn_channel_pumps`] spawn, and the line-draining helpers they share.

use std::io;

use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader, Lines,
};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};

use crate::channel::{ChildMessage, ShepherdMessage};
use crate::runner::{LogCtl, LogLine, RunnerError, StdinWrite};

use super::FINAL_DRAIN;
#[cfg(unix)]
use super::READ_BUFFER;
use super::log_file::{LogFile, LogFiles, LogSink, PipeFds};

/// What the pump does after handling one line result.
enum AfterLine {
    /// The stream is live; keep reading it.
    KeepReading,
    /// The stream reached EOF or failed; stop reading THIS stream.
    StreamEnded,
    /// The owning sheep task dropped its `logs` receiver; stop entirely.
    LogsClosed,
}

/// Handles one line read from a stream: appends it to that stream's file,
/// forwards it on `logs_tx`, and reports what the pump should do next.
///
/// The wait for room on `logs_tx` keeps serving `ctl_rx`; see
/// [`reserve_slot`] for the cycle that would otherwise close.
async fn deliver_line<O, E>(
    result: io::Result<Option<String>>,
    err: bool,
    files: &mut LogFiles,
    logs_tx: &mpsc::Sender<LogLine>,
    ctl_rx: &mut mpsc::Receiver<LogCtl>,
    streams: &mut Streams<O, E>,
) -> AfterLine
where
    O: AsyncRead + Unpin,
    E: AsyncRead + Unpin,
{
    match result {
        Ok(Some(line)) => {
            files.stream(err).append(&line).await;
            let Some(slot) = reserve_slot(logs_tx, files, ctl_rx, streams).await else {
                return AfterLine::LogsClosed;
            };
            slot.send(LogLine { err, line });
            AfterLine::KeepReading
        }
        Ok(None) => AfterLine::StreamEnded, // normally the child exiting
        Err(error) => {
            tracing::error!(path = ?files.stream(err).path, %error, "log stream read failed");
            AfterLine::StreamEnded
        }
    }
}

/// Waits for room on `logs_tx`, serving control requests and idle flushes
/// while it waits. `None` once the `logs` receiver is gone.
///
/// A pump parked on a full `logs` channel is parked for as long as the sheep
/// task takes, which is unbounded, so without the idle-flush branch the line
/// just appended would sit in the buffer for exactly that long.
///
/// A `select!` handler is not cancellable, so a bare `send().await` inside
/// one stops the pump polling `ctl_rx` for as long as the wait lasts. The
/// party that makes room on `logs` is the sheep task, the same party a
/// reopen's acknowledgement travels back to, so that would close a cycle.
async fn reserve_slot<'tx, O, E>(
    logs_tx: &'tx mpsc::Sender<LogLine>,
    files: &mut LogFiles,
    ctl_rx: &mut mpsc::Receiver<LogCtl>,
    streams: &mut Streams<O, E>,
) -> Option<mpsc::Permit<'tx, LogLine>>
where
    O: AsyncRead + Unpin,
    E: AsyncRead + Unpin,
{
    loop {
        // Recomputed every iteration from the stored mark, so losing the
        // race never extends the window.
        let flush_at = files.flush_deadline();
        // Every branch is documented cancel-safe, as `select!` requires: a
        // `reserve` that loses the race has taken no slot, a `recv` that
        // loses it has taken no message, and a `sleep_until` that loses it
        // is rebuilt against the same absolute deadline.
        tokio::select! {
            slot = logs_tx.reserve() => return slot.ok(),
            ctl = ctl_rx.recv() => match ctl {
                Some(ctl) => files.serve(ctl, streams).await,
                // The line in hand is still owed to the receiver. Awaited
                // outside the `select!` rather than looping, since a closed
                // receiver is ready on every poll.
                None => return logs_tx.reserve().await.ok(),
            },
            () = sleep_until(flush_at.unwrap_or_else(Instant::now)), if flush_at.is_some() => {
                files.flush_idle().await;
            }
        }
    }
}

/// The next line from an optional stream, or a future that never resolves
/// once there is no stream left to read.
///
/// The pump's `select!` needs a branch it can leave in place after a stream
/// ends: a ready `None` would be re-selected on every poll and spin the
/// loop, while pending forever drops the branch out of contention.
///
/// Cancel-safe, as a `select!` branch must be: a partially read line stays
/// in the `Lines` buffer instead of being lost to another branch.
async fn next_line<R>(lines: &mut Option<Lines<BufReader<R>>>) -> io::Result<Option<String>>
where
    R: AsyncRead + Unpin,
{
    match lines {
        Some(lines) => lines.next_line().await,
        None => core::future::pending().await,
    }
}

/// One stream's reader, at the capacity a handover reasons about.
///
/// A free function rather than an inline `BufReader::with_capacity` at each
/// call site, so [`drain_ready`]'s bound and the reader's real capacity
/// cannot drift apart.
#[cfg(unix)]
fn with_read_buffer<R: AsyncRead>(reader: R) -> BufReader<R> {
    BufReader::with_capacity(READ_BUFFER, reader)
}

/// See the unix definition above; nothing here reasons about the capacity.
#[cfg(not(unix))]
fn with_read_buffer<R: AsyncRead>(reader: R) -> BufReader<R> {
    BufReader::new(reader)
}

/// A pump's two line readers, held in one place so a control request can
/// reach them.
///
/// [`LogCtl::ReportFds`] is served from two places, the pump's own `select!`
/// and [`reserve_slot`]'s, and a local would be visible to only one.
///
/// `None` per stream is one that has reached EOF or failed. The pump drops
/// the reader at that moment, which closes the descriptor, so a stream with
/// no reader has neither a number nor bytes left to write.
pub(super) struct Streams<O, E> {
    /// The stdout reader, until stdout ends.
    pub(super) out: Option<Lines<BufReader<O>>>,
    /// The stderr reader, until stderr ends.
    pub(super) err: Option<Lines<BufReader<E>>>,
}

/// Writes out the whole lines one stream's reader is already holding, and
/// touches the pipe behind it not at all.
///
/// This is what a descriptor report owes the successor: the reader has taken
/// up to [`super::READ_BUFFER`] off the pipe the successor will never see
/// there, and the `execve` destroys it. Reading the pipe here would refill
/// the reader as fast as it empties it; stopping at "no whole line is
/// buffered" makes the buffer strictly shrink, since a `read_line` that
/// finds its delimiter already buffered returns without a syscall.
///
/// The partial line at the end of the buffer is left behind. Nothing drained
/// here goes on `logs_tx`: those bus subscribers go with the image.
#[cfg(unix)]
pub(super) async fn drain_ready<R>(lines: &mut Option<Lines<BufReader<R>>>, file: &mut LogFile)
where
    R: AsyncRead + Unpin,
{
    let Some(reader) = lines.as_mut() else {
        return;
    };
    while reader.get_ref().buffer().contains(&b'\n') {
        // A delimiter already in the buffer means no `read(2)`, so nothing
        // new arrives to replace what is written.
        let Ok(Some(line)) = reader.next_line().await else {
            return;
        };
        file.append(&line).await;
    }
}

/// Writes out what the streams still hold once the sheep task has let go,
/// and stops after [`FINAL_DRAIN`] however much is left.
///
/// Files only: `logs_tx` is closed, which is what brought us here.
///
/// Each stream is retired on EOF or a read failure, so a child that has
/// exited ends this in one poll per stream. The budget covers the other
/// case: a lamb that inherited a write end keeps the pipe open, and without
/// a bound the pump would follow it for as long as it cared to talk.
///
/// A retired stream does not clear its number from `files.pipes` and does
/// not need to: the caller `break`s as soon as this returns.
async fn final_drain<O, E>(files: &mut LogFiles, streams: &mut Streams<O, E>)
where
    O: AsyncRead + Unpin,
    E: AsyncRead + Unpin,
{
    let drained = timeout(FINAL_DRAIN, async {
        while streams.out.is_some() || streams.err.is_some() {
            // Bound before the `match`: the future borrows `streams`, and a
            // scrutinee's temporaries outlive the arms.
            tokio::select! {
                result = next_line(&mut streams.out) => {
                    match result {
                        Ok(Some(line)) => files.stream(false).append(&line).await,
                        Ok(None) | Err(_) => streams.out = None,
                    }
                }
                result = next_line(&mut streams.err) => {
                    match result {
                        Ok(Some(line)) => files.stream(true).append(&line).await,
                        Ok(None) | Err(_) => streams.err = None,
                    }
                }
            }
        }
    })
    .await;
    if drained.is_err() {
        tracing::debug!("a pump left the pipe still open at its sheep task's exit");
    }
}

/// Pumps a sheep's stdout and stderr to completion, and stays reachable the
/// whole time it does.
///
/// Every line is appended to its stream's file and then forwarded on
/// `logs_tx`. A [`LogCtl`] is served between lines, while no line is flowing
/// at all, and while the pump waits for room on `logs_tx`. One task for both
/// streams, so one [`LogCtl::Reopen`] swaps both files and answers once.
///
/// The file write is issued before the line is forwarded but lands in a
/// [`BufWriter`](tokio::io::BufWriter), so a line seen on `logs_tx` need not
/// be in the file yet. [`LogCtl::Reopen`], [`LogCtl::Flush`] and
/// [`super::IDLE_FLUSH`] are the barriers. A reopen waits behind the pump's
/// own file I/O, with no timeout.
pub(super) fn spawn_log_pump<O, E>(
    stdout: Option<O>,
    stderr: Option<E>,
    out_sink: LogSink,
    err_sink: LogSink,
    logs_tx: mpsc::Sender<LogLine>,
    mut ctl_rx: mpsc::Receiver<LogCtl>,
    pipes: PipeFds,
) where
    O: AsyncRead + Unpin + Send + 'static,
    E: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut files = LogFiles {
            out: LogFile::from_sink(out_sink).await,
            err: LogFile::from_sink(err_sink).await,
            pipes,
            #[cfg(unix)]
            parked: false,
        };
        let mut streams = Streams {
            out: stdout.map(|reader| with_read_buffer(reader).lines()),
            err: stderr.map(|reader| with_read_buffer(reader).lines()),
        };

        while streams.out.is_some() || streams.err.is_some() {
            // Recomputed every iteration from the stored mark;
            // `reserve_slot` carries the same branch.
            let flush_at = files.flush_deadline();
            tokio::select! {
                result = next_line(&mut streams.out), if files.reading() => {
                    // Bound before the `match`: the future borrows `streams`,
                    // and a scrutinee's temporaries outlive the arms, so an
                    // arm could not clear the reader it was reading.
                    let after =
                        deliver_line(result, false, &mut files, &logs_tx, &mut ctl_rx, &mut streams)
                            .await;
                    match after {
                        AfterLine::KeepReading => {}
                        // Dropping the reader closes the descriptor, so the
                        // number stops being ours in the same statement it
                        // stops being readable.
                        AfterLine::StreamEnded => {
                            streams.out = None;
                            #[cfg(unix)]
                            {
                                files.pipes.out = None;
                            }
                        }
                        AfterLine::LogsClosed => break,
                    }
                }
                result = next_line(&mut streams.err), if files.reading() => {
                    let after =
                        deliver_line(result, true, &mut files, &logs_tx, &mut ctl_rx, &mut streams)
                            .await;
                    match after {
                        AfterLine::KeepReading => {}
                        // As above: the number goes with the reader.
                        AfterLine::StreamEnded => {
                            streams.err = None;
                            #[cfg(unix)]
                            {
                                files.pipes.err = None;
                            }
                        }
                        AfterLine::LogsClosed => break,
                    }
                }
                ctl = ctl_rx.recv() => {
                    match ctl {
                        Some(ctl) => files.serve(ctl, &mut streams).await,
                        None => break, // nothing holds a `log_ctl` sender
                    }
                }
                // The owning sheep task dropped its `logs` receiver. Its own
                // branch, because a pump whose child forked a lamb holding
                // the pipe reaches no EOF and has no next line to notice it
                // on. Cancel-safe: a closed channel stays closed.
                () = logs_tx.closed() => break,

                // Cancel-safe: rebuilt against the same absolute deadline
                // every iteration, so losing the race costs nothing.
                () = sleep_until(flush_at.unwrap_or_else(Instant::now)), if flush_at.is_some() => {
                    files.flush_idle().await;
                }
            }
        }
        // On the way out rather than in the branch that prompted it: four
        // exits reach this line and each can leave a line unread. Not while
        // parked, since a parked pump is holding the pipe for a successor to
        // adopt.
        if files.reading() {
            final_drain(&mut files, &mut streams).await;
        }
        // A `BufWriter` cannot flush itself as it drops, and every way out
        // of the loop above drops both of them.
        files.flush_idle().await;
    });
}

/// Writes lines to one child's stdin, one at a time, acknowledging each.
///
/// Serial on purpose: two concurrent writers to one pipe can interleave
/// mid-line, and a REPL reading the result would see a command neither
/// caller sent. A line queued behind one the app is not reading waits, so
/// the caller bounds its own wait; abandoning a write halfway would leave a
/// partial line in the pipe.
///
/// A request whose caller has stopped listening is dropped rather than
/// written: delivering later would send a line the operator was told was not
/// written. The line already inside `write_all` is past that point. Ends
/// when the last sender drops, giving the app EOF on stdin.
pub(super) fn spawn_stdin_pump<W>(stdin: Option<W>, mut rx: mpsc::Receiver<StdinWrite>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Some(mut stdin) = stdin else {
            // `Stdio::piped()` was set and `child.stdin` was still `None`.
            // Answering nothing would hang every caller.
            while let Some(StdinWrite { done, .. }) = rx.recv().await {
                let _ = done.send(Err(RunnerError::WriteFailed(
                    "this child has no stdin pipe".to_string(),
                )));
            }
            return;
        };
        while let Some(StdinWrite { line, done }) = rx.recv().await {
            if done.is_closed() {
                // The supervisor's `STDIN_WRITE_TIMEOUT` expired and it
                // dropped the receiver. Writing now would deliver a line the
                // operator was already told was not written.
                continue;
            }
            let mut bytes = line.into_bytes();
            // Exactly one terminator, appended here and nowhere else: the
            // wire carries the line without one (`Request::SendLine::line`).
            bytes.push(b'\n');
            let result = match stdin.write_all(&bytes).await {
                Ok(()) => stdin.flush().await,
                Err(error) => Err(error),
            };
            let _ = done.send(result.map_err(|error| RunnerError::WriteFailed(error.to_string())));
        }
    });
}

/// Wires the daemon side of the shepherd channel: a reader task decodes
/// newline-JSON [`ChildMessage`]s onto `from_child_tx`; a writer task encodes
/// [`ShepherdMessage`]s taken from `to_child_rx` back onto the socket.
///
/// Generic over the transport: the daemon's end is a `UnixStream` half of a
/// socketpair on unix and an accepted named-pipe server instance on Windows.
/// `tokio::io::split` rather than `UnixStream::into_split`, since
/// `NamedPipeServer` has no `into_split` of its own.
pub(super) fn spawn_channel_pumps<S>(
    daemon_end: S,
    from_child_tx: mpsc::Sender<ChildMessage>,
    mut to_child_rx: mpsc::Receiver<ShepherdMessage>,
) where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(daemon_end);

    tokio::spawn(async move {
        let mut lines = BufReader::new(read_half).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match serde_json::from_str::<ChildMessage>(&line) {
                    Ok(msg) => {
                        if from_child_tx.send(msg).await.is_err() {
                            break; // owning sheep task dropped from_child
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%line, %error, "malformed shepherd-channel frame");
                    }
                },
                Ok(None) => break, // child closed its end, normally at exit
                Err(error) => {
                    tracing::error!(%error, "shepherd-channel read failed");
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Some(msg) = to_child_rx.recv().await {
            let mut line = match serde_json::to_string(&msg) {
                Ok(json) => json,
                Err(error) => {
                    tracing::error!(%error, "shepherd message encode failed");
                    continue;
                }
            };
            line.push('\n');
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break; // child closed its end
            }
        }
    });
}
