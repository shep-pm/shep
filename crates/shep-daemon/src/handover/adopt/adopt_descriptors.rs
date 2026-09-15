use super::super::{CarriedFds, CarriedSheep, Handover};
use super::rehearse_blob::refuse_repeated_fds;
use crate::sys;
use std::fs::File;
use std::io;
use std::os::fd::{OwnedFd, RawFd};
use tokio::net::unix::pipe;

/// Everything a successor was handed, rebuilt into objects it can use.
///
/// `Debug` is derived: descriptor numbers, a socket, a sheep's name and pid,
/// and no environment value (see [`Handover`]).
#[derive(Debug)]
pub struct Adopted {
    /// The control listener, on the same socket the predecessor was serving.
    pub listener: tokio::net::UnixListener,
    /// Every sheep the blob described, in the blob's own order.
    pub sheep: Vec<AdoptedSheep>,
    /// The pidfile, held for the process's life so its `flock` is not
    /// released. Never written through here: an `execve` keeps the pid, so
    /// the number already in the file is this process's own.
    pub pidfile: File,
}

/// One sheep's output plumbing, rebuilt, and its input plumbing with it.
///
/// `None` on any of the first four means an instance that is registered and
/// not running, the only reason a blob names no descriptor. A descriptor
/// named and missing is a refusal, not a `None`.
#[derive(Debug)]
pub struct AdoptedSheep {
    /// What the blob said about this sheep.
    pub carried: CarriedSheep,
    /// The read end of its stdout pipe, as an async reader.
    pub out_pipe: Option<pipe::Receiver>,
    /// The read end of its stderr pipe, as an async reader.
    pub err_pipe: Option<pipe::Receiver>,
    /// The appending handle on its stdout log file.
    pub out_log: Option<tokio::fs::File>,
    /// The appending handle on its stderr log file.
    pub err_log: Option<tokio::fs::File>,
    /// The write end of its stdin pipe, for a sheep whose app asked for one.
    ///
    /// The only handle here the daemon writes to rather than reads from.
    /// `None` for the commoner sheep that has `/dev/null` on fd 0.
    pub stdin_pipe: Option<pipe::Sender>,
    /// The daemon's end of its shepherd-channel socketpair, whose other end
    /// is the child's fd 3.
    ///
    /// The only handle here that goes both ways: the successor splits it
    /// into the same reader and writer a spawn wires. `None` for a sheep
    /// whose app asked for no channel, one that is not running, and one whose
    /// child has already closed fd 3.
    pub channel: Option<tokio::net::UnixStream>,
}

/// Rebuild everything `blob` describes, around descriptors this process
/// inherited rather than opened.
///
/// # Errors
///
/// A descriptor the blob names twice, is not open, is the wrong kind for
/// its slot, or could not register with the runtime, naming the sheep and
/// the stream. No partial success: a caller that cannot rehydrate refuses to boot.
///
/// # Panics
///
/// Panics if called outside a tokio runtime with IO enabled.
#[track_caller]
pub fn adopt(blob: &Handover) -> io::Result<Adopted> {
    refuse_repeated_fds(blob)?;
    let listener = adopt_listener(blob.listener_fd)?;
    let sheep = blob
        .sheep
        .iter()
        .map(adopt_sheep)
        .collect::<io::Result<Vec<_>>>()?;
    // Last: an earlier refusal leaves this descriptor open and unowned, so
    // the `flock` it carries stays held.
    let pidfile = adopt_fd(blob.pidfile_fd, "the pidfile lock")?;
    Ok(Adopted {
        listener,
        sheep,
        pidfile,
    })
}

/// Take ownership of `fd`, reporting what it was for when it is not open.
pub(super) fn adopt_fd(fd: RawFd, what: &str) -> io::Result<File> {
    sys::adopt_handover_fd(fd).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} did not survive the handover: {error}"),
        )
    })
}

/// Rebuild the control listener on the descriptor it was already bound to.
///
/// Non-blocking is set rather than assumed. It is a file status flag and does
/// survive the exec, but `tokio::net::UnixListener::from_std` refuses a
/// blocking socket rather than fixing one.
pub(super) fn adopt_listener(fd: RawFd) -> io::Result<tokio::net::UnixListener> {
    let file = adopt_fd(fd, "the control listener")?;
    let listener = std::os::unix::net::UnixListener::from(OwnedFd::from(file));
    listener.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(listener)
}

/// Rebuild one sheep's six handles.
fn adopt_sheep(carried: &CarriedSheep) -> io::Result<AdoptedSheep> {
    let CarriedFds {
        out_pipe,
        err_pipe,
        out_log,
        err_log,
        stdin,
        channel,
    } = carried.fds;
    let name = &carried.name;
    Ok(AdoptedSheep {
        out_pipe: adopt_pipe(out_pipe, name, "stdout")?,
        err_pipe: adopt_pipe(err_pipe, name, "stderr")?,
        out_log: adopt_log(out_log, name, "stdout")?,
        err_log: adopt_log(err_log, name, "stderr")?,
        stdin_pipe: adopt_stdin(stdin, name)?,
        channel: adopt_channel(channel, name)?,
        carried: carried.clone(),
    })
}

/// Rebuild one shepherd channel's daemon end as an async socket, if the
/// blob named one.
///
/// The kind check is `peer_addr`, since
/// `std::os::unix::net::UnixStream::from(OwnedFd)` is infallible and checks
/// nothing: `ENOTSOCK` for anything that is not a socket, `ENOTCONN` for one
/// that is listening rather than connected, which this daemon's own control
/// listener is. Non-blocking is set rather than assumed, as in
/// [`adopt_listener`]. Nothing is paired again: the child's fd 3 is the other
/// end of this same socketpair.
pub(super) fn adopt_channel(
    fd: Option<RawFd>,
    sheep: &str,
) -> io::Result<Option<tokio::net::UnixStream>> {
    let Some(fd) = fd else { return Ok(None) };
    let file = adopt_fd(fd, &format!("sheep '{sheep}' shepherd channel"))?;
    let socket = std::os::unix::net::UnixStream::from(OwnedFd::from(file));
    socket.peer_addr().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("sheep '{sheep}' shepherd channel is not a connected socket: {error}"),
        )
    })?;
    socket.set_nonblocking(true)?;
    tokio::net::UnixStream::from_std(socket).map(Some)
}

/// Rebuild one pipe read end as an async reader, if the blob named one.
///
/// `pipe::Receiver::from_file` checks that the descriptor really is a pipe
/// open for reading and sets non-blocking itself, so a blob that crossed two
/// numbers is refused rather than pumped from a file that never yields a
/// line.
pub(super) fn adopt_pipe(
    fd: Option<RawFd>,
    sheep: &str,
    stream: &str,
) -> io::Result<Option<pipe::Receiver>> {
    let Some(fd) = fd else { return Ok(None) };
    let file = adopt_fd(fd, &format!("sheep '{sheep}' {stream} pipe"))?;
    pipe::Receiver::from_file(file).map(Some).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("sheep '{sheep}' {stream} pipe is not a readable pipe: {error}"),
        )
    })
}

/// Rebuild one stdin write end as an async writer, if the blob named one.
///
/// `pipe::Sender::from_file` refuses a descriptor that is not a pipe or is
/// not open for writing, so a blob naming the end the child reads from is
/// refused rather than adopted into a `shep whisper` that can never land.
/// Nothing is repaired: the child's fd 0 is the other end of this same pipe.
pub(super) fn adopt_stdin(fd: Option<RawFd>, sheep: &str) -> io::Result<Option<pipe::Sender>> {
    let Some(fd) = fd else { return Ok(None) };
    let file = adopt_fd(fd, &format!("sheep '{sheep}' stdin pipe"))?;
    pipe::Sender::from_file(file).map(Some).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("sheep '{sheep}' stdin is not a writable pipe: {error}"),
        )
    })
}

/// Rebuild one log handle, if the blob named one.
///
/// Wrapped, never reopened, which is what preserves `O_APPEND`.
pub(super) fn adopt_log(
    fd: Option<RawFd>,
    sheep: &str,
    stream: &str,
) -> io::Result<Option<tokio::fs::File>> {
    let Some(fd) = fd else { return Ok(None) };
    let file = adopt_fd(fd, &format!("sheep '{sheep}' {stream} log"))?;
    Ok(Some(tokio::fs::File::from_std(file)))
}

#[cfg(test)]
mod tests {

    use super::super::super::CarriedFds;

    use std::os::fd::IntoRawFd as _;

    use std::time::Duration;
    use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};

    use super::super::testing::*;
    use super::*;

    #[tokio::test]
    async fn an_adopted_listener_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let blob = blob_with(&socket, Vec::new());

        let adopted = adopt(&blob).unwrap();

        let listener = adopted.listener;
        let accept = tokio::spawn(async move { listener.accept().await });
        let _client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        // Bounded: an adopted listener that never became readable would
        // hang the whole test binary rather than failing this case.
        tokio::time::timeout(Duration::from_secs(10), accept)
            .await
            .expect("the adopted listener must accept")
            .unwrap()
            .expect("the adopted listener accepts");
    }

    /// A blob naming one number twice is refused before anything is adopted.
    ///
    /// Two owners close one descriptor twice, and the second close lands on
    /// whatever this process opened in between. The listener's own number is
    /// the one repeated, since reaching a duplicate mid-way would already
    /// have built the first owner.
    #[tokio::test]
    async fn a_blob_naming_one_descriptor_twice_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let mut blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
            })],
        );
        blob.sheep[0].fds.out_log = Some(blob.listener_fd);

        let err = adopt(&blob).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
        assert!(
            err.to_string().contains("more than once"),
            "the refusal must say what is wrong with the blob: {err}"
        );
    }

    #[tokio::test]
    async fn an_adopted_log_handle_still_appends() {
        // Not merely writable: appending. A handle reopened without
        // `O_APPEND` passes a naive write test and corrupts a rotation, so
        // the assertion is that a write at offset 0 still lands at the end.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let log = dir.path().join("web-out.log");
        std::fs::write(&log, b"first\n").unwrap();
        let handle = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: Some(handle.into_raw_fd()),
                err_log: None,
                stdin: None,
                channel: None,
            })],
        );

        let mut adopted = adopt(&blob).unwrap();

        let mut out = adopted.sheep[0].out_log.take().expect("an adopted log");
        out.seek(std::io::SeekFrom::Start(0)).await.unwrap();
        out.write_all(b"second\n").await.unwrap();
        out.flush().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "first\nsecond\n",
            "a write at offset 0 overwrote the file, so O_APPEND was lost"
        );
    }

    #[tokio::test]
    async fn a_blob_naming_a_descriptor_that_is_not_open_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: Some(NEVER_OPEN),
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
            })],
        );

        let err = adopt(&blob).expect_err("a descriptor that is not open must refuse");

        let text = err.to_string();
        assert!(
            text.contains("web"),
            "the refusal must name the sheep: {text}"
        );
        assert!(
            text.contains("stdout"),
            "the refusal must name the stream: {text}"
        );
    }

    #[tokio::test]
    async fn a_refused_rehydrate_leaves_the_pidfile_lock_held() {
        // The pidfile is adopted last, so a failure before it leaves that
        // descriptor open and unowned, and its `flock` held.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: Some(NEVER_OPEN),
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
            })],
        );
        let pidfile_fd = blob.pidfile_fd;

        adopt(&blob).expect_err("a descriptor that is not open must refuse");

        assert!(
            nix::fcntl::fcntl(pidfile_fd, nix::fcntl::FcntlArg::F_GETFD).is_ok(),
            "the pidfile descriptor was closed by a failed rehydrate"
        );
    }

    #[tokio::test]
    async fn an_adopted_pipe_reads_what_was_written_before_the_adoption() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let (reader, mut writer) = std::io::pipe().unwrap();
        std::io::Write::write_all(&mut writer, b"a line\n").unwrap();
        drop(writer);
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: Some(reader.into_raw_fd()),
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
            })],
        );

        let mut adopted = adopt(&blob).unwrap();

        let out = adopted.sheep[0].out_pipe.take().expect("an adopted pipe");
        let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(out));
        // Bounded: an adopted pipe that produced nothing would hang the
        // binary instead of failing here.
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("the adopted pipe must produce the line written before it")
            .unwrap();
        assert_eq!(line.as_deref(), Some("a line"));
    }

    /// The direction is the whole case: every other descriptor a sheep
    /// carries is one the daemon reads from, and a blob naming the wrong end
    /// of this pair would still adopt and still be a pipe.
    #[tokio::test]
    async fn an_adopted_stdin_pipe_writes_to_the_end_the_child_reads() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        // The child's fd 0 stays here, exactly as it does across a real
        // exec: the daemon carries only the write end.
        let (mut child_end, daemon_end) = std::io::pipe().unwrap();
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: Some(daemon_end.into_raw_fd()),
                channel: None,
            })],
        );

        let mut adopted = adopt(&blob).unwrap();

        let mut stdin = adopted.sheep[0]
            .stdin_pipe
            .take()
            .expect("an adopted stdin pipe");
        stdin.write_all(b"whisper\n").await.unwrap();
        stdin.flush().await.unwrap();
        // A blocking read of bytes already written, so there is nothing to
        // wait for and nothing to time out.
        let mut buf = [0_u8; 8];
        std::io::Read::read_exact(&mut child_end, &mut buf).expect("the child end must read");
        assert_eq!(&buf, b"whisper\n");
    }

    /// A read end passes `is_pipe` and would be adopted as a writer, so every
    /// `shep whisper` after the handover would fail on a descriptor the
    /// successor was told was fine.
    #[tokio::test]
    async fn a_pipe_read_end_offered_as_stdin_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let (reader, _writer) = std::io::pipe().unwrap();
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: Some(reader.into_raw_fd()),
                channel: None,
            })],
        );

        let err = adopt(&blob).expect_err("a read end is not something to write to");

        let text = err.to_string();
        assert!(
            text.contains("web"),
            "the refusal must name the sheep: {text}"
        );
        assert!(
            text.contains("stdin"),
            "the refusal must name what could not be adopted: {text}"
        );
    }

    #[tokio::test]
    async fn a_log_file_offered_as_a_pipe_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        // `into_raw_fd`, because `adopt` takes ownership of whatever the blob
        // names: a `File` owning the same number would close it twice.
        let file = tempfile::tempfile().unwrap();
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: Some(file.into_raw_fd()),
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: None,
            })],
        );

        let err = adopt(&blob).expect_err("a file is not a pipe");

        assert!(err.to_string().contains("web"), "{err}");
    }

    /// Both directions in one case, because the failure to catch is a number
    /// naming the wrong end of the pair and a case that only wrote would pass
    /// on a socket the child cannot answer. `child_end` is what an app holds
    /// on fd 3.
    #[tokio::test]
    async fn an_adopted_channel_carries_both_directions() {
        use tokio::io::AsyncBufReadExt as _;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let (daemon_end, child_end) = std::os::unix::net::UnixStream::pair().unwrap();
        child_end.set_nonblocking(true).unwrap();
        let child_end = tokio::net::UnixStream::from_std(child_end).unwrap();
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: Some(daemon_end.into_raw_fd()),
            })],
        );

        let mut adopted = adopt(&blob).unwrap();
        let channel = adopted.sheep[0]
            .channel
            .take()
            .expect("an adopted shepherd channel");
        let (read_half, mut write_half) = tokio::io::split(channel);

        // Shepherd to child, which is what `shutdown_with_message` and a
        // `shep trigger` both ride.
        let (child_read, mut child_write) = tokio::io::split(child_end);
        let mut child = tokio::io::BufReader::new(child_read);
        write_half
            .write_all(b"{\"kind\":\"shutdown\"}\n")
            .await
            .unwrap();
        let mut line = String::new();
        child
            .read_line(&mut line)
            .await
            .expect("the child end must read");
        assert_eq!(line, "{\"kind\":\"shutdown\"}\n");

        // Child to shepherd, which is what `{"kind":"ready"}` and every
        // action reply ride.
        child_write
            .write_all(b"{\"kind\":\"ready\"}\n")
            .await
            .unwrap();
        let mut back = String::new();
        tokio::io::BufReader::new(read_half)
            .read_line(&mut back)
            .await
            .expect("the daemon end must read");
        assert_eq!(back, "{\"kind\":\"ready\"}\n");
    }

    /// `UnixStream::from(OwnedFd)` is infallible and checks nothing, unlike
    /// the `from_file` constructors the pipes go through, so a number handed
    /// to the next `open` would be adopted as a socket. A plain file is the
    /// cheapest stand-in.
    #[tokio::test]
    async fn a_file_offered_as_a_shepherd_channel_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let file = tempfile::tempfile().unwrap();
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: Some(file.into_raw_fd()),
            })],
        );

        let err = adopt(&blob).expect_err("a file is not a connected socket");

        let text = err.to_string();
        assert!(
            text.contains("web"),
            "the refusal must name the sheep: {text}"
        );
        assert!(
            text.contains("shepherd channel"),
            "the refusal must name what could not be adopted: {text}"
        );
    }

    /// This daemon's own control listener is the one number in a blob that is
    /// a socket and is not a channel, so it is the wrong number the kind check
    /// most plausibly meets. `getpeername` answers `ENOTCONN`, which a check
    /// asking only "is it a socket" would miss.
    #[tokio::test]
    async fn a_listening_socket_offered_as_a_shepherd_channel_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("shep.sock");
        let other = dir.path().join("other.sock");
        let listening = std::os::unix::net::UnixListener::bind(&other).unwrap();
        let blob = blob_with(
            &socket,
            vec![carried(CarriedFds {
                out_pipe: None,
                err_pipe: None,
                out_log: None,
                err_log: None,
                stdin: None,
                channel: Some(listening.into_raw_fd()),
            })],
        );

        let err = adopt(&blob).expect_err("a listener is not a connected socket");

        assert!(
            err.to_string().contains("shepherd channel"),
            "the refusal must name what could not be adopted: {err}"
        );
    }
}
