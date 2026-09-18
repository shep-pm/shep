//! Stdin delivery, channel shuttling, spawn fd-reporting, preflight
//! classification and the zero-pid guard.

use std::collections::BTreeMap;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use nix::sys::signal::Signal;
#[cfg(unix)]
use shep_core::signals::OperatorSignal;
#[cfg(unix)]
use tokio::io::DuplexStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

#[cfg(unix)]
use crate::channel::{ChildMessage, ShepherdMessage};
#[cfg(unix)]
use crate::runner::{AdoptSpec, LogCtl, ProcessRunner, RunningProcess};
use crate::runner::{Preflight, SpawnSpec, StdinWrite};

use super::super::CHANNEL_CAPACITY;
#[cfg(unix)]
use super::super::TokioRunner;
#[cfg(unix)]
use super::super::log_file::{LogSink, PipeFds};
#[cfg(unix)]
use super::super::pump::spawn_log_pump;
use super::super::pump::spawn_stdin_pump;
use super::super::runner::{PATH_LIST_SEPARATOR, summarise_path, what_exec_will_find};
#[cfg(unix)]
use super::super::signal_group;
use super::*;

/// Room in the in-memory pipe standing in for a child's stdin.
///
/// Four bytes, far under the first line the stdin case writes, so the
/// pump parks inside `write_all` on that first request and cannot reach
/// the next one until the test starts reading. That parking is what
/// makes the case's ordering a fact rather than a hope.
const STDIN_BUFFER: usize = 4;

// Fails if the pump writes a line whose caller has stopped waiting: the
// supervisor abandons the `oneshot` at `STDIN_WRITE_TIMEOUT`, risking a
// late write arriving after `not_written` was already returned. Real
// clock: the forcing mechanism is the pipe draining and closing.
#[tokio::test]
async fn a_line_whose_caller_stopped_waiting_is_dropped_rather_than_written_later() {
    use tokio::io::AsyncReadExt as _;

    let (mut child_end, daemon_end) = tokio::io::duplex(STDIN_BUFFER);
    let (to_stdin, rx) = mpsc::channel(CHANNEL_CAPACITY);
    spawn_stdin_pump(Some(daemon_end), rx);

    // Wedges the pump: eight bytes into a four-byte pipe nobody is
    // reading yet.
    let (first_done, first_ack) = oneshot::channel();
    to_stdin
        .send(StdinWrite {
            line: "AAAAAAAA".to_string(),
            done: first_done,
        })
        .await
        .unwrap();

    // The abandoned one. Sent while the pump is parked on the first, so
    // it is still in the queue when its caller gives up.
    let (second_done, second_ack) = oneshot::channel();
    to_stdin
        .send(StdinWrite {
            line: "BBBB".to_string(),
            done: second_done,
        })
        .await
        .unwrap();
    drop(second_ack);

    // A live caller behind it, so the case can tell "dropped the
    // abandoned line" from "stopped writing altogether".
    let (third_done, third_ack) = oneshot::channel();
    to_stdin
        .send(StdinWrite {
            line: "CCCC".to_string(),
            done: third_done,
        })
        .await
        .unwrap();
    drop(to_stdin);

    let mut written = Vec::new();
    timeout(PUMP_DEADLINE, child_end.read_to_end(&mut written))
        .await
        .expect("the pump must drain and close the pipe")
        .expect("reading the pipe must succeed");

    assert_eq!(
        String::from_utf8(written).unwrap(),
        "AAAAAAAA\nCCCC\n",
        "the abandoned line must not reach the app, and the live one must"
    );
    assert!(
        timeout(PUMP_DEADLINE, first_ack)
            .await
            .expect("the first write must be acknowledged")
            .expect("its sender must outlive the write")
            .is_ok()
    );
    assert!(
        timeout(PUMP_DEADLINE, third_ack)
            .await
            .expect("the third write must be acknowledged")
            .expect("its sender must outlive the write")
            .is_ok()
    );
}

/// fails if a descriptor report leaves out the sheep's stdin write end.
///
/// The number belongs to the stdin pump, but the log pump is told it,
/// exactly as it is told its two reader numbers. A report that dropped
/// it would hand the successor a sheep whose `shep whisper` writes into
/// a descriptor the exec closed.
#[cfg(unix)]
#[tokio::test]
async fn a_report_names_the_stdin_write_end_it_was_told_about() {
    let dir = tempfile::tempdir().unwrap();
    // Held for the length of the case, as the stdin pump holds it in
    // production: a number whose owner has dropped it is a number the
    // next `open` may be handed.
    let (_child_end, daemon_end) = std::io::pipe().unwrap();
    // One live stream, so the pump has something to be reading. A pump
    // handed no streams at all ends before it can answer anything.
    let (_out_writer, out_reader) = tokio::io::duplex(STREAM_BUFFER);
    let (logs_tx, _logs) = mpsc::channel(CHANNEL_CAPACITY);
    let (ctl, ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let pipes = PipeFds {
        out: None,
        err: None,
        stdin: Some(daemon_end.as_raw_fd()),
        channel: None,
    };
    spawn_log_pump(
        Some(out_reader),
        None::<DuplexStream>,
        LogSink::Path(dir.path().join("out.log")),
        LogSink::Path(dir.path().join("err.log")),
        logs_tx,
        ctl_rx,
        pipes,
    );

    let (done, ack) = oneshot::channel();
    ctl.send(LogCtl::ReportFds { done }).await.unwrap();
    let fds = timeout(PUMP_DEADLINE, ack)
        .await
        .expect("a descriptor report must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement");

    assert_eq!(fds.stdin, Some(daemon_end.as_raw_fd()));
}

/// fails if an adopted sheep's stdin write end is not wired back to
/// `to_stdin`.
///
/// The read half of this feature. Carrying the descriptor is only half
/// of it: the successor has to put a pump back on the daemon's end, or
/// every `shep whisper` after a handover answers on a channel with
/// nothing behind it.
#[cfg(unix)]
#[tokio::test]
async fn an_adopted_stdin_pipe_carries_a_line_to_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child_end, daemon_end) = std::io::pipe().unwrap();
    let daemon_end = tokio::net::unix::pipe::Sender::from_file(std::fs::File::from(
        std::os::fd::OwnedFd::from(daemon_end),
    ))
    .expect("the daemon's end of a stdin pipe is writable");

    let (_proc, io) = TokioRunner::new()
        .adopt(adopt_spec(&dir, Some(daemon_end), None))
        .expect("the real runner must be able to adopt");

    let (done, ack) = oneshot::channel();
    io.to_stdin
        .send(StdinWrite {
            line: "whisper".to_string(),
            done,
        })
        .await
        .expect("an adopted sheep must still have a stdin pump");
    timeout(PUMP_DEADLINE, ack)
        .await
        .expect("the write must be acknowledged")
        .expect("the pump must outlive the write")
        .expect("the write must succeed");

    let mut buf = [0_u8; 8];
    std::io::Read::read_exact(&mut child_end, &mut buf).expect("the child end must read");
    assert_eq!(&buf, b"whisper\n");
}

/// fails if an adopted sheep that never had a stdin pipe is given a
/// channel nothing drains.
///
/// `is_closed()` is the one question a caller asks about `to_stdin`
/// (see [`crate::runner::ProcIo::to_stdin`]), so a dangling receiver
/// would have the supervisor wait out its whole `STDIN_WRITE_TIMEOUT`
/// on a sheep that has no fd 0 at all.
#[cfg(unix)]
#[tokio::test]
async fn an_adopted_sheep_without_a_stdin_pipe_has_a_closed_channel() {
    let dir = tempfile::tempdir().unwrap();

    let (_proc, io) = TokioRunner::new()
        .adopt(adopt_spec(&dir, None, None))
        .expect("the real runner must be able to adopt");

    assert!(io.to_stdin.is_closed());
}

/// fails if a descriptor report leaves out the sheep's shepherd
/// channel.
///
/// The number belongs to that channel's two pump tasks rather than to
/// this one, exactly as the stdin number belongs to the stdin pump, and
/// the log pump is told it for the same reason: it is the only party a
/// snapshot asks. A report that dropped it would hand the successor a
/// sheep whose fd 3 the exec closed, so a `shep trigger` reaches
/// nothing and the app sees its channel end for no reason it can
/// observe.
#[cfg(unix)]
#[tokio::test]
async fn a_report_names_the_shepherd_channel_it_was_told_about() {
    let dir = tempfile::tempdir().unwrap();
    // A real socketpair, held for the length of the case exactly as the
    // channel's own pumps hold it in production.
    let (daemon_end, _child_end) = std::os::unix::net::UnixStream::pair().unwrap();
    let (_out_writer, out_reader) = tokio::io::duplex(STREAM_BUFFER);
    let (logs_tx, _logs) = mpsc::channel(CHANNEL_CAPACITY);
    let (ctl, ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let pipes = PipeFds {
        out: None,
        err: None,
        stdin: None,
        channel: Some(daemon_end.as_raw_fd()),
    };
    spawn_log_pump(
        Some(out_reader),
        None::<DuplexStream>,
        LogSink::Path(dir.path().join("out.log")),
        LogSink::Path(dir.path().join("err.log")),
        logs_tx,
        ctl_rx,
        pipes,
    );

    let (done, ack) = oneshot::channel();
    ctl.send(LogCtl::ReportFds { done }).await.unwrap();
    let fds = timeout(PUMP_DEADLINE, ack)
        .await
        .expect("a descriptor report must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement");

    assert_eq!(fds.channel, Some(daemon_end.as_raw_fd()));
}

/// fails if an adopted shepherd channel is not wired back to both
/// `to_child` and `from_child`.
///
/// Both directions in one case, because the failure to catch is a
/// successor that rebuilt one pump and not the other, and either half
/// alone looks healthy from the other side. Writing proves
/// `shutdown_with_message` and `shep trigger` still land; reading proves
/// `{"kind":"ready"}` and every action reply still come back, which is
/// what a `wait_ready` sheep's whole lifecycle turns on.
#[cfg(unix)]
#[tokio::test]
async fn an_adopted_shepherd_channel_carries_both_directions() {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let dir = tempfile::tempdir().unwrap();
    // `child_end` is what an app holds on fd 3, unmoved by the handover.
    // Both ends are async here rather than one of each: the write below
    // is served by a task the adoption spawned, so a blocking read on
    // this thread would park the runtime before that task ever ran.
    let (daemon_end, child_end) = tokio::net::UnixStream::pair().unwrap();
    let (child_read, mut child_write) = tokio::io::split(child_end);
    let mut child = tokio::io::BufReader::new(child_read);

    let (_proc, mut io) = TokioRunner::new()
        .adopt(adopt_spec(&dir, None, Some(daemon_end)))
        .expect("the real runner must be able to adopt");

    io.to_child
        .send(ShepherdMessage::Shutdown)
        .await
        .expect("an adopted sheep must still have a channel writer");
    let mut line = String::new();
    timeout(PUMP_DEADLINE, child.read_line(&mut line))
        .await
        .expect("the shepherd's message must reach the child's end")
        .expect("the child end must read");
    assert_eq!(line.trim_end(), r#"{"kind":"shutdown"}"#);

    child_write
        .write_all(b"{\"kind\":\"ready\"}\n")
        .await
        .unwrap();
    let back = timeout(PUMP_DEADLINE, io.from_child.recv())
        .await
        .expect("an adopted sheep must still have a channel reader")
        .expect("the reader must forward what the child said");
    assert_eq!(back, ChildMessage::Ready);
}

/// fails if an adopted sheep that never had a channel is given ends
/// nothing drains.
///
/// `is_closed()` is the one question the supervisor asks about
/// `to_child` (see `SheepSlot::open_channel`), so a dangling receiver
/// would have a `shep trigger` against a sheep with no fd 3 wait out its
/// whole `action_timeout` instead of answering `NoChannel` at once.
#[cfg(unix)]
#[tokio::test]
async fn an_adopted_sheep_without_a_channel_has_closed_channel_ends() {
    let dir = tempfile::tempdir().unwrap();

    let (_proc, mut io) = TokioRunner::new()
        .adopt(adopt_spec(&dir, None, None))
        .expect("the real runner must be able to adopt");

    assert!(io.to_child.is_closed());
    assert!(
        io.from_child.recv().await.is_none(),
        "a sheep with no channel must report one that is over, not one that is quiet"
    );
}

/// The plainest adoption, with `stdin_pipe` and `channel` the only
/// handles it carries.
///
/// Every other handle is `None`, which is the shape a sheep whose log
/// opens had failed arrives in; nothing here reads them. The pid is this
/// process's own because nothing below ever waits it: an adoption
/// records a number, and the reaper is what would go looking for it.
#[cfg(unix)]
fn adopt_spec(
    dir: &tempfile::TempDir,
    stdin_pipe: Option<tokio::net::unix::pipe::Sender>,
    channel: Option<tokio::net::UnixStream>,
) -> AdoptSpec {
    AdoptSpec {
        pid: std::process::id(),
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        out_pipe: None,
        err_pipe: None,
        out_log: None,
        err_log: None,
        stdin_pipe,
        channel,
        reaper: Arc::new(crate::runner::AdoptedReaper::new()),
    }
}

/// A spec for a real child, whose fd 0 is the point of the two cases
/// below.
///
/// A real child rather than a harness, since the question is what the
/// spawn reads off the handle it is about to give away: an in-memory
/// stand-in has no descriptor to read.
///
/// The program is the caller's, and the choice matters more than it
/// looks: a child that exits before the report is answered takes its
/// log pump with it, and the case then fails on a dropped
/// acknowledgement rather than on anything about descriptors.
#[cfg(unix)]
fn child_spec(dir: &tempfile::TempDir, program: &str, args: &[&str], stdin: bool) -> SpawnSpec {
    SpawnSpec {
        name: "web".to_string(),
        program: program.to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        cwd: None,
        env: BTreeMap::new(),
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        channel: false,
        stdin,
        credentials: None,
    }
}

/// fails if a spawn does not report the descriptor it put on the child's
/// fd 0.
///
/// The number is read off `child.stdin` before the spawn hands it to the
/// stdin pump, which is the only moment it is knowable, and getting that
/// order wrong reports `None` for every sheep an operator can whisper
/// to. Checked as a write end of a pipe rather than merely as some
/// number: a report that named the wrong one of the five handles a spawn
/// holds would still be `Some`.
#[cfg(unix)]
#[tokio::test]
async fn a_spawn_reports_the_write_end_it_put_on_the_childs_stdin() {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    let dir = tempfile::tempdir().unwrap();
    // `cat` with a real pipe on fd 0 waits on it, so it is still there
    // to be reported on, and it exits when `io` is dropped below.
    let (_proc, io) = TokioRunner::new()
        .spawn(&child_spec(&dir, "/bin/cat", &[], true))
        .unwrap();

    let (done, ack) = oneshot::channel();
    io.log_ctl.send(LogCtl::ReportFds { done }).await.unwrap();
    let fds = timeout(PUMP_DEADLINE, ack)
        .await
        .expect("a descriptor report must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement");

    let stdin = fds
        .stdin
        .expect("a sheep spawned with a stdin pipe carries its write end");
    let flags = OFlag::from_bits_truncate(fcntl(stdin, FcntlArg::F_GETFL).unwrap());
    assert_eq!(
        flags & OFlag::O_ACCMODE,
        OFlag::O_WRONLY,
        "the number reported for stdin must be the end the daemon writes"
    );
    assert_ne!(Some(stdin), fds.out_pipe);
    assert_ne!(Some(stdin), fds.err_pipe);
    drop(io);
}

/// fails if a sheep that never asked for a pipe on fd 0 carries a
/// descriptor for one.
///
/// `/dev/null` is what such a child has there, and it belongs to the
/// child alone: naming a number here would have the successor adopt
/// something no `shep whisper` will ever reach.
#[cfg(unix)]
#[tokio::test]
async fn a_spawn_without_stdin_reports_no_descriptor_for_it() {
    let dir = tempfile::tempdir().unwrap();
    // Not `cat` here: with `/dev/null` on fd 0 it reads EOF and exits at
    // once, which ends its log pump and leaves this case racing a
    // report against a pump that is already gone.
    let (mut proc, io) = TokioRunner::new()
        .spawn(&child_spec(&dir, "/bin/sleep", &["30"], false))
        .unwrap();

    let (done, ack) = oneshot::channel();
    io.log_ctl.send(LogCtl::ReportFds { done }).await.unwrap();
    let fds = timeout(PUMP_DEADLINE, ack)
        .await
        .expect("a descriptor report must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement");

    assert_eq!(fds.stdin, None);
    // Killed rather than waited out: nothing here reads the child, and
    // a `sleep` left behind outlives the test binary.
    proc.signal_process(OperatorSignal::Kill).unwrap();
    drop(io);
}

/// A [`SpawnSpec`] carrying only what `what_exec_will_find` reads. Every
/// other field is left at whatever is cheapest: nothing below spawns
/// anything, so nothing below can be affected by them.
fn preflight_spec(program: &str, cwd: Option<PathBuf>, path: Option<&str>) -> SpawnSpec {
    let mut env = BTreeMap::new();
    if let Some(path) = path {
        env.insert("PATH".to_string(), path.to_string());
    }
    SpawnSpec {
        name: "web".to_string(),
        program: program.to_string(),
        args: Vec::new(),
        cwd,
        env,
        out_file: PathBuf::from("/dev/null"),
        err_file: PathBuf::from("/dev/null"),
        channel: false,
        stdin: false,
        credentials: None,
    }
}

/// fails if a form the preflight must not decide starts being refused.
///
/// This is the direction that costs a working Flockfile, so it is the
/// list worth pinning as a sweep rather than one representative case.
/// Every entry here is a real shape from the testbed Flockfile that
/// produced the defect: an absolute path, one relative to a `cwd`
/// (`obscura`), a bare command on PATH (`node`, `npx`).
#[test]
fn a_program_that_is_really_there_is_nothing_to_report() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("srv");
    fs::write(&bin, "#!/bin/sh\n").unwrap();
    let path_dir = dir.path().to_string_lossy().into_owned();

    for spec in [
        preflight_spec(&bin.to_string_lossy(), None, None),
        preflight_spec("./srv", Some(dir.path().to_path_buf()), None),
        preflight_spec("srv", None, Some(&path_dir)),
        // The second PATH entry rather than the first, so a search that
        // only ever looks at one directory fails here.
        preflight_spec("srv", None, Some(&format!("/nonexistent:{path_dir}"))),
        // Not decidable, and so not decided: a relative path with no
        // `cwd` would be resolved against whatever directory the
        // shepherd was autostarted from.
        preflight_spec("./srv", None, None),
        // Likewise a bare name with no PATH to search.
        preflight_spec("srv", None, None),
        preflight_spec("srv", None, Some("")),
        preflight_spec("", None, None),
    ] {
        assert_eq!(
            what_exec_will_find(&spec),
            Preflight::Unknown,
            "reported something about a spec it cannot be certain about: {spec:?}"
        );
    }
}

/// Fails if `Impossible`, the verdict that refuses the whole batch, is
/// ever returned for anything but a path, since a path is the one claim
/// about the filesystem the daemon can settle.
///
/// The reason string carries the resolved path, not the
/// `./proto-enum-api` the operator wrote, so an operator can tell which
/// directory it was looked for in.
#[test]
fn an_absent_path_is_impossible_and_names_the_path_that_was_tried() {
    let dir = tempfile::tempdir().unwrap();

    assert_eq!(
        what_exec_will_find(&preflight_spec(
            "./proto-enum-api",
            Some(dir.path().to_path_buf()),
            None,
        )),
        // `join`, not a `/` in the format string: the separator shep
        // resolved this path with is the platform's, and on Windows
        // that is a backslash.
        Preflight::Impossible(format!(
            "no such file: {}",
            dir.path().join("./proto-enum-api").display()
        )),
    );
    // Absolute, and `/nonexistent/srv` is not absolute on Windows: a
    // path with no drive letter is relative to the current drive, and
    // this arm is reached only for a program the daemon can resolve
    // without a `cwd`.
    let absent = if cfg!(windows) {
        r"C:\nonexistent\srv"
    } else {
        "/nonexistent/srv"
    };
    assert_eq!(
        what_exec_will_find(&preflight_spec(absent, None, None)),
        Preflight::Impossible(format!("no such file: {absent}")),
    );
}

/// Fails if a filesystem error that is not "no such file" is ever read
/// as absence.
///
/// [`std::path::Path::exists`] returns `false` on any [`fs::metadata`]
/// error, so an unreadable directory or an unsettled mount would
/// otherwise refuse a whole batch over a filesystem merely unavailable
/// for a moment.
///
/// Two provocations: `ENOTDIR` (an invalid filename on Windows, where a
/// blocked file answers absence instead), and `EACCES` via a `chmod
/// 000` directory, skipped under root, which bypasses it. Mode
/// restored before the assertion, or a panic leaves the `TempDir`
/// unable to `Drop` through the locked directory.
#[test]
fn a_filesystem_error_that_is_not_absence_is_never_impossible() {
    let dir = tempfile::tempdir().unwrap();

    // ENOTDIR: `wall` is a file, so nothing can be under it.
    #[cfg(unix)]
    let unreadable = {
        let wall = dir.path().join("wall");
        fs::write(&wall, "not a directory").unwrap();
        wall.join("srv")
    };
    // ERROR_INVALID_NAME: `<` cannot appear in a Windows filename, so
    // the name is refused before lookup. A path through a file would
    // answer ERROR_PATH_NOT_FOUND instead, which is absence, making
    // the assertion below vacuous.
    #[cfg(windows)]
    let unreadable = dir.path().join("no<such<name");
    let kind = fs::metadata(&unreadable).unwrap_err().kind();
    assert_ne!(
        kind,
        io::ErrorKind::NotFound,
        "the provocation must be an error OTHER than not-found, or this \
         case proves nothing: {kind:?}"
    );
    assert_eq!(
        what_exec_will_find(&preflight_spec(&unreadable.to_string_lossy(), None, None)),
        Preflight::Unknown,
        "a path shep could not read is a suspicion, not a certainty, and \
         must never refuse a batch"
    );

    // EACCES: an unreadable directory between the cwd and the program.
    // Unix only: see this test's doc for why Windows gets no
    // equivalent rather than a weaker one.
    #[cfg(unix)]
    {
        if nix::unistd::Uid::effective().is_root() {
            return;
        }
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let behind_the_wall = locked.join("srv");
        let observed = fs::metadata(&behind_the_wall)
            .map(|_| ())
            .map_err(|e| e.kind());
        let verdict = what_exec_will_find(&preflight_spec(
            &behind_the_wall.to_string_lossy(),
            None,
            None,
        ));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            observed,
            Err(io::ErrorKind::PermissionDenied),
            "the chmod must actually bite, or the assertion below is vacuous"
        );
        assert_eq!(
            verdict,
            Preflight::Unknown,
            "a permission error on the way to the program must not take the rest \
             of the flock down with it"
        );
    }
}

/// fails if a bare command off the PATH ever becomes `Impossible`.
///
/// `Doubtful` is reported and carried on with, never refused: the
/// daemon's own PATH under the unit `shep startup` installs is not the
/// shell an operator tested in, so a flock whose one Node app cannot
/// resolve `node` must still bring up every other app.
///
/// The PATH searched is in the message on purpose: naming which one
/// sends an operator to the terminal path that actually works.
#[test]
fn a_bare_command_off_the_path_is_only_ever_doubtful() {
    let found = what_exec_will_find(&preflight_spec("node", None, Some("/nonexistent")));

    assert_eq!(
        found,
        Preflight::Doubtful("`node` is not on the shepherd's PATH (/nonexistent)".to_string()),
    );
    assert!(
        !matches!(found, Preflight::Impossible(_)),
        "a claim about an environment must never refuse a batch"
    );
}

/// fails if a long PATH is printed in full, or a short one is not.
///
/// This message reaches a terminal now, not only the shepherd's log:
/// `spawn_fresh` puts a `Doubtful` reason into the reply once that app's
/// own spawn has failed. A full interactive shell's PATH is unreadably
/// long (see `super::super::runner::PATH_ENTRIES_IN_MESSAGE`'s own
/// doc), and dumping that into `error[spawn_failed]:` buries the
/// sentence that matters.
///
/// The short case is the one that must survive intact: a `shep startup`
/// unit with no PATH of its own gets `assemble`'s three-entry fallback,
/// and seeing those three IS the diagnosis.
#[test]
fn a_long_path_is_summarised_and_a_startup_units_own_path_is_not() {
    // Spelled in the platform's own PATH syntax. A unix PATH handed to
    // a Windows build is one entry, not six, so every assertion below
    // would be about a string this function never sees.
    let sep = PATH_LIST_SEPARATOR;
    let join = |entries: &[&str]| entries.join(&sep.to_string());

    let fallback = join(&["/usr/local/bin", "/usr/bin", "/bin"]);
    assert_eq!(
        summarise_path(&fallback),
        fallback,
        "the PATH a unit actually gets must print in full"
    );

    let long = join(&["/a", "/b", "/c", "/d", "/e", "/f"]);
    assert_eq!(
        summarise_path(&long),
        format!("{} and 2 more entries", join(&["/a", "/b", "/c", "/d"]))
    );

    // Exactly at the cap, which is where an off-by-one would show.
    let capped = join(&["/a", "/b", "/c", "/d"]);
    assert_eq!(summarise_path(&capped), capped);
}

// Everything else in this module needs a real OS child and lives in
// `tests/real_runner.rs`; this one case needs no process at all.
/// `cfg(unix)` alongside `signal_group`, the function it guards. There
/// is no negative-pid primitive on Windows and so no zero-pid hazard:
/// `kill_tree` addresses a job handle, which cannot accidentally name
/// the daemon's own group the way `kill(0, ..)` can.
#[cfg(unix)]
#[test]
fn a_zero_pid_is_refused_before_it_can_reach_the_daemons_own_group() {
    // `SIGCONT`, not a lethal signal: if `signal_group`'s zero guard is
    // ever deleted, this assertion must fail rather than take the test
    // harness's own process group down with it.
    let err = signal_group(0, Signal::SIGCONT).unwrap_err();
    assert_eq!(
        err.to_string(),
        "signal delivery failed: pid 0 is not a signallable process id"
    );
}
