//! [`TokioRunner`] and [`TokioProc`]: [`crate::runner::ProcessRunner`] and
//! [`crate::runner::RunningProcess`] over real child processes.
//!
//! A spawn puts the child in its own process group, so
//! [`RunningProcess::signal`] and [`RunningProcess::kill_tree`] reach the
//! whole group without touching the daemon's own. It optionally wires an
//! fd-3 socketpair as the shepherd channel and hands it to
//! [`super::pump::spawn_channel_pumps`].
//!
//! `command_fds::FdMapping` holds the parent's copy of the child's fd 3
//! inside the `Command`, so dropping the `Command` right after `spawn()`
//! gives the daemon's end a clean EOF at the child's exit.

use std::fs;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::Stdio;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use command_fds::{CommandFdExt, FdMapping};
#[cfg(unix)]
use nix::sys::signal::{self, Signal};
#[cfg(unix)]
use nix::unistd::Pid;
use shep_core::signals::OperatorSignal;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::channel::CHANNEL_VERSION;
#[cfg(unix)]
use crate::runner::{AdoptSpec, AdoptedReaper};
use crate::runner::{
    ExitOutcome, Preflight, ProcIo, ProcessRunner, RunnerError, RunningProcess, SpawnSpec,
    StopSignal,
};

use super::CHANNEL_CAPACITY;
#[cfg(unix)]
use super::log_file::carried_sink;
use super::log_file::{LogSink, PipeFds};
use super::pump::{spawn_channel_pumps, spawn_log_pump, spawn_stdin_pump};

/// Real [`crate::runner::ProcessRunner`] over actual OS processes.
#[derive(Debug, Default)]
pub struct TokioRunner;

/// The exit code [`TokioProc::kill_tree`] terminates a job with.
///
/// Windows exits carry no signal number, so this is all a reader of
/// `ProcessInfo::last_exit` sees for a sheep the daemon killed. `137` is
/// `128 + 9`, what `commands::reap::classify` reads on unix for "killed by
/// SIGKILL".
#[cfg(windows)]
const KILL_TREE_EXIT_CODE: u32 = 137;

/// Distinguishes one spawn's shepherd-channel pipe from every other's.
///
/// The pipe namespace is machine-global, so a name must be unique across this
/// daemon's flock and any other daemon on the host: process id plus this
/// counter, never the sheep's name, which two `$SHEP_HOME`s could share.
/// Monotonic, so a restarted sheep never inherits a dying predecessor's name.
#[cfg(windows)]
static NEXT_CHANNEL_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

impl TokioRunner {
    /// Builds a runner that spawns real OS child processes.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// A live real OS child, produced by [`TokioRunner`]'s
/// [`crate::runner::ProcessRunner::spawn`].
#[derive(Debug)]
pub struct TokioProc {
    /// Captured at spawn: `Child::id` reports `None` once the child has been
    /// waited, and a kill ladder may still need the pid then. It is also the
    /// whole of an adopted sheep's identity.
    pid: u32,
    proc: Supervised,
    /// The job object this sheep and everything it spawns belong to: Windows'
    /// stand-in for the unix process group, and what
    /// [`RunningProcess::kill_tree`] terminates.
    ///
    /// Held for the proc's whole life: the handle is the group, so dropping it
    /// leaves nothing to address the tree by.
    #[cfg(windows)]
    job: crate::sys_windows::Job,
}

/// Where this proc's exit comes from: tokio, or a targeted `waitpid`.
///
/// An adopted sheep crossed an `execve` into a successor that has no `Child`
/// for it and no way to make one, so [`AdoptedReaper`] collects its exit.
/// Only the wait differs: `signal`, `signal_process` and `kill_tree` all
/// address the pid.
#[derive(Debug)]
enum Supervised {
    /// Started by this daemon, and waited by tokio.
    Spawned(Child),
    /// Inherited across a handover, and waited by the successor's reaper.
    #[cfg(unix)]
    Adopted(Arc<AdoptedReaper>),
}

impl RunningProcess for TokioProc {
    fn pid(&self) -> u32 {
        self.pid
    }

    async fn wait(&mut self) -> ExitOutcome {
        let child = match &mut self.proc {
            Supervised::Spawned(child) => child,
            // Cancel-safe: the reaper remembers and replays a status it has
            // taken. Its error means something else reaped the pid, which
            // lands on the same degenerate outcome as the arm below.
            #[cfg(unix)]
            Supervised::Adopted(reaper) => {
                return match reaper.wait(self.pid).await {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        tracing::error!(pid = self.pid, %error, "adopted process wait failed");
                        ExitOutcome {
                            code: None,
                            signal: None,
                        }
                    }
                };
            }
        };
        // Cancel-safe: `Child::wait` replays its cached result rather than
        // restarting.
        match child.wait().await {
            Ok(status) => ExitOutcome {
                code: status.code(),
                // Nothing kills a Windows process by signal, so there is no
                // number for an exit to carry; see `KILL_TREE_EXIT_CODE`.
                #[cfg(windows)]
                signal: None,
                #[cfg(unix)]
                signal: status.signal(),
            },
            Err(error) => {
                // The `wait4()` itself failed, e.g. something else reaped the
                // pid. `wait` has no error variant, so report a terminal one.
                tracing::error!(pid = self.pid, %error, "process wait() failed");
                ExitOutcome {
                    code: None,
                    signal: None,
                }
            }
        }
    }

    #[cfg(unix)]
    fn signal(&mut self, sig: StopSignal) -> Result<(), RunnerError> {
        signal_group(self.pid, to_nix_signal(sig))
    }

    /// Refuses every signal: Windows has no way to deliver anything
    /// SIGTERM-shaped to an arbitrary process.
    ///
    /// `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group)` reaches only
    /// console processes sharing a console with the caller, and a detached
    /// shepherd shares a console with nothing. An `Ok(())` would tell the
    /// ladder a polite stop was delivered and turn every `shep stop` into a
    /// silent hang and kill.
    ///
    /// An app on the shepherd channel never reaches this: `kill::kill_process`
    /// sends `ShepherdMessage::Shutdown` instead. Any other app costs its full
    /// `kill_timeout` and ends in [`Self::kill_tree`].
    #[cfg(windows)]
    fn signal(&mut self, sig: StopSignal) -> Result<(), RunnerError> {
        Err(RunnerError::SignalFailed(format!(
            "Windows cannot deliver {sig:?} to another process; \
             an app that needs a graceful stop must opt into the shepherd \
             channel (shutdown_with_message), otherwise the stop escalates \
             to a forced termination after kill_timeout"
        )))
    }

    #[cfg(unix)]
    fn kill_tree(&mut self) -> Result<(), RunnerError> {
        signal_group(self.pid, Signal::SIGKILL)
    }

    /// Terminates the sheep's whole job: every process it spawned, however
    /// deeply nested.
    ///
    /// Stronger than the unix rung: a grandchild that calls `setsid` escapes
    /// its process group, while a job member cannot leave its job or spawn
    /// outside it, since `sys_windows::Job::create` grants no breakaway.
    #[cfg(windows)]
    fn kill_tree(&mut self) -> Result<(), RunnerError> {
        self.job
            .terminate(KILL_TREE_EXIT_CODE)
            .map_err(|error| RunnerError::SignalFailed(error.to_string()))
    }

    /// `SIGKILL` is delivered; the other eight names are refused by name.
    ///
    /// Per-signal rather than per-verb, so `shep signal <sheep> SIGKILL` keeps
    /// working while `SIGHUP` says what it cannot do. Seven of the nine have
    /// no delivery mechanism to a foreign Windows process at all. `Int` is
    /// refused as a judgement: `GenerateConsoleCtrlEvent(CTRL_C_EVENT, ..)`
    /// exists, but Ctrl+C is disabled by default under
    /// `CREATE_NEW_PROCESS_GROUP`, which is how every sheep is spawned.
    ///
    /// Per-process, matching the unix arm's positive-pid `kill`: this leaves
    /// the sheep's lambs running, unlike [`Self::kill_tree`].
    #[cfg(windows)]
    fn signal_process(&mut self, sig: OperatorSignal) -> Result<(), RunnerError> {
        let Supervised::Spawned(child) = &mut self.proc;
        match sig {
            OperatorSignal::Kill => child
                .start_kill()
                .map_err(|error| RunnerError::SignalFailed(error.to_string())),
            other => Err(RunnerError::SignalFailed(format!(
                "Windows has no way to deliver {other:?} to another process; \
                 only SIGKILL is available here"
            ))),
        }
    }

    #[cfg(unix)]
    fn signal_process(&mut self, sig: OperatorSignal) -> Result<(), RunnerError> {
        // Positive pid, unlike `signal_group`'s negative one: this reaches the
        // sheep alone, that one reaches its whole group.
        let pid = i32::try_from(self.pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| {
                RunnerError::SignalFailed(format!(
                    "pid {} is not a signallable process id",
                    self.pid
                ))
            })?;
        signal::kill(Pid::from_raw(pid), to_nix_operator_signal(sig))
            .map_err(|error| RunnerError::SignalFailed(error.to_string()))
    }
}

// Group-wide for both stop rungs and for the exec prober's timeout path
// (`probes/os.rs`, the reason this is `pub(crate)`): a wrapper script that
// forks without exec'ing leaves its child in the sheep's group, and a
// leader-only signal would leave that child running orphaned.
/// Sends `sig` to the whole process group led by `pid`.
///
/// `-pid` names the group `spawn`'s `process_group(0)` establishes. A
/// descendant that forks and then calls `setsid` lands in its own session,
/// which neither stop rung reaches.
///
/// # Errors
///
/// [`RunnerError::SignalFailed`] if `pid` is not a signallable process id, or
/// the `kill(2)` itself failed (typically `ESRCH`: no group led by `pid`).
#[cfg(unix)]
pub(crate) fn signal_group(pid: u32, sig: Signal) -> Result<(), RunnerError> {
    // `-0` is `0`, and `kill(0, ..)` means the daemon's own group: a zero pid
    // must never reach the syscall.
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| {
            RunnerError::SignalFailed(format!("pid {pid} is not a signallable process id"))
        })?;
    signal::kill(Pid::from_raw(-pid), sig)
        .map_err(|error| RunnerError::SignalFailed(error.to_string()))
}

/// Maps [`StopSignal`] to the nix [`Signal`] it names.
///
/// An explicit match, not `Signal::try_from(sig.as_raw())`, so an unmapped
/// variant is a compile error rather than a runtime one.
#[cfg(unix)]
fn to_nix_signal(sig: StopSignal) -> Signal {
    match sig {
        StopSignal::Term => Signal::SIGTERM,
        StopSignal::Int => Signal::SIGINT,
        StopSignal::Quit => Signal::SIGQUIT,
        StopSignal::Usr2 => Signal::SIGUSR2,
        StopSignal::Kill => Signal::SIGKILL,
    }
}

/// Maps [`OperatorSignal`] to the nix [`Signal`] it names.
///
/// shep-core holds no raw signal numbers, since they differ by platform
/// (`SIGUSR1` is 10 on Linux and 30 on macOS), so the two vocabularies meet
/// here.
#[cfg(unix)]
fn to_nix_operator_signal(sig: OperatorSignal) -> Signal {
    match sig {
        OperatorSignal::Hup => Signal::SIGHUP,
        OperatorSignal::Int => Signal::SIGINT,
        OperatorSignal::Quit => Signal::SIGQUIT,
        OperatorSignal::Term => Signal::SIGTERM,
        OperatorSignal::Usr1 => Signal::SIGUSR1,
        OperatorSignal::Usr2 => Signal::SIGUSR2,
        OperatorSignal::Winch => Signal::SIGWINCH,
        OperatorSignal::Cont => Signal::SIGCONT,
        OperatorSignal::Kill => Signal::SIGKILL,
    }
}

/// What exec will make of `spec.program`, before anything is spawned.
///
/// A `/` makes it a path, absolute or relative to `spec.cwd`, whose absence
/// is [`Preflight::Impossible`] and refuses the caller's whole batch. Without
/// one it is a bare command resolved through `spec.env`'s own `PATH`, at most
/// [`Preflight::Doubtful`]: a `shep startup` unit's `PATH` is not the
/// operator's shell's, so refusing would keep a flock down over one app's
/// interpreter.
///
/// [`Preflight::Unknown`] for everything else, and existence only. Read
/// through [`definitely_absent`] rather than [`Path::exists`], which
/// collapses a permission error into "absent".
pub(super) fn what_exec_will_find(spec: &SpawnSpec) -> Preflight {
    if spec.program.is_empty() {
        return Preflight::Unknown;
    }
    let program = Path::new(&spec.program);
    // A `/` is the claim that this is a path; Windows spells it `\`. Missing
    // that claim costs only the clear refusal: the fall-through looks the
    // program up on PATH, misses, and the spawn proceeds anyway.
    if spec.program.contains('/') || spec.program.contains(std::path::MAIN_SEPARATOR) {
        let full = if program.is_absolute() {
            program.to_path_buf()
        } else {
            match &spec.cwd {
                Some(cwd) => cwd.join(program),
                None => return Preflight::Unknown,
            }
        };
        if !definitely_absent(&full) {
            return Preflight::Unknown;
        }
        return Preflight::Impossible(format!("no such file: {}", full.display()));
    }
    let Some(path) = spec.env.get("PATH").filter(|value| !value.is_empty()) else {
        return Preflight::Unknown;
    };
    // Absent only if every entry answers a plain `NotFound`: one unreadable
    // directory means exec may still find the program there. `split_paths`
    // rather than a separator of our own, since a Windows entry carries a `:`
    // of its own and may be quoted.
    for dir in std::env::split_paths(path).filter(|dir| !dir.as_os_str().is_empty()) {
        match fs::metadata(dir.join(program)) {
            Ok(_) => return Preflight::Unknown,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Preflight::Unknown,
        }
    }
    Preflight::Doubtful(format!(
        "`{}` is not on the shepherd's PATH ({})",
        spec.program,
        summarise_path(path)
    ))
}

/// How many `PATH` entries a preflight message names before summarising the
/// rest.
///
/// Four: [`base_env`](crate::assemble)'s fallback for a startup unit with no
/// `PATH` is three entries, so the case an operator hits prints in full with
/// one to spare. What gets cut off is an interactive shell's `PATH`, which is
/// unreadable in a terminal error.
const PATH_ENTRIES_IN_MESSAGE: usize = 4;

/// What separates one `PATH` entry from the next.
///
/// Display only: the lookup in `what_exec_will_find` goes through
/// `std::env::split_paths`, which also understands quoting.
pub(super) const PATH_LIST_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

/// `path` as a message should print it: in full when short, and otherwise its
/// first [`PATH_ENTRIES_IN_MESSAGE`] entries with a count of the rest.
pub(super) fn summarise_path(path: &str) -> String {
    let entries: Vec<&str> = path
        .split(PATH_LIST_SEPARATOR)
        .filter(|dir| !dir.is_empty())
        .collect();
    if entries.len() <= PATH_ENTRIES_IN_MESSAGE {
        return path.to_string();
    }
    format!(
        "{} and {} more entries",
        entries[..PATH_ENTRIES_IN_MESSAGE].join(&PATH_LIST_SEPARATOR.to_string()),
        entries.len() - PATH_ENTRIES_IN_MESSAGE,
    )
}

/// Whether the filesystem says, without qualification, that `path` is not
/// there.
///
/// `NotFound` and nothing else. [`Path::exists`] returns `false` on any
/// [`fs::metadata`] error, so a permission error, an unsettled mount and a
/// race would all read as absent and have `what_exec_will_find` refuse a
/// whole batch over a filesystem that was unavailable for a moment.
///
/// Follows symlinks, as exec does. A directory and a file with no execute bit
/// both answer `Ok` and so are not absent.
fn definitely_absent(path: &Path) -> bool {
    matches!(fs::metadata(path), Err(err) if err.kind() == io::ErrorKind::NotFound)
}

impl ProcessRunner for TokioRunner {
    type Proc = TokioProc;

    /// Reports a `program` that provably is not there, and nothing else.
    ///
    /// `program` is what `assemble` resolved `script` and `interpreter` down
    /// to, so an app running `npx next start` is checked at `npx` and `next`
    /// is npx's business. See `what_exec_will_find`.
    fn preflight(&self, spec: &SpawnSpec) -> Preflight {
        what_exec_will_find(spec)
    }

    /// Takes a sheep this image inherited rather than started.
    ///
    /// Nothing is spawned, opened or signalled. The carried pipe read ends go
    /// to the same log pump a spawn feeds, the carried log handles are written
    /// through rather than reopened, and the pid is the one the sheep has been
    /// running under all along.
    ///
    /// Stdin and the shepherd channel are both carried, so `shep whisper`
    /// reaches the same fd 0 and the child's fd 3 is undisturbed. What a blob
    /// named neither of is closed here rather than left dangling, so a
    /// caller's `is_closed()` says so at once.
    #[cfg(unix)]
    fn adopt(&self, spec: AdoptSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        let AdoptSpec {
            pid,
            out_file,
            err_file,
            out_pipe,
            err_pipe,
            out_log,
            err_log,
            stdin_pipe,
            channel,
            reaper,
        } = spec;

        let (logs_tx, logs_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (log_ctl_tx, log_ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
        // Read before the handles move into their pumps: these are the
        // numbers the next handover carries.
        let pipes = PipeFds {
            out: out_pipe.as_ref().map(AsRawFd::as_raw_fd),
            err: err_pipe.as_ref().map(AsRawFd::as_raw_fd),
            stdin: stdin_pipe.as_ref().map(AsRawFd::as_raw_fd),
            channel: channel.as_ref().map(AsRawFd::as_raw_fd),
        };
        spawn_log_pump(
            out_pipe,
            err_pipe,
            carried_sink(out_file, out_log),
            carried_sink(err_file, err_log),
            logs_tx,
            log_ctl_rx,
            pipes,
        );

        let (from_child_tx, from_child) = mpsc::channel(CHANNEL_CAPACITY);
        let (to_child, to_child_rx) = mpsc::channel(CHANNEL_CAPACITY);
        if let Some(channel) = channel {
            spawn_channel_pumps(channel, from_child_tx, to_child_rx);
        } else {
            drop(from_child_tx);
            drop(to_child_rx);
        }
        let (to_stdin, to_stdin_rx) = mpsc::channel(CHANNEL_CAPACITY);
        if let Some(stdin_pipe) = stdin_pipe {
            spawn_stdin_pump(Some(stdin_pipe), to_stdin_rx);
        } else {
            drop(to_stdin_rx);
        }

        Ok((
            TokioProc {
                pid,
                proc: Supervised::Adopted(reaper),
            },
            ProcIo {
                logs: logs_rx,
                from_child,
                to_child,
                log_ctl: log_ctl_tx,
                to_stdin,
            },
        ))
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        // `SpawnSpec::env` promises no daemon-env leakage beyond this map, and
        // `Command` inherits the daemon's ambient env without the clear.
        command.env_clear();
        command.envs(&spec.env);
        // `/dev/null` unless the app asked for a pipe: many programs decide
        // they are non-interactive from a closed fd 0. See `AppConfig::stdin`.
        command.stdin(if spec.stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        // New process group rooted at the child, so `kill_tree`'s
        // negative-pid `SIGKILL` cannot reach the daemon's own group.
        #[cfg(unix)]
        command.process_group(0);

        // Containment itself happens after `spawn()`: a process joins a job
        // only once it exists. These flags make that assignment meaningful by
        // keeping a Ctrl+C in the shepherd's console off the flock, and a
        // console child from flashing up a window nobody can draw.
        #[cfg(windows)]
        {
            /// Roots a new console process group at the child.
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            /// Runs a console application without allocating a console window.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }

        #[cfg(unix)]
        if let Some(creds) = spec.credentials {
            // std sets the gid before the uid, which is the order a privilege
            // drop requires. `CommandExt::groups` is unstable and unused, and
            // std still calls `setgroups(0, NULL)` before `setuid()` whenever
            // `.uid()` is set, so the child gets no supplementary groups.
            if let Some(gid) = creds.gid {
                command.gid(gid);
            }
            command.uid(creds.uid);
        }

        // `privilege::resolve` refuses `user`/`group` on Windows long before
        // a spawn, so this is an assertion rather than an error path: real
        // privilege drop there needs a plaintext password or an LSA logon
        // session, and a partial version would be worse than the refusal.
        #[cfg(windows)]
        debug_assert!(
            spec.credentials.is_none(),
            "privilege::resolve must refuse user/group on Windows before a spawn is reached"
        );

        let (from_child_tx, from_child_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (to_child_tx, to_child_rx) = mpsc::channel(CHANNEL_CAPACITY);

        // A named pipe the child opens by name, since `command-fds` is
        // unix-only and `cmd.exe` has no fd-3 redirection. Same wire format;
        // only the handle moves. `SHEP_CHANNEL_PIPE` is exported and
        // `SHEP_CHANNEL_FD` is not, so an app branches on the variable.
        #[cfg(windows)]
        if spec.channel {
            use shep_core::transport;

            // Unique per spawn, so two instances cannot share a channel. The
            // nonce closes prediction, not observation: the pipe namespace
            // lists to any local user and the `accept` below authenticates
            // nobody. A restrictive DACL needs unsafe; see deferred.md.
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|error| {
                RunnerError::SpawnFailed(format!("shepherd channel pipe name: {error}"))
            })?;
            let pipe = std::path::PathBuf::from(format!(
                r"\\.\pipe\shep-channel-{}-{}-{:032x}",
                std::process::id(),
                NEXT_CHANNEL_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
                u128::from_ne_bytes(nonce)
            ));
            let mut listener = transport::Listener::bind(&pipe).map_err(|error| {
                RunnerError::SpawnFailed(format!("shepherd channel pipe: {error}"))
            })?;
            command.env("SHEP_CHANNEL_PIPE", &pipe);
            command.env("SHEP_CHANNEL_VERSION", CHANNEL_VERSION);

            // On a task, since `spawn` is synchronous and the child cannot
            // connect until it is started below. The `closed()` arm bounds
            // it: an app that never opens the pipe would otherwise hold the
            // listener and a sender for the daemon's life. Both are cancel-safe.
            let from_child_tx = from_child_tx.clone();
            tokio::spawn(async move {
                let watcher = from_child_tx.clone();
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok(daemon_end) => {
                            spawn_channel_pumps(daemon_end, from_child_tx, to_child_rx);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "shepherd channel accept failed");
                        }
                    },
                    () = watcher.closed() => {}
                }
            });
        } else {
            drop(to_child_rx);
        }

        // The block below moves `daemon_end` into its pumps, and `PipeFds` is
        // assembled after the spawn: the number is in reach only here.
        #[cfg(unix)]
        let mut channel_fd: Option<RawFd> = None;
        #[cfg(unix)]
        if spec.channel {
            command.env("SHEP_CHANNEL_FD", "3");
            // Not negotiation: an app cannot be asked what it speaks, but one
            // that wants to be defensive can check what it is given.
            command.env("SHEP_CHANNEL_VERSION", CHANNEL_VERSION);
            let (daemon_end, child_end) = UnixStream::pair().map_err(|error| {
                RunnerError::SpawnFailed(format!("shepherd channel socketpair: {error}"))
            })?;
            let std_child_end = child_end.into_std().map_err(|error| {
                RunnerError::SpawnFailed(format!("shepherd channel into_std: {error}"))
            })?;
            // `UnixStream::pair()` sets `O_NONBLOCK` on both ends for tokio's
            // half, and the child inherits it across the exec: a plain
            // `read <&3` would get `EAGAIN` rather than parking. The daemon's
            // own end is a separate descriptor and stays non-blocking.
            std_child_end.set_nonblocking(false).map_err(|error| {
                RunnerError::SpawnFailed(format!("shepherd channel set_nonblocking: {error}"))
            })?;
            let child_fd = OwnedFd::from(std_child_end);
            command
                .as_std_mut()
                .fd_mappings(vec![FdMapping {
                    parent_fd: child_fd,
                    child_fd: 3,
                }])
                .map_err(|error| {
                    RunnerError::SpawnFailed(format!("shepherd channel fd mapping: {error}"))
                })?;
            channel_fd = Some(daemon_end.as_raw_fd());
            spawn_channel_pumps(daemon_end, from_child_tx, to_child_rx);
        } else {
            // No channel: closed rather than dangling, so `from_child.recv()`
            // reports closed at once and a stray send fails fast.
            drop(from_child_tx);
            drop(to_child_rx);
        }

        let mut child = command
            .spawn()
            .map_err(|error| RunnerError::SpawnFailed(error.to_string()))?;
        // Closes the parent's copy of the fd-3 socketpair end here rather
        // than at the end of the scope, so the daemon's end sees a clean EOF.
        drop(command);

        let pid = child.id().ok_or_else(|| {
            RunnerError::SpawnFailed("child exited before its pid could be read".to_string())
        })?;

        // As early as containment can happen: the child exists and everything
        // it spawns from here inherits the job. Fatal on failure, because a
        // sheep outside its job is one `kill_tree` cannot reach and `shep
        // stop` would report success over a running process.
        #[cfg(windows)]
        let job = {
            let job = crate::sys_windows::Job::create().map_err(|error| {
                RunnerError::SpawnFailed(format!("job object for {}: {error}", spec.name))
            })?;
            let handle = child.raw_handle().ok_or_else(|| {
                RunnerError::SpawnFailed("child exited before it could be contained".to_string())
            })?;
            if let Err(error) = job.assign(handle) {
                // Running and in no job: nothing could stop its descendants
                // afterwards, so it must not be left behind.
                let _ = child.start_kill();
                return Err(RunnerError::SpawnFailed(format!(
                    "could not contain {} in a job object: {error}",
                    spec.name
                )));
            }
            job
        };

        let (logs_tx, logs_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (log_ctl_tx, log_ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
        // Before the `take`s below move the handles: the only place the
        // numbers a handover carries are known.
        #[cfg(unix)]
        let pipes = PipeFds {
            out: child.stdout.as_ref().map(AsRawFd::as_raw_fd),
            err: child.stderr.as_ref().map(AsRawFd::as_raw_fd),
            stdin: child.stdin.as_ref().map(AsRawFd::as_raw_fd),
            // Read further up, before the daemon end moved into its pumps.
            // Nothing on `child` names it: the child's side is fd 3.
            channel: channel_fd,
        };
        #[cfg(not(unix))]
        let pipes = PipeFds;
        spawn_log_pump(
            child.stdout.take(),
            child.stderr.take(),
            LogSink::Path(spec.out_file.clone()),
            LogSink::Path(spec.err_file.clone()),
            logs_tx,
            log_ctl_rx,
            pipes,
        );

        let (to_stdin_tx, to_stdin_rx) = mpsc::channel(CHANNEL_CAPACITY);
        if spec.stdin {
            spawn_stdin_pump(child.stdin.take(), to_stdin_rx);
        } else {
            // Dropped rather than dangling, so a caller's `is_closed()` says
            // "no pipe here" at once.
            drop(to_stdin_rx);
        }

        let io = ProcIo {
            logs: logs_rx,
            from_child: from_child_rx,
            to_child: to_child_tx,
            log_ctl: log_ctl_tx,
            to_stdin: to_stdin_tx,
        };
        Ok((
            TokioProc {
                pid,
                proc: Supervised::Spawned(child),
                #[cfg(windows)]
                job,
            },
            io,
        ))
    }
}
