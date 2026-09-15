#[cfg(unix)]
use super::AdoptedReaper;
use super::log_protocol::{ExitOutcome, LogCtl, LogLine};
use crate::channel::{ChildMessage, ShepherdMessage};
use crate::privilege::Credentials;
use core::fmt;
use shep_core::signals::OperatorSignal;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

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
    /// the child's next write gets `EPIPE`/`SIGPIPE`. The supervisor clones
    /// it (`SheepSlot::log_ctl`); what keeps a clone from stretching a
    /// pump's life is the pump's own exit on the `logs` receiver going away.
    ///
    /// A send that fails means the pump is already gone, which makes a
    /// reopen a no-op rather than an error.
    pub log_ctl: mpsc::Sender<LogCtl>,
    /// The shepherd's writing end of this sheep's stdin.
    ///
    /// Always present, and closed rather than absent when the app asked for
    /// no pipe: the runner drops the receiving end, so `is_closed()` is the
    /// one question a caller asks, the same shape [`Self::to_child`] uses.
    ///
    /// Hold it only for as long as the child is alive: the task on the far
    /// end parks on `recv()`, so a sender kept past the child's exit parks
    /// that task and holds the pipe's write end with it.
    pub to_stdin: mpsc::Sender<StdinWrite>,
}

/// One line to write to a sheep's stdin, and where the answer goes.
///
/// The acknowledgement is the point, as on [`LogCtl`]: an `mpsc::send` only
/// proves the message was queued, and the line may still be sitting behind a
/// pipe the app has stopped reading. The `oneshot` fires after the bytes are
/// written and flushed.
#[derive(Debug)]
pub struct StdinWrite {
    /// The line, without its terminator: the writer appends one `\n`.
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
/// to carry.
///
/// `Debug` is derived: descriptor numbers, a pid and two log paths are all in
/// `shep flock` already, and no env value reaches this type.
#[cfg(unix)]
#[derive(Debug)]
pub struct AdoptSpec {
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
/// command is a claim about the daemon's own environment, which is not the
/// shell the operator tested in: `node` from homebrew or nvm resolves in a
/// terminal and under no `shep startup` unit. Refusing a batch on the second
/// would keep twelve apps down over one interpreter.
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
#[derive(Clone, PartialEq, Eq)]
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
    /// Open the shepherd channel (fd 3 socketpair)
    pub channel: bool,
    /// Pipe the child's stdin, so `shep whisper` can write to it. `false`
    /// gives the child `/dev/null` on fd 0, which is what every sheep gets
    /// unless its config sets `stdin = true`.
    pub stdin: bool,
    /// Unix uid/gid to drop to before exec (`None` inherits the daemon's own
    /// identity). Resolved once per `Start` by `crate::privilege::resolve`;
    /// see that module for how `user`/`group` config names become this.
    pub credentials: Option<Credentials>,
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
    /// [`tokio::process::Child::wait`] guarantees; the scripted fake mirrors
    /// it by fixing its exit deadline once, at spawn.
    ///
    /// The future is `Send` (RPITIT) because the sheep task that owns the
    /// proc is `tokio::spawn`'ed.
    fn wait(&mut self) -> impl core::future::Future<Output = ExitOutcome> + Send;

    /// Sends a signal to the sheep's whole process group
    ///
    /// Group-wide, not leader-only: a `thing & wait` wrapper's forked child
    /// stays in its own group, and a leader-only signal would leave it
    /// running and untracked. Implementors must spawn each child as the
    /// leader of a fresh group; this and [`Self::kill_tree`] address it by
    /// [`Self::pid`], so a child that escapes with `setsid` is beyond both.
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

// Every case here is `#[cfg(unix)]`, as is everything they exercise: the uid
// model `loose_ancestor` reads, the mode bits it tests, and
// `std::os::unix::fs::symlink`.
#[cfg(all(test, unix))]
mod tests {

    use std::collections::BTreeMap;

    use std::path::PathBuf;

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
            cwd: None,
            env: BTreeMap::new(),
            out_file: PathBuf::from("/tmp/web-out.log"),
            err_file: PathBuf::from("/tmp/web-err.log"),
            channel: false,
            stdin: false,
            credentials: None,
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
                 env: <2 vars>, .. }"
        );
    }
}
