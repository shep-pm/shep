//! The operator's real terminal: raw mode, stdin, and its size.
//!
//! Nothing here is unit-tested, because a test has no terminal to put in raw
//! mode. [`super::session`] takes the same facts as channels instead.

use std::io::{self, Read as _};

use crossterm::terminal::enable_raw_mode;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use super::{Size, Tty};
use crate::lookout::term;

/// How many bytes one read of stdin takes. A paste arrives as several reads.
const READ_BYTES: usize = 4096;

/// Reads stdin on a thread of its own, one chunk per read.
///
/// Not tokio's stdin: its read runs on a blocking thread that dropping the
/// runtime waits for, and a read nothing satisfies would hang the exit.
fn read_stdin() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(64);
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; READ_BYTES];
        while let Ok(read) = stdin.read(&mut buffer) {
            if read == 0 || tx.blocking_send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

/// The terminal's current size.
fn window_size() -> io::Result<Size> {
    let (cols, rows) = crossterm::terminal::size()?;
    Ok(Size { rows, cols })
}

/// Forwards each `SIGWINCH` as the terminal's new size.
fn watch_resizes() -> io::Result<(mpsc::Receiver<Size>, tokio::task::JoinHandle<()>)> {
    let mut winch = signal(SignalKind::window_change())?;
    let (tx, rx) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        while winch.recv().await.is_some() {
            let Ok(size) = window_size() else { continue };
            if tx.send(size).await.is_err() {
                break;
            }
        }
    });
    Ok((rx, task))
}

/// Puts the terminal in raw mode and returns what [`super::session`] reads
/// from it. Dropping the [`Tty`] puts the terminal back.
///
/// # Errors
///
/// Whatever the terminal refused: no size, no signal handler, or no raw mode
/// because stdin is not a terminal.
pub(super) fn open() -> io::Result<Tty> {
    let size = window_size()?;
    let (resizes, watcher) = watch_resizes()?;
    term::install_panic_hook();
    // Armed before raw mode goes on, so a failure between the two cannot
    // leave the terminal raw.
    let restore = term::RestoreGuard::with_action(move || {
        watcher.abort();
        term::restore();
    });
    enable_raw_mode()?;
    Ok(Tty {
        size,
        keys: read_stdin(),
        resizes,
        restore,
    })
}
