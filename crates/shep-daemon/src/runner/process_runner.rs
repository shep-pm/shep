#[cfg(unix)]
use super::AdoptedReaper;
use super::dog_link::DogLink;
use super::log_protocol::{ExitOutcome, LogCtl, LogLine};
use crate::channel::{ChildMessage, ShepherdMessage};
use crate::privilege::Credentials;
use bytes::Bytes;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use shep_core::signals::OperatorSignal;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Typed stop signal
///
/// [`StopSignal::as_raw`] gives the unix number so fake and real runners
/// record identical [`ExitOutcome`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    /// `SIGTERM`: graceful stop request
    Term,
    /// `SIGINT`: interrupt
    Int,
    /// `SIGQUIT`: quit, core-dumping by default
    Quit,
    /// `SIGUSR2`: user-defined signal 2
    Usr2,
    /// `SIGKILL`: unblockable, immediate
    Kill,
}

impl StopSignal {
    /// The raw unix signal number
    #[must_use]
    pub fn as_raw(self) -> i32 {
        match self {
            Self::Term => 15,
            Self::Int => 2,
            Self::Quit => 3,
            Self::Usr2 => 12,
            Self::Kill => 9,
        }
    }
}

/// IO endpoints handed back by spawn; the runner pumps internally.
///
/// The sheep task owns this and must drain every receiver: an undrained
/// `from_child` back-pressures a metric-emitting child until it stalls on
/// its own fd-3 write.
#[derive(Debug)]
pub struct ProcIo {
    /// stdout+stderr lines
    pub logs: mpsc::Receiver<LogLine>,
    /// Parsed child→daemon shepherd-channel messages
    pub from_child: mpsc::Receiver<ChildMessage>,
    /// daemon→child shepherd-channel sender
    pub to_child: mpsc::Sender<ShepherdMessage>,
    /// Control channel into this sheep's log pump
    ///
    /// The pump is the only reader of the child's stdout and stderr, and it
    /// ends when the last of these senders drops, so hold this while the
    /// child is alive: ending the pump drops the read ends of both pipes and
    /// the child's next write gets `EPIPE`/`SIGPIPE`. A send that fails
    /// means the pump is already gone, which makes a reopen a no-op.
    pub log_ctl: mpsc::Sender<LogCtl>,
    /// The shepherd's writing end of this sheep's stdin.
    ///
    /// Always present, and closed rather than absent when the app asked for
    /// no pipe: the runner drops the receiving end, so `is_closed()` is the
    /// one question a caller asks, the same shape [`Self::to_child`] uses.
    ///
    /// Hold it only for as long as the child is alive.
    pub to_stdin: mpsc::Sender<StdinWrite>,
    /// A hold on the master of this sheep's pseudo-terminal, for a sheep that
    /// runs on one. `None` for a sheep on pipes.
    ///
    /// Like [`Self::to_stdin`], hold it only for as long as the child is
    /// alive.
    pub pty: Option<PtyHandle>,
}

/// What a [`PtyHandle`] asks of the task that owns the master.
///
/// One queue for both, and the same task that types a whispered line, so a
/// raw write and a whisper cannot interleave mid-write.
#[derive(Debug)]
#[cfg_attr(
    windows,
    allow(dead_code, reason = "only the unix pty pump reads these")
)]
pub(crate) enum PtyCommand {
    /// Type these bytes as they are.
    Input {
        /// The bytes, with no terminator added.
        bytes: Vec<u8>,
        /// Fires once the bytes have landed, or with why they could not.
        done: oneshot::Sender<Result<(), PtyError>>,
    },
    /// Set the window size and ask the sheep to redraw with `SIGWINCH`.
    Resize {
        /// Rows of the new window.
        rows: u16,
        /// Columns of the new window.
        cols: u16,
        /// Fires once the size is set, or with why it could not be.
        done: oneshot::Sender<Result<(), PtyError>>,
    },
}

/// Why a [`PtyHandle`] could not act on its terminal.
///
/// `#[non_exhaustive]`: an out-of-tree consumer can match this exhaustively,
/// and a third cause would break them with no version bump.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyError {
    /// The sheep's terminal has closed: the child's last descriptor on it is
    /// gone, or the task that owned the master has ended.
    Gone,
    /// The terminal is open and the OS refused the write or the size
    /// (carries the OS message).
    Failed(String),
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gone => f.write_str("the sheep's terminal has closed"),
            Self::Failed(msg) => write!(f, "terminal write failed: {msg}"),
        }
    }
}

impl core::error::Error for PtyError {}

/// The master of a pty sheep's terminal: its raw output, and what types into
/// it.
///
/// Cheap to clone. Every clone reaches the one terminal, and every method
/// fails with [`PtyError::Gone`] once the sheep is gone.
#[derive(Debug, Clone)]
pub struct PtyHandle {
    output: Weak<broadcast::Sender<Bytes>>,
    control: mpsc::Sender<PtyCommand>,
    closed: Arc<AtomicBool>,
}

impl PtyHandle {
    /// Builds a handle over the runner's end of the three.
    ///
    /// `closed` is set by whoever reads the master once the terminal ends.
    /// The handle holds `output` weakly: the reader owns the sender, so the
    /// channel closes, and a subscriber's `recv` ends, when the reader does.
    pub(crate) fn new(
        output: &Arc<broadcast::Sender<Bytes>>,
        control: mpsc::Sender<PtyCommand>,
        closed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            output: Arc::downgrade(output),
            control,
            closed,
        }
    }

    /// A receiver of what the child writes to its terminal from now on: the
    /// raw bytes, escape codes and all, as chunked by the reads.
    ///
    /// A new receiver sees no earlier output, and one that falls behind loses
    /// the oldest chunks, as [`broadcast::Receiver::recv`] says. Once the
    /// terminal has closed, `recv` fails with `Closed` after the last chunk.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        match self.output.upgrade() {
            Some(output) => output.subscribe(),
            None => broadcast::channel(1).1,
        }
    }

    /// Types `bytes` into the terminal as they are.
    ///
    /// Whole: no whispered line lands between its first byte and its last.
    ///
    /// # Errors
    ///
    /// - [`PtyError::Gone`] once the sheep's terminal has closed.
    /// - [`PtyError::Failed`] if the OS refused the write.
    pub async fn send(&self, bytes: Vec<u8>) -> Result<(), PtyError> {
        self.ask(|done| PtyCommand::Input { bytes, done }).await
    }

    /// Sets the window to `rows` by `cols` and signals `SIGWINCH` whether or
    /// not the size changed: to the terminal's foreground group where the OS
    /// will name it, and to the group the sheep leads where it will not.
    ///
    /// The kernel sends that signal only on a change, so an attach at the size
    /// the terminal already has would otherwise draw nothing. A redraw the
    /// shepherd could not ask for is its own to log, not an error here.
    ///
    /// # Errors
    ///
    /// - [`PtyError::Gone`] once the sheep's terminal has closed.
    /// - [`PtyError::Failed`] if the OS refused the size.
    pub async fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        self.ask(|done| PtyCommand::Resize { rows, cols, done })
            .await
    }

    /// Whether the terminal has closed, so every call would fail.
    ///
    /// The one question a caller asks of a handle it kept, as
    /// `Sender::is_closed` is for [`ProcIo::to_stdin`].
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.control.is_closed()
    }

    async fn ask(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<(), PtyError>>) -> PtyCommand,
    ) -> Result<(), PtyError> {
        if self.is_closed() {
            return Err(PtyError::Gone);
        }
        let (done, answer) = oneshot::channel();
        self.control
            .send(command(done))
            .await
            .map_err(|_| PtyError::Gone)?;
        // A dropped sender means the owning task ended before it answered.
        answer.await.map_err(|_| PtyError::Gone)?
    }
}

/// One line to write to a sheep's stdin, and where the answer goes.
///
/// The acknowledgement is the point, as on [`LogCtl`]: an `mpsc::send` only
/// proves the message was queued, and the line may still be sitting behind a
/// pipe the app has stopped reading. The `oneshot` fires after the bytes are
/// written and flushed.
#[derive(Debug)]
pub struct StdinWrite {
    /// The line, without its terminator: the writer appends one, `\n` for a
    /// pipe and `\r` for a terminal.
    pub line: String,
    /// Fires once the line has landed, or with why it could not.
    ///
    /// A dropped sender means the writer task ended before serving this
    /// request, which happens when the child's stdin closed; the caller reads
    /// that as the pipe being gone.
    pub done: oneshot::Sender<Result<(), RunnerError>>,
}

/// One inherited sheep, and everything a runner needs to supervise it again.
///
/// Produced by the successor's `handover::adopt` and consumed by
/// [`ProcessRunner::adopt`]. The handles are owned, since adopting is taking
/// ownership of them; `None` on a pair means the predecessor had no handle
/// to carry. `Debug` is derived: no env value reaches this type.
#[cfg(unix)]
#[derive(Debug)]
pub struct AdoptSpec {
    /// The sheep's name, which a dog's connection is attributed to.
    pub name: String,
    /// The pid the sheep has been running under all along, unchanged by the
    /// handover.
    pub pid: u32,
    /// Where its stdout is logged, kept so a later rotation can reopen it.
    pub out_file: PathBuf,
    /// Where its stderr is logged, kept for the same reason.
    pub err_file: PathBuf,
    /// The read end of its stdout pipe, still the one the child writes into.
    pub out_pipe: Option<tokio::net::unix::pipe::Receiver>,
    /// The read end of its stderr pipe, likewise.
    pub err_pipe: Option<tokio::net::unix::pipe::Receiver>,
    /// The appending handle on its stdout log, written through rather than
    /// reopened so `O_APPEND` survives.
    pub out_log: Option<tokio::fs::File>,
    /// The appending handle on its stderr log, likewise.
    pub err_log: Option<tokio::fs::File>,
    /// Whether each line written through `out_log` and `err_log` starts
    /// with a [`shep_core::logstamp`] stamp, as [`SpawnSpec::log_timestamps`].
    pub log_timestamps: bool,
    /// Whether its stop signal goes to the leader alone, as
    /// [`SpawnSpec::leader_only_stop`].
    pub leader_only_stop: bool,
    /// The write end of its stdin pipe, still the one the child reads from,
    /// for a sheep whose app asked for one.
    ///
    /// The one handle here the daemon writes to. `None` is the commoner
    /// sheep, which has `/dev/null` on fd 0.
    pub stdin_pipe: Option<tokio::net::unix::pipe::Sender>,
    /// The daemon's end of its shepherd-channel socketpair, still the one
    /// whose other end is the child's fd 3.
    ///
    /// Goes both ways, so an adoption puts both pumps back on it. `None` for
    /// a sheep whose app asked for no channel, and for one whose child has
    /// closed fd 3.
    pub channel: Option<tokio::net::UnixStream>,
    /// The daemon's end of the dog's own connection, still the one whose other
    /// end is the child's fd 4. `None` for a sheep that is not a dog, and for
    /// a dog whose connection had closed, which falls back to the control
    /// socket.
    pub dog_link: Option<tokio::net::UnixStream>,
    /// The master of its pseudo-terminal, still the one whose slave is the
    /// child's stdio.
    ///
    /// Goes both ways: the log pump reads it and the stdin pump types into
    /// it. `None` for a sheep that runs on pipes.
    pub pty: Option<std::fs::File>,
    /// What `dog_link` had subscribed to, or `None` if its dog had not yet
    /// sent its `Hello`. Read only with `dog_link`.
    pub dog_topics: Option<Vec<String>>,
    /// The dog `dog_link` is a launch link for, when the sheep is not itself
    /// a dog: the connection then reads only this sheep's table.
    pub launch_dog: Option<String>,
    /// The one reaper this successor waits every adopted pid through.
    ///
    /// Shared rather than owned per sheep: a status can be collected once,
    /// so two reapers racing on one pid would leave one meeting `ECHILD`.
    pub reaper: std::sync::Arc<AdoptedReaper>,
}

/// What a [`ProcessRunner`] can tell about a [`SpawnSpec`] before anything is
/// spawned
///
/// The line that matters runs between [`Self::Impossible`] and
/// [`Self::Doubtful`], and it separates two kinds of claim. A path with a
/// `/` is a claim about the filesystem, which the daemon can check. A bare
/// command is a claim about the daemon's own environment, not the operator's shell.
// `#[non_exhaustive]`: an out-of-tree consumer can match this exhaustively,
// and a fourth verdict would break them with no version bump.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    /// Nothing is knowable in advance.
    ///
    /// Not "this will work": every form an implementation declines to decide
    /// arrives here alongside every form it decided is fine. A caller may
    /// only act on the other two variants.
    Unknown,
    /// The spawn cannot succeed, as a certainty. Carries one reason, no
    /// trailing punctuation, ready to be printed after a sheep's name.
    ///
    /// A caller registering a batch should refuse the whole batch and
    /// register none of it.
    Impossible(String),
    /// The spawn looks like it will fail, and a caller must not refuse a
    /// batch over it. Carries a reason on the same terms as
    /// [`Self::Impossible`]. Report it and carry on: the spawn then fails for
    /// that one sheep as it would have anyway.
    Doubtful(String),
}

/// Everything a spawn needs, pre-assembled by the assembler (a later task)
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SpawnSpec {
    /// Sheep name (for logging/tracing, not passed to the child)
    pub name: String,
    /// Executable path or name (resolved via `PATH` if bare)
    pub program: String,
    /// Argument vector, `argv[1..]`
    pub args: Vec<String>,
    /// Working directory; `None` inherits the daemon's
    pub cwd: Option<PathBuf>,
    /// Environment variables, fully resolved (no daemon-env leakage beyond this map)
    pub env: BTreeMap<String, String>,
    /// File stdout is appended to
    pub out_file: PathBuf,
    /// File stderr is appended to
    pub err_file: PathBuf,
    /// Start each line appended to `out_file` and `err_file` with a
    /// [`shep_core::logstamp`] stamp. `false` writes the child's lines as
    /// they came.
    pub log_timestamps: bool,
    /// Open the shepherd channel (fd 3 socketpair)
    pub channel: bool,
    /// Pipe the child's stdin, so `shep whisper` can write to it. `false`
    /// gives the child `/dev/null` on fd 0, which is what every sheep gets
    /// unless its config sets `stdin = true`. Ignored under [`Self::pty`],
    /// whose terminal takes a whisper without it.
    pub stdin: bool,
    /// Run the child on a pseudo-terminal: the slave is its fd 0, 1 and 2 and
    /// its controlling terminal, and the log gets what it printed as plain
    /// lines. `false` gives it pipes. Unix only; `normalize` refuses it on
    /// Windows.
    pub pty: bool,
    /// Unix uid/gid to drop to before exec (`None` inherits the daemon's own
    /// identity). Resolved once per `Start` by `crate::privilege::resolve`;
    /// see that module for how `user`/`group` config names become this.
    pub credentials: Option<Credentials>,
    /// Hand the child a connection to the shepherd: on unix a dog's fd 4
    /// socketpair, open from spawn and named in
    /// [`shep_core::dogs::DOG_LINK_FD_VAR`], and on Windows a named pipe the
    /// dog opens, named in [`shep_core::dogs::DOG_LINK_PIPE_VAR`].
    ///
    /// Set by the supervisor for a dog and never by `assemble`, so a sheep
    /// cannot ask for one.
    pub dog_link: bool,
    /// Hand the child a connection scoped to its own table, for a sheep a dog
    /// launches: the dog it launches through, which is the connection's
    /// `dog`, and the spec's own `name` as its sheep.
    ///
    /// Opened as [`Self::dog_link`] is, on fd 4 and in `$SHEP_DOG_FD`, so the
    /// launch runs under any uid. Set by [`through_dog`](crate::assemble::through_dog).
    /// Unix only. A sheep with `listen` sockets gets none, as fd 4 is theirs.
    pub launch_link: Option<String>,
    /// Send the stop signal to the leader alone, not its process group.
    ///
    /// Set with a `dogs.sandbox` table: the sandbox passes the signal on to
    /// the command, so a group signal would reach it twice on macOS.
    /// [`RunningProcess::kill_tree`] stays group-wide, and `max_memory`
    /// still counts the whole tree.
    pub leader_only_stop: bool,
    /// The sockets the shepherd bound for this sheep's `listen` table, passed
    /// to the child from fd 3 upward in name order. `None`, and an empty set,
    /// give the child neither the descriptors nor any `LISTEN_*` variable.
    ///
    /// Shared by every instance of the sheep and held by the supervisor, so
    /// the sockets outlive any one child. Unix only: `normalize` refuses
    /// `listen` on Windows.
    pub listen: Option<std::sync::Arc<crate::listen::ListenSockets>>,
    /// The cgroup this sheep's child joins before it execs, so everything it
    /// forks is accounted there. The runner creates it.
    ///
    /// `None` on a host with no delegated cgroup v2 subtree, and on every
    /// platform but Linux, where the runner ignores it.
    pub cgroup: Option<PathBuf>,
    /// Bytes to write to the cgroup's `memory.max` before the child joins it,
    /// for a sheep with `memory_enforce = "kernel"`. `None` leaves the cgroup
    /// uncapped.
    ///
    /// Read only with [`Self::cgroup`] set, and only on Linux.
    pub memory_max: Option<u64>,
    /// Microseconds of CPU time per 100 ms period to write to the cgroup's
    /// `cpu.max` before the child joins it, for a sheep with `max_cpu_cores`.
    /// `None` leaves the cgroup unthrottled.
    ///
    /// Read only with [`Self::cgroup`] set, and only on Linux.
    pub cpu_quota: Option<u64>,
    /// The task limit (processes and threads) to write to the cgroup's `pids.max` before the child
    /// joins it, for a sheep with `max_tasks`. `None` leaves the cgroup
    /// unlimited.
    ///
    /// Read only with [`Self::cgroup`] set, and only on Linux.
    pub pids_max: Option<u32>,
}

/// Redacted: `env` and `args` both carry whatever the operator configured,
/// a resolved `{{secret:...}}` included, and this type is the one handed to
/// `Command::envs` at exec.
impl fmt::Debug for SpawnSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnSpec")
            .field("name", &self.name)
            .field("program", &self.program)
            .field("args", &format_args!("<{} args>", self.args.len()))
            .field("cwd", &self.cwd)
            .field("env", &format_args!("<{} vars>", self.env.len()))
            .field("pty", &self.pty)
            .finish_non_exhaustive()
    }
}

/// Error type returned from spawn and process control
///
/// `#[non_exhaustive]`: a future process-control primitive, a cgroup freeze
/// or a Windows job-object failure, would need its own variant rather than
/// stretching one of these, and an out-of-tree matcher should not break.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// The OS refused the spawn (exec failure, permissions, missing binary)
    SpawnFailed(String),
    /// Signal delivery failed (already reaped, `EPERM`)
    SignalFailed(String),
    /// A write to a child's stdin failed (carries the OS message, or the
    /// shepherd's own bound when the app was not reading).
    WriteFailed(String),
    /// A sheep inherited across a handover could not be taken back under
    /// supervision: this runner does not adopt at all, or the carried
    /// handles could not be wired to a pump.
    AdoptFailed(String),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpawnFailed(msg) => write!(f, "process spawn failed: {msg}"),
            Self::SignalFailed(msg) => write!(f, "signal delivery failed: {msg}"),
            Self::WriteFailed(msg) => write!(f, "stdin write failed: {msg}"),
            Self::AdoptFailed(msg) => write!(f, "process adoption failed: {msg}"),
        }
    }
}

impl core::error::Error for RunnerError {}

/// A live child.
pub trait RunningProcess: Send + 'static {
    /// The OS process id
    fn pid(&self) -> u32;

    /// Resolves exactly once with the exit outcome
    ///
    /// # Cancellation safety
    ///
    /// Dropping the returned future and calling `wait` again neither
    /// restarts the wait nor loses progress toward it, as
    /// [`tokio::process::Child::wait`] guarantees.
    fn wait(&mut self) -> impl core::future::Future<Output = ExitOutcome> + Send;

    /// Sends a signal to the sheep's whole process group
    ///
    /// Group-wide: a `thing & wait` wrapper's forked child stays in its own
    /// group, and a leader-only signal would leave it running and untracked.
    /// Implementors must spawn each child as the leader of a fresh group; this
    /// and [`Self::kill_tree`] address it by [`Self::pid`], so a `setsid`
    /// escapee is beyond both. A sandboxed sheep is the exception: see
    /// [`SpawnSpec::leader_only_stop`].
    ///
    /// # Errors
    ///
    /// - [`RunnerError::SignalFailed`] if delivery failed (already reaped,
    ///   `EPERM`).
    fn signal(&mut self, sig: StopSignal) -> Result<(), RunnerError>;

    /// Sends `sig` to this sheep's own process, never its process group.
    ///
    /// Not group-wide, unlike [`Self::signal`]: this exists for a
    /// conversation between an operator and one application, and a `SIGHUP`
    /// broadcast to the group would reach whatever `sh` and runtime children
    /// are in it. The default refuses rather than widening to the group.
    ///
    /// # Errors
    ///
    /// - [`RunnerError::SignalFailed`] if delivery failed (`ESRCH`, `EPERM`)
    ///   or this implementation has no per-process delivery at all.
    fn signal_process(&mut self, sig: OperatorSignal) -> Result<(), RunnerError> {
        let _ = sig;
        Err(RunnerError::SignalFailed(
            "this runner cannot signal a single process".to_string(),
        ))
    }

    /// SIGKILLs the whole process group/tree
    ///
    /// The escalation rung above [`Self::signal`]: same group, same
    /// process-group assumption, but a signal nothing can catch or ignore.
    ///
    /// # Errors
    ///
    /// - [`RunnerError::SignalFailed`] if delivery failed (already reaped,
    ///   `EPERM`).
    fn kill_tree(&mut self) -> Result<(), RunnerError>;
}

/// Spawn seam between engine and OS
pub trait ProcessRunner: Send + Sync + 'static {
    /// The live-child type this runner produces
    type Proc: RunningProcess;

    /// Spawns per the spec, returning the proc + its IO bundle
    ///
    /// Must be called from within a Tokio runtime context: both
    /// implementations spawn background tasks internally to pump IO.
    ///
    /// # Errors
    ///
    /// - [`RunnerError::SpawnFailed`] on an exec failure, bad permissions or
    ///   a missing binary.
    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), RunnerError>;

    /// What is knowable about `spec` before anything is spawned
    ///
    /// Lets a caller refuse a whole batch before registering any of it,
    /// rather than registering the apps ahead of the one that cannot spawn.
    /// See [`Preflight`] for which verdicts a caller may refuse a batch over.
    /// The default answers [`Preflight::Unknown`], which is also the honest
    /// answer for a runner that never touches the filesystem.
    #[must_use]
    fn preflight(&self, spec: &SpawnSpec) -> Preflight {
        let _ = spec;
        Preflight::Unknown
    }

    /// Takes the stream of dog connections this runner opens at spawn
    ///
    /// One [`DogLink`] arrives per spawn whose [`SpawnSpec::dog_link`] was
    /// set, once the child has a pid. The first call returns the receiver and
    /// every later one `None`, so a runner has one server. The default is
    /// `None`: a runner that opens no connections has nothing to serve.
    fn dog_links(&self) -> Option<mpsc::UnboundedReceiver<DogLink>> {
        None
    }

    /// Rebuilds a proc around a sheep this process inherited, not started
    ///
    /// The successor half of a handover, unix only as the handover is. Every
    /// handle in `spec` crossed an `execve` with its `FD_CLOEXEC` cleared, so
    /// the sheep never noticed: same pid, same pipes, same open file
    /// description on each log. Nothing here spawns or signals.
    ///
    /// # Errors
    ///
    /// - [`RunnerError::AdoptFailed`] if this runner cannot take a process it
    ///   did not spawn (what the default answers), or if the carried handles
    ///   could not be wired to a pump.
    #[cfg(unix)]
    fn adopt(&self, spec: AdoptSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        let _ = spec;
        Err(RunnerError::AdoptFailed(
            "this runner cannot adopt a process it did not spawn".to_string(),
        ))
    }
}

// Unix-gated because the module this case was split out of is; nothing it
// touches needs the gate.
#[cfg(all(test, unix))]
mod tests {

    use super::*;

    /// This type sits on the exec boundary, so a `tracing` call that
    /// formatted it would put every configured secret in the daemon's log.
    /// `args` counts as much as `env` now that a `{{secret:...}}` resolves
    /// into it: `--token=<value>` on an argv is a value the same way
    /// `TOKEN=<value>` on an environment is. Exact string pinned so a
    /// `derive(Debug)` refactor fails here (IR-41).
    #[test]
    fn debug_redacts_env_values_and_args() {
        let mut spec = SpawnSpec {
            name: "web".to_string(),
            program: "./srv".to_string(),
            args: vec!["--token=sk-live-abc".to_string(), "--port=8080".to_string()],
            ..Default::default()
        };
        spec.env.insert(
            "DATABASE_URL".to_string(),
            "postgres://user:hunter2@db".to_string(),
        );
        spec.env
            .insert("API_KEY".to_string(), "sk-live-abc".to_string());

        assert_eq!(
            format!("{spec:?}"),
            "SpawnSpec { name: \"web\", program: \"./srv\", args: <2 args>, cwd: None, \
                 env: <2 vars>, pty: false, .. }"
        );
    }
}
