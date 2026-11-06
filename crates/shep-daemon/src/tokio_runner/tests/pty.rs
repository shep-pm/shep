//! A `pty = true` sheep on a real pseudo-terminal: what the child finds on
//! its descriptors, and what reaches the log.

use nix::unistd::{Pid, getpgid, getsid};
use tokio::time::timeout;

use super::super::TokioRunner;
use super::PUMP_DEADLINE;
use super::stop_signal::is_gone_within_deadline;
use crate::runner::{ProcIo, ProcessRunner, RunnerError, RunningProcess, SpawnSpec, StdinWrite};
use tokio::sync::oneshot;

/// A spec that runs `script` under `/bin/sh` on a terminal.
fn pty_spec(dir: &tempfile::TempDir, script: &str) -> SpawnSpec {
    SpawnSpec {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        pty: true,
        ..Default::default()
    }
}

/// Every line `script` prints, up to the end of its terminal.
async fn printed_by(script: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = TokioRunner::new().spawn(&pty_spec(&dir, script)).unwrap();
    let mut lines = Vec::new();
    timeout(PUMP_DEADLINE, async {
        while let Some(log) = io.logs.recv().await {
            assert!(!log.err, "a terminal has one stream, reported as stdout");
            lines.push(log.line);
        }
    })
    .await
    .expect("the log pump must end when the child's terminal closes");
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit");
    lines
}

/// A sheep that stays up, for a case that inspects it from outside.
fn a_sheep_that_stays_up(
    dir: &tempfile::TempDir,
) -> (<TokioRunner as ProcessRunner>::Proc, ProcIo) {
    TokioRunner::new()
        .spawn(&pty_spec(dir, "exec /bin/sleep 60"))
        .unwrap()
}

#[tokio::test]
async fn a_pty_sheep_sees_a_terminal_on_fd_0_1_and_2() {
    let lines = printed_by(
        "for fd in 0 1 2; do if [ -t $fd ]; then echo \"$fd tty\"; else echo \"$fd not\"; fi; done",
    )
    .await;

    assert_eq!(lines, ["0 tty", "1 tty", "2 tty"]);
}

#[tokio::test]
async fn a_pty_sheep_has_the_pty_as_its_controlling_terminal() {
    // Opening `/dev/tty` fails with `ENXIO` for a process with none.
    let lines =
        printed_by("if /bin/sh -c ': </dev/tty' 2>/dev/null; then echo has; else echo none; fi")
            .await;

    assert_eq!(lines, ["has"]);
}

#[tokio::test]
async fn a_pty_sheep_is_its_own_process_group_leader() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, _io) = a_sheep_that_stays_up(&dir);
    let pid = Pid::from_raw(i32::try_from(proc.pid()).unwrap());

    assert_eq!(getpgid(Some(pid)), Ok(pid));
    assert_eq!(getsid(Some(pid)), Ok(pid));

    proc.kill_tree().unwrap();
}

#[tokio::test]
async fn a_pty_sheeps_window_is_200_by_50() {
    let lines = printed_by("/bin/stty size").await;

    assert_eq!(lines, ["50 200"]);
}

#[tokio::test]
async fn a_pty_sheep_logs_colour_codes_as_plain_text() {
    let lines = printed_by(r"printf '\033[1;31mred\033[0m and plain\n'").await;

    assert_eq!(lines, ["red and plain"]);
}

#[tokio::test]
async fn kill_tree_reaches_a_lamb_of_a_pty_sheep() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "/bin/sleep 60 & echo $!; wait"))
        .unwrap();
    let line = timeout(PUMP_DEADLINE, io.logs.recv())
        .await
        .expect("the sheep must report its lamb")
        .expect("the log pump must not end first");
    let lamb = Pid::from_raw(line.line.trim().parse().expect("the line is a pid"));

    proc.kill_tree().unwrap();

    assert!(
        is_gone_within_deadline(lamb).await,
        "SIGKILL to the group must reach the lamb"
    );
}

/// Whispers `line` and waits for the answer.
async fn try_whisper(io: &ProcIo, line: &str) -> Result<(), RunnerError> {
    let (done, ack) = oneshot::channel();
    io.to_stdin
        .send(StdinWrite {
            line: line.to_string(),
            done,
        })
        .await
        .expect("a pty sheep's stdin pump must be listening");
    timeout(PUMP_DEADLINE, ack)
        .await
        .expect("the whisper must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement")
}

/// Whispers `line`, which must land.
async fn whisper(io: &ProcIo, line: &str) {
    try_whisper(io, line).await.expect("the write must land");
}

/// The bytes `od` printed, as one space-separated string: its padding differs
/// between systems.
fn hex_bytes(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The next line the sheep logs, which must arrive in time.
async fn next_log_line(io: &mut ProcIo) -> String {
    timeout(PUMP_DEADLINE, io.logs.recv())
        .await
        .expect("the sheep must log a line")
        .expect("the log pump must not end first")
        .line
}

#[tokio::test]
async fn a_whispered_line_reaches_a_pty_sheep_with_no_stdin_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "read line; echo \"got:$line\""))
        .unwrap();
    assert!(!io.to_stdin.is_closed(), "a pty sheep takes a whisper");

    whisper(&io, "hello").await;

    // The terminal's echo of the typed line comes first, then the program's.
    assert_eq!(next_log_line(&mut io).await, "hello");
    assert_eq!(next_log_line(&mut io).await, "got:hello");
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit once it has its line");
}

#[tokio::test]
async fn a_pty_sheep_reading_raw_input_sees_a_carriage_return() {
    let dir = tempfile::tempdir().unwrap();
    let script = "stty raw -echo; echo ready; dd bs=1 count=3 2>/dev/null | od -An -tx1";
    let (mut proc, mut io) = TokioRunner::new().spawn(&pty_spec(&dir, script)).unwrap();
    // Raw mode is set before this line prints, so the whisper is not cooked.
    assert_eq!(next_log_line(&mut io).await, "ready");

    whisper(&io, "hi").await;

    assert_eq!(hex_bytes(&next_log_line(&mut io).await), "68 69 0d");
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit once it has its bytes");
}

#[tokio::test]
async fn a_piped_sheep_still_gets_a_newline() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SpawnSpec {
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            "dd bs=1 count=3 2>/dev/null | od -An -tx1".to_string(),
        ],
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        stdin: true,
        ..Default::default()
    };
    let (mut proc, mut io) = TokioRunner::new().spawn(&spec).unwrap();

    whisper(&io, "hi").await;

    assert_eq!(hex_bytes(&next_log_line(&mut io).await), "68 69 0a");
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit once it has its bytes");
}

#[tokio::test]
async fn a_line_a_terminal_would_cut_is_refused_rather_than_sent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, io) = a_sheep_that_stays_up(&dir);

    let answer = try_whisper(&io, &"x".repeat(2000)).await;

    assert!(
        matches!(&answer, Err(RunnerError::WriteFailed(why)) if why.contains("2001")),
        "a line over the terminal's buffer must be refused with its size: {answer:?}"
    );
    proc.kill_tree().unwrap();
}
