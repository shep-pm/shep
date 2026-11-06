//! A sheep's pseudo-terminal: the pair a `pty = true` child runs on, the
//! master's reader, and the task that types into it.

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
use rustix::pty;
use rustix::termios::{self, LocalModes, Winsize};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, ReadBuf};
use tokio::net::unix::pipe::Receiver;
use tokio::process::{ChildStdout, Command};
use tokio::sync::{broadcast, mpsc};

use super::CHANNEL_CAPACITY;
use super::ansi::AnsiLines;
use super::pump::write_line;
use crate::runner::{PtyCommand, PtyError, PtyHandle, StdinWrite};

/// Rows of every sheep's window. Fixed: nothing resizes it yet, and a program
/// that wraps to the window should wrap once, wide enough for a log line.
pub(super) const PTY_ROWS: u16 = 50;

/// Columns of every sheep's window.
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
/// its own process group, so the caller sets no `process_group`.
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
    pub(super) fn into_reader(self, tap: broadcast::Sender<Bytes>) -> PtyReader {
        PtyReader { master: self, tap }
    }

    /// An [`AsyncWrite`] of keys to the child, over the same master.
    pub(super) fn writer(&self) -> PtyWriter {
        PtyWriter {
            fd: Arc::clone(&self.fd),
        }
    }
}

/// Chunks a slow subscriber may fall behind by before it loses the oldest.
const PTY_TAP_CAPACITY: usize = 64;

/// Puts a master to work: its raw output to a [`PtyHandle`]'s subscribers and
/// then the log pump, and one task typing into it for `whispers` and the
/// handle alike.
///
/// Returns what the log pump reads, and the handle. Must run inside a tokio
/// runtime.
pub(super) fn wire(master: PtyMaster, whispers: mpsc::Receiver<StdinWrite>) -> (Stdout, PtyHandle) {
    let (tap, _) = broadcast::channel(PTY_TAP_CAPACITY);
    let (control, commands) = mpsc::channel(CHANNEL_CAPACITY);
    let handle = PtyHandle::new(tap.clone(), control, Arc::clone(&master.closed));
    spawn_pty_pump(master.writer(), whispers, commands);
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
                let result = set_window(self.fd.get_ref(), rows, cols)
                    .map_err(|error| PtyError::Failed(error.to_string()));
                let _ = done.send(result);
            }
        }
    }

    /// Writes `bytes` in pieces a terminal holds, with no terminator.
    ///
    /// A cooked terminal keeps what is typed until a newline in one buffer
    /// and drops the rest, so a line over [`PTY_WRITE_MAX`] there is refused
    /// before any byte is written. A raw terminal hands bytes to the child as
    /// they come, so the pieces go through whatever their length.
    async fn write_raw(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        let cooked = termios::tcgetattr(self.fd.get_ref())
            .map_err(|error| PtyError::Failed(error.to_string()))?
            .local_modes
            .contains(LocalModes::ICANON);
        if cooked {
            // `\r` ends a line too: the default input flags turn it into `\n`.
            let longest = bytes
                .split_inclusive(|byte| matches!(byte, b'\n' | b'\r'))
                .map(<[u8]>::len)
                .max()
                .unwrap_or(0);
            within_terminal_buffer(longest).map_err(|error| PtyError::Failed(error.to_string()))?;
        }
        for piece in bytes.chunks(PTY_WRITE_MAX) {
            self.write_all(piece)
                .await
                .map_err(|error| PtyError::Failed(error.to_string()))?;
        }
        Ok(())
    }
}

impl AsyncWrite for PtyWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        within_terminal_buffer(buf.len())?;
        loop {
            let mut ready = ready!(this.fd.poll_write_ready(cx))?;
            let wrote =
                ready.try_io(|fd| rustix::io::write(fd.get_ref(), buf).map_err(io::Error::from));
            if let Ok(result) = wrote {
                return Poll::Ready(result);
            }
        }
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
    tap: broadcast::Sender<Bytes>,
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
    use super::*;

    use tokio::io::AsyncReadExt as _;
    use tokio::time::timeout;

    /// A child that has exited is a slave with no holder. The bytes it wrote
    /// still arrive, then the stream ends cleanly.
    #[tokio::test]
    async fn a_master_whose_child_exited_reads_as_end_of_file() {
        let (master, slave) = open().unwrap();
        rustix::io::write(&slave, b"bye\n").unwrap();
        drop(slave);

        let mut seen = Vec::new();
        timeout(
            std::time::Duration::from_secs(5),
            master
                .into_reader(broadcast::channel(1).0)
                .read_to_end(&mut seen),
        )
        .await
        .expect("the reader never saw the end of the stream")
        .expect("end of stream must not be an error");

        assert_eq!(seen, b"bye\r\n");
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
}
