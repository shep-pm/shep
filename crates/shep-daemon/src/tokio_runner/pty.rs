//! A sheep's pseudo-terminal: the pair a `pty = true` child runs on, and the
//! master's reader.

use core::pin::Pin;
use core::task::{Context, Poll, ready};

use std::io;
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::sync::Arc;

use rustix::fs::{self, Mode, OFlags};
use rustix::pty;
use rustix::termios::{self, Winsize};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdout, Command};

use super::ansi::AnsiLines;

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

/// The master end of a sheep's terminal, registered with the reactor.
///
/// Shared, so the reader and the [`PtyWriter`] work one descriptor.
pub(super) struct PtyMaster {
    fd: Arc<AsyncFd<OwnedFd>>,
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
    termios::tcsetwinsize(
        &slave,
        Winsize {
            ws_row: PTY_ROWS,
            ws_col: PTY_COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;
    let flags = fs::fcntl_getfl(&master)?;
    fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;
    // Tokio deprecated `new` for `register`, which needs tokio 1.53.3 where
    // the workspace floor is 1.41. An `OwnedFd` is the open, stable
    // descriptor `register` asks for, so the unsoundness does not apply.
    #[allow(deprecated, reason = "register needs a newer tokio floor")]
    let fd = AsyncFd::new(master)?;
    Ok((PtyMaster { fd: Arc::new(fd) }, slave))
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
    /// The child's terminal, filtered to plain lines.
    Terminal(AnsiLines<PtyReader>),
}

impl Stdout {
    /// The master's output as plain lines.
    pub(super) fn terminal(master: PtyMaster) -> Self {
        Self::Terminal(AnsiLines::new(master.into_reader()))
    }
}

impl AsyncRead for Stdout {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Pipe(pipe) => Pin::new(pipe).poll_read(cx, buf),
            Self::Terminal(lines) => Pin::new(lines).poll_read(cx, buf),
        }
    }
}

impl PtyMaster {
    /// The master as an [`AsyncRead`] of what the child wrote.
    pub(super) fn into_reader(self) -> PtyReader {
        PtyReader { master: self }
    }

    /// An [`AsyncWrite`] of keys to the child, over the same master.
    pub(super) fn writer(&self) -> PtyWriter {
        PtyWriter {
            fd: Arc::clone(&self.fd),
        }
    }
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
        loop {
            let mut ready = ready!(this.master.fd.poll_read_ready(cx))?;
            let read = ready.try_io(|fd| {
                rustix::io::read(fd.get_ref(), buf.initialize_unfilled()).map_err(io::Error::from)
            });
            match read {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error))
                    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                {
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
            master.into_reader().read_to_end(&mut seen),
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
