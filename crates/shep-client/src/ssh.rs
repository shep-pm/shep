//! Reaching another host's shepherd through the system `ssh`: [`SshOptions`],
//! [`SshCause`], and the dial behind [`Client::connect_ssh`].
//!
//! The command is `ssh -T -- <dest> shep dial-stdio`. The child's pipes are
//! joined into one byte stream and handed to [`Connection::open_over`]. The
//! handshake is then the one a local socket gets. ssh is never made a
//! session leader: it reads passphrase and host-key prompts from the
//! terminal. A background process group is stopped by `SIGTTIN` when it tries.
//!
//! [`Client::connect_ssh`]: crate::Client::connect_ssh

use core::fmt;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::process::{Child, ChildStderr, Command};
use tokio::sync::oneshot;

use shep_core::destination::Destination;
use shep_core::exit;

use crate::connection::{ConnectError, Connection};

/// Most of ssh's stderr kept for an error message, in bytes.
const STDERR_CAP: usize = 2048;

/// How long a finished connection lets the ssh child exit on its own before
/// it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// How long a failed handshake waits for the child to exit, and for its
/// stderr to drain, before explaining the failure without them.
const DIAGNOSIS_WAIT: Duration = Duration::from_millis(1000);

/// ssh's own exit status for a failure of its own, as opposed to the remote
/// command's.
const SSH_FAILED: i32 = 255;

/// The remote shell's exit status for a command it cannot find.
const COMMAND_NOT_FOUND: i32 = 127;

/// How [`Client::connect_ssh_with`](crate::Client::connect_ssh_with) runs ssh.
///
/// Non-exhaustive, with chained setters, so no struct literal is written
/// outside this crate. The default runs `ssh` from the `PATH` with batch
/// mode off and no timeout, suiting one person at a terminal. A caller
/// dialing many hosts sets [`Self::batch_mode`] and [`Self::timeout`]. `Debug`
/// is derived: a path, a flag and a duration carry no secret.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SshOptions {
    program: PathBuf,
    batch_mode: bool,
    timeout: Option<Duration>,
    autostart: bool,
}

impl Default for SshOptions {
    fn default() -> Self {
        Self {
            program: PathBuf::from("ssh"),
            batch_mode: false,
            timeout: None,
            autostart: false,
        }
    }
}

impl SshOptions {
    /// Runs `program` in place of `ssh`. The seam for a stand-in in tests.
    #[must_use]
    pub fn program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Passes `-o BatchMode=yes`, so ssh fails instead of prompting.
    #[must_use]
    pub fn batch_mode(mut self, batch_mode: bool) -> Self {
        self.batch_mode = batch_mode;
        self
    }

    /// Asks the remote `shep dial-stdio` to boot the host's shepherd from its
    /// saved roll when none answers, by ending the command line in
    /// `--autostart`.
    ///
    /// Off by default: a dial that finds no shepherd reports it. A remote shep
    /// older than this flag exits with a usage error, which reads as
    /// [`SshCause::RemoteTooOld`].
    #[must_use]
    pub fn autostart(mut self, autostart: bool) -> Self {
        self.autostart = autostart;
        self
    }

    /// The timeout a dial runs under: these options' own, or `budget` when it
    /// is smaller or these options set none.
    pub(crate) fn timeout_or(&self, budget: Option<Duration>) -> Option<Duration> {
        match (self.timeout, budget) {
            (Some(own), Some(budget)) => Some(own.min(budget)),
            (own, budget) => own.or(budget),
        }
    }

    /// Bounds the spawn and the handshake together, failing as
    /// [`ConnectError::HandshakeTimeout`]. `None` bounds nothing, so a person
    /// may answer a prompt.
    #[must_use]
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Why dialling a host through ssh failed, inside [`ConnectError::Ssh`].
///
/// Non-exhaustive: expect more variants. `Debug` is derived: a program path,
/// a destination and ssh's own sanitised complaint carry no secret.
#[derive(Debug)]
#[non_exhaustive]
pub enum SshCause {
    /// The ssh program could not be run.
    Spawn {
        /// The program that was run, as [`SshOptions::program`] named it.
        program: PathBuf,
        /// The OS error the spawn returned.
        source: io::Error,
    },
    /// ssh exited 255: it could not reach the host or log in.
    Transport {
        /// ssh's own complaint, with control characters dropped.
        stderr: String,
    },
    /// The remote shell could not find `shep`: it exited 127, or its stderr
    /// says a command was not found or not recognized.
    CommandNotFound,
    /// The remote `shep dial-stdio` found no shepherd running.
    NoShepherd,
    /// The remote shep has no `dial-stdio` verb.
    RemoteTooOld,
    /// The handshake failed and nothing above explains it. A remote login
    /// script that prints to stdout is the likely cause.
    LoginOutput(Box<ConnectError>),
}

impl SshCause {
    /// Writes the message for a dial of `destination`.
    pub(crate) fn write_for(&self, f: &mut fmt::Formatter<'_>, destination: &str) -> fmt::Result {
        match self {
            Self::Spawn { program, source } => {
                write!(f, "could not run `{}`: {source}", program.display())
            }
            Self::Transport { stderr } if stderr.is_empty() => {
                write!(f, "ssh could not reach `{destination}` (exit 255)")
            }
            Self::Transport { stderr } => {
                write!(f, "ssh could not reach `{destination}`: {stderr}")
            }
            Self::CommandNotFound => write!(
                f,
                "`shep` was not found on the PATH of a non-interactive ssh session on \
                 `{destination}`; check with `ssh {destination} shep --version`, and put \
                 shep's directory on PATH in a file a non-interactive shell reads, such as \
                 `~/.zshenv`, or in `~/.bashrc` above its interactive-only return"
            ),
            Self::NoShepherd => write!(f, "no shepherd is running on `{destination}`"),
            Self::RemoteTooOld => write!(
                f,
                "the shep on `{destination}` has no `dial-stdio` verb; upgrade shep on that host"
            ),
            Self::LoginOutput(inner) => write!(
                f,
                "the handshake with `{destination}` failed ({inner}); a login script on that \
                 host printing to stdout breaks the stream, and `ssh {destination} true` \
                 should print nothing"
            ),
        }
    }
}

impl fmt::Display for SshCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_for(f, "<destination>")
    }
}

impl core::error::Error for SshCause {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::LoginOutput(inner) => Some(inner.as_ref()),
            Self::Transport { .. }
            | Self::CommandNotFound
            | Self::NoShepherd
            | Self::RemoteTooOld => None,
        }
    }
}

/// The first [`STDERR_CAP`] bytes ssh wrote to stderr, filled by a task.
#[derive(Clone, Default)]
struct StderrTail(Arc<Mutex<Vec<u8>>>);

impl StderrTail {
    /// Reads `stderr` to its end, keeping the head and discarding the rest so
    /// a chatty child never blocks on a full pipe.
    async fn drain(self, mut stderr: ChildStderr) {
        let mut chunk = [0_u8; 512];
        while let Ok(read @ 1..) = stderr.read(&mut chunk).await {
            let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            let room = STDERR_CAP.saturating_sub(kept.len());
            kept.extend_from_slice(&chunk[..read.min(room)]);
        }
    }

    /// What was kept, as text safe to print: every control character but
    /// newline and tab dropped, since a remote host chose these bytes and a
    /// terminal will act on an escape sequence.
    fn text(&self) -> String {
        let kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&kept)
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
            .collect::<String>()
            .trim()
            .to_owned()
    }
}

/// The child's stdout and stdin as one stream, whose drop tells the task
/// watching the child that the connection is over.
struct SshStream {
    io: tokio::io::Join<tokio::process::ChildStdout, tokio::process::ChildStdin>,
    _ended: oneshot::Sender<()>,
}

impl AsyncRead for SshStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for SshStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Runs `ssh` to `destination` and handshakes over its pipes, bounded by
/// `timeout` when there is one.
///
/// # Errors
///
/// See [`Client::connect_ssh_with`](crate::Client::connect_ssh_with).
pub(crate) async fn open(
    destination: &Destination,
    options: &SshOptions,
    timeout: Option<Duration>,
    dog_name: Option<&str>,
) -> Result<Connection, ConnectError> {
    // a spent budget must not start an ssh child it would only kill
    if let Some(after) = timeout.filter(Duration::is_zero) {
        return Err(ConnectError::HandshakeTimeout { after });
    }
    dial(destination, options, timeout, dog_name).await
}

/// Spawns ssh and handshakes over it. `timeout` bounds the handshake, which
/// is all that waits once the spawn call has returned, but not the diagnosis
/// of a failure: that has its own short waits, and a deadline landing in the
/// middle of it would trade the cause for a bare timeout.
async fn dial(
    destination: &Destination,
    options: &SshOptions,
    timeout: Option<Duration>,
    dog_name: Option<&str>,
) -> Result<Connection, ConnectError> {
    let mut command = Command::new(&options.program);
    command.arg("-T");
    if options.batch_mode {
        command.args(["-o", "BatchMode=yes"]);
    }
    command.args(["--", destination.as_str(), "shep", "dial-stdio"]);
    if options.autostart {
        command.arg("--autostart");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|source| ConnectError::Ssh {
        destination: destination.to_string(),
        cause: SshCause::Spawn {
            program: options.program.clone(),
            source,
        },
    })?;

    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        unreachable!("stdin, stdout and stderr were all piped");
    };
    let tail = StderrTail::default();
    let draining = tokio::spawn(tail.clone().drain(stderr));

    let (ended, connection_over) = oneshot::channel();
    let stream = SshStream {
        io: tokio::io::join(stdout, stdin),
        _ended: ended,
    };
    let handshake = Connection::open_over(stream, dog_name);
    let outcome = match timeout {
        // dropping the future drops `child`, which kills it
        Some(after) => tokio::time::timeout(after, handshake)
            .await
            .map_err(|_elapsed| ConnectError::HandshakeTimeout { after })?,
        None => handshake.await,
    };
    match outcome {
        Ok(connection) => {
            tokio::spawn(reap_when_over(child, connection_over));
            Ok(connection)
        }
        Err(failed) => Err(explain(destination, &mut child, draining, &tail, failed).await),
    }
}

/// Lets the ssh child exit on its own once the connection is over, and kills
/// it if it has not within [`EXIT_GRACE`], so no ssh outlives its client.
async fn reap_when_over(mut child: Child, connection_over: oneshot::Receiver<()>) {
    // resolves with `Err` when the stream drops its sender: that is the signal
    let _ = connection_over.await;
    if tokio::time::timeout(EXIT_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
}

/// Turns a failed handshake into the error that says what went wrong on the
/// far side, from the child's exit status and stderr.
///
/// A refusal is returned as it is: the far side answered, and its words are
/// the explanation.
async fn explain(
    destination: &Destination,
    child: &mut Child,
    draining: tokio::task::JoinHandle<()>,
    tail: &StderrTail,
    failed: ConnectError,
) -> ConnectError {
    if matches!(failed, ConnectError::ProtocolMismatch { .. }) {
        return failed;
    }
    let status = tokio::time::timeout(DIAGNOSIS_WAIT, child.wait())
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|status| status.code());
    // best effort: a background ssh holding the pipe open leaves what it said so far
    let _ = tokio::time::timeout(DIAGNOSIS_WAIT, draining).await;
    let stderr = tail.text();

    let cause = if status == Some(SSH_FAILED) {
        SshCause::Transport { stderr }
    } else if status == Some(COMMAND_NOT_FOUND)
        || stderr.contains("CommandNotFoundException")
        || stderr.contains("is not recognized as")
    {
        SshCause::CommandNotFound
    } else if status == Some(i32::from(exit::DAEMON_UNREACHABLE)) {
        SshCause::NoShepherd
    } else if status == Some(i32::from(exit::USAGE)) && stderr.contains("dial-stdio") {
        SshCause::RemoteTooOld
    } else {
        SshCause::LoginOutput(Box::new(failed))
    };
    ConnectError::Ssh {
        destination: destination.to_string(),
        cause,
    }
}

/// Stand-ins for ssh that tests across this crate run in its place.
#[cfg(all(test, unix))]
pub(crate) mod stand_in {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use shep_core::protocol::{HelloAck, HelloReply, PROTOCOL_VERSION, encode_frame};

    /// Writes an executable `#!/bin/sh` stand-in for ssh into `dir`.
    pub(crate) fn stand_in(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A length-prefixed `reply`, the bytes a shepherd puts on the wire.
    fn framed(reply: &HelloReply) -> Vec<u8> {
        let payload = encode_frame(reply).unwrap();
        let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend_from_slice(&payload);
        bytes
    }

    pub(crate) fn ack() -> HelloAck {
        HelloAck {
            daemon_version: "9.9.9".into(),
            protocol: PROTOCOL_VERSION,
            pid: 4242,
            min_supported: None,
        }
    }

    /// Writes `reply.bin` into `dir`: a framed ack a stand-in can `cat`.
    pub(crate) fn write_reply(dir: &Path) {
        std::fs::write(dir.join("reply.bin"), framed(&Ok(ack()))).unwrap();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::stand_in::{ack, stand_in, write_reply};
    use super::*;
    use crate::client::Client;

    fn options(program: PathBuf) -> SshOptions {
        SshOptions::default().program(program)
    }

    /// The failure of a dial that must fail.
    async fn failure(dest: &str, options: &SshOptions) -> ConnectError {
        Client::connect_ssh_with(dest, options)
            .await
            .expect_err("the stand-in was meant to fail")
    }

    fn recorded_args(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("args.txt"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    const RECORD_ARGS: &str =
        "for a in \"$@\"; do echo \"$a\"; done > \"$(dirname \"$0\")/args.txt\"";

    #[tokio::test]
    async fn the_command_line_is_dash_t_dash_dash_destination_shep_dial_stdio() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(dir.path(), &format!("{RECORD_ARGS}\nexit 255"));

        let _ = failure("deploy@build-01", &options(program)).await;

        assert_eq!(
            recorded_args(dir.path()),
            ["-T", "--", "deploy@build-01", "shep", "dial-stdio"]
        );
    }

    #[tokio::test]
    async fn autostart_appends_its_flag_after_dial_stdio() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(dir.path(), &format!("{RECORD_ARGS}\nexit 255"));

        let _ = failure("build-01", &options(program).autostart(true)).await;

        assert_eq!(
            recorded_args(dir.path()),
            ["-T", "--", "build-01", "shep", "dial-stdio", "--autostart"]
        );
    }

    #[tokio::test]
    async fn batch_mode_puts_its_option_before_the_dashes() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(dir.path(), &format!("{RECORD_ARGS}\nexit 255"));

        let _ = failure("build-01", &options(program).batch_mode(true)).await;

        assert_eq!(
            recorded_args(dir.path()),
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "--",
                "build-01",
                "shep",
                "dial-stdio"
            ]
        );
    }

    #[tokio::test]
    async fn a_canned_ack_through_a_stand_in_completes_the_handshake_and_the_client_reports_its_host()
     {
        let dir = tempfile::tempdir().unwrap();
        write_reply(dir.path());
        let program = stand_in(
            dir.path(),
            "cat \"$(dirname \"$0\")/reply.bin\"\ncat >/dev/null",
        );

        let client = Client::connect_ssh_with("build-01", &options(program))
            .await
            .unwrap();

        assert_eq!(client.daemon(), &ack());
        assert_eq!(client.host(), Some("build-01"));
    }

    #[tokio::test]
    async fn exit_255_names_the_destination_and_carries_ssh_s_own_complaint() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(
            dir.path(),
            "echo 'ssh: connect to host build-01 port 22: Connection refused' >&2\nexit 255",
        );

        let err = failure("build-01", &options(program)).await;

        let SshCause::Transport { stderr } = cause_of(&err) else {
            panic!("exit 255 is ssh's own failure, got {err:?}");
        };
        assert!(stderr.contains("Connection refused"), "{stderr}");
        let shown = err.to_string();
        assert!(shown.contains("`build-01`"), "{shown}");
        assert!(shown.contains("Connection refused"), "{shown}");
    }

    /// The cause of an `Ssh` failure, or a panic naming what came instead.
    fn cause_of(err: &ConnectError) -> &SshCause {
        match err {
            ConnectError::Ssh { cause, .. } => cause,
            other => panic!("expected an ssh failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_127_names_the_path_fix_and_the_check_command() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(
            dir.path(),
            "echo 'sh: shep: command not found' >&2\nexit 127",
        );

        let err = failure("build-01", &options(program)).await;

        assert!(
            matches!(cause_of(&err), SshCause::CommandNotFound),
            "{err:?}"
        );
        let shown = err.to_string();
        assert!(
            shown.contains("`shep` was not found on the PATH"),
            "{shown}"
        );
        assert!(shown.contains("ssh build-01 shep --version"), "{shown}");
        assert!(shown.contains("~/.zshenv"), "{shown}");
        assert!(shown.contains("~/.bashrc"), "{shown}");
    }

    #[tokio::test]
    async fn powershell_command_not_found_text_reads_as_a_missing_shep() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(
            dir.path(),
            "echo \"shep : The term 'shep' is not recognized as the name of a cmdlet\" >&2\n\
             echo 'CategoryInfo : ObjectNotFound: (shep:String) [], CommandNotFoundException' >&2\n\
             exit 1",
        );

        let err = failure("utm-win", &options(program)).await;

        assert!(
            matches!(cause_of(&err), SshCause::CommandNotFound),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_remote_dial_stdio_exiting_with_the_unreachable_code_reads_as_no_shepherd_on_the_host()
     {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(dir.path(), "echo 'no shepherd is running' >&2\nexit 5");

        let err = failure("build-01", &options(program)).await;

        assert!(matches!(cause_of(&err), SshCause::NoShepherd), "{err:?}");
        assert!(
            err.to_string()
                .contains("no shepherd is running on `build-01`"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_remote_without_dial_stdio_is_named_too_old() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(
            dir.path(),
            "echo \"error: unrecognized subcommand 'dial-stdio'\" >&2\nexit 2",
        );

        let err = failure("build-01", &options(program)).await;

        assert!(matches!(cause_of(&err), SshCause::RemoteTooOld), "{err:?}");
        assert!(
            err.to_string().contains("upgrade shep on that host"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_missing_ssh_program_is_a_spawn_error_naming_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-ssh");

        let err = failure("build-01", &options(missing.clone())).await;

        let SshCause::Spawn { program, .. } = cause_of(&err) else {
            panic!("a program that cannot run is a spawn error, got {err:?}");
        };
        assert_eq!(program, &missing);
        assert!(err.to_string().contains("no-such-ssh"), "{err}");
        assert!(
            core::error::Error::source(&err).is_some(),
            "the io error is the source"
        );
    }

    #[tokio::test]
    async fn a_destination_ssh_would_read_as_an_option_is_refused_before_anything_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let program = stand_in(dir.path(), &format!("touch '{}'", ran.display()));

        for bad in ["", "-oProxyCommand=x", "a b", "host\nname"] {
            let err = failure(bad, &options(program.clone())).await;
            assert!(
                matches!(err, ConnectError::Destination(_)),
                "{bad:?}: {err:?}"
            );
        }

        assert!(!ran.exists(), "ssh was run for a refused destination");
    }

    #[tokio::test]
    async fn captured_stderr_is_capped_and_loses_control_characters() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(
            dir.path(),
            "printf 'before\\033[31m red\\033]0;title\\007\\ra\\tb\\nline two\\n' >&2\n\
             i=0; while [ $i -lt 400 ]; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxx' >&2; i=$((i+1)); done\n\
             exit 255",
        );

        let err = failure("build-01", &options(program)).await;

        let SshCause::Transport { stderr } = cause_of(&err) else {
            panic!("exit 255 is ssh's own failure, got {err:?}");
        };
        assert!(stderr.len() <= STDERR_CAP, "{} bytes kept", stderr.len());
        assert!(
            stderr.starts_with("before[31m red]0;titlea\tb\nline two\nxxx"),
            "{stderr:?}"
        );
        assert!(
            stderr
                .chars()
                .all(|ch| !ch.is_control() || ch == '\n' || ch == '\t'),
            "{stderr:?}"
        );
    }

    #[tokio::test]
    async fn garbage_on_stdout_adds_the_login_output_hint() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(
            dir.path(),
            "echo 'Welcome to build-01, last login: yesterday'\ncat >/dev/null",
        );

        let err = failure("build-01", &options(program)).await;

        assert!(
            matches!(cause_of(&err), SshCause::LoginOutput(_)),
            "{err:?}"
        );
        assert!(
            err.to_string()
                .contains("`ssh build-01 true` should print nothing"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn dropping_the_client_ends_the_ssh_child() {
        let dir = tempfile::tempdir().unwrap();
        write_reply(dir.path());
        let program = stand_in(
            dir.path(),
            "echo $$ > \"$(dirname \"$0\")/pid\"\n\
             cat \"$(dirname \"$0\")/reply.bin\"\nsleep 30",
        );
        let client = Client::connect_ssh_with("build-01", &options(program))
            .await
            .unwrap();
        let pid = nix::unistd::Pid::from_raw(
            std::fs::read_to_string(dir.path().join("pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        assert!(
            nix::sys::signal::kill(pid, None).is_ok(),
            "the child should be running"
        );

        drop(client);

        let gone = tokio::time::timeout(Duration::from_secs(10), async {
            while nix::sys::signal::kill(pid, None).is_ok() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(gone.is_ok(), "ssh child {pid} outlived its client");
    }

    #[tokio::test]
    async fn a_timeout_bounds_a_stand_in_that_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let program = stand_in(dir.path(), "sleep 30");
        let options = options(program).timeout(Some(Duration::from_millis(200)));

        let err = failure("build-01", &options).await;

        let ConnectError::HandshakeTimeout { after } = err else {
            panic!("a silent ssh must time out, got {err:?}");
        };
        assert_eq!(after, Duration::from_millis(200));
    }

    #[tokio::test]
    async fn a_spent_timeout_fails_before_anything_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let program = stand_in(dir.path(), &format!("touch '{}'", ran.display()));
        let options = options(program).timeout(Some(Duration::ZERO));

        let err = failure("build-01", &options).await;

        assert!(
            matches!(err, ConnectError::HandshakeTimeout { after } if after.is_zero()),
            "{err:?}"
        );
        assert!(!ran.exists(), "ssh was run with no time left");
    }
}
