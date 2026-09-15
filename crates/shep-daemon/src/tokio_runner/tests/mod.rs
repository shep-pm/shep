//! Shared harness for the tokio_runner test suite: [`PumpHarness`] and the
//! helpers built on it, used by [`log_io`] and [`handover`] (a third
//! module, spawn_io, is pending separately).
//!
//! Tests needing a real child live in `tests/real_runner.rs`; a
//! `tokio::io::duplex` half stands in for the pump's `AsyncRead` here.
//! `a_flush_reports_the_write_its_file_never_took` drives a `LogFile`
//! directly, the only way to force a write failure. Real clock, not paused,
//! except where a module opts a case into `start_paused = true`.

use std::fs;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncWrite, AsyncWriteExt as _, DuplexStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

#[cfg(unix)]
use crate::handover::CarriedFds;
use crate::runner::{LogCtl, LogLine, ReopenError};

use super::CHANNEL_CAPACITY;
#[cfg(unix)]
use super::IDLE_FLUSH;
use super::log_file::{LogSink, PipeFds};
use super::pump::spawn_log_pump;

mod handover;
mod log_io;
mod spawn_io;

/// How long a pump gets to answer before a test calls it hung. A pump
/// that is working answers in microseconds; this is slack for a loaded
/// runner, not an expected duration.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);

/// Room in each in-memory pipe standing in for a child's stdout/stderr.
///
/// Sized so a case can hand the pump more lines than `logs` will hold
/// without the writing side parking first: with a buffer that small,
/// "the pump stopped reading" and "the test stopped writing" become the
/// same observation.
const STREAM_BUFFER: usize = 4096;

/// One pump over two streams and two real files: everything
/// [`spawn_log_pump`] takes, with no child process involved.
///
/// Generic over the writing side only, so the descriptor cases can swap
/// the in-memory pair for a real one: an in-memory pipe has no
/// descriptor, so the pump is told its stream numbers rather than
/// reading them off its own readers. Every case about bytes rather
/// than descriptors uses the cheaper [`PumpHarness::start`].
struct PumpHarness<W = DuplexStream> {
    dir: tempfile::TempDir,
    out_path: PathBuf,
    err_path: PathBuf,
    out_writer: W,
    err_writer: W,
    logs: mpsc::Receiver<LogLine>,
    ctl: mpsc::Sender<LogCtl>,
    /// The stream descriptor numbers this harness handed the pump, which
    /// is what a descriptor report has to answer with.
    pipes: PipeFds,
}

impl PumpHarness {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.log");
        let err_path = dir.path().join("err.log");
        let (out_writer, out_reader) = tokio::io::duplex(STREAM_BUFFER);
        let (err_writer, err_reader) = tokio::io::duplex(STREAM_BUFFER);
        let (logs_tx, logs) = mpsc::channel(CHANNEL_CAPACITY);
        let (ctl, ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
        spawn_log_pump(
            Some(out_reader),
            Some(err_reader),
            LogSink::Path(out_path.clone()),
            LogSink::Path(err_path.clone()),
            logs_tx,
            ctl_rx,
            PipeFds::default(),
        );
        Self {
            dir,
            out_path,
            err_path,
            out_writer,
            err_writer,
            logs,
            ctl,
            pipes: PipeFds::default(),
        }
    }
}

/// A pump reading two real pipes, for the cases that are about
/// descriptor numbers rather than about bytes.
///
/// `tokio::net::unix::pipe` is what a child's stdout actually is, so the
/// numbers this hands the pump are the same kind of thing a spawn hands
/// it, and the test can hold the writing ends open for as long as it
/// needs the reading ends to stay valid.
#[cfg(unix)]
impl PumpHarness<tokio::net::unix::pipe::Sender> {
    fn start_over_pipes() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.log");
        let err_path = dir.path().join("err.log");
        let (out_writer, out_reader) = tokio::net::unix::pipe::pipe().unwrap();
        let (err_writer, err_reader) = tokio::net::unix::pipe::pipe().unwrap();
        let pipes = PipeFds {
            out: Some(out_reader.as_raw_fd()),
            err: Some(err_reader.as_raw_fd()),
            stdin: None,
            channel: None,
        };
        let (logs_tx, logs) = mpsc::channel(CHANNEL_CAPACITY);
        let (ctl, ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
        spawn_log_pump(
            Some(out_reader),
            Some(err_reader),
            LogSink::Path(out_path.clone()),
            LogSink::Path(err_path.clone()),
            logs_tx,
            ctl_rx,
            pipes,
        );
        Self {
            dir,
            out_path,
            err_path,
            out_writer,
            err_writer,
            logs,
            ctl,
            pipes,
        }
    }
}

impl<W: AsyncWrite + Unpin> PumpHarness<W> {
    /// Sends a [`LogCtl::ReportFds`] and waits for the answer.
    ///
    /// Generic over the writing side rather than living with the
    /// descriptor cases, because what a report does to the pump is not
    /// about descriptors at all: it flushes, it drains, and it parks.
    /// The cases about those three run over the cheaper in-memory pair.
    #[cfg(unix)]
    async fn report_fds(&self) -> CarriedFds {
        let (done, ack) = oneshot::channel();
        self.ctl
            .send(LogCtl::ReportFds { done })
            .await
            .expect("the pump must still be reading its control channel");
        timeout(PUMP_DEADLINE, ack)
            .await
            .expect("a descriptor report must be acknowledged")
            .expect("the pump must answer rather than drop the acknowledgement")
    }

    /// Sends a [`LogCtl::Resume`], which carries no acknowledgement.
    ///
    /// Nothing to wait for by design (see the variant): every case that
    /// resumes a pump then waits on what the pump does next, which is a
    /// stronger barrier than an answer would be.
    #[cfg(unix)]
    async fn resume(&self) {
        self.ctl
            .send(LogCtl::Resume)
            .await
            .expect("a parked pump must still be reading its control channel");
    }

    /// Writes one line into the chosen stream and waits for the pump to
    /// hand it back on `logs`, proof it read the line and issued the
    /// file write. Also orders the two streams: a test never has to
    /// guess which line arrives first.
    async fn feed(&mut self, err: bool, line: &str) {
        let writer = if err {
            &mut self.err_writer
        } else {
            &mut self.out_writer
        };
        writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        let observed = timeout(PUMP_DEADLINE, self.logs.recv())
            .await
            .expect("the pump must forward a line it has read")
            .expect("the pump must not end while its streams are open");
        assert_eq!(
            observed,
            LogLine {
                err,
                line: line.to_string()
            }
        );
    }

    /// Sends a [`LogCtl::Reopen`], waits for its acknowledgement, and
    /// requires success: every caller reopens paths the pump can open.
    async fn reopen(&self) {
        let outcome = self.reopen_for_answer().await;
        assert_eq!(outcome, Ok(()), "this reopen must have worked");
    }

    /// [`PumpHarness::reopen`] for the case where the answer itself is
    /// the assertion.
    async fn reopen_for_answer(&self) -> Result<(), ReopenError> {
        let (done, ack) = oneshot::channel();
        self.ctl
            .send(LogCtl::Reopen { done })
            .await
            .expect("the pump must still be reading its control channel");
        timeout(PUMP_DEADLINE, ack)
            .await
            .expect("a reopen must be acknowledged")
            .expect("the pump must answer rather than drop the acknowledgement")
    }

    /// Sends a [`LogCtl::Flush`], waits for its acknowledgement, and
    /// requires success: every caller flushes handles the pump can write.
    async fn flush(&self) {
        let (done, ack) = oneshot::channel();
        self.ctl
            .send(LogCtl::Flush { done })
            .await
            .expect("the pump must still be reading its control channel");
        let outcome = timeout(PUMP_DEADLINE, ack)
            .await
            .expect("a flush must be acknowledged")
            .expect("the pump must answer rather than drop the acknowledgement");
        assert_eq!(outcome, Ok(()), "this flush must have worked");
    }
}

/// One log file's contents with [`super::log_file::LogFile::append`]'s
/// per-line stamp taken back off, so a test can assert on the sheep's own
/// bytes.
///
/// The stamp is the daemon's and moves every run, so pinning it would
/// assert on the clock. `a_line_carries_the_time_it_was_written` checks
/// the stamp itself; this checks it is present and well formed as a
/// side effect of parsing (see [`unstamped`]).
fn log_text(path: &Path) -> String {
    unstamped(&fs::read_to_string(path).unwrap())
}

/// Drops the per-line stamp from every line of `text` that carries one,
/// leaving the rest untouched.
///
/// Uses [`shep_core::logstamp::strip`], the same call `bleats` makes, so
/// tests compare against what an operator sees. Recognises a stamp
/// rather than assuming one: some tests write a line straight to the log
/// file, ahead of any pump.
fn unstamped(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        out.push_str(shep_core::logstamp::strip(line));
        out.push('\n');
    }
    out
}

/// Waits for `path` to hold exactly `expected`, ignoring the per-line
/// stamps (see [`log_text`]).
///
/// A line on `logs` has reached the stream's buffer, not necessarily the
/// file (see [`spawn_log_pump`]'s ordering note), so this polls for
/// [`super::IDLE_FLUSH`] to write it through. A reopen acknowledgement
/// would be a cleaner barrier, but the point here is what the current
/// handle does, and a reopen replaces it.
async fn assert_file_settles(path: &Path, expected: &str) {
    let settled = timeout(PUMP_DEADLINE, async {
        while unstamped(&fs::read_to_string(path).unwrap_or_default()) != expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "{}: expected {expected:?}, found {:?}",
        path.display(),
        fs::read_to_string(path)
    );
}

/// Whether `fd` names something open in this process.
///
/// `F_GETFD` is the cheapest question the kernel answers about a
/// descriptor, and it is the one `handover::fds` already asks.
#[cfg(unix)]
fn is_open(fd: RawFd) -> bool {
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).is_ok()
}

/// How many times `a_last_line_written_before_the_sheep_task_lets_go_reaches_the_file`
/// reconstructs the race.
///
/// The two ready branches are picked at random, so a broken pump passes
/// one attempt about two times in five. Thirty-two takes that to
/// roughly 2e-13, costing nothing extra once the pump is right.
const RACE_ATTEMPTS: usize = 32;

/// Fills one pipe with whole lines until the kernel refuses another, and
/// answers with how many went in.
///
/// Uses `try_write`, not a timed `write_all`: saturation must be
/// something the kernel reports, since a full pipe and a merely slow
/// pump look identical to a wait. A short write's partial line is left
/// uncounted, so the answer is an exact count of whole lines.
///
/// `cfg(unix)`: `tokio::net::unix` does not exist on Windows.
#[cfg(unix)]
async fn fill_pipe(writer: &tokio::net::unix::pipe::Sender, line: &str) -> usize {
    // `try_write` reports `WouldBlock` for a not-yet-writable pipe as
    // readily as for a full one, so this establishes writability first,
    // before the loop starts counting refusals as saturation.
    timeout(PUMP_DEADLINE, writer.writable())
        .await
        .expect("an empty pipe is writable")
        .expect("the pipe is open");

    let mut whole = 0usize;
    loop {
        match writer.try_write(line.as_bytes()) {
            Ok(written) if written == line.len() => whole += 1,
            Ok(_) => return whole,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return whole,
            Err(error) => {
                panic!("the fill failed for a reason that is not a full pipe: {error}")
            }
        }
    }
}

/// How long a case watches a parked pump before believing it.
///
/// Several [`super::IDLE_FLUSH`] windows, because that is what turns a
/// pump that read a line into a file that shows one: a pump still
/// reading appends within microseconds and the idle flush writes it
/// through 50ms later, so a file unchanged across six of those windows
/// is a pump that never read.
#[cfg(unix)]
const STILL_WINDOWS: u32 = 6;

/// Lines a case writes straight into a stream, bypassing
/// [`PumpHarness::feed`], when the point is a pump that has fallen
/// behind rather than one keeping up.
///
/// More than `CHANNEL_CAPACITY`, so the pump fills `logs`, parks in
/// `reserve_slot`, and holds the rest in its reader.
#[cfg(unix)]
const BURST: u32 = 60;

/// Fails if `path` changes at all over the next few flush windows.
///
/// The negative half of the parking case, and the reason it is a window
/// rather than a single read: a pump that is still reading loses this
/// on its first poll, while a parked one cannot lose it at any length.
#[cfg(unix)]
async fn assert_file_holds_still(path: &Path, expected: &str) {
    for window in 1..=STILL_WINDOWS {
        tokio::time::sleep(IDLE_FLUSH).await;
        assert_eq!(
            log_text(path),
            expected,
            "{}: a parked pump wrote during flush window {window}",
            path.display()
        );
    }
}

/// Waits until nothing more will fit on `logs`, which is the state a
/// handover finds a chatty sheep's pump in.
///
/// Forces both cases below: with the channel full, the pump parks
/// inside `reserve_slot`, so everything read past that point sits in
/// its reader until a report drains it.
#[cfg(unix)]
async fn wait_for_a_full_logs_channel(logs: &mpsc::Receiver<LogLine>) {
    let filled = timeout(PUMP_DEADLINE, async {
        while logs.len() < CHANNEL_CAPACITY {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        filled.is_ok(),
        "the pump must fall behind a burst it cannot forward"
    );
}

/// Everything still readable from a pipe handle, to EOF.
///
/// `WouldBlock` is retried rather than treated as the end: the handle
/// shares its open file description with a `tokio` reader, which put
/// the description in non-blocking mode, so an empty moment reads as an
/// error rather than as a wait.
#[cfg(unix)]
async fn read_to_eof(handle: std::io::PipeReader) -> String {
    let mut out = Vec::new();
    let mut buf = [0_u8; 4096];
    let drained = timeout(PUMP_DEADLINE, async {
        loop {
            match std::io::Read::read(&mut &handle, &mut buf) {
                Ok(0) => return,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(error) => panic!("the successor's handle must be readable: {error}"),
            }
        }
    })
    .await;
    assert!(
        drained.is_ok(),
        "the pipe must reach EOF once the sheep is gone"
    );
    String::from_utf8(out).expect("a pipe of ASCII lines")
}

/// The smallest buffer a pipe starts with on any host these cases run
/// on: macOS opens one at 16 KiB, Linux at 64.
///
/// The case below writes its whole run in one go into a pipe whose
/// reader has stopped draining it, so the run has to fit under this or
/// the writing side parks and the test deadlocks instead of failing.
#[cfg(unix)]
const SMALLEST_PIPE: usize = 16 * 1024;

/// One line of the run below, sized so the arithmetic in it is exact.
#[cfg(unix)]
const RUN_LINE: usize = 8;

/// Hands the runtime to the pump task and back.
///
/// `#[tokio::test]` runs on a current-thread runtime, so yielding is
/// what lets the pump run at all: dropping a duplex writer wakes its
/// read with EOF, and retiring a stream from there has no await of its
/// own, so the pump reaches its next park before this returns. The
/// repeats are slack for a pump that wakes with other work already
/// queued, not a race the count papers over.
async fn let_the_pump_settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
