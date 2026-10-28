//! Behavioral tests for [`shep_daemon::tokio_runner::TokioRunner`] against
//! real `/bin/sh` child processes.
//!
//! Runs on the real, unpaused clock, in a separate binary from the
//! paused-clock unit tests.
//!
//! The cases live one module per what it proves: `logs` the log plane,
//! `signals` the signal paths, `channel` fd 3, `identity` the credentials a
//! spawn asks the OS for, `environment` the `PATH` and stdin a child
//! inherits, `adoption` the seam a successor reaches a running sheep through.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use shep_core::signals::OperatorSignal;
use shep_daemon::channel::{ChildMessage, ShepherdMessage};
use shep_daemon::privilege::Credentials;
use shep_daemon::runner::{
    AdoptSpec, AdoptedReaper, ExitOutcome, LogLine, ProcIo, ProcessRunner, RunningProcess,
    SpawnSpec, StdinWrite, StopSignal,
};
use shep_daemon::tokio_runner::TokioRunner;

mod adoption;
mod channel;
mod dog_link;
mod environment;
mod identity;
mod logs;
mod signals;

pub(crate) use channel::*;

/// Builds a `/bin/sh -c <script>` spec writing logs into a fresh tempdir.
fn sh_spec(script: &str, channel: bool, out_file: PathBuf, err_file: PathBuf) -> SpawnSpec {
    SpawnSpec {
        name: "real-runner-test".to_string(),
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: None,
        env: BTreeMap::new(),
        out_file,
        err_file,
        log_timestamps: true,
        channel,
        stdin: false,
        credentials: None,
        dog_link: false,
    }
}

/// Builds a spec running `program args...` with logs under `dir`, for an
/// arbitrary program rather than `sh_spec`'s single shell script.
fn spec_for(dir: &tempfile::TempDir, program: &str, args: &[&str]) -> SpawnSpec {
    let (out_file, err_file) = (dir.path().join("out.log"), dir.path().join("err.log"));
    SpawnSpec {
        program: program.to_string(),
        args: args.iter().map(|s| (*s).to_string()).collect(),
        ..sh_spec("", false, out_file, err_file)
    }
}

/// How long a log line gets to travel from the pump's `write_all` to the
/// file: slack for a loaded runner, not an expected duration.
const LOG_WRITE_DEADLINE: Duration = Duration::from_secs(5);

/// Twice the other budgets in this tier, for the cases written with more
/// slack.
///
/// Nothing distinguishes those cases: the same [`reaped`] wait runs on
/// [`REAP_DEADLINE`] in some tests and on this in others. Both are named so a
/// slow-CI fix has one place to go.
const LONG_DEADLINE: Duration = Duration::from_secs(10);

/// The child's next log line, or a named panic for either way there is not
/// one: nothing arrives inside `deadline`, or the pump's channel closes.
///
/// `want` says what the caller was waiting for, and both panics carry it.
///
/// # Panics
/// If no line arrives within `deadline`, or the log channel closes first.
async fn recv_log(io: &mut ProcIo, deadline: Duration, want: &str) -> LogLine {
    tokio::time::timeout(deadline, io.logs.recv())
        .await
        .unwrap_or_else(|_| panic!("{want}: nothing arrived within {deadline:?}"))
        .unwrap_or_else(|| panic!("{want}: the log channel closed first"))
}

/// A log file's contents with the daemon's per-line timestamp stripped,
/// so an assertion is about what the sheep wrote.
///
/// A missing or unreadable file reads as empty, which is what a poll on
/// a file not yet created wants.
fn unstamped_file(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut out = String::new();
    for line in text.lines() {
        out.push_str(shep_core::logstamp::strip(line));
        out.push('\n');
    }
    out
}

/// Waits for `path` to hold exactly `expected`, failing at
/// [`LOG_WRITE_DEADLINE`].
///
/// Polls rather than sleeping a fixed guess, the same shape as
/// [`assert_reaped`]: bounded by what must eventually be true, not by a
/// guessed number.
async fn await_file_contents(path: &Path, expected: &str) {
    let settled = tokio::time::timeout(LOG_WRITE_DEADLINE, async {
        while unstamped_file(path) != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "{}: expected {expected:?}, found {:?}",
        path.display(),
        fs::read_to_string(path)
    );
}

/// Waits for `proc` to exit, failing at `deadline`, and hands back the outcome.
///
/// `why` and [`Location::caller`]'s line ride on the panic. `#[track_caller]` is
/// a no-op on an `async fn`, so this is a plain `fn` that reads the caller first.
///
/// # Panics
/// If `proc` has not exited within `deadline`.
#[track_caller]
fn reaped<'a, P: RunningProcess>(
    proc: &'a mut P,
    deadline: Duration,
    why: &'static str,
) -> impl Future<Output = ExitOutcome> + 'a {
    let site = Location::caller();
    async move {
        tokio::time::timeout(deadline, proc.wait())
            .await
            .unwrap_or_else(|_| panic!("{why} (at {site})"))
    }
}

/// How long [`assert_reaped`] waits for a pid to leave the process table:
/// slack for a loaded runner, not an expected duration.
const REAP_DEADLINE: Duration = Duration::from_secs(5);

/// Last-resort net: kills any process left alive when a test panics, so a
/// failing assertion never leaks a real process into the rest of the run.
///
/// Fires only while panicking. On the success path the test has already
/// proven these pids are gone, and signalling one the OS may have recycled
/// is a hazard. SIGKILLs the whole process group (`-pid`, not `pid`):
/// `TokioRunner` spawns every child as its own group leader.
struct Reaper(Vec<i32>);

impl Reaper {
    /// One spawned sheep's pid, the shape every spawn in this tier needs.
    fn of(proc: &impl RunningProcess) -> Self {
        Self(vec![i32::try_from(proc.pid()).unwrap()])
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        for &pid in &self.0 {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

/// Polls `kill(pid, None)` for `ESRCH` rather than sleeping a fixed guess.
/// `kill(pid, None)` still returns `Ok` for a zombie, so only `ESRCH`
/// proves the process is gone rather than exited-but-unreaped.
async fn assert_reaped(pid: i32, what: &str) {
    let reaped = tokio::time::timeout(REAP_DEADLINE, async {
        loop {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => break,
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await;
    assert!(reaped.is_ok(), "{what} (pid {pid}) is still alive");
}

/// How long one shepherd-channel exchange gets: slack for a loaded
/// runner, not an expected duration.
const CHANNEL_DEADLINE: Duration = Duration::from_secs(5);
