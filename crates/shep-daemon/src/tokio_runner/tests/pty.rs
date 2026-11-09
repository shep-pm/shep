//! A `pty = true` sheep on a real pseudo-terminal: what the child finds on
//! its descriptors, and what reaches the log.

use std::time::Duration;

use nix::unistd::{Pid, getpgid, getsid};
use tokio::time::timeout;

use super::super::CHANNEL_CAPACITY;
use super::super::TokioRunner;
use super::super::pty::{PTY_COLS, PTY_ROWS};
use super::PUMP_DEADLINE;
use super::log_text;
use super::stop_signal::is_gone_within_deadline;
use crate::runner::{
    ProcIo, ProcessRunner, PtyError, PtyHandle, RunnerError, RunningProcess, SpawnSpec, StdinWrite,
};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
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

/// A sheep that stays up and logs one line for every `SIGWINCH` it catches.
///
/// The trap runs once the `sleep` the signal interrupted returns, so a case
/// that counts signals reads one line per signal, not one per burst.
fn a_sheep_that_traps_window_signals(
    dir: &tempfile::TempDir,
) -> (<TokioRunner as ProcessRunner>::Proc, ProcIo) {
    TokioRunner::new()
        .spawn(&pty_spec(
            dir,
            "trap 'echo winch' WINCH; echo ready; while :; do sleep 1; done",
        ))
        .unwrap()
}

/// A sheep that stays up and reads its terminal, logging the length of every
/// line it is handed.
///
/// A sheep that never reads leaves what is typed to it in the terminal's input
/// queue, which macOS caps near 1024 bytes across all its lines, so a case
/// that types more than one line needs one that drains.
fn a_sheep_that_reads_its_terminal(
    dir: &tempfile::TempDir,
) -> (<TokioRunner as ProcessRunner>::Proc, ProcIo) {
    TokioRunner::new()
        .spawn(&pty_spec(
            dir,
            r#"stty -echo; echo ready; while read line; do echo "got ${#line}"; done"#,
        ))
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

/// The handle a pty sheep hands out.
fn handle_of(io: &ProcIo) -> PtyHandle {
    io.pty.clone().expect("a pty sheep hands out a handle")
}

/// What the subscriber has been sent so far, once it holds `needle`.
async fn raw_until(rx: &mut Receiver<bytes::Bytes>, needle: &[u8]) -> Vec<u8> {
    let mut seen = Vec::new();
    timeout(PUMP_DEADLINE, async {
        while !seen.windows(needle.len()).any(|window| window == needle) {
            seen.extend_from_slice(&rx.recv().await.expect("the sheep's output must not end"));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the subscriber never saw {needle:?}, only {seen:?}"));
    seen
}

#[tokio::test]
async fn a_piped_sheep_hands_out_no_handle() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SpawnSpec {
        pty: false,
        ..pty_spec(&dir, "exec /bin/sleep 60")
    };
    let (mut proc, io) = TokioRunner::new().spawn(&spec).unwrap();

    assert!(io.pty.is_none());
    proc.kill_tree().unwrap();
}

#[tokio::test]
async fn a_subscriber_sees_the_raw_bytes_with_escape_codes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, r"read go; printf '\033[1;31mred\033[0m\n'"))
        .unwrap();
    let mut rx = handle_of(&io).subscribe();

    whisper(&io, "go").await;

    let seen = raw_until(&mut rx, b"red\x1b[0m").await;
    assert!(
        seen.windows(10).any(|window| window == b"\x1b[1;31mred"),
        "the colour code must reach a subscriber: {seen:?}"
    );
    // The log pump is unchanged: the same output reaches it as plain text.
    assert_eq!(next_log_line(&mut io).await, "go");
    assert_eq!(next_log_line(&mut io).await, "red");
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit");
}

#[tokio::test]
async fn a_resize_reaches_the_child_as_a_new_window_size() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "read go; /bin/stty size"))
        .unwrap();

    handle_of(&io).resize(10, 40).await.unwrap();
    whisper(&io, "go").await;

    assert_eq!(next_log_line(&mut io).await, "go");
    assert_eq!(next_log_line(&mut io).await, "10 40");
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit");
}

#[tokio::test]
async fn a_resize_to_the_same_size_still_signals_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = a_sheep_that_traps_window_signals(&dir);
    let handle = handle_of(&io);
    assert_eq!(next_log_line(&mut io).await, "ready");

    // The size the sheep was spawned with, so the kernel signals neither
    // resize and every `winch` line here is the shepherd's own.
    handle.resize(PTY_ROWS, PTY_COLS).await.unwrap();
    assert_eq!(next_log_line(&mut io).await, "winch");
    handle.resize(PTY_ROWS, PTY_COLS).await.unwrap();
    assert_eq!(next_log_line(&mut io).await, "winch");

    proc.kill_tree().unwrap();
}

#[tokio::test]
async fn raw_input_and_a_whispered_line_do_not_interleave() {
    const RUN: usize = 200;
    let dir = tempfile::tempdir().unwrap();
    // Raw mode with no echo, and `cat` writing back what it is typed.
    let (mut proc, io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "stty raw -echo; echo ready; exec cat"))
        .unwrap();
    let handle = handle_of(&io);
    let mut rx = handle.subscribe();
    raw_until(&mut rx, b"ready").await;

    let (b_line, d_line) = ("b".repeat(RUN), "d".repeat(RUN));
    let (a, b, c, d, e) = tokio::join!(
        handle.send(vec![b'a'; RUN]),
        whisper(&io, &b_line),
        handle.send(vec![b'c'; RUN]),
        whisper(&io, &d_line),
        handle.send(vec![b'e'; RUN]),
    );
    a.unwrap();
    c.unwrap();
    e.unwrap();
    let _: ((), ()) = (b, d);

    // Five writers, each of one byte value: a write cut by another shows as a
    // sixth run. The `\r` a whisper ends on and any `\n` left from `ready`
    // are not typed bytes, so only `a` to `e` count.
    let typed = |byte: &u8| matches!(byte, b'a'..=b'e');
    let mut echoed = Vec::new();
    timeout(PUMP_DEADLINE, async {
        while echoed.iter().filter(|byte| typed(byte)).count() < RUN * 5 {
            echoed.extend_from_slice(&rx.recv().await.expect("cat must keep running"));
        }
    })
    .await
    .expect("cat must write back everything typed");
    echoed.retain(typed);
    let mut runs = echoed.chunk_by(|x, y| x == y).collect::<Vec<_>>();
    runs.sort_by_key(|run| run[0]);
    assert_eq!(
        runs.iter()
            .map(|run| (run[0], run.len()))
            .collect::<Vec<_>>(),
        [
            (b'a', RUN),
            (b'b', RUN),
            (b'c', RUN),
            (b'd', RUN),
            (b'e', RUN)
        ],
        "each writer's bytes must arrive as one run"
    );
    proc.kill_tree().unwrap();
}

#[tokio::test]
async fn a_cooked_terminal_refuses_raw_input_it_would_cut() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, io) = a_sheep_that_stays_up(&dir);
    let handle = handle_of(&io);

    let answer = handle.send(vec![b'x'; 3000]).await;

    assert!(
        matches!(&answer, Err(PtyError::Failed(why)) if why.contains("3000")),
        "an unterminated run over the buffer must be refused with its size: {answer:?}"
    );
    proc.kill_tree().unwrap();
}

#[tokio::test]
async fn an_unfinished_line_counts_across_sends_and_whispers() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = a_sheep_that_reads_its_terminal(&dir);
    let handle = handle_of(&io);
    // The sheep has set its terminal and is reading, so every line the case
    // ends is drained out of the terminal's input queue before the next begins.
    assert_eq!(next_log_line(&mut io).await, "ready");

    handle.send(vec![b'x'; 600]).await.unwrap();
    let second = handle.send(vec![b'y'; 600]).await;
    assert!(
        matches!(&second, Err(PtyError::Failed(why)) if why.contains("1200")),
        "the first 600 bytes are still held, so the next 600 must be refused: {second:?}"
    );
    let whisper = try_whisper(&io, &"z".repeat(600)).await;
    assert!(
        matches!(&whisper, Err(RunnerError::WriteFailed(why)) if why.contains("1201")),
        "a whisper lands on the same unfinished line: {whisper:?}"
    );

    // A newline ends the line, so the count starts over. The length the sheep
    // reports is the first send alone, so neither refusal wrote anything.
    handle.send(b"\n".to_vec()).await.unwrap();
    assert_eq!(next_log_line(&mut io).await, "got 600");

    // The count is fresh, so 600 bytes land where 600 bytes were refused, and
    // land whole.
    handle.send(vec![b'y'; 600]).await.unwrap();
    handle.send(b"\n".to_vec()).await.unwrap();
    assert_eq!(next_log_line(&mut io).await, "got 600");
    proc.kill_tree().unwrap();
}

#[tokio::test]
async fn a_raw_terminal_takes_raw_input_of_any_length() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "stty raw -echo; echo ready; exec cat"))
        .unwrap();
    let handle = handle_of(&io);
    let mut rx = handle.subscribe();
    raw_until(&mut rx, b"ready").await;

    handle.send(vec![b'x'; 3000]).await.unwrap();

    proc.kill_tree().unwrap();
}

#[tokio::test]
async fn a_handle_fails_after_the_sheep_exits() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "echo bye"))
        .unwrap();
    let handle = handle_of(&io);
    timeout(PUMP_DEADLINE, async {
        while io.logs.recv().await.is_some() {}
    })
    .await
    .expect("the log pump must end when the child's terminal closes");
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit");

    assert_eq!(handle.send(b"x".to_vec()).await, Err(PtyError::Gone));
    assert_eq!(handle.resize(24, 80).await, Err(PtyError::Gone));
}

#[tokio::test]
async fn a_subscriber_sees_the_stream_end_while_a_handle_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "sleep 1; echo bye"))
        .unwrap();
    let handle = handle_of(&io);
    let mut rx = handle.subscribe();

    timeout(PUMP_DEADLINE, async {
        while let Ok(_) | Err(RecvError::Lagged(_)) = rx.recv().await {}
    })
    .await
    .expect("recv must end once the terminal closes, though the handle lives");
    assert!(matches!(
        handle.subscribe().recv().await,
        Err(RecvError::Closed)
    ));
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit");
}

#[tokio::test]
async fn no_receiver_means_no_copy() {
    let dir = tempfile::tempdir().unwrap();
    let (mut proc, mut io) = TokioRunner::new()
        .spawn(&pty_spec(&dir, "echo early; read go; echo late"))
        .unwrap();
    assert_eq!(next_log_line(&mut io).await, "early");

    // Subscribed after "early" was read, so a copy made for nobody and kept
    // would show here.
    let mut rx = handle_of(&io).subscribe();
    whisper(&io, "go").await;

    let seen = raw_until(&mut rx, b"late").await;
    assert!(
        !seen.windows(5).any(|window| window == b"early"),
        "output read before anyone subscribed must not be replayed: {seen:?}"
    );
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the script must exit");
}

/// Fails if a pty pump parks on a full `logs` channel and never comes back to
/// its master: the lines past the channel's capacity never reach the file,
/// and macOS drops what the terminal queued outright once its last slave has
/// closed.
#[tokio::test]
async fn a_pty_sheep_exits_and_logs_every_line_with_the_logs_channel_full() {
    const LINES: usize = CHANNEL_CAPACITY + 8;
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("out.log");
    let script =
        format!("i=1; while [ $i -le {LINES} ]; do echo \"line-$i\"; i=$((i+1)); done; echo LAST");
    let (mut proc, io) = TokioRunner::new().spawn(&pty_spec(&dir, &script)).unwrap();
    // `io` is held and its `logs` receiver is never read, which is what a
    // sheep task parked in `kill_process` does to a pump.
    timeout(PUMP_DEADLINE, proc.wait())
        .await
        .expect("the sheep must exit with nobody reading its live log bus");

    // `wait` answers for the child, not for its pump, and the pump is what
    // writes the file, `LIVE_WAIT` after the line that fills `logs`. A pump
    // drops its control receiver only as it ends, after its final flush, so
    // a closed `log_ctl` is the barrier that every line has been written.
    let ended = timeout(PUMP_DEADLINE, async {
        while !io.log_ctl.is_closed() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "a pty pump must come back to its master and end rather than park on a full `logs`"
    );

    // By content, not by a line count: a count cannot say which line a pump
    // lost, and a terminal can add lines of its own.
    let logged: Vec<String> = log_text(&out_path).lines().map(str::to_string).collect();
    let missing: Vec<String> = (1..=LINES)
        .map(|n| format!("line-{n}"))
        .chain(std::iter::once("LAST".to_string()))
        .filter(|want| !logged.contains(want))
        .collect();
    assert!(
        missing.is_empty(),
        "every line the sheep wrote must reach its log file: missing {missing:?} of {LINES} \
         lines and `LAST`, saw {logged:?}"
    );
}
