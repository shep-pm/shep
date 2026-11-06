//! Handles the scripted runner hands out in place of real OS ones.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};

use crate::runner::{PtyCommand, PtyHandle};

/// Six real, distinct, open descriptors for a fake pump to report.
///
/// `/dev/null`, since the fake never writes to them: a handover blob only
/// needs the numbers to name something open. Six covers the widest shape a
/// blob carries, stdin pipe and shepherd channel included.
/// # Panics
/// If `/dev/null` cannot be opened six times: the process is out of descriptors.
#[cfg(unix)]
#[track_caller]
pub(super) fn open_reportable_fds() -> ([std::fs::File; 6], crate::handover::CarriedFds) {
    use std::os::fd::AsRawFd as _;

    let files = core::array::from_fn(|_| {
        std::fs::File::open("/dev/null").expect("a test host must be able to open /dev/null")
    });
    let files: [std::fs::File; 6] = files;
    let fds = crate::handover::CarriedFds {
        out_pipe: Some(files[0].as_raw_fd()),
        err_pipe: Some(files[1].as_raw_fd()),
        out_log: Some(files[2].as_raw_fd()),
        err_log: Some(files[3].as_raw_fd()),
        stdin: Some(files[4].as_raw_fd()),
        channel: Some(files[5].as_raw_fd()),
        dog_link: None,
        pty: None,
    };
    (files, fds)
}

/// The test side of a scripted terminal: what the daemon typed and sized, and
/// the means to speak from it.
#[derive(Debug)]
pub(super) struct PtyTap {
    /// The reader's end, as the real master's reader owns it. `None` once
    /// [`Self::end`] has closed the terminal.
    output: Mutex<Option<Arc<broadcast::Sender<Bytes>>>>,
    input: Mutex<Vec<u8>>,
    sizes: Mutex<Vec<(u16, u16)>>,
    closed: Arc<AtomicBool>,
}

impl PtyTap {
    /// Sends `bytes` to every subscriber, as one read of the master.
    pub(super) fn emit(&self, bytes: &[u8]) {
        if let Some(output) = &*self.output.lock().unwrap() {
            let _ = output.send(Bytes::copy_from_slice(bytes));
        }
    }

    /// Closes the terminal: subscribers see the end after their last chunk.
    pub(super) fn end(&self) {
        self.closed.store(true, Ordering::Release);
        self.output.lock().unwrap().take();
    }

    /// Everything typed into the terminal, in order.
    pub(super) fn input(&self) -> Vec<u8> {
        self.input.lock().unwrap().clone()
    }

    /// Every window size set, as `(rows, cols)`, in order.
    pub(super) fn sizes(&self) -> Vec<(u16, u16)> {
        self.sizes.lock().unwrap().clone()
    }

    /// How many receivers of the terminal's output are still alive.
    pub(super) fn subscribers(&self) -> usize {
        self.output
            .lock()
            .unwrap()
            .as_ref()
            .map_or(0, |output| output.receiver_count())
    }
}

/// A handle on a terminal that answers every request, and the tap that reads
/// back what it was asked.
///
/// Needs a tokio runtime, as the rest of a spawn does.
pub(super) fn scripted_pty() -> (PtyHandle, Arc<PtyTap>) {
    let output = Arc::new(broadcast::channel(super::scripted_runner::CHANNEL_CAPACITY).0);
    let (control, mut commands) = mpsc::channel(super::scripted_runner::CHANNEL_CAPACITY);
    let closed = Arc::new(AtomicBool::new(false));
    let handle = PtyHandle::new(&output, control, Arc::clone(&closed));
    let tap = Arc::new(PtyTap {
        output: Mutex::new(Some(output)),
        input: Mutex::new(Vec::new()),
        sizes: Mutex::new(Vec::new()),
        closed,
    });
    let served = Arc::clone(&tap);
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            let done = match command {
                PtyCommand::Input { bytes, done } => {
                    served.input.lock().unwrap().extend_from_slice(&bytes);
                    done
                }
                PtyCommand::Resize { rows, cols, done } => {
                    served.sizes.lock().unwrap().push((rows, cols));
                    done
                }
            };
            let _ = done.send(Ok(()));
        }
    });
    (handle, tap)
}
