//! Adoption: the seam a successor reaches a still-running sheep
//! through.
//!
//! Spawned via `std::process::Command`, so tokio holds no `Child`; only
//! `AdoptedReaper`'s targeted `waitpid` collects the exit.

use super::*;

/// A child the way an adopted sheep reaches a successor: no `tokio::Child`
/// anywhere, so nothing but the reaper will ever wait on it.
fn adopted_child(script: &str) -> std::process::Child {
    std::process::Command::new("/bin/sh")
        .args(["-c", script])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn a shell")
}

/// The plainest adoption: a pid and its two log paths, no carried handles.
///
/// Every handle is `None` here, which is the shape a sheep whose log opens
/// had failed arrives in. The cases that are about handles fill them in.
fn adopt_spec(dir: &tempfile::TempDir, pid: u32, reaper: &Arc<AdoptedReaper>) -> AdoptSpec {
    AdoptSpec {
        name: "web".to_owned(),
        pid,
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        out_pipe: None,
        err_pipe: None,
        out_log: None,
        err_log: None,
        log_timestamps: true,
        stdin_pipe: None,
        channel: None,
        dog_link: None,
        dog_topics: None,
        reaper: Arc::clone(reaper),
    }
}

/// fails if an adopted sheep's real exit never reaches the supervisor.
///
/// Proves the reaper is reachable through `RunningProcess`, the type the
/// supervisor actually holds, so an adopted sheep's exit drives
/// autorestart and the EXIT column like a spawned one's does.
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the reaper collects these statuses; a Child::wait would take them first"
)]
async fn an_adopted_proc_reports_its_real_exit() {
    let dir = tempfile::tempdir().unwrap();
    let child = adopted_child("exit 3");
    let pid = child.id();
    let reaper = Arc::new(AdoptedReaper::new());

    let (mut proc, io) = TokioRunner::new()
        .adopt(adopt_spec(&dir, pid, &reaper))
        .expect("the real runner must be able to adopt");
    assert_eq!(
        proc.pid(),
        pid,
        "an adopted proc keeps the pid it was given"
    );

    let outcome = reaped(
        &mut proc,
        LONG_DEADLINE,
        "the adopted pid must be reaped within the budget",
    )
    .await;
    assert_eq!((outcome.code, outcome.signal), (Some(3), None));
    drop(io);
}

/// fails if the adopted arm disturbed the path every sheep takes.
///
/// `wait` now chooses between two sources of an exit; the spawned one is
/// the choice every running flock depends on.
#[tokio::test]
async fn a_spawned_proc_still_reports_its_real_exit() {
    let dir = tempfile::tempdir().unwrap();
    let runner = TokioRunner::new();
    let (mut proc, io) = runner
        .spawn(&spec_for(&dir, "/bin/sh", &["-c", "exit 4"]))
        .unwrap();

    let outcome = reaped(
        &mut proc,
        LONG_DEADLINE,
        "a spawned child must be waited within the budget",
    )
    .await;
    assert_eq!((outcome.code, outcome.signal), (Some(4), None));
    drop(io);
}

/// fails if an adopted sheep killed by a signal reports a code instead.
///
/// `output/rows.rs`'s `exit_cell` renders a code and a signal differently,
/// so collapsing the two makes the EXIT column lie about how a sheep died.
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the reaper collects these statuses; a Child::wait would take them first"
)]
async fn an_adopted_proc_reports_a_signal_as_a_signal() {
    let dir = tempfile::tempdir().unwrap();
    let child = adopted_child("sleep 30");
    let pid = child.id();
    let reaper = Arc::new(AdoptedReaper::new());

    let (mut proc, io) = TokioRunner::new()
        .adopt(adopt_spec(&dir, pid, &reaper))
        .expect("the real runner must be able to adopt");
    // Per-process rather than `kill_tree`: a child spawned here is in the
    // test binary's group, not its own, so there is no group to address.
    // `signal_process` targets the pid instead.
    proc.signal_process(OperatorSignal::Kill)
        .expect("SIGKILL the adopted sheep");

    let outcome = reaped(
        &mut proc,
        LONG_DEADLINE,
        "the adopted pid must be reaped within the budget",
    )
    .await;
    assert_eq!((outcome.code, outcome.signal), (None, Some(9)));
    drop(io);
}

/// fails if an adopted sheep's output stops reaching its log file.
///
/// The carried pipe read end must feed the same pump a spawn feeds, and
/// the carried log handle must be written through rather than reopened.
/// The line already in the file proves the second half: a handle that
/// lost `O_APPEND` would overwrite it.
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the reaper collects these statuses; a Child::wait would take them first"
)]
async fn an_adopted_pump_appends_the_carried_pipes_lines_through_the_carried_handle() {
    let dir = tempfile::tempdir().unwrap();
    let out_file = dir.path().join("out.log");
    fs::write(&out_file, "before-the-handover\n").unwrap();

    let mut child = adopted_child("echo after-the-handover");
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let carried_pipe = tokio::net::unix::pipe::Receiver::from_file(std::fs::File::from(
        std::os::fd::OwnedFd::from(stdout),
    ))
    .expect("a child's stdout is a readable pipe");
    let carried_log = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&out_file)
        .await
        .expect("open the log the predecessor was appending to");

    let reaper = Arc::new(AdoptedReaper::new());
    let mut spec = adopt_spec(&dir, pid, &reaper);
    spec.out_file = out_file.clone();
    spec.out_pipe = Some(carried_pipe);
    spec.out_log = Some(carried_log);

    let (mut proc, mut io) = TokioRunner::new()
        .adopt(spec)
        .expect("the real runner must be able to adopt");

    let line = recv_log(
        &mut io,
        LONG_DEADLINE,
        "the carried pipe must still be pumped",
    )
    .await;
    assert_eq!(line.line, "after-the-handover");
    await_file_contents(&out_file, "before-the-handover\nafter-the-handover\n").await;

    let outcome = reaped(
        &mut proc,
        LONG_DEADLINE,
        "the adopted pid must be reaped within the budget",
    )
    .await;
    assert_eq!(outcome.code, Some(0));
}

/// A `/bin/sh` child that holds the far end of a socketpair on fd 3 and
/// echoes each shepherd message back once, like [`channel_echo_script`]
/// for a spawned sheep.
///
/// Spawned with `command-fds`, not `TokioRunner::spawn`: the case below is
/// about the successor's half, so the daemon side of the socketpair has to be
/// one this test owns. A `std::process::Command`, so tokio holds no `Child`.
fn child_holding_a_channel(child_end: std::os::fd::OwnedFd, rounds: u32) -> std::process::Child {
    use command_fds::{CommandFdExt as _, FdMapping};

    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", &channel_echo_script(rounds)]);
    command
        .fd_mappings(vec![FdMapping {
            parent_fd: child_end,
            child_fd: 3,
        }])
        .expect("map the socketpair onto the child's fd 3");
    command.spawn().expect("spawn a shell holding fd 3")
}

/// fails if an adopted shepherd channel does not reach a real app blocked
/// on `read -r line <&3`.
///
/// Two things only a real process can check. The child is a separate open
/// file description, so `set_nonblocking(true)` on the daemon's end must not
/// reach it, or the child's `read` would get `EAGAIN` and die with status 3.
/// And the reply must come back through the reader task the adoption rebuilt.
#[tokio::test]
#[expect(
    clippy::zombie_processes,
    reason = "the reaper collects this status; a Child::wait would take it first"
)]
async fn an_adopted_channel_reaches_a_real_child_that_blocks_on_fd_3() {
    let dir = tempfile::tempdir().unwrap();
    let (daemon_end, child_end) = std::os::unix::net::UnixStream::pair().unwrap();
    // Cleared for the child's end exactly as the spawn path clears it: both
    // ends come back non-blocking from `pair()`, and a shell `read` on one
    // fails instead of parking.
    child_end.set_nonblocking(false).unwrap();
    let child = child_holding_a_channel(std::os::fd::OwnedFd::from(child_end), 1);
    let pid = child.id();

    daemon_end.set_nonblocking(true).unwrap();
    let daemon_end = tokio::net::UnixStream::from_std(daemon_end).unwrap();
    let reaper = Arc::new(AdoptedReaper::new());
    let mut spec = adopt_spec(&dir, pid, &reaper);
    spec.channel = Some(daemon_end);

    let (mut proc, mut io) = TokioRunner::new()
        .adopt(spec)
        .expect("the real runner must be able to adopt");

    io.to_child
        .send(ShepherdMessage::Action {
            name: "round-1".to_string(),
            params: None,
            id: 1,
        })
        .await
        .expect("an adopted sheep must still have a channel writer");
    let reply = tokio::time::timeout(CHANNEL_DEADLINE, io.from_child.recv())
        .await
        .expect("the child must answer over the adopted channel")
        .expect("the adopted reader must forward the reply");
    assert_eq!(
        reply,
        ChildMessage::ActionReply {
            action: "round-1".to_string(),
            body: "ok".to_string(),
            id: None,
        }
    );

    let outcome = reaped(
        &mut proc,
        REAP_DEADLINE,
        "the adopted pid must be reaped within the budget",
    )
    .await;
    assert_eq!(
        outcome.code,
        Some(0),
        "status 3 is the child's own `read` failing, which is a non-blocking fd 3"
    );
}
