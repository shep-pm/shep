//! The tasks [`spawn_log_pump`], [`spawn_stdin_pump`] and
//! [`spawn_channel_pumps`] spawn, and the line-draining helpers they share.

use core::time::Duration;
use std::io;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::time::{Instant, Sleep, sleep_until, timeout, timeout_at};

use crate::channel::{ChildMessage, ShepherdMessage};
use crate::runner::{LogCtl, LogLine, RunnerError, StdinWrite};

use super::CHANNEL_CAPACITY;
use super::FINAL_DRAIN;
#[cfg(unix)]
use super::READ_BUFFER;
use super::line_reader::LineReader;
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

/// How long a pty pump may wait for room on `logs` across one stall before
/// it skips live copies until a reader drains the channel.
///
/// macOS drops what a terminal slave queued about 0.6s after that slave's last
/// descriptor closes, and a stop holds the sheep task in `kill_process` for up
/// to `kill_timeout`. A wait here must stay under that grace: this pump is the
/// master's only reader. 100ms sits under a 500ms read that still landed.
pub(super) const LIVE_WAIT: Duration = Duration::from_millis(100);

/// What waiting for room on `logs` came back with.
enum Slot<'tx> {
    /// Room was found; the line in hand may be sent.
    Got(mpsc::Permit<'tx, LogLine>),
    /// The owning sheep task dropped its `logs` receiver.
    Closed,
    /// No room, and this pump is not waiting for one.
    GaveUp,
}

/// Takes a slot that is free right now, or names which of the two reasons
/// there is none.
fn take_slot<'tx>(logs_tx: &'tx mpsc::Sender<LogLine>) -> Slot<'tx> {
    match logs_tx.try_reserve() {
        Ok(slot) => Slot::Got(slot),
        Err(TrySendError::Closed(())) => Slot::Closed,
        Err(TrySendError::Full(())) => Slot::GaveUp,
    }
}

/// Handles one line read from a stream: appends it to that stream's file,
/// forwards it on `logs_tx`, and reports what the pump should do next.
///
/// The wait for room on `logs_tx` keeps serving `ctl_rx`; see
/// [`reserve_slot`] for the cycle that would otherwise close. A pty pump
/// bounds that whole stall at [`LIVE_WAIT`] and then drops the live copy
/// alone: the line is already in the file.
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
            // A reader that has drained `logs` kept up with this pump, so its
            // next stall gets a whole `LIVE_WAIT` to spend.
            if logs_tx.capacity() == CHANNEL_CAPACITY {
                files.behind = false;
                files.live_waited = Duration::ZERO;
            }
            let slot = reserve_slot(logs_tx, files, ctl_rx, streams).await;
            match slot {
                Slot::Got(slot) => {
                    slot.send(LogLine { err, line });
                    AfterLine::KeepReading
                }
                Slot::Closed => AfterLine::LogsClosed,
                Slot::GaveUp => {
                    if !files.behind {
                        files.behind = true;
                        tracing::warn!(
                            path = ?files.stream(err).path,
                            "log bus full past {LIVE_WAIT:?}; live copies are skipped until it drains"
                        );
                    }
                    AfterLine::KeepReading
                }
            }
        }
        Ok(None) => AfterLine::StreamEnded, // normally the child exiting
        Err(error) => {
            tracing::error!(path = ?files.stream(err).path, %error, "log stream read failed");
            AfterLine::StreamEnded
        }
    }
}

/// Waits for room on `logs_tx`, serving control requests and idle flushes
/// while it waits, and gives up once this pump's live-bus budget is spent.
///
/// A pump parked on a full `logs` channel is parked for as long as the sheep
/// task takes, so without the idle-flush branch the line just appended would
/// sit in the buffer that long. A `select!` handler is not cancellable, so the
/// bound is an arm and not a `timeout` around this whole call.
async fn reserve_slot<'tx, O, E>(
    logs_tx: &'tx mpsc::Sender<LogLine>,
    files: &mut LogFiles,
    ctl_rx: &mut mpsc::Receiver<LogCtl>,
    streams: &mut Streams<O, E>,
) -> Slot<'tx>
where
    O: AsyncRead + Unpin,
    E: AsyncRead + Unpin,
{
    // The common case skips the `select!`, whose timer is registered and
    // removed per call under a lock every worker thread contends for.
    let first = take_slot(logs_tx);
    if !matches!(first, Slot::GaveUp) {
        return first;
    }
    // The channel is full. A piped pump waits as long as the backpressure a
    // pipe gives it asks. A pty pump has `LIVE_WAIT` across a whole stall, so
    // a reader that frees a slot just in time on every line cannot hold it at
    // that reader's pace, it being the only reader its terminal has.
    let left = files
        .keeps_reading()
        .then(|| LIVE_WAIT.saturating_sub(files.live_waited));
    if left == Some(Duration::ZERO) {
        return Slot::GaveUp;
    }
    let live_by = left.map(|left| Instant::now() + left);
    let mut waiting = Instant::now();
    loop {
        // Recomputed every iteration from the stored mark, so losing the
        // race never extends the window.
        let flush_at = files.flush_deadline();
        // Every branch is documented cancel-safe, as `select!` requires: a
        // `reserve` that loses the race has taken no slot, a `recv` that
        // loses it has taken no message, and a `sleep_until` that loses it
        // is rebuilt against the same absolute deadline.
        tokio::select! {
            slot = logs_tx.reserve() => match slot {
                Ok(slot) => {
                    files.live_waited += waiting.elapsed();
                    return Slot::Got(slot);
                }
                // `reserve` waits, so a failure is a closed receiver.
                Err(_) => return Slot::Closed,
            },
            ctl = ctl_rx.recv() => match ctl {
                Some(ctl) => {
                    files.serve(ctl, streams).await;
                    // Serving a request is not waiting on `logs`, so a park
                    // for a handover spends nothing of this pump's budget.
                    waiting = Instant::now();
                }
                // The line in hand is still owed to the receiver. Awaited
                // outside the `select!` rather than looping, since a closed
                // receiver is ready on every poll. `reserve` on its own is
                // cancel-safe, so here the bound can be a `timeout`.
                None => {
                    return match live_by {
                        Some(at) => match timeout_at(at, logs_tx.reserve()).await {
                            Ok(Ok(slot)) => {
                                files.live_waited += waiting.elapsed();
                                Slot::Got(slot)
                            }
                            Ok(Err(_)) => Slot::Closed,
                            Err(_) => {
                                files.live_waited += waiting.elapsed();
                                Slot::GaveUp
                            }
                        },
                        None => match logs_tx.reserve().await {
                            Ok(slot) => Slot::Got(slot),
                            Err(_) => Slot::Closed,
                        },
                    };
                }
            },
            () = sleep_until(flush_at.unwrap_or_else(Instant::now)), if flush_at.is_some() => {
                files.flush_idle().await;
            }
            // Re-checked rather than given up on sight: a slot that came
            // ready in the same poll is worth taking.
            () = sleep_until(live_by.unwrap_or_else(Instant::now)), if live_by.is_some() => {
                files.live_waited += waiting.elapsed();
                return take_slot(logs_tx);
            }
        }
    }
}

/// The next line from an optional stream, or a future that never resolves
/// once there is no stream left to read.
///
/// The pump's `select!` needs a branch it can leave in place after a stream
/// ends: a ready `None` would be re-selected on every poll and spin the loop,
/// while pending forever drops the branch out of contention. Cancel-safe, as a
/// `select!` branch must be: see [`LineReader::read_line`].
async fn next_line_ref<R>(lines: &mut Option<LineReader<R>>) -> io::Result<Option<&str>>
where
    R: AsyncRead + Unpin,
{
    match lines {
        Some(lines) => lines.read_line().await,
        None => core::future::pending().await,
    }
}

/// [`next_line_ref`] with the line copied out, for a caller that keeps it
/// past the next read: the pump holds it while it waits for room on `logs`.
/// Cancel-safe for the same reason.
async fn next_line<R>(lines: &mut Option<LineReader<R>>) -> io::Result<Option<String>>
where
    R: AsyncRead + Unpin,
{
    match lines {
        Some(lines) => lines.read_line_owned().await,
        None => core::future::pending().await,
    }
}

/// Points `timer` at `deadline`, touching the timer wheel only when the
/// deadline moved or was retired.
///
/// The pump keeps one timer across lines: building one per `select!`
/// registers and removes it under a lock every worker thread contends for.
fn arm_idle_flush(timer: &mut Option<Pin<Box<Sleep>>>, deadline: Option<Instant>) {
    match (timer.as_mut(), deadline) {
        (_, None) => *timer = None,
        (Some(sleep), Some(at)) if sleep.deadline() == at => {}
        (Some(sleep), Some(at)) => sleep.as_mut().reset(at),
        (None, Some(at)) => *timer = Some(Box::pin(sleep_until(at))),
    }
}

/// Resolves when `timer` fires, and never while it is unarmed.
///
/// Cancel-safe: a `Sleep` that loses a `select!` race stays registered at
/// the same deadline.
async fn idle_flush_due(timer: &mut Option<Pin<Box<Sleep>>>) {
    match timer {
        Some(sleep) => sleep.as_mut().await,
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
/// and [`reserve_slot`]'s, and a local would be visible to only one. `None`
/// per stream is one that has reached EOF or failed, and the pump drops the
/// reader at that moment, which closes the descriptor.
pub(super) struct Streams<O, E> {
    /// The stdout reader, until stdout ends.
    pub(super) out: Option<LineReader<O>>,
    /// The stderr reader, until stderr ends.
    pub(super) err: Option<LineReader<E>>,
}

/// Writes out the whole lines one stream's reader is already holding, and
/// touches the pipe behind it not at all.
///
/// This is what a descriptor report owes the successor: the reader has taken
/// up to [`super::READ_BUFFER`] off the pipe the successor will never see
/// there, and the `execve` destroys it. Stopping at "no whole line is
/// buffered" makes the buffer strictly shrink.
#[cfg(unix)]
pub(super) async fn drain_ready<R>(lines: &mut Option<LineReader<R>>, file: &mut LogFile)
where
    R: AsyncRead + Unpin,
{
    let Some(reader) = lines.as_mut() else {
        return;
    };
    while reader.get_ref().buffer().contains(&b'\n') {
        // A delimiter already in the buffer means no `read(2)`, so nothing
        // new arrives to replace what is written.
        let Ok(Some(line)) = reader.read_line().await else {
            return;
        };
        file.append(line).await;
    }
}

/// Writes out what the streams still hold once the sheep task has let go,
/// and stops after [`FINAL_DRAIN`] however much is left. Files only:
/// `logs_tx` is closed, which is what brought us here.
///
/// Each stream is retired on EOF or a read failure, so a child that has
/// exited ends this in one poll per stream. The budget covers the other case:
/// a lamb that inherited a write end keeps the pipe open.
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
                result = next_line_ref(&mut streams.out) => {
                    match result {
                        Ok(Some(line)) => files.stream(false).append(line).await,
                        Ok(None) | Err(_) => streams.out = None,
                    }
                }
                result = next_line_ref(&mut streams.err) => {
                    match result {
                        Ok(Some(line)) => files.stream(true).append(line).await,
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
/// `logs_tx`, which lands in a [`BufWriter`](tokio::io::BufWriter): a line
/// seen there need not be in the file yet. One task for both streams, so one
/// [`LogCtl::Reopen`] swaps both files and answers once.
#[allow(
    clippy::too_many_arguments,
    reason = "eight: both streams, both sinks, both channels, the fds, the stamp"
)]
pub(super) fn spawn_log_pump<O, E>(
    stdout: Option<O>,
    stderr: Option<E>,
    out_sink: LogSink,
    err_sink: LogSink,
    stamped: bool,
    logs_tx: mpsc::Sender<LogLine>,
    mut ctl_rx: mpsc::Receiver<LogCtl>,
    pipes: PipeFds,
) where
    O: AsyncRead + Unpin + Send + 'static,
    E: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut files = LogFiles {
            out: LogFile::from_sink(out_sink, stamped).await,
            err: LogFile::from_sink(err_sink, stamped).await,
            pipes,
            #[cfg(unix)]
            parked: false,
            behind: false,
            live_waited: Duration::ZERO,
        };
        let mut streams = Streams {
            out: stdout.map(|reader| LineReader::new(with_read_buffer(reader))),
            err: stderr.map(|reader| LineReader::new(with_read_buffer(reader))),
        };

        let mut idle_flush = None;
        while streams.out.is_some() || streams.err.is_some() {
            // Recomputed every iteration from the stored mark;
            // `reserve_slot` carries the same branch.
            arm_idle_flush(&mut idle_flush, files.flush_deadline());
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
                                files.pipes.pty = None;
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

                () = idle_flush_due(&mut idle_flush) => files.flush_idle().await,
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
/// Serial on purpose: two concurrent writers can interleave mid-line. A line
/// queued behind one the app is not reading waits, so the caller bounds its
/// own wait. A request whose caller has stopped listening is dropped.
///
/// `terminator` ends each line: `b'\n'` for a pipe, `b'\r'` for a terminal.
pub(super) fn spawn_stdin_pump<W>(
    stdin: Option<W>,
    terminator: u8,
    mut rx: mpsc::Receiver<StdinWrite>,
) where
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
        while let Some(request) = rx.recv().await {
            write_line(&mut stdin, terminator, request).await;
        }
    });
}

/// Writes one whispered line to `stdin` and answers its caller.
///
/// A request whose caller has stopped listening is dropped unwritten.
pub(super) async fn write_line<W>(stdin: &mut W, terminator: u8, request: StdinWrite)
where
    W: AsyncWrite + Unpin,
{
    let StdinWrite { line, done } = request;
    if done.is_closed() {
        // The supervisor's `STDIN_WRITE_TIMEOUT` expired and it
        // dropped the receiver. Writing now would deliver a line the
        // operator was already told was not written.
        return;
    }
    let mut bytes = line.into_bytes();
    // Exactly one terminator, appended here and nowhere else: the
    // wire carries the line without one (`Request::SendLine::line`).
    bytes.push(terminator);
    let result = match stdin.write_all(&bytes).await {
        Ok(()) => stdin.flush().await,
        Err(error) => Err(error),
    };
    let _ = done.send(result.map_err(|error| RunnerError::WriteFailed(error.to_string())));
}

/// Wires the daemon side of the shepherd channel: a reader task decodes
/// newline-JSON [`ChildMessage`]s onto `from_child_tx`; a writer task encodes
/// [`ShepherdMessage`]s taken from `to_child_rx` back onto the socket.
///
/// Generic over the transport: the daemon's end is a `UnixStream` half of a
/// socketpair on unix and an accepted named-pipe server instance on Windows.
/// `tokio::io::split`, since `NamedPipeServer` has no `into_split` of its own.
pub(super) fn spawn_channel_pumps<S>(
    daemon_end: S,
    from_child_tx: mpsc::Sender<ChildMessage>,
    mut to_child_rx: mpsc::Receiver<ShepherdMessage>,
) where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(daemon_end);

    tokio::spawn(async move {
        let mut lines = LineReader::new(BufReader::new(read_half));
        loop {
            match lines.read_line().await {
                Ok(Some(line)) => match serde_json::from_str::<ChildMessage>(line) {
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
