//! Handles the scripted runner hands out in place of real OS ones.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

/// A handle on a terminal that takes whatever it is sent and says nothing.
///
/// Needs a tokio runtime, as the rest of a spawn does.
pub(super) fn quiet_pty() -> PtyHandle {
    let output = Arc::new(broadcast::channel(1).0);
    let (control, mut commands) = mpsc::channel(super::scripted_runner::CHANNEL_CAPACITY);
    let handle = PtyHandle::new(&output, control, Arc::new(AtomicBool::new(false)));
    tokio::spawn(async move {
        // Owns the sender, as a real terminal's reader does.
        let _output = output;
        while let Some(command) = commands.recv().await {
            let (PtyCommand::Input { done, .. } | PtyCommand::Resize { done, .. }) = command;
            let _ = done.send(Ok(()));
        }
    });
    handle
}
