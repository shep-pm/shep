//! Finding the channel this process was given, and opening it.
//!
//! Branch on which environment variable is present, never on the platform.
//! The daemon sets exactly one of them and never both: `SHEP_CHANNEL_FD` on
//! unix, `SHEP_CHANNEL_PIPE` on Windows. Neither means no channel was opened
//! for this process, which is the ordinary case.

use std::path::PathBuf;

use core::sync::atomic::{AtomicBool, Ordering};

use crate::ChannelError;

/// The descriptor number variable, set on unix only.
pub const FD_VAR: &str = "SHEP_CHANNEL_FD";
/// The named pipe variable, set on Windows only.
pub const PIPE_VAR: &str = "SHEP_CHANNEL_PIPE";
/// The wire version stamp, set on both platforms whenever a channel exists.
pub const VERSION_VAR: &str = "SHEP_CHANNEL_VERSION";

/// The lowest descriptor number the channel can arrive on.
///
/// The shepherd maps the socketpair's child end onto 3, never lower.
/// Descriptors 0, 1 and 2 are the app's own standard streams.
const FIRST_INHERITABLE_FD: i32 = 3;

/// Where this process's channel is, if it has one.
///
/// # Debug
///
/// Derived, so it prints the Windows pipe path with its random
/// suffix. That is deliberate and has a test.
///
/// The suffix is not a secret. The pipe namespace lists to any
/// local user, so a log already reveals it. [`crate::Channel`] and
/// the reader half print handles instead, matching `std::fs::File`'s
/// own undecorated Debug on Windows.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// An inherited descriptor number, from `SHEP_CHANNEL_FD`.
    Descriptor(i32),
    /// A named pipe path, from `SHEP_CHANNEL_PIPE`.
    Pipe(PathBuf),
    /// Neither variable is set. Not an error.
    Absent,
}

/// Reads the environment and says where the channel is.
///
/// # Errors
///
/// [`ChannelError::Unusable`] when `SHEP_CHANNEL_FD` holds something that
/// is not a descriptor, or a number below 3. A broken environment, not an
/// absent channel, so it fails loudly.
pub fn discover() -> Result<Endpoint, ChannelError> {
    if let Some(raw) = std::env::var_os(FD_VAR) {
        return descriptor_from(&raw.to_string_lossy());
    }
    if let Some(raw) = std::env::var_os(PIPE_VAR) {
        return Ok(Endpoint::Pipe(PathBuf::from(raw)));
    }
    Ok(Endpoint::Absent)
}

/// Reads one `SHEP_CHANNEL_FD` value and says whether it can be the channel.
///
/// Split out of [`discover`] so the refusals below need no environment variable.
/// Setting one is `unsafe` in edition 2024, which this crate forbids
/// outside its one descriptor site.
/// It would also race any other test sharing the process.
///
/// A negative number is not an owned descriptor at all. Taking 1 would
/// be worse than a merely wrong number.
/// This crate would own the app's stdout, write JSON into it, and close it on drop.
fn descriptor_from(text: &str) -> Result<Endpoint, ChannelError> {
    let Ok(fd) = text.trim().parse::<i32>() else {
        return Err(ChannelError::Unusable(format!("{FD_VAR}={text}")));
    };
    if fd < FIRST_INHERITABLE_FD {
        return Err(ChannelError::Unusable(format!(
            "{FD_VAR}={fd} must be {FIRST_INHERITABLE_FD} or above: the shepherd passes \
             the channel as {FIRST_INHERITABLE_FD}, and 0, 1 and 2 are this process's own \
             standard streams"
        )));
    }
    Ok(Endpoint::Descriptor(fd))
}

/// The duplex this platform carries the channel on.
#[cfg(unix)]
pub(crate) type Transport = std::os::unix::net::UnixStream;
/// The duplex this platform carries the channel on.
#[cfg(windows)]
pub(crate) type Transport = std::fs::File;

/// The half the reader thread owns.
///
/// The duplex itself on unix. A socketpair's two ends are independent
/// open file descriptions. A `read` parked on one costs a concurrent
/// `write` nothing.
#[cfg(unix)]
pub(crate) type ReadHalf = Transport;
/// The half the reader thread owns.
///
/// [`PipeReader`] on Windows, not a plain type alias. See it for the
/// deadlock it exists to avoid.
#[cfg(windows)]
pub(crate) type ReadHalf = PipeReader;

/// How long the Windows reader sleeps between peeks at an empty pipe.
///
/// The price of not parking inside `ReadFile`. Compared to
/// `action_timeout` and `kill_timeout`, both in seconds, 20 ms is
/// invisible. Shorter wastes cycles for nothing; longer shows up in
/// how fast an app answers `shep trigger`.
#[cfg(windows)]
const PIPE_POLL_INTERVAL: core::time::Duration = core::time::Duration::from_millis(20);

/// Reads the channel's named pipe without parking inside `ReadFile`.
///
/// The shepherd hands a Windows app one pipe instance. The writer gets a
/// duplicate handle onto the same object. Windows serialises every
/// operation on it, so a blocking read holds it against the writer.
/// Peeking avoids that park. The halves are still not independent: a
/// peek can wait behind an in-progress `WriteFile`.
/// `FILE_FLAG_OVERLAPPED` on both handles is what would separate them.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct PipeReader {
    /// The channel's pipe. Also duplicated for the writer thread, which is
    /// the whole reason this type cannot simply block.
    pipe: std::fs::File,
}

#[cfg(windows)]
impl PipeReader {
    /// How many bytes are buffered.
    ///
    /// `None` once the shepherd has closed its end and every sent byte
    /// is drained.
    ///
    /// # Errors
    ///
    /// Whatever `PeekNamedPipe` reports, except the two codes that mean
    /// the far end is gone. Those codes are this channel's clean end
    /// of stream. `serve`'s reader loop ends on them without treating
    /// them as a failure.
    fn buffered(&self) -> std::io::Result<Option<u32>> {
        use std::os::windows::io::AsRawHandle as _;

        use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;

        let mut available: u32 = 0;
        // SAFETY: `self.pipe`'s handle is open and borrowed for the call, so
        // it cannot close underneath us. `connect` opened it for reading, as
        // `PeekNamedPipe` requires. Null `lpBuffer` and zero size query counts
        // without copying. `&raw mut available` is a live `u32` for the call.
        #[allow(unsafe_code)]
        let reported = unsafe {
            PeekNamedPipe(
                self.pipe.as_raw_handle(),
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
                &raw mut available,
                core::ptr::null_mut(),
            )
        };
        if reported == 0 {
            let error = std::io::Error::last_os_error();
            let ended = matches!(
                error.raw_os_error(),
                Some(code)
                    if code == ERROR_BROKEN_PIPE as i32
                        || code == ERROR_PIPE_NOT_CONNECTED as i32
            );
            return if ended { Ok(None) } else { Err(error) };
        }
        Ok(Some(available))
    }
}

#[cfg(windows)]
impl std::io::Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let Some(buffered) = self.buffered()? else {
                return Ok(0);
            };
            if buffered == 0 {
                std::thread::sleep(PIPE_POLL_INTERVAL);
                continue;
            }
            // Never more than the peek just reported. Asking for more would
            // park this thread in the kernel again. That would revive the
            // deadlock this type exists to avoid.
            let want = buf.len().min(buffered as usize);
            return self.pipe.read(&mut buf[..want]);
        }
    }
}

/// Hands the reader thread its half, wrapped in whatever this platform
/// needs it to be.
#[cfg(unix)]
fn read_half(transport: Transport) -> ReadHalf {
    transport
}

/// Hands the reader thread its half, wrapped in whatever this platform
/// needs it to be.
#[cfg(windows)]
fn read_half(transport: Transport) -> ReadHalf {
    PipeReader { pipe: transport }
}

/// Guards the inherited channel from being taken twice in one process.
///
/// Consumed only by the branch that actually takes a channel. A refusal
/// takes nothing. `Endpoint::Absent`, or a `Descriptor`/`Pipe` naming a
/// mechanism this platform lacks, must not consume the guard. Otherwise
/// a later, legitimate call would be refused for no reason.
static CHANNEL_TAKEN: AtomicBool = AtomicBool::new(false);

/// Refuses a descriptor that cannot be the shepherd's channel.
///
/// `getsockname` answers for any socket, connected or not. A shepherd
/// that has already closed its end still passes.
/// It reports `ENOTSOCK` for a plain file or pipe, `EBADF` for a closed
/// number.
/// A socket this process owns for its own reasons passes. Nothing the
/// kernel offers names the descriptor a parent handed over.
///
/// # Errors
///
/// [`ChannelError::Unusable`] when the descriptor is not an open socket.
/// It is left open either way.
#[cfg(unix)]
fn refuse_unless_socket(fd: i32) -> Result<(), ChannelError> {
    use std::os::fd::FromRawFd as _;

    // SAFETY: `ManuallyDrop` is the invariant. The value is never dropped,
    // so nothing closes a descriptor this process may not own.
    // `local_addr` reads through `&self` and moves nothing.
    #[allow(unsafe_code)]
    let probe = core::mem::ManuallyDrop::new(unsafe { Transport::from_raw_fd(fd) });
    probe.local_addr().map_err(|error| {
        ChannelError::Unusable(format!(
            "{FD_VAR}={fd} is not an open socket ({error}): the shepherd passes \
             the channel as one end of a socketpair"
        ))
    })?;
    Ok(())
}

/// Opens the endpoint, returning the reader's half and the writer's clone.
///
/// # Errors
///
/// - [`ChannelError::Unusable`] when the endpoint names a mechanism this
///   platform does not have, or a descriptor that is not an open socket.
/// - [`ChannelError::Io`] when the pipe cannot be opened or the descriptor
///   cannot be cloned.
/// - [`ChannelError::AlreadyTaken`] when this process already took its
///   channel.
pub(crate) fn connect(endpoint: &Endpoint) -> Result<(ReadHalf, Transport), ChannelError> {
    let transport = match endpoint {
        #[cfg(unix)]
        Endpoint::Descriptor(fd) => {
            // Probes before claiming, like the Windows arm below. A refusal
            // must leave the guard alone, or one bad descriptor would refuse
            // every later call in this process.
            refuse_unless_socket(*fd)?;
            if CHANNEL_TAKEN.swap(true, Ordering::SeqCst) {
                return Err(ChannelError::AlreadyTaken);
            }
            use std::os::fd::FromRawFd as _;
            // SAFETY: soundness rests on the shepherd naming a descriptor it
            // handed this process. The probe above rules out everything but
            // another socket this process owns. `CHANNEL_TAKEN` makes this arm
            // reachable once per process.
            #[allow(unsafe_code)]
            unsafe {
                Transport::from_raw_fd(*fd)
            }
        }
        #[cfg(windows)]
        Endpoint::Pipe(path) => {
            // Opens before claiming, unlike the unix arm, because this
            // step can fail. Claiming first would mark the channel taken
            // by a process that never took it. Every later attempt would
            // then be refused for nothing.
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(ChannelError::Io)?;
            if CHANNEL_TAKEN.swap(true, Ordering::SeqCst) {
                // Defensive, not expected. The shepherd creates one pipe
                // instance, so a second caller's `open` should already
                // fail busy. Dropping here is still correct if it somehow
                // does not. Two owners of one pipe is worse than a refusal.
                drop(opened);
                return Err(ChannelError::AlreadyTaken);
            }
            opened
        }
        #[cfg(unix)]
        Endpoint::Pipe(path) => {
            return Err(ChannelError::Unusable(format!(
                "{PIPE_VAR}={} names a Windows named pipe and this is not Windows",
                path.display()
            )));
        }
        #[cfg(windows)]
        Endpoint::Descriptor(fd) => {
            return Err(ChannelError::Unusable(format!(
                "{FD_VAR}={fd} names an inherited descriptor and Windows does not inherit one"
            )));
        }
        Endpoint::Absent => {
            return Err(ChannelError::Unusable(
                "no channel: neither variable is set".to_string(),
            ));
        }
    };
    // Failure here releases the claim only on Windows. Unix already owns
    // the descriptor irrevocably once `from_raw_fd` returns, so the claim
    // stands regardless. Windows has nothing irrevocable yet. Keeping the
    // claim would wrongly refuse a later attempt that might work.
    let writer = match transport.try_clone() {
        Ok(writer) => writer,
        Err(error) => {
            // Drops the handle before clearing the claim. The pipe is then
            // free at the moment the flag says so. A racing caller fails at
            // `open`, not at the claim, since this arm opens first.
            #[cfg(windows)]
            {
                drop(transport);
                CHANNEL_TAKEN.store(false, Ordering::SeqCst);
            }
            return Err(ChannelError::Io(error));
        }
    };
    Ok((read_half(transport), writer))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::fd::{AsRawFd as _, IntoRawFd as _};
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    use super::*;

    /// Fails if [`Endpoint`]'s `Debug` starts redacting the path, or
    /// drops it. The decision not to redact lives on the type.
    /// Change it there first if this test should start failing.
    ///
    /// Windows path, but platform-independent: `Path`'s `Debug` escapes
    /// a backslash the same way everywhere.
    #[test]
    fn the_pipe_endpoint_prints_its_path_in_full() {
        let endpoint = Endpoint::Pipe(PathBuf::from(
            r"\\.\pipe\shep-channel-1234-0-0123456789abcdef0123456789abcdef",
        ));
        assert_eq!(
            format!("{endpoint:?}"),
            r#"Pipe("\\\\.\\pipe\\shep-channel-1234-0-0123456789abcdef0123456789abcdef")"#
        );
    }

    /// Fails if the other two variants start carrying something new.
    /// That is the cheap half of the same guard.
    #[test]
    fn the_other_endpoints_print_only_what_they_hold() {
        assert_eq!(format!("{:?}", Endpoint::Descriptor(3)), "Descriptor(3)");
        assert_eq!(format!("{:?}", Endpoint::Absent), "Absent");
    }

    /// Fails if a descriptor the shepherd could never have passed is
    /// accepted. `from_raw_fd` needs a valid, owned descriptor, and -1
    /// is not one.
    #[test]
    fn a_negative_descriptor_is_refused() {
        match descriptor_from("-1") {
            Err(ChannelError::Unusable(what)) => assert!(
                what.contains(&format!("{FD_VAR}=-1")),
                "the refusal names neither the variable nor the value: {what}"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Fails if `SHEP_CHANNEL_FD=1` is accepted. Worse in practice than
    /// a merely wrong number. Taking 1 would give this crate ownership
    /// of the app's stdout. It would write JSON into it and close it
    /// on drop.
    #[test]
    fn stdout_is_refused_as_a_descriptor() {
        match descriptor_from("1") {
            Err(ChannelError::Unusable(what)) => assert!(
                what.contains(&format!("{FD_VAR}=1")),
                "the refusal names neither the variable nor the value: {what}"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Fails if the floor rejects the descriptor the shepherd actually
    /// passes. That would make the two refusals above pass for the
    /// wrong reason.
    #[test]
    fn the_descriptor_the_shepherd_passes_is_accepted() {
        assert!(matches!(
            descriptor_from(" 3 "),
            Ok(Endpoint::Descriptor(FIRST_INHERITABLE_FD))
        ));
    }

    /// Fails if a descriptor that is not a socket is adopted.
    /// Fails too if the refusal closes it.
    /// Adopting one would write frames into a file the process opened
    /// for itself, then close it under the owner.
    #[cfg(unix)]
    #[test]
    fn a_descriptor_that_is_not_a_socket_is_refused_and_left_open() {
        use std::io::{Read as _, Seek as _, Write as _};

        let path =
            std::env::temp_dir().join(format!("shep-channel-not-a-socket-{}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("open a plain file to point the descriptor at");
        std::fs::remove_file(&path).expect("unlink the plain file");

        match connect(&Endpoint::Descriptor(file.as_raw_fd())) {
            Err(ChannelError::Unusable(what)) => assert!(
                what.contains("is not an open socket"),
                "the refusal does not say what is wrong with the descriptor: {what}"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }

        // The point of the refusal, not a second assertion about it. A
        // probe that closed the descriptor would leave the number free
        // for the next `open` to hand to someone else.
        file.write_all(b"still open")
            .expect("write after the refusal");
        file.rewind().expect("rewind after the refusal");
        let mut back = String::new();
        file.read_to_string(&mut back)
            .expect("read after the refusal");
        assert_eq!(back, "still open");
    }

    /// The only test that takes the channel. `CHANNEL_TAKEN` is
    /// process-global, and tests share one process. The refusal above
    /// probes before claiming, so it leaves the guard for this one.
    ///
    /// Asserts the transition, not just the second call's error. The
    /// first call must succeed here too, or a `connect` that refused
    /// everything would still pass.
    #[cfg(unix)]
    #[test]
    fn a_descriptor_can_only_be_taken_once() {
        let (ours, _theirs) = UnixStream::pair().expect("socketpair");
        let fd = ours.into_raw_fd();
        let endpoint = Endpoint::Descriptor(fd);

        let first = connect(&endpoint);
        assert!(first.is_ok(), "first take should succeed: {first:?}");

        let second = connect(&endpoint);
        assert!(matches!(second, Err(ChannelError::AlreadyTaken)));

        // `first` stays bound, not `let _ = ...`, through both calls.
        // Its transport keeps the descriptor open, so it cannot close
        // out from under `second`.
        drop(first);
    }
}
