//! Drives the crate's own handlers as a real child process on a real
//! channel. It runs on whichever platform this is built for. Unix wires
//! an inherited descriptor onto fd 3. Windows hands the child the name
//! of a pipe it opens itself.
//!
//! The test re-execs its own binary. Cargo defines `CARGO_BIN_EXE_<name>`
//! for a `[[bin]]` target, not for an example.
//!
//! The shepherd end is the production `ServerOptions` call the daemon's
//! listener makes. A bug there would show up here too.

use std::process::{Command, Stdio};
use std::time::Duration;

/// Set in the child's environment to mean "run handlers, not assertions".
/// Checked first in the test function, so nothing above it runs twice.
const CHILD_VAR: &str = "SHEP_CHANNEL_TEST_CHILD";

/// The test function's name, passed to the re-exec'd binary as an exact
/// filter. Keep it in sync with the `#[test] fn` below by hand. Libtest
/// names tests by plain string, so nothing else enforces it.
const TEST_NAME: &str = "a_real_child_finds_its_channel_and_answers";

/// How long the parent waits for the child to answer before calling it
/// hung. This is slack for a loaded runner, not an expected duration.
/// Every read below is bounded by it. A hung child fails the test
/// instead of hanging the suite.
const DEADLINE: Duration = Duration::from_secs(10);

/// A substring of `serve.rs`'s private no-channel advice. The full string
/// is not exported from the crate. This is just enough to prove the
/// warning did not fire.
const NO_CHANNEL_SNIPPET: &str = "no channel on this process";

/// Registers the same handlers `examples/answers.rs` does, then parks.
/// Never returns: the parent kills this process once its assertions are
/// done. There is nothing to fall through to.
fn run_as_child() -> ! {
    let shepherd = shep_channel::serve();
    shepherd.on_action("gc", |params, _name| {
        format!("collected, params={params:?}")
    });
    shepherd.on_shutdown(|| std::process::exit(0));
    shepherd.ready().expect("say ready");
    shepherd.metric("rps", 42.0);

    loop {
        std::thread::park();
    }
}

#[test]
fn a_real_child_finds_its_channel_and_answers() {
    if std::env::var_os(CHILD_VAR).is_some() {
        run_as_child();
    }

    let (mut shepherd, child) = open_channel_and_spawn_child();
    let child = ChildGuard(Some(child));

    // On Windows this line also checks for the deadlock. A stuck writer
    // thread would never reach it. The shepherd waits here before it
    // sends anything else.
    assert_eq!(
        shepherd.read_line(),
        "{\"kind\":\"ready\"}",
        "the child never said ready"
    );

    assert_eq!(
        shepherd.read_line(),
        "{\"kind\":\"metric\",\"name\":\"rps\",\"value\":42.0}"
    );

    shepherd.write_line("{\"kind\":\"action\",\"name\":\"gc\",\"params\":\"now\",\"id\":7}");
    assert_eq!(
        shepherd.read_line(),
        "{\"kind\":\"action-reply\",\"action\":\"gc\",\"body\":\"collected, params=Some(\\\"now\\\")\",\"id\":7}"
    );

    // The rule this crate exists for, against a real process.
    shepherd.write_line("{\"kind\":\"action\",\"name\":\"typo\",\"id\":8}");
    assert_eq!(
        shepherd.read_line(),
        "{\"kind\":\"action-reply\",\"action\":\"typo\",\"body\":\"unknown action: typo\",\"id\":8}"
    );

    let mut child = child.take();
    child.kill().expect("kill");
    // `wait_with_output`, not a bare `wait`, because it also collects the
    // piped stderr without a manual read. The process is already dead, so
    // this returns as soon as the OS reaps it.
    let output = child.wait_with_output().expect("reap and collect stderr");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(NO_CHANNEL_SNIPPET),
        "a process with a real channel warned as though it had none: {stderr}"
    );
}

/// The parts of the child's command that do not depend on how the channel
/// reaches it.
///
/// `SHEP_NAME` is load-bearing: `serve()` gates its no-channel advice on
/// it. Without it the child stays silent for the wrong reason, proving
/// nothing. With it set, the advice must not fire.
fn base_command() -> Command {
    let exe = std::env::current_exe().expect("path to this test binary");
    let mut command = Command::new(exe);
    command
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_VAR, "1")
        .env("SHEP_CHANNEL_VERSION", "1")
        .env("SHEP_NAME", "answers")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Kills and reaps the wrapped child on drop, if [`ChildGuard::take`] was
/// never called. A failed `assert_eq!` unwinds without killing the child.
/// Without this guard it would park as an orphan indefinitely.
struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    /// Hands back the child for an explicit kill, disarming the drop guard.
    ///
    /// # Panics
    ///
    /// If called more than once.
    #[track_caller]
    fn take(mut self) -> std::process::Child {
        self.0.take().expect("ChildGuard::take called twice")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The shepherd's end of a socketpair, read and written a line at a time.
#[cfg(unix)]
struct ShepherdSide {
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
impl ShepherdSide {
    /// The next line the child sent, without its newline.
    ///
    /// # Panics
    ///
    /// If the child sends nothing within [`DEADLINE`], or closes its end.
    #[track_caller]
    fn read_line(&mut self) -> String {
        use std::io::BufRead as _;

        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .expect("the child sent nothing within the deadline");
        assert!(read > 0, "the child closed its end of the channel");
        line.trim_end().to_string()
    }

    /// Sends one line to the child, newline appended.
    ///
    /// # Panics
    ///
    /// If the write fails.
    #[track_caller]
    fn write_line(&mut self, line: &str) {
        use std::io::Write as _;

        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .expect("write to the socket");
    }
}

/// Spawns the child with a socketpair mapped onto its fd 3, and keeps the
/// shepherd's end.
///
/// `FdMapping` takes an `OwnedFd`, which a `UnixStream` converts into, so
/// this needs no `unsafe` at all.
#[cfg(unix)]
fn open_channel_and_spawn_child() -> (ShepherdSide, std::process::Child) {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    use command_fds::{CommandFdExt as _, FdMapping};

    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    // The child inherits a blocking descriptor, the same as a real app
    // gets. `tokio_runner.rs` clears `O_NONBLOCK`, so a plain read parks
    // instead of returning `EAGAIN`.
    theirs.set_nonblocking(false).expect("blocking");
    ours.set_read_timeout(Some(DEADLINE)).expect("deadline");

    let mut command = base_command();
    command
        .env_remove("SHEP_CHANNEL_PIPE")
        .env("SHEP_CHANNEL_FD", "3");
    command
        .fd_mappings(vec![FdMapping {
            parent_fd: OwnedFd::from(theirs),
            child_fd: 3,
        }])
        .expect("map the socketpair to fd 3");
    // Guarded from the moment it exists, not from the caller.
    // Everything between here and the return is fallible. A panic in it
    // would otherwise leave the child parked with nothing owning it.
    let child = ChildGuard(Some(
        command
            .spawn()
            .expect("spawn the re-exec'd test binary as the channel child"),
    ));

    let writer = ours.try_clone().expect("clone");
    let side = ShepherdSide {
        reader: std::io::BufReader::new(ours),
        writer,
    };
    (side, child.take())
}

/// The shepherd's end of a named pipe, read and written a line at a time.
///
/// Async underneath, because the daemon's own pipe is a
/// `tokio::net::windows::named_pipe::NamedPipeServer`. Anything else would
/// test a pipe the daemon does not create. Every call blocks on the
/// runtime, so the test body reads the same on both platforms.
#[cfg(windows)]
struct ShepherdSide {
    runtime: tokio::runtime::Runtime,
    pipe: tokio::io::BufReader<tokio::net::windows::named_pipe::NamedPipeServer>,
}

#[cfg(windows)]
impl ShepherdSide {
    /// The next line the child sent, without its newline.
    ///
    /// # Panics
    ///
    /// If the child sends nothing within [`DEADLINE`], or closes its end.
    #[track_caller]
    fn read_line(&mut self) -> String {
        use tokio::io::AsyncBufReadExt as _;

        // Split the borrow: `block_on` takes `&self` on the runtime.
        // The read needs `&mut` on the pipe. They are different fields.
        let Self { runtime, pipe } = self;
        let mut line = String::new();
        let read = runtime
            .block_on(async { tokio::time::timeout(DEADLINE, pipe.read_line(&mut line)).await })
            .expect("the child sent nothing within the deadline")
            .expect("read from the pipe");
        assert!(read > 0, "the child closed its end of the channel");
        line.trim_end().to_string()
    }

    /// Sends one line to the child, newline appended.
    ///
    /// # Panics
    ///
    /// If the write fails.
    #[track_caller]
    fn write_line(&mut self, line: &str) {
        use tokio::io::AsyncWriteExt as _;

        let Self { runtime, pipe } = self;
        let framed = format!("{line}\n");
        runtime
            .block_on(async {
                pipe.write_all(framed.as_bytes()).await?;
                pipe.flush().await
            })
            .expect("write to the pipe");
    }
}

/// Creates the channel's pipe and spawns the child pointed at it by name.
/// Waits for the child to open it.
///
/// The order matches the daemon's: the pipe exists before the child does.
/// The child's own open cannot lose a race with it. `connect` resolves
/// once the child has opened its end. `serve()` does that at startup,
/// before it registers a handler.
#[cfg(windows)]
fn open_channel_and_spawn_child() -> (ShepherdSide, std::process::Child) {
    use tokio::net::windows::named_pipe::ServerOptions;

    // Unique per process, as the daemon's is unique per spawn. This test
    // is the only thing in its own process that opens one. A pid is
    // enough here, where the daemon needs a nonce.
    let name = format!(r"\\.\pipe\shep-channel-test-{}", std::process::id());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime for the shepherd end");
    // `ServerOptions::create` registers with the reactor. It must run
    // inside the runtime's context, even though it is not itself async.
    let server = {
        let _guard = runtime.enter();
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)
            .expect("create the channel's pipe")
    };

    let mut command = base_command();
    command
        .env_remove("SHEP_CHANNEL_FD")
        .env("SHEP_CHANNEL_PIPE", &name);
    // Guarded from the moment it exists, not from the caller. The wait
    // below is the failure this test detects. It must already be
    // parked before that wait can panic.
    let child = ChildGuard(Some(
        command
            .spawn()
            .expect("spawn the re-exec'd test binary as the channel child"),
    ));

    runtime
        .block_on(async { tokio::time::timeout(DEADLINE, server.connect()).await })
        .expect("the child never opened the pipe within the deadline")
        .expect("accept the child's connection");

    let side = ShepherdSide {
        pipe: tokio::io::BufReader::new(server),
        runtime,
    };
    (side, child.take())
}
