use super::fake_process::{FakeIo, FakeProc, ProcState};
use super::proc_script::ProcScript;
use crate::channel::{ChildMessage, ShepherdMessage};
use crate::privilege::Credentials;
use crate::runner::{LogCtl, Preflight, ProcIo, ProcessRunner, RunnerError, SpawnSpec, StdinWrite};
use core::fmt;
use shep_core::signals::OperatorSignal;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::{Duration, Instant};

/// Capacity of every channel the fake wires up: generous enough that no
/// test blocks on backpressure without meaning to.
const CHANNEL_CAPACITY: usize = 32;

/// Delay used by [`ProcScript::never_exits`], [`ProcScript::ignores_signals`]
/// and [`ProcScript::never_reports_its_exit`].
///
/// Large enough that no test's `tokio::time::advance` ever reaches it, small
/// enough (~30 days of milliseconds) to stay far under tokio's timer-wheel
/// range and never risk an `Instant + Duration` overflow.
pub(super) const NEVER_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// One spawn's shared state plus its still-unclaimed [`FakeIo`] test handles
struct SpawnedProc {
    state: Arc<ProcState>,
    io: Option<FakeIo>,
    /// The sheep name this spawn carried, copied off [`SpawnSpec::name`] and
    /// read back via [`ScriptedRunner::spawn_index_of`].
    ///
    /// Every other accessor here is indexed by spawn ORDER, which a test
    /// that starts several apps at once cannot predict without asserting on
    /// the very ordering it is trying to be independent of. This is how a
    /// case that picked one app by name (see
    /// [`ScriptedRunner::with_a_pump_that_never_reports`]) then reads that
    /// app's counters back.
    name: String,
    /// `false` once this spawn's log-control task has ended, read back via
    /// [`ScriptedRunner::log_ctl_live`].
    log_ctl_live: Arc<AtomicBool>,
    /// How many [`LogCtl::Reopen`] requests this spawn's log-control task has
    /// answered, read back via [`ScriptedRunner::reopens`].
    reopens: Arc<AtomicU32>,
    /// How many [`LogCtl::Flush`] requests it has answered, read back via
    /// [`ScriptedRunner::flushes`].
    flushes: Arc<AtomicU32>,
    /// How many [`LogCtl::Resume`] requests it has been sent, read back via
    /// [`ScriptedRunner::resumes`].
    #[cfg(unix)]
    resumes: Arc<AtomicU32>,
    /// Every line written to this spawn's stdin, in write order, read back
    /// via [`ScriptedRunner::stdin_lines`].
    stdin_lines: Arc<Mutex<Vec<String>>>,
    /// The identity this spawn was asked to run under, copied off
    /// [`SpawnSpec::credentials`], read back via
    /// [`ScriptedRunner::spawned_as`].
    ///
    /// The fake runs no program, so it cannot become anyone; recording what
    /// it was asked for is the only way a supervisor-tier test can assert
    /// the identity a spawn actually carried rather than merely that a
    /// spawn happened.
    credentials: Option<Credentials>,
}

/// The pid [`ScriptedRunner`] gives the first proc it spawns; each later
/// spawn gets the next number up.
///
/// Named so a fixture that has to describe that proc's process table (a
/// scripted memory sampler, say) can say which pid it means instead of
/// repeating a literal this file is free to change.
pub const FIRST_SCRIPTED_PID: u32 = 1000;

/// Deterministic fake [`ProcessRunner`] driven by a pre-scripted [`ProcScript`] per spawn.
pub struct ScriptedRunner {
    scripts: Mutex<VecDeque<ProcScript>>,
    /// The pid every spawn reports, when [`ScriptedRunner::spawning_at`] set
    /// one; otherwise pids come from the spawn index.
    pid: Mutex<Option<u32>>,
    /// Sheep names whose [`ProcessRunner::spawn`] fails, by name because
    /// that is what a caller has.
    ///
    /// Sibling to [`Self::refuse`] and needed for the same class of reason:
    /// scripts are consumed in spawn order, so a test cannot make one
    /// particular app of several fail by arranging the script list. Reaching
    /// `do_start`'s per-app failure handling needs a failure that lands on a
    /// named app while its neighbours succeed.
    ///
    /// Checked before a script is popped, so a sheep named here consumes
    /// nothing and the apps around it still get the scripts they were meant
    /// to have.
    fail_spawn: Mutex<Vec<String>>,
    /// Sheep names whose [`ProcessRunner::preflight`] answers
    /// [`Preflight::Impossible`], by name because that is what a caller has.
    ///
    /// Empty by default, which is this fake's whole point: it reads nothing
    /// from a spec and so answers [`Preflight::Unknown`] for everything, the
    /// same as the trait's own default. The supervisor's validating pass is
    /// otherwise unreachable from a unit test, and a test written against
    /// the default cannot fail no matter what that pass does.
    refuse: Mutex<Vec<String>>,
    /// Sheep names whose log-control task accepts a [`LogCtl::ReportFds`]
    /// and never answers it, by name for the same reason as its two
    /// siblings above: a case needs one named app of several to go silent
    /// while its neighbours answer, and script order cannot say that.
    ///
    /// Models a pump wedged on a filesystem that has stopped completing
    /// writes: the request is delivered, only the acknowledgement never
    /// comes. Only `ReportFds` goes unanswered; the task stays live and
    /// still serves [`LogCtl::Resume`], which lets a case tell "never
    /// parked" apart from "gone".
    #[cfg(unix)]
    deaf_pump: Mutex<Vec<String>>,
    /// State + IO for every spawn, indexed by spawn order, behind ONE lock.
    /// Two separate `Mutex`es here (one for state, one for IO) would let
    /// concurrent spawns interleave their critical sections and desync a
    /// proc's state from its `FakeIo` at the same index; one lock makes
    /// "assign the next index, push both pieces" atomic.
    spawned: Mutex<Vec<SpawnedProc>>,
}

impl fmt::Debug for ScriptedRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedRunner").finish_non_exhaustive()
    }
}

/// Six real, distinct, open descriptors for a fake pump to report, and the
/// handles that keep them open.
///
/// `/dev/null`, since the fake never writes to them: a handover blob only
/// needs the numbers to name something open. Six covers the widest shape a
/// blob carries, stdin pipe and shepherd channel included.
///
/// # Panics
///
/// If `/dev/null` cannot be opened six times: the process is out of
/// descriptors.
#[cfg(unix)]
#[track_caller]
fn open_reportable_fds() -> ([std::fs::File; 6], crate::handover::CarriedFds) {
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
    };
    (files, fds)
}

impl ScriptedRunner {
    /// Builds a runner that hands out `scripts` to spawns in order
    #[must_use]
    pub fn new(scripts: Vec<ProcScript>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            spawned: Mutex::new(Vec::new()),
            refuse: Mutex::new(Vec::new()),
            fail_spawn: Mutex::new(Vec::new()),
            #[cfg(unix)]
            deaf_pump: Mutex::new(Vec::new()),
            pid: Mutex::new(None),
        }
    }

    /// Makes every spawn report `pid` instead of [`FIRST_SCRIPTED_PID`] and
    /// the numbers above it.
    ///
    /// For the one fixture shape the default cannot express: a scripted
    /// process that also opens a real socket to the daemon, so peer
    /// credentials name the same pid the caller passes as
    /// `std::process::id()`.
    #[must_use]
    pub fn spawning_at(self, pid: u32) -> Self {
        *self.pid.lock().unwrap() = Some(pid);
        self
    }

    /// Makes these sheep's pumps accept a [`LogCtl::ReportFds`] and never
    /// answer it.
    ///
    /// For reaching the handover's own deadline, which nothing else in this
    /// fake can arm: every other pump here answers instantly, so a snapshot
    /// taken over them can never be the one that waits.
    #[cfg(unix)]
    #[must_use]
    pub fn with_a_pump_that_never_reports(self, names: &[&str]) -> Self {
        *self.deaf_pump.lock().unwrap() = names.iter().map(|n| (*n).to_string()).collect();
        self
    }

    /// The spawn index the sheep named `name` was given, or `None` if this
    /// runner never spawned it.
    ///
    /// Every counter here is indexed by spawn order, and a test that starts
    /// several apps in one call would otherwise have to assume which order
    /// the supervisor spawned them in to read one app's counter back.
    #[must_use]
    pub fn spawn_index_of(&self, name: &str) -> Option<usize> {
        self.spawned
            .lock()
            .unwrap()
            .iter()
            .position(|p| p.name == name)
    }

    /// Makes `spawn` fail for these sheep, without consuming a script.
    ///
    /// For reaching `do_start`'s per-app failure handling, which needs one
    /// named app of several to fail while the others come up. Script order
    /// alone cannot express that.
    #[must_use]
    pub fn failing_to_spawn(self, names: &[&str]) -> Self {
        *self.fail_spawn.lock().unwrap() = names.iter().map(|n| (*n).to_string()).collect();
        self
    }

    /// Makes `preflight` answer [`Preflight::Impossible`] for these sheep.
    ///
    /// For reaching the supervisor's pre-registration validating pass, which
    /// no other fake behaviour can enter.
    #[must_use]
    pub fn refusing(self, names: &[&str]) -> Self {
        *self.refuse.lock().unwrap() = names.iter().map(|n| (*n).to_string()).collect();
        self
    }

    /// `kill_tree()` call count per proc, indexed by spawn order: the only
    /// kill-assertion accessor.
    #[must_use]
    pub fn kill_counts(&self) -> Vec<u32> {
        self.spawned
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.state.kill_count.load(Ordering::SeqCst))
            .collect()
    }

    /// The credentials the spawn at `spawn_index` was asked to apply, as
    /// they stood on its [`SpawnSpec`].
    ///
    /// `None` means the spawn carried no credentials at all, which is the
    /// child running as the shepherd. A test about a privilege drop must
    /// assert on this rather than on the spawn's existence: a downgraded
    /// spawn and a correct one are alike in every other way the fake can
    /// report.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[must_use]
    #[track_caller]
    pub fn spawned_as(&self, spawn_index: usize) -> Option<Credentials> {
        self.spawned.lock().unwrap()[spawn_index].credentials
    }

    /// How many spawns this runner has been asked for.
    ///
    /// A refused spawn that never reached the runner does not appear here,
    /// which is what lets a test say "nothing was started" rather than
    /// "nothing came up".
    #[must_use]
    pub fn spawn_count(&self) -> usize {
        self.spawned.lock().unwrap().len()
    }

    /// Every raw signal number an explicit `signal()` call has recorded for
    /// the proc spawned at `spawn_index`, in call order. A `Shutdown`
    /// message never appears here, only real `signal()` calls do.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[must_use]
    #[track_caller]
    pub fn signals(&self, spawn_index: usize) -> Vec<i32> {
        self.spawned.lock().unwrap()[spawn_index]
            .state
            .signals
            .lock()
            .unwrap()
            .clone()
    }

    /// Every [`OperatorSignal`] a `signal_process` call has recorded for the
    /// proc spawned at `spawn_index`, in call order.
    ///
    /// Separate from [`Self::signals`]'s group-wide deliveries: which of the
    /// two the supervisor called is what a `shep signal` test asks.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[must_use]
    #[track_caller]
    pub fn process_signals(&self, spawn_index: usize) -> Vec<OperatorSignal> {
        self.spawned.lock().unwrap()[spawn_index]
            .state
            .process_signals
            .lock()
            .unwrap()
            .clone()
    }

    /// Whether the log-control task for the proc spawned at `spawn_index` is
    /// still running.
    ///
    /// It runs while something holds a [`ProcIo::log_ctl`](crate::runner::ProcIo::log_ctl) sender, something
    /// holds the [`ProcIo::logs`](crate::runner::ProcIo::logs) receiver, and the proc has not exited,
    /// unless a lamb holds the pipe
    /// ([`ProcScript::with_a_lamb_holding_the_pipe`]).
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[must_use]
    #[track_caller]
    pub fn log_ctl_live(&self, spawn_index: usize) -> bool {
        self.spawned.lock().unwrap()[spawn_index]
            .log_ctl_live
            .load(Ordering::SeqCst)
    }

    /// How many [`LogCtl::Reopen`] requests the proc spawned at `spawn_index`
    /// has been sent.
    ///
    /// The fake writes no files, so this proves only that the request
    /// reached this spawn's end of [`ProcIo::log_ctl`](crate::runner::ProcIo::log_ctl). Whether a reopened
    /// handle lands on a recreated inode is `tests/real_runner.rs`'s question.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[must_use]
    #[track_caller]
    pub fn reopens(&self, spawn_index: usize) -> u32 {
        self.spawned.lock().unwrap()[spawn_index]
            .reopens
            .load(Ordering::SeqCst)
    }

    /// How many [`LogCtl::Flush`] requests the proc spawned at `spawn_index`
    /// has been sent.
    ///
    /// A separate counter from [`Self::reopens`], not one shared "control
    /// requests" total, so a `flush` wired to the wrong variant still moves
    /// the wrong counter. The fake writes no files, so this proves only
    /// that the request reached this spawn's end of [`ProcIo::log_ctl`](crate::runner::ProcIo::log_ctl).
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[must_use]
    #[track_caller]
    pub fn flushes(&self, spawn_index: usize) -> u32 {
        self.spawned.lock().unwrap()[spawn_index]
            .flushes
            .load(Ordering::SeqCst)
    }

    /// How many [`LogCtl::Resume`] requests the proc spawned at
    /// `spawn_index` has been sent.
    ///
    /// A handover that reported and then refused owes one of these to
    /// every pump it reported to, or that sheep's log stops for good.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[cfg(unix)]
    #[must_use]
    #[track_caller]
    pub fn resumes(&self, spawn_index: usize) -> u32 {
        self.spawned.lock().unwrap()[spawn_index]
            .resumes
            .load(Ordering::SeqCst)
    }

    /// Every line the daemon has written to the stdin of the proc spawned at
    /// `spawn_index`, in write order.
    ///
    /// Proves only that the line reached [`ProcIo::to_stdin`](crate::runner::ProcIo::to_stdin) intact; a
    /// real `\n`-terminated line landing in a real fd 0 is
    /// `tests/real_runner.rs`'s question.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range.
    #[must_use]
    #[track_caller]
    pub fn stdin_lines(&self, spawn_index: usize) -> Vec<String> {
        self.spawned.lock().unwrap()[spawn_index]
            .stdin_lines
            .lock()
            .unwrap()
            .clone()
    }

    /// Takes the [`FakeIo`] test-side handles for the proc spawned at `spawn_index`
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range or its `FakeIo` was already taken.
    #[must_use]
    #[track_caller]
    pub fn io_handles(&self, spawn_index: usize) -> FakeIo {
        self.spawned
            .lock()
            .unwrap()
            .get_mut(spawn_index)
            .and_then(|p| p.io.take())
            .expect("io_handles: no unclaimed IO bundle at this spawn index")
    }
}

impl ProcessRunner for ScriptedRunner {
    type Proc = FakeProc;

    /// [`Preflight::Impossible`] for a name given to [`Self::refusing`],
    /// [`Preflight::Unknown`] for everything else.
    fn preflight(&self, spec: &SpawnSpec) -> Preflight {
        if self.refuse.lock().unwrap().contains(&spec.name) {
            return Preflight::Impossible(format!("no such file: {}", spec.program));
        }
        Preflight::Unknown
    }

    /// `program`, `args`, `env`, `cwd`, `out_file` and `err_file` are read
    /// by nothing here: the fake runs no program and writes no files, which
    /// keeps it deterministic and instant under the paused clock.
    ///
    /// `name` matches [`ScriptedRunner::failing_to_spawn`]; `channel` and
    /// `stdin` gate behavior below, since `begin_action` treats them as
    /// load-bearing; `credentials` is recorded, never applied, since the
    /// fake drops no privilege ([`ScriptedRunner::spawned_as`]).
    ///
    /// Real fd-3 delivery, refusal and timeout are proven against
    /// [`crate::tokio_runner::TokioRunner`] in `tests/daemon_e2e.rs`
    /// instead, since nothing here is a real socketpair to a real child.
    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        // Before the script is popped, so a sheep named to `failing_to_spawn`
        // leaves the list alone for the apps around it.
        if self.fail_spawn.lock().unwrap().contains(&spec.name) {
            return Err(RunnerError::SpawnFailed(
                "No such file or directory (os error 2)".to_string(),
            ));
        }
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| RunnerError::SpawnFailed("script exhausted".to_string()))?;

        let (exited_tx, mut exited_rx) = watch::channel(false);
        let state = Arc::new(ProcState {
            exit_deadline: Instant::now() + Duration::from_millis(script.delay_ms),
            outcome: script.outcome,
            obeys_signal: script.obeys_signal,
            obeys_kill: script.obeys_kill,
            signal_notify: Notify::new(),
            pending_signal: Mutex::new(None),
            signals: Mutex::new(Vec::new()),
            process_signals: Mutex::new(Vec::new()),
            kill_notify: Notify::new(),
            kill_count: AtomicU32::new(0),
            resolved: Mutex::new(None),
            exited: exited_tx,
        });

        let (logs_tx, logs_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (from_child_tx, from_child_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (to_child_tx, raw_to_child_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (relay_tx, relay_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (log_ctl_tx, mut log_ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);

        // The fake writes no files, so there is nothing to reopen; what it
        // must do is answer every request like a live pump, and stop
        // answering once the proc exits (unless a lamb holds the pipe), so
        // that branch stays reachable to `tests/real_runner.rs`.
        let log_ctl_live = Arc::new(AtomicBool::new(true));
        let ctl_live = Arc::clone(&log_ctl_live);
        let reopens = Arc::new(AtomicU32::new(0));
        let reopen_count = Arc::clone(&reopens);
        let flushes = Arc::new(AtomicU32::new(0));
        let flush_count = Arc::clone(&flushes);
        #[cfg(unix)]
        let resumes = Arc::new(AtomicU32::new(0));
        #[cfg(unix)]
        let resume_count = Arc::clone(&resumes);
        let lamb_holds_the_pipe = script.lamb_holds_the_pipe;
        let logs_for_pump = logs_tx.clone();
        #[cfg(unix)]
        let deaf = self.deaf_pump.lock().unwrap().contains(&spec.name);
        tokio::spawn(async move {
            #[cfg(unix)]
            let mut reportable_fds: Option<(
                [std::fs::File; 6],
                crate::handover::CarriedFds,
            )> = None;
            // Every unanswered report, kept alive rather than dropped: a
            // deaf pump answers no caller while it lives. When it ends the
            // senders drop and each waiter gets an acknowledgement error.
            #[cfg(unix)]
            let mut held_reports: Vec<
                tokio::sync::oneshot::Sender<crate::handover::CarriedFds>,
            > = Vec::new();
            loop {
                tokio::select! {
                    ctl = log_ctl_rx.recv() => match ctl {
                        // Both arms count BEFORE they answer, so a test that
                        // observes the acknowledgement can read the count
                        // without racing this task.
                        Some(LogCtl::Reopen { done }) => {
                            reopen_count.fetch_add(1, Ordering::SeqCst);
                            // Always `Ok`: the fake has no open that could
                            // fail. A pump that cannot reopen is tested
                            // against `supervisor`'s `FailingPumpRunner`.
                            let _ = done.send(Ok(()));
                        }
                        Some(LogCtl::Flush { done }) => {
                            flush_count.fetch_add(1, Ordering::SeqCst);
                            // Always `Ok`, for the same reason: with no
                            // handle there is nothing queued that could
                            // fail to land.
                            let _ = done.send(Ok(()));
                        }
                        // Six `/dev/null` handles stand in for real
                        // descriptors, opened once. A deaf pump holds
                        // `done` instead of answering.
                        #[cfg(unix)]
                        Some(LogCtl::ReportFds { done }) => {
                            if deaf {
                                held_reports.push(done);
                            } else {
                                let fds = reportable_fds.get_or_insert_with(open_reportable_fds);
                                let _ = done.send(fds.1);
                            }
                        }
                        // Counted rather than acted on: the fake has no
                        // streams to resume reading. Asserts only that an
                        // abandoned handover sent this.
                        #[cfg(unix)]
                        Some(LogCtl::Resume) => {
                            resume_count.fetch_add(1, Ordering::SeqCst);
                        }
                        None => break, // nothing holds ProcIo::log_ctl
                    },
                    // The owner dropped ProcIo::logs. A real pump ends on
                    // this whether or not a line is flowing, and it is the
                    // only thing that ends one whose streams a lamb is
                    // holding open.
                    () = logs_for_pump.closed() => break,
                    // Resolves when `wait()` latches an outcome, and errors
                    // once nothing holds this proc's state any more. Either
                    // way the proc is over, which reaches the pump as both
                    // streams hitting EOF, unless a lamb inherited them.
                    _ = exited_rx.changed(), if !lamb_holds_the_pipe => break,
                }
            }
            // Stored before this task returns, so the flag is already false
            // by the time `log_ctl_rx` drops and closes the channel: a test
            // that waits on the close never then reads a stale `true`.
            ctl_live.store(false, Ordering::SeqCst);
        });

        // Watches its own to_child stream: a `Shutdown` message resolves an
        // obeys_signal wait via `record_shutdown`, not `record_signal`, so
        // it never appears in `ScriptedRunner::signals`. Gated on
        // `spec.channel`, mirroring the real runner's `false` branch.
        let (fake_from_child_tx, fake_to_child_rx) = if spec.channel {
            let relay_state = Arc::clone(&state);
            tokio::spawn(async move {
                let mut raw_rx = raw_to_child_rx;
                while let Some(msg) = raw_rx.recv().await {
                    if msg == ShepherdMessage::Shutdown {
                        relay_state.record_shutdown();
                    }
                    if relay_tx.send(msg).await.is_err() {
                        break; // observer dropped FakeIo::to_child_rx; stop relaying
                    }
                }
            });
            (from_child_tx, relay_rx)
        } else {
            drop(from_child_tx);
            drop(raw_to_child_rx);
            drop(relay_tx);
            let (stand_in_tx, stand_in_rx) = mpsc::channel::<ChildMessage>(1);
            drop(stand_in_rx);
            let (dead_tx, dead_rx) = mpsc::channel::<ShepherdMessage>(1);
            drop(dead_tx);
            (stand_in_tx, dead_rx)
        };

        // Gated on `spec.stdin`, mirroring `spec.channel` above: `false`
        // drops the receiver here, exactly as the real runner does, so
        // `to_stdin.is_closed()` answers immediately.
        let (to_stdin_tx, to_stdin_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let stdin_lines = Arc::new(Mutex::new(Vec::new()));
        if spec.stdin {
            let reads_stdin = script.reads_stdin;
            let lines = Arc::clone(&stdin_lines);
            let mut rx = to_stdin_rx;
            tokio::spawn(async move {
                // Withheld acknowledgements are held, not dropped, so an
                // awaiting caller hangs (the shape a real full pipe leaves)
                // instead of seeing a spurious "channel closed" error.
                let mut withheld = Vec::new();
                while let Some(StdinWrite { line, done }) = rx.recv().await {
                    // Recorded either way: `never_reads_its_stdin` withholds
                    // the acknowledgement, not the record, so the line is
                    // kept while the caller stays pending.
                    lines.lock().unwrap().push(line);
                    if reads_stdin {
                        let _ = done.send(Ok(()));
                    } else {
                        withheld.push(done);
                    }
                }
            });
        } else {
            drop(to_stdin_rx);
        }

        let mut spawned = self.spawned.lock().unwrap();
        let index = spawned.len();
        spawned.push(SpawnedProc {
            state: Arc::clone(&state),
            io: Some(FakeIo {
                logs_tx,
                from_child_tx: fake_from_child_tx,
                to_child_rx: fake_to_child_rx,
            }),
            name: spec.name.clone(),
            log_ctl_live,
            reopens,
            flushes,
            #[cfg(unix)]
            resumes,
            stdin_lines,
            credentials: spec.credentials,
        });
        drop(spawned);

        let proc_io = ProcIo {
            logs: logs_rx,
            from_child: from_child_rx,
            to_child: to_child_tx,
            log_ctl: log_ctl_tx,
            to_stdin: to_stdin_tx,
        };
        // Arbitrary but deterministic: real pids come from the OS in the real runner.
        let pid = self
            .pid
            .lock()
            .unwrap()
            .unwrap_or_else(|| FIRST_SCRIPTED_PID + u32::try_from(index).unwrap_or(u32::MAX));
        Ok((FakeProc { pid, state }, proc_io))
    }
}

#[cfg(test)]
mod tests {
    use super::super::proc_script::ProcScript;

    use crate::channel::{ChildMessage, ShepherdMessage};
    use tokio::time::Duration;

    use crate::runner::{
        ExitOutcome, LogCtl, ProcessRunner, RunnerError, RunningProcess, SpawnSpec, StopSignal,
    };

    use super::super::testing::*;
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn const_exit_resolves_immediately_with_code() {
        let runner = ScriptedRunner::new(vec![ProcScript::const_exit(0)]);
        let (mut proc, _io) = runner.spawn(&spec()).unwrap();
        let outcome = proc.wait().await;
        assert_eq!(
            outcome,
            ExitOutcome {
                code: Some(0),
                signal: None
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stable_then_exit_resolves_after_advancing_its_delay() {
        let runner = ScriptedRunner::new(vec![ProcScript::stable_then_exit(5_000, 0)]);
        let (mut proc, _io) = runner.spawn(&spec()).unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        let outcome = proc.wait().await;
        assert_eq!(
            outcome,
            ExitOutcome {
                code: Some(0),
                signal: None
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn signal_before_wait_resolves_with_raw_signal_number() {
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (mut proc, _io) = runner.spawn(&spec()).unwrap();
        proc.signal(StopSignal::Int).unwrap();
        let outcome = proc.wait().await;
        assert_eq!(
            outcome,
            ExitOutcome {
                code: None,
                signal: Some(StopSignal::Int.as_raw())
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn signal_during_pending_wait_resolves() {
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (proc, _io) = runner.spawn(&spec()).unwrap();
        let mut waiter = proc.clone();
        let mut signaler = proc;
        let handle = tokio::spawn(async move { waiter.wait().await });
        tokio::time::advance(Duration::from_millis(1)).await;
        signaler.signal(StopSignal::Term).unwrap();
        let outcome = handle.await.unwrap();
        assert_eq!(
            outcome,
            ExitOutcome {
                code: None,
                signal: Some(StopSignal::Term.as_raw())
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn kill_tree_resolves_pending_wait_and_counts() {
        let runner = ScriptedRunner::new(vec![ProcScript::ignores_signals()]);
        let (proc, _io) = runner.spawn(&spec()).unwrap();
        let mut waiter = proc.clone();
        let mut killer = proc;
        let handle = tokio::spawn(async move { waiter.wait().await });
        tokio::time::advance(Duration::from_millis(1)).await;
        killer.kill_tree().unwrap();
        let outcome = handle.await.unwrap();
        assert_eq!(
            outcome,
            ExitOutcome {
                code: None,
                signal: Some(9)
            }
        );
        assert_eq!(runner.kill_counts(), vec![1]);
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_script_errors_on_spawn() {
        let runner = ScriptedRunner::new(vec![]);
        let err = runner.spawn(&spec()).unwrap_err();
        assert_eq!(
            err,
            RunnerError::SpawnFailed("script exhausted".to_string())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_message_resolves_wait_and_is_observable() {
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (mut proc, io) = runner.spawn(&spec()).unwrap();
        let mut fake_io = runner.io_handles(0);

        io.to_child.send(ShepherdMessage::Shutdown).await.unwrap();

        let outcome = proc.wait().await;
        assert_eq!(
            outcome,
            ExitOutcome {
                code: None,
                signal: Some(StopSignal::Term.as_raw())
            }
        );
        // A Shutdown message is not an explicit signal() call (IMPORTANT-3): it
        // resolves the wait via record_shutdown, which never touches `signals`.
        assert!(runner.signals(0).is_empty());

        let observed = fake_io.to_child_rx.recv().await.unwrap();
        assert_eq!(observed, ShepherdMessage::Shutdown);
    }

    #[tokio::test(start_paused = true)]
    async fn io_handles_relay_both_directions() {
        let runner = ScriptedRunner::new(vec![ProcScript::const_exit(0)]);
        let (_proc, mut io) = runner.spawn(&spec()).unwrap();
        let mut fake_io = runner.io_handles(0);

        fake_io
            .from_child_tx
            .send(ChildMessage::Ready)
            .await
            .unwrap();
        let received = io.from_child.recv().await.unwrap();
        assert_eq!(received, ChildMessage::Ready);

        let sent = ShepherdMessage::Action {
            name: "gc".to_string(),
            params: None,
            id: 0,
        };
        io.to_child.send(sent.clone()).await.unwrap();
        let observed = fake_io.to_child_rx.recv().await.unwrap();
        assert_eq!(observed, sent);
    }

    /// Fails if the fake drops its control receiver instead of answering:
    /// the send fails, or the acknowledgement resolves `Err` because the
    /// `oneshot` sender was dropped unanswered. Either way a caller that
    /// awaits a reopen would hang against every scripted proc.
    ///
    /// This tier proves the acknowledgement and nothing else: the fake
    /// writes no files, so it has no handle to swap. What a reopened
    /// handle does to a real inode is `tokio_runner`'s own tests.
    #[tokio::test(start_paused = true)]
    async fn a_reopen_is_acknowledged_by_the_scripted_runner() {
        // One script for one spawn: a second would answer
        // `SpawnFailed("script exhausted")`.
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (_proc, io) = runner.spawn(&spec()).unwrap();

        let (done, ack) = tokio::sync::oneshot::channel();
        io.log_ctl.send(LogCtl::Reopen { done }).await.unwrap();

        // Bounded rather than a bare await: an unanswered reopen must fail
        // this test, not hang it. Under the paused clock the deadline fires
        // as soon as the runtime is idle, so a passing run costs nothing.
        let outcome = tokio::time::timeout(Duration::from_secs(5), ack)
            .await
            .expect("a reopen must be acknowledged")
            .expect("the fake must answer rather than drop the acknowledgement");
        assert_eq!(
            outcome,
            Ok(()),
            "a fake with no files to open has nothing that can fail"
        );
    }

    /// Fails if the fake's control task outlives its proc: `log_ctl.send`
    /// would keep succeeding against a proc that exited long ago, and the
    /// "the pump is already gone" branch every caller of
    /// [`ProcIo::log_ctl`](crate::runner::ProcIo::log_ctl) is told to take would be unreachable from this
    /// tier. A test for reopening a stopped sheep would then pass whether or
    /// not the code handled one.
    #[tokio::test(start_paused = true)]
    async fn a_proc_that_has_exited_closes_its_control_channel() {
        // One script for one spawn: a second would answer
        // `SpawnFailed("script exhausted")` and never reach the channel.
        let runner = ScriptedRunner::new(vec![ProcScript::const_exit(0)]);
        let (mut proc, io) = runner.spawn(&spec()).unwrap();
        assert!(runner.log_ctl_live(0), "sanity: live before the proc exits");

        proc.wait().await;

        // Bounded rather than an immediate assertion: the control task ends
        // on its own schedule, so this waits for the close instead of
        // requiring it to have already happened.
        tokio::time::timeout(Duration::from_secs(5), io.log_ctl.closed())
            .await
            .expect("a proc that has exited must close its control channel");
        assert!(!runner.log_ctl_live(0));

        let (done, ack) = tokio::sync::oneshot::channel();
        assert!(
            io.log_ctl.send(LogCtl::Reopen { done }).await.is_err(),
            "a reopen aimed at an exited proc must fail to send"
        );
        // A request that never reaches a pump resolves the caller's
        // acknowledgement as an error rather than leaving it pending.
        assert!(ack.await.is_err());
    }

    /// A fake that accepted every write would make `no_stdin` unreachable
    /// from the engine tier, the same trap `spec.channel` had to be taught.
    #[tokio::test(start_paused = true)]
    async fn a_spawn_without_stdin_hands_back_a_closed_writer() {
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (_proc, io) = runner
            .spawn(&SpawnSpec {
                stdin: false,
                ..spec()
            })
            .unwrap();
        assert!(io.to_stdin.is_closed());
    }

    /// The counterpart: a fake that closed every writer would make the
    /// case above pass for the wrong reason.
    #[tokio::test(start_paused = true)]
    async fn a_spawn_with_stdin_hands_back_a_live_writer() {
        let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
        let (_proc, io) = runner
            .spawn(&SpawnSpec {
                stdin: true,
                ..spec()
            })
            .unwrap();
        assert!(!io.to_stdin.is_closed());
    }
}
