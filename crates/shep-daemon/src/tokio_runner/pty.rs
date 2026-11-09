//! A sheep's pseudo-terminal: the pair a `pty = true` child runs on, the
//! master's reader, and the task that types into it.

use core::future::poll_fn;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, ready};

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::process::Stdio;
use std::sync::Arc;

use bytes::Bytes;
use rustix::fs::{self, Mode, OFlags};
use rustix::process::{Pid, Signal};
use rustix::pty;
use rustix::termios::{self, LocalModes, Winsize};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::unix::pipe::Receiver;
use tokio::process::{ChildStdout, Command};
use tokio::sync::{broadcast, mpsc};

use super::CHANNEL_CAPACITY;
use super::ansi::AnsiLines;
use super::pump::write_line;
use crate::runner::{PtyCommand, PtyError, PtyHandle, StdinWrite};

/// Rows of every sheep's window at spawn: fixed for every sheep, and a program
/// that wraps to the window should wrap once, wide enough for a log line. An
/// attach replaces it with the operator's own size.
pub(super) const PTY_ROWS: u16 = 50;

/// Columns of every sheep's window at spawn.
pub(super) const PTY_COLS: u16 = 200;

/// A new master, close-on-exec from the moment it exists. The shepherd forks
/// other children on other threads, and a flag set afterwards leaves a gap in
/// which one inherits the master and holds the slave open.
#[cfg(target_os = "linux")]
fn open_master() -> io::Result<OwnedFd> {
    use rustix::pty::OpenptFlags;
    Ok(pty::openpt(
        OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC,
    )?)
}

/// `openpt` has no close-on-exec flag off Linux, so this opens the clone
/// device it opens, with `O_CLOEXEC` in the same call.
#[cfg(not(target_os = "linux"))]
fn open_master() -> io::Result<OwnedFd> {
    Ok(fs::open(
        "/dev/ptmx",
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// Sets the window of the terminal `fd` is an end of.
fn set_window(fd: impl rustix::fd::AsFd, rows: u16, cols: u16) -> io::Result<()> {
    Ok(termios::tcsetwinsize(
        fd,
        Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?)
}

/// The group a resize asks to redraw: the terminal's own foreground group
/// where it names one, and the group `sheep_pid` leads where it does not.
///
/// macOS answers `tcgetpgrp` with `ENOTTY` for any caller that does not hold
/// that terminal as its controlling terminal (`TCGETPGRP(3)`), and the
/// shepherd never holds it: the child takes it with `setsid` and
/// `TIOCSCTTY` (`sys.rs`). The fallback is the same group until the sheep's
/// app moves the terminal's foreground group onto one of its children.
///
/// # Errors
///
/// `sheep_pid` has no `kill(2)` form: 0, which would signal the shepherd's
/// own group, or a number above `i32::MAX`.
fn redraw_group(named: Option<Pid>, sheep_pid: u32) -> io::Result<Pid> {
    named
        .or_else(|| Pid::from_raw(i32::try_from(sheep_pid).ok()?))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pid {sheep_pid} is not a signallable process id"),
            )
        })
}

/// Asks the sheep's terminal to redraw, with `SIGWINCH`.
///
/// `TIOCSWINSZ` sends it only when the size changes, so a resize to the size
/// the terminal already has must send it itself. A resize that does change the
/// size gets the kernel's signal beside this one.
///
/// # Errors
///
/// [`redraw_group`] refused the pid, or `kill(2)` failed (`ESRCH` for a sheep
/// that is already gone).
fn ask_for_a_redraw(fd: impl rustix::fd::AsFd, sheep_pid: u32) -> io::Result<()> {
    let named = termios::tcgetpgrp(fd);
    if let Err(error) = &named {
        tracing::debug!(%error, "this terminal names no foreground group");
    }
    let group = redraw_group(named.ok(), sheep_pid)?;
    Ok(rustix::process::kill_process_group(group, Signal::WINCH)?)
}

/// The master end of a sheep's terminal, registered with the reactor.
///
/// Shared, so the reader and the [`PtyWriter`] work one descriptor.
pub(super) struct PtyMaster {
    fd: Arc<AsyncFd<OwnedFd>>,
    /// Set once the reader has seen the terminal end or been dropped.
    closed: Arc<AtomicBool>,
}

impl PtyMaster {
    /// Sets `master` non-blocking and registers it with the reactor.
    fn register(master: OwnedFd) -> io::Result<Self> {
        let flags = fs::fcntl_getfl(&master)?;
        fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;
        // Tokio deprecated `new` for `register`, which needs tokio 1.53.3 where
        // the workspace floor is 1.41. An `OwnedFd` is the open, stable
        // descriptor `register` asks for, so the unsoundness does not apply.
        #[allow(deprecated, reason = "register needs a newer tokio floor")]
        let fd = AsyncFd::new(master)?;
        Ok(Self {
            fd: Arc::new(fd),
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Takes a master a predecessor image carried across an exec.
    ///
    /// The kind is checked again, since the number came out of a blob: a
    /// descriptor that is not a terminal is refused. Must run inside a tokio
    /// runtime.
    ///
    /// # Errors
    ///
    /// `file` is not a terminal, or it could not be made non-blocking or
    /// registered with the reactor.
    pub(super) fn from_file(file: File) -> io::Result<Self> {
        if !termios::isatty(&file) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a carried pty master is not a terminal",
            ));
        }
        Self::register(OwnedFd::from(file))
    }

    /// The master's descriptor number, which a handover carries.
    pub(super) fn raw_fd(&self) -> RawFd {
        self.fd.get_ref().as_raw_fd()
    }
}

/// Opens a pair sized [`PTY_ROWS`] by [`PTY_COLS`]: the master the shepherd
/// reads, and the slave the child gets for fd 0, 1 and 2.
///
/// The slave is opened `O_NOCTTY` so the shepherd does not take it as its own
/// controlling terminal. Must run inside a tokio runtime.
///
/// # Errors
///
/// Any of `posix_openpt`, `grantpt`, `unlockpt`, `ptsname`, the slave open,
/// the window size or the master's registration failed.
pub(super) fn open() -> io::Result<(PtyMaster, OwnedFd)> {
    let master = open_master()?;
    pty::grantpt(&master)?;
    pty::unlockpt(&master)?;
    let name = pty::ptsname(&master, Vec::new())?;
    let slave = fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    set_window(&slave, PTY_ROWS, PTY_COLS)?;
    Ok((PtyMaster::register(master)?, slave))
}

/// Puts `command`'s child on a new terminal: the slave as its fd 0, 1 and 2
/// and its controlling terminal. Returns the master.
///
/// The parent's copies of the slave close when `command` drops, so the
/// caller drops it once the child is spawned, and the master then reads the
/// end of the stream when the child's last descriptor closes. The child leads
/// its own process group, so the caller sets no `process_group`. Start
/// reading the master before the child exits: macOS drops what the slave
/// queued once that close leaves nothing reading it (#1590).
///
/// # Errors
///
/// [`open`] failed, or the slave could not be duplicated.
pub(super) fn attach(command: &mut Command) -> io::Result<PtyMaster> {
    let (master, slave) = open()?;
    command.stdin(Stdio::from(slave.try_clone()?));
    command.stdout(Stdio::from(slave.try_clone()?));
    command.stderr(Stdio::from(slave));
    crate::sys::take_terminal_on_exec(command.as_std_mut());
    Ok(master)
}

/// What a sheep's log pump reads as its stdout.
pub(super) enum Stdout {
    /// The child's stdout pipe.
    Pipe(ChildStdout),
    /// A stdout pipe's read end a predecessor image carried across an exec.
    Carried(Receiver),
    /// The child's terminal, filtered to plain lines.
    Terminal(AnsiLines<PtyReader>),
}

impl AsyncRead for Stdout {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Pipe(pipe) => Pin::new(pipe).poll_read(cx, buf),
            Self::Carried(pipe) => Pin::new(pipe).poll_read(cx, buf),
            Self::Terminal(lines) => Pin::new(lines).poll_read(cx, buf),
        }
    }
}

impl PtyMaster {
    /// The master as an [`AsyncRead`] of what the child wrote, each chunk
    /// copied to `tap` first while anyone listens.
    pub(super) fn into_reader(self, tap: Arc<broadcast::Sender<Bytes>>) -> PtyReader {
        PtyReader { master: self, tap }
    }

    /// An [`AsyncWrite`] of keys to the child, over the same master, holding
    /// `sheep_pid` as the group a resize asks to redraw.
    pub(super) fn writer(&self, sheep_pid: u32) -> PtyWriter {
        PtyWriter {
            fd: Arc::clone(&self.fd),
            pending: 0,
            sheep_pid,
        }
    }
}

/// Chunks a slow subscriber may fall behind by before it loses the oldest.
const PTY_TAP_CAPACITY: usize = 64;

/// Puts a master to work: its raw output to a [`PtyHandle`]'s subscribers and
/// then the log pump, and one task typing into it for `whispers` and the
/// handle alike.
///
/// `sheep_pid` is the sheep that leads the group a resize asks to redraw.
/// Returns what the log pump reads, and the handle. Must run inside a tokio
/// runtime.
pub(super) fn wire(
    master: PtyMaster,
    whispers: mpsc::Receiver<StdinWrite>,
    sheep_pid: u32,
) -> (Stdout, PtyHandle) {
    let tap = Arc::new(broadcast::channel(PTY_TAP_CAPACITY).0);
    let (control, commands) = mpsc::channel(CHANNEL_CAPACITY);
    let handle = PtyHandle::new(&tap, control, Arc::clone(&master.closed));
    spawn_pty_pump(master.writer(sheep_pid), whispers, commands);
    let stdout = Stdout::Terminal(AnsiLines::new(master.into_reader(tap)));
    (stdout, handle)
}

/// Types into the master, one request at a time, whichever queue it came on.
///
/// One task for both, so a handle's bytes and a whispered line each land
/// whole. Ends once every sender of both queues has dropped.
fn spawn_pty_pump(
    mut writer: PtyWriter,
    mut whispers: mpsc::Receiver<StdinWrite>,
    mut commands: mpsc::Receiver<PtyCommand>,
) {
    tokio::spawn(async move {
        let (mut whispers_open, mut commands_open) = (true, true);
        while whispers_open || commands_open {
            tokio::select! {
                // Enter sends a carriage return, which a cooked terminal
                // turns into the newline the program reads.
                whisper = whispers.recv(), if whispers_open => match whisper {
                    Some(request) => write_line(&mut writer, b'\r', request).await,
                    None => whispers_open = false,
                },
                command = commands.recv(), if commands_open => match command {
                    Some(command) => writer.serve(command).await,
                    None => commands_open = false,
                },
            }
        }
    });
}

/// Most bytes one write to the master may carry: macOS's `MAX_CANON` is 1024
/// and Linux's line buffer is 4096, and a cooked terminal drops what exceeds
/// it, so the smaller one less a byte is the bound.
pub(super) const PTY_WRITE_MAX: usize = 1023;

/// Refuses a write the line discipline would cut short.
fn within_terminal_buffer(len: usize) -> io::Result<()> {
    if len > PTY_WRITE_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a line of {len} bytes is over the {PTY_WRITE_MAX} a terminal holds"),
        ));
    }
    Ok(())
}

/// Keys typed to the child's terminal: what its fd 0 reads, after the line
/// discipline.
///
/// The stdin pump hands it one whole line per write, and a line over
/// [`PTY_WRITE_MAX`] is refused unwritten.
pub(super) struct PtyWriter {
    fd: Arc<AsyncFd<OwnedFd>>,
    /// Bytes typed since the last line end, which a cooked terminal still
    /// holds as one unfinished line.
    pending: usize,
    /// The sheep whose terminal this is, for the group a resize signals.
    sheep_pid: u32,
}

impl PtyWriter {
    /// Answers one request of a [`PtyHandle`].
    async fn serve(&mut self, command: PtyCommand) {
        match command {
            PtyCommand::Input { bytes, done } => {
                if done.is_closed() {
                    return;
                }
                let _ = done.send(self.write_raw(&bytes).await);
            }
            PtyCommand::Resize { rows, cols, done } => {
                // The answer is about the size. A redraw the shepherd could
                // not ask for is its own to report, not a reason to fail an
                // attach that got its window.
                let result = set_window(self.fd.get_ref(), rows, cols)
                    .map_err(|error| PtyError::Failed(error.to_string()));
                if result.is_ok()
                    && let Err(error) = ask_for_a_redraw(self.fd.get_ref(), self.sheep_pid)
                {
                    tracing::warn!(
                        pid = self.sheep_pid,
                        %error,
                        "a resize could not ask the sheep to redraw"
                    );
                }
                let _ = done.send(result);
            }
        }
    }

    /// Writes `bytes` in pieces a terminal holds, with no terminator.
    ///
    /// Refused before any byte is written if a cooked terminal would cut it
    /// (see [`Self::check_line`]). A raw terminal hands bytes to the child as
    /// they come, so the pieces go through whatever their length.
    async fn write_raw(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        self.check_line(bytes)
            .map_err(|error| PtyError::Failed(error.to_string()))?;
        let mut rest = bytes;
        while !rest.is_empty() {
            let piece = &rest[..rest.len().min(PTY_WRITE_MAX)];
            let wrote = poll_fn(|cx| self.poll_put(cx, piece))
                .await
                .map_err(|error| PtyError::Failed(error.to_string()))?;
            rest = &rest[wrote..];
        }
        Ok(())
    }

    /// Refuses `bytes` if a cooked terminal would drop part of them.
    ///
    /// A cooked terminal keeps what is typed in one buffer until a newline
    /// and drops what goes past it, so the line `bytes` ends, with whatever
    /// earlier writes left unterminated in `pending`, must fit
    /// [`PTY_WRITE_MAX`], and so must each later line in `bytes`. A raw
    /// terminal keeps no line, so there is nothing to count.
    fn check_line(&mut self, bytes: &[u8]) -> io::Result<()> {
        let cooked = termios::tcgetattr(self.fd.get_ref())?
            .local_modes
            .contains(LocalModes::ICANON);
        if !cooked {
            self.pending = 0;
            return Ok(());
        }
        let longest = bytes
            .split_inclusive(|byte| ends_line(*byte))
            .enumerate()
            .map(|(index, line)| line.len() + if index == 0 { self.pending } else { 0 })
            .max()
            .unwrap_or(0);
        within_terminal_buffer(longest)
    }

    /// One write to the master, with no check, noting what it leaves
    /// unterminated.
    fn poll_put(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        loop {
            let mut ready = ready!(self.fd.poll_write_ready(cx))?;
            let wrote =
                ready.try_io(|fd| rustix::io::write(fd.get_ref(), buf).map_err(io::Error::from));
            if let Ok(result) = wrote {
                if let Ok(count) = result {
                    self.note_typed(&buf[..count]);
                }
                return Poll::Ready(result);
            }
        }
    }

    /// Tracks the bytes a cooked terminal now holds as an unfinished line.
    fn note_typed(&mut self, typed: &[u8]) {
        self.pending = match typed.iter().rposition(|byte| ends_line(*byte)) {
            Some(end) => typed.len() - end - 1,
            None => self.pending + typed.len(),
        };
    }
}

/// Whether a cooked terminal reads `byte` as the end of a line: `\r` too, as
/// the default input flags turn it into `\n`.
fn ends_line(byte: u8) -> bool {
    matches!(byte, b'\n' | b'\r')
}

impl AsyncWrite for PtyWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        within_terminal_buffer(buf.len())?;
        this.check_line(buf)?;
        this.poll_put(cx, buf)
    }

    /// A write to the master is already with the line discipline.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    /// The master stays open for the reader, so there is nothing to close.
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// What the child wrote to its terminal, as a byte stream that ends when the
/// last slave descriptor closes.
pub(super) struct PtyReader {
    master: PtyMaster,
    /// The only strong owner of the sender, so a handle's subscribers see the
    /// stream end when this drops.
    tap: Arc<broadcast::Sender<Bytes>>,
}

impl PtyReader {
    /// Copies what a read just brought in to the tap, if anyone listens.
    fn tee(&self, fresh: &[u8]) {
        if self.tap.receiver_count() > 0 {
            let _ = self.tap.send(Bytes::copy_from_slice(fresh));
        }
    }
}

impl Drop for PtyReader {
    fn drop(&mut self) {
        self.master.closed.store(true, Ordering::Release);
    }
}

impl AsyncRead for PtyReader {
    /// A master whose slave side has closed fails `read(2)` with `EIO` on
    /// Linux and returns 0 on macOS: both end the stream, so no caller meets
    /// an error for a child that merely exited.
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        loop {
            let mut ready = ready!(this.master.fd.poll_read_ready(cx))?;
            let read = ready.try_io(|fd| {
                rustix::io::read(fd.get_ref(), buf.initialize_unfilled()).map_err(io::Error::from)
            });
            match read {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    if count == 0 {
                        this.master.closed.store(true, Ordering::Release);
                    } else {
                        this.tee(&buf.filled()[before..]);
                    }
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error))
                    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                {
                    this.master.closed.store(true, Ordering::Release);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_would_block) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use tokio::io::AsyncReadExt as _;
    use tokio::time::timeout;

    use super::*;

    /// How long a master gets to answer before a case calls it hung.
    const MASTER_DEADLINE: Duration = Duration::from_secs(5);

    /// What the slave writes to its terminal.
    const WRITTEN: &[u8] = b"bye\n";

    /// `WRITTEN` as the master hands it back: the terminal's `ONLCR` turns
    /// the `\n` into `\r\n`.
    const SEEN: &[u8] = b"bye\r\n";

    /// What a slave wrote reaches its master, and the master then ends the
    /// stream cleanly once the last slave closes.
    ///
    /// The reader exists before the write and the read finishes while a
    /// slave is still open: macOS answers a read of a master whose last
    /// slave closed with 0 and drops what that slave queued (#1590), so a
    /// case that starts reading after the close tests the platform.
    #[tokio::test]
    async fn a_master_hands_back_what_its_slave_wrote_and_then_ends() {
        let (master, slave) = open().unwrap();
        let mut reader = master.into_reader(Arc::new(broadcast::channel(1).0));
        rustix::io::write(&slave, WRITTEN).unwrap();

        let mut buf = [0u8; 64];
        let mut seen = Vec::new();
        timeout(MASTER_DEADLINE, async {
            while seen != SEEN {
                let read = reader
                    .read(&mut buf)
                    .await
                    .expect("the master must answer a read");
                assert_ne!(read, 0, "the master ended at {seen:?} before {SEEN:?}");
                seen.extend_from_slice(&buf[..read]);
            }
        })
        .await
        .expect("the master never handed back what its slave wrote");

        drop(slave);
        let mut rest = Vec::new();
        timeout(MASTER_DEADLINE, reader.read_to_end(&mut rest))
            .await
            .expect("the reader never saw the end of the stream")
            .expect("the end of the stream must not be an error");
        assert!(rest.is_empty(), "the master read {rest:?} past its own end");
    }

    /// Linux hands back what a slave queued before its last descriptor
    /// closed, and only then fails the read with `EIO`.
    ///
    /// That order is Linux's: macOS ends the stream at the close and the
    /// queued bytes never reach the reader, so this case is gated to Linux
    /// rather than left to fail there.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_linux_master_hands_back_queued_bytes_before_it_ends_with_eio() {
        let (master, slave) = open().unwrap();
        rustix::io::write(&slave, WRITTEN).unwrap();
        drop(slave);

        let mut seen = Vec::new();
        timeout(
            MASTER_DEADLINE,
            master
                .into_reader(Arc::new(broadcast::channel(1).0))
                .read_to_end(&mut seen),
        )
        .await
        .expect("the reader never saw the end of the stream")
        .expect("`EIO` must end the stream, not fail it");

        assert_eq!(seen, SEEN);
    }

    #[test]
    fn a_write_is_refused_one_byte_past_what_a_terminal_holds() {
        assert!(within_terminal_buffer(PTY_WRITE_MAX).is_ok());
        let refusal = within_terminal_buffer(PTY_WRITE_MAX + 1).unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "a line of 1024 bytes is over the 1023 a terminal holds"
        );
    }

    /// The terminal's own foreground group is the one to ask.
    #[test]
    fn a_terminal_that_names_a_group_is_asked_there() {
        assert_eq!(
            redraw_group(Pid::from_raw(4242), 111).unwrap(),
            Pid::from_raw(4242).unwrap()
        );
    }

    /// A terminal that names none, which is what macOS answers a shepherd that
    /// does not hold it as its controlling terminal, falls back to the group
    /// the sheep leads.
    #[test]
    fn a_terminal_that_names_none_falls_back_to_the_sheeps_own_group() {
        assert_eq!(
            redraw_group(None, 111).unwrap(),
            Pid::from_raw(111).unwrap()
        );
    }

    /// A pid with no `kill(2)` form is refused rather than sent: `kill(-0)` is
    /// `kill(0)`, the shepherd's own group.
    #[test]
    fn a_pid_with_no_kill_form_is_refused() {
        assert_eq!(
            redraw_group(None, 0).unwrap_err().to_string(),
            "pid 0 is not a signallable process id"
        );
        assert!(redraw_group(None, u32::MAX).is_err());
    }
}
