//! Which processes a stop signal reaches: the leader alone for a sandboxed
//! sheep, the whole group otherwise, and `kill_tree` always the group.

use nix::errno::Errno;
use nix::sys::signal;
use nix::unistd::Pid;
use tokio::time::{sleep, timeout};

use super::super::TokioRunner;
use super::PUMP_DEADLINE;
use crate::runner::{ProcIo, ProcessRunner, RunningProcess, SpawnSpec, StopSignal};
use core::time::Duration;

/// Starts a leader that forks one `sleep` into its own group and reports the
/// sleeper's pid on stdout, then waits on it.
///
/// A real group of two is the point: the question is which of them a signal
/// lands on, and nothing in-memory has a group to address.
async fn leader_with_a_sleeper(
    dir: &tempfile::TempDir,
    leader_only_stop: bool,
) -> (<TokioRunner as ProcessRunner>::Proc, ProcIo, Pid) {
    let spec = SpawnSpec {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "sleep 60 & echo $!; wait".to_string()],
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        leader_only_stop,
        ..Default::default()
    };
    let (proc, mut io) = TokioRunner::new().spawn(&spec).unwrap();
    let line = timeout(PUMP_DEADLINE, io.logs.recv())
        .await
        .expect("the leader must report its sleeper")
        .expect("the log pump must not end first");
    let sleeper = Pid::from_raw(line.line.trim().parse().expect("the line is a pid"));
    (proc, io, sleeper)
}

/// Waits until `pid` is gone. Signal 0 still succeeds on a zombie, so this
/// waits for init to reap the orphan, which it does promptly.
async fn is_gone_within_deadline(pid: Pid) -> bool {
    timeout(PUMP_DEADLINE, async {
        while signal::kill(pid, None) != Err(Errno::ESRCH) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn a_leader_only_stop_signal_reaches_the_leader_and_not_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, _io, sleeper) = leader_with_a_sleeper(&dir, true).await;

    proc.signal(StopSignal::Term).unwrap();
    let outcome = timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the leader must die of SIGTERM");

    assert_eq!(outcome.signal, Some(StopSignal::Term.as_raw()));
    assert_eq!(
        signal::kill(sleeper, None),
        Ok(()),
        "the sleeper must survive a leader-only stop"
    );

    proc.kill_tree().unwrap();
    assert!(
        is_gone_within_deadline(sleeper).await,
        "SIGKILL still reaches the whole group"
    );
}

#[tokio::test]
async fn a_stop_signal_without_leader_only_reaches_the_whole_group() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, _io, sleeper) = leader_with_a_sleeper(&dir, false).await;

    proc.signal(StopSignal::Term).unwrap();
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the leader must die of SIGTERM");

    assert!(
        is_gone_within_deadline(sleeper).await,
        "a group stop signal must reach the sleeper too"
    );
}
