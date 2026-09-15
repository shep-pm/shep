//! Buffering, flushing, reopen, rotation, line-tearing and stamp timing.

use core::pin::Pin;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncWrite, BufWriter};
use tokio::sync::oneshot;
use tokio::time::{Instant, timeout};

use shep_core::logstamp::{LOG_STAMP_BYTES, stamp_into};

use crate::runner::LogCtl;

use super::super::log_file::{LogFile, LogFiles, PipeFds, open_append, record_lock};
use super::super::{CHANNEL_CAPACITY, IDLE_FLUSH, LOG_BUFFER};
use super::*;

/// Lines longer than `LOG_BUFFER` force the multi-`poll_write` case
/// `O_APPEND` alone cannot make atomic, which is what this guards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn narration_cannot_tear_a_line_the_pump_is_writing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("torn.log");

    let long = "x".repeat(LOG_BUFFER + 512);
    let pump = {
        let path = path.clone();
        let long = long.clone();
        tokio::spawn(async move {
            let mut log = LogFile::open(path).await;
            for i in 0..1200 {
                log.append(&format!("{i} {long}")).await;
                if i % 4 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            log.flush().await.expect("the pump's own flush");
        })
    };
    let narrator = {
        let path = path.clone();
        tokio::spawn(async move {
            for i in 0..600 {
                let mut written = String::new();
                stamp_into(&mut written);
                written.push_str(&format!("[shep] narration {i}\n"));
                let file = open_append(&path).await.expect("the narration handle");
                let _record = record_lock(&path).lock_owned().await;
                let mut file = file;
                use tokio::io::AsyncWriteExt as _;
                file.write_all(written.as_bytes()).await.expect("write");
                file.flush().await.expect("flush");
            }
        })
    };
    pump.await.expect("the pump task");
    narrator.await.expect("the narration task");

    let text = std::fs::read_to_string(&path).expect("the log is readable");
    let mut lines = 0_usize;
    for line in text.lines() {
        lines += 1;
        let once = shep_core::logstamp::strip(line);
        assert_ne!(
            once, line,
            "a line reached the file with no stamp: {line:.80}"
        );
        assert_eq!(
            shep_core::logstamp::strip(once),
            once,
            "a line carries two stamps, so a record was torn: {line:.80}"
        );
    }
    assert_eq!(lines, 1800, "every record reached the file exactly once");
}

/// Fails if a line the child wrote before its sheep task let go never
/// reaches the log file.
///
/// The pump's `select!` has a branch for the sheep task dropping its
/// `logs` receiver, for a lamb that holds the pipe open past the
/// child's own exit. That branch competes with the read branches rather
/// than following them, so a child that writes and exits can leave both
/// ready in the same poll.
///
/// No child here: the harness writes the line itself and then does what
/// `run_sheep` does on return, isolating the race from the process
/// lifecycle that normally hides it.
#[tokio::test]
async fn a_last_line_written_before_the_sheep_task_lets_go_reaches_the_file() {
    for attempt in 0..RACE_ATTEMPTS {
        let mut pump = PumpHarness::start();
        pump.out_writer.write_all(b"last-words\n").await.unwrap();
        // The child exiting, then `run_sheep` breaking out of its loop:
        // the write end closes and the receiver goes, in that order,
        // with the line still unread in between.
        drop(pump.out_writer);
        drop(pump.err_writer);
        drop(pump.logs);

        let settled = timeout(PUMP_DEADLINE, async {
            while !fs::read_to_string(&pump.out_path)
                .unwrap_or_default()
                .contains("last-words")
            {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        assert!(
            settled.is_ok(),
            "attempt {attempt}: the pump dropped the line it had not read yet, \
             leaving {:?}",
            fs::read_to_string(&pump.out_path)
        );
    }
}

/// The same line, lost the other way: through the control channel
/// rather than through `logs`.
///
/// `logs` and `log_ctl` do not close together ordinarily: the sheep task
/// drops `logs` when its child exits, while the slot's `log_ctl` sender
/// outlives it until a delete or shutdown. Those are the cases where
/// both close at once, giving the control arm's own `None` a fourth way
/// out of the loop that can win the same random pick.
///
/// The drain runs once, on the way out, not inside whichever branch
/// triggered it, so it cannot matter which exit was taken.
#[tokio::test]
async fn a_last_line_survives_the_control_channel_closing_with_the_logs() {
    for attempt in 0..RACE_ATTEMPTS {
        let mut pump = PumpHarness::start();
        pump.out_writer.write_all(b"last-words\n").await.unwrap();
        drop(pump.out_writer);
        drop(pump.err_writer);
        // A delete: the slot's sender and the sheep task's receiver go
        // in the same moment, so all four exits are live at once.
        drop(pump.ctl);
        drop(pump.logs);

        let settled = timeout(PUMP_DEADLINE, async {
            while !fs::read_to_string(&pump.out_path)
                .unwrap_or_default()
                .contains("last-words")
            {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        assert!(
            settled.is_ok(),
            "attempt {attempt}: the pump took an exit that skips the drain, \
             leaving {:?}",
            fs::read_to_string(&pump.out_path)
        );
    }
}

/// A sink standing in for the [`tokio::fs::File`] a real [`LogFile`]
/// holds, counting the writes that reach it.
///
/// Bytes on disk come out the same whether or not appends are batched;
/// only the write count shows it, so a counter is what pins
/// [`LOG_BUFFER`] against a future append that writes straight through.
#[derive(Clone, Debug, Default)]
struct WriteCounter {
    writes: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
}

impl WriteCounter {
    fn writes(&self) -> usize {
        self.writes.load(Ordering::Relaxed)
    }

    fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }
}

impl AsyncWrite for WriteCounter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(buf.len(), Ordering::Relaxed);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Fails if an appended line still costs one write to the file.
///
/// `tokio::fs::File` hands each write to the blocking pool: 32.8 us of
/// daemon CPU per line against 0.99 us for the `write(2)` underneath.
/// A buffer turns N writes into one per bufferful, and the write count
/// is the only observable that shows it.
///
/// Paused clock: no time passes, so [`IDLE_FLUSH`] never fires, and
/// every write counted is the buffer spilling or the closing flush.
#[tokio::test(start_paused = true)]
async fn a_run_of_lines_costs_one_write_per_bufferful_not_one_per_line() {
    // 69 characters: `append`'s newline makes a 70-byte line, the size
    // `LOG_BUFFER` was measured against. The stamp is counted
    // separately so 70 stays that number.
    const LINE: &str = "012345678901234567890123456789012345678901234567890123456789012345678";
    const LINE_BYTES: usize = LOG_STAMP_BYTES + LINE.len() + 1;
    let lines = 3 * LOG_BUFFER / LINE_BYTES;

    let sink = WriteCounter::default();
    let mut log = LogFile {
        record: record_lock(std::path::Path::new("test")),
        path: PathBuf::from("counted.log"),
        handle: Some(BufWriter::with_capacity(LOG_BUFFER, sink.clone())),
        buffered_since: None,
        stamp: String::new(),
    };
    for _ in 0..lines {
        log.append(LINE).await;
    }
    log.flush().await.unwrap();

    let total = lines * LINE_BYTES;
    let ceiling = total.div_ceil(LOG_BUFFER) + 1; // + the closing flush's partial buffer
    assert_eq!(
        sink.bytes(),
        total,
        "buffering must not lose or repeat a byte"
    );
    assert!(
        sink.writes() <= ceiling,
        "{lines} lines cost {} writes; one per bufferful plus the closing flush is {ceiling}",
        sink.writes()
    );
    assert!(
        sink.writes() < lines,
        "a write per line is the regression this exists to catch: \
         {lines} lines, {} writes",
        sink.writes()
    );
}

/// Fails if a line only leaves the buffer when the buffer fills or when
/// something asks: that is, if [`IDLE_FLUSH`] bounds nothing.
///
/// A sheep that logs once and goes quiet, with no `Flush`, no reopen,
/// and no second line to push the first out. One line is nowhere near
/// [`LOG_BUFFER`], so only the idle flush can write it through.
///
/// Paused clock: the wait is on [`IDLE_FLUSH`], and a real 50ms would be
/// a claim about the machine. Counting sink: a file would only confirm
/// the bytes left once the blocking pool caught up, the same claim
/// through the filesystem.
#[tokio::test(start_paused = true)]
async fn a_line_from_a_sheep_that_then_goes_quiet_still_reaches_its_file() {
    const LINE: &str = "the-only-line";
    let sink = WriteCounter::default();
    let mut log = LogFile {
        record: record_lock(std::path::Path::new("test")),
        path: PathBuf::from("quiet.log"),
        handle: Some(BufWriter::with_capacity(LOG_BUFFER, sink.clone())),
        buffered_since: None,
        stamp: String::new(),
    };

    log.append(LINE).await;
    assert_eq!(
        sink.bytes(),
        0,
        "one short line must sit in the buffer, or there is nothing for the idle flush to do"
    );

    let armed = log
        .buffered_since
        .expect("an appended line must arm the flush deadline");
    tokio::time::sleep_until(armed + IDLE_FLUSH).await;
    log.flush().await.unwrap();

    assert_eq!(
        sink.bytes(),
        LOG_STAMP_BYTES + LINE.len() + 1,
        "the stamp, the line and its newline must reach the file once the window closes"
    );
    assert_eq!(
        log.buffered_since, None,
        "a flush must retire the deadline, or the pump re-arms it every window"
    );
}

/// Fails if a line reaches its file without the time it was written.
///
/// `mtime` answers for the whole file and only until something touches
/// it again, so a per-line stamp is what an operator can trust after a
/// rotation.
///
/// Asserts the shape and width, not a value: the value is the wall
/// clock, and pinning it would test the clock instead. The width is
/// what every reader stripping the prefix depends on, and what
/// [`LOG_STAMP_BYTES`] claims.
#[tokio::test]
async fn a_line_carries_the_time_it_was_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stamped.log");
    let mut log = LogFile::open(path.clone()).await;

    log.append("the-sheep-said-this").await;
    log.flush().await.expect("the file must be writable");

    let written = fs::read_to_string(&path).unwrap();
    let line = written.strip_suffix('\n').expect("one whole line");
    let (stamp, rest) = line.split_at(LOG_STAMP_BYTES);
    assert_eq!(
        rest, "the-sheep-said-this",
        "the sheep's own bytes must survive the prefix, whole and unaltered"
    );
    let stamp = stamp
        .strip_suffix(' ')
        .expect("one space must separate the stamp from the line");
    let parsed = chrono::DateTime::parse_from_rfc3339(stamp)
        .unwrap_or_else(|err| panic!("{stamp:?} must parse as RFC 3339: {err}"));
    // A minute of slack, against a line written a moment ago: wide
    // enough that a loaded runner cannot fail it, narrow enough that a
    // stamp reading the epoch, the wrong unit, or the wrong offset does.
    let drift = (chrono::Utc::now() - parsed.to_utc()).num_seconds().abs();
    assert!(
        drift < 60,
        "{stamp:?} is {drift}s from now; the stamp must be the moment the line was written"
    );
}

/// Fails if the idle-flush window is measured from the newest buffered
/// line rather than the oldest: a stream logging just inside the window
/// would then keep pushing the deadline out, and "buffered but not on
/// disk" would have no bound at all.
///
/// Paused clock: the question is arithmetic over `Instant`s, and an
/// append that fits in the buffer touches no file to wait on.
#[tokio::test(start_paused = true)]
async fn the_idle_flush_window_is_measured_from_the_oldest_buffered_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = LogFiles {
        out: LogFile::open(dir.path().join("out.log")).await,
        err: LogFile::open(dir.path().join("err.log")).await,
        pipes: PipeFds::default(),
        #[cfg(unix)]
        parked: false,
    };
    assert_eq!(
        files.flush_deadline(),
        None,
        "an untouched pair owes no flush"
    );

    let oldest = Instant::now();
    files.stream(false).append("out-first").await;
    assert_eq!(files.flush_deadline(), Some(oldest + IDLE_FLUSH));

    // Both streams, so the deadline is answering for the pair rather
    // than for whichever one happened to be written last.
    tokio::time::advance(IDLE_FLUSH / 2).await;
    files.stream(false).append("out-second").await;
    files.stream(true).append("err-first").await;
    assert_eq!(
        files.flush_deadline(),
        Some(oldest + IDLE_FLUSH),
        "a later line must not push the window out"
    );

    files.flush_idle().await;
    assert_eq!(
        files.flush_deadline(),
        None,
        "a flush must retire the deadline it satisfied"
    );
}

/// Fails if a rotation loses or duplicates what the buffers were
/// holding.
///
/// Every line below fits in [`LOG_BUFFER`], so at the rename the file may
/// hold none of them: a reopen that dropped its handle without flushing
/// would lose the lot, and one that flushed after swapping handles would
/// write them into the fresh file, leaving a gap in the archive and
/// stale lines in the live log. Exact equality on both paths tells
/// those apart from a reopen that got it right.
#[tokio::test]
async fn a_rotation_lands_every_buffered_line_exactly_once() {
    let mut pump = PumpHarness::start();
    let mut before = String::new();
    for n in 0..40 {
        let line = format!("before-{n}");
        pump.feed(false, &line).await;
        before.push_str(&line);
        before.push('\n');
    }
    assert!(
        before.len() < LOG_BUFFER,
        "the case needs lines small enough that the buffer may still hold them"
    );

    let rotated = pump.dir.path().join("out.log.1");
    fs::rename(&pump.out_path, &rotated).unwrap();
    pump.reopen().await;

    assert_eq!(log_text(&rotated), before);
    assert_eq!(log_text(&pump.out_path), "");

    let mut after = String::new();
    for n in 0..40 {
        let line = format!("after-{n}");
        pump.feed(false, &line).await;
        after.push_str(&line);
        after.push('\n');
    }
    pump.flush().await;

    assert_eq!(log_text(&pump.out_path), after);
    assert_eq!(
        log_text(&rotated),
        before,
        "the archive must have stopped growing at the swap"
    );
}

/// Fails if the `Reopen` arm acknowledges without opening the paths
/// again: the renamed inodes keep receiving every later line and the
/// live paths never come back, `create`-mode rotation silently
/// producing an empty log forever.
#[tokio::test]
async fn a_reopen_moves_both_streams_onto_the_recreated_paths() {
    let mut pump = PumpHarness::start();
    pump.feed(false, "before-out").await;
    pump.feed(true, "before-err").await;

    // The rotator's rename: the pump's handles now point at inodes that
    // answer to a different name, and the paths it was given are gone.
    let rotated_out = pump.dir.path().join("out.log.1");
    let rotated_err = pump.dir.path().join("err.log.1");
    fs::rename(&pump.out_path, &rotated_out).unwrap();
    fs::rename(&pump.err_path, &rotated_err).unwrap();
    assert!(
        !pump.out_path.exists(),
        "sanity: the rename really moved it"
    );

    pump.reopen().await;

    // No polling here: the acknowledgement is a real barrier, because
    // the reopen flushes the old handle before dropping it.
    assert_eq!(log_text(&rotated_out), "before-out\n");
    assert_eq!(log_text(&rotated_err), "before-err\n");
    assert_eq!(log_text(&pump.out_path), "");
    assert_eq!(log_text(&pump.err_path), "");

    pump.feed(false, "after-out").await;
    pump.feed(true, "after-err").await;
    pump.reopen().await; // second reopen, wanted here only as the flush

    assert_eq!(log_text(&pump.out_path), "after-out\n");
    assert_eq!(log_text(&pump.err_path), "after-err\n");
    // Both archives stopped growing the moment the handles were swapped.
    assert_eq!(log_text(&rotated_out), "before-out\n");
    assert_eq!(log_text(&rotated_err), "before-err\n");
}

/// Fails if the reopen opens the path without `.append(true)`: the
/// handle would then carry its own offset across an external truncation
/// and write the next line past a sparse hole, instead of at offset 0.
#[tokio::test]
async fn a_reopened_handle_still_appends_so_a_truncation_leaves_no_hole() {
    let mut pump = PumpHarness::start();
    pump.feed(false, "first").await;
    fs::rename(&pump.out_path, pump.dir.path().join("out.log.1")).unwrap();
    pump.reopen().await;

    // Pushes the reopened handle's offset off zero: at offset zero,
    // appending and writing at a fixed position look identical.
    pump.feed(false, "second").await;
    assert_file_settles(&pump.out_path, "second\n").await;

    // The copytruncate rotator: it copies the file elsewhere and
    // truncates this one in place, leaving the pump's handle open on the
    // same inode at size zero.
    fs::File::create(&pump.out_path).unwrap();
    assert_eq!(fs::metadata(&pump.out_path).unwrap().len(), 0);

    pump.feed(false, "third").await;
    assert_file_settles(&pump.out_path, "third\n").await;
}

/// Fails if [`open_append`] stops opening through `open_log_path`:
/// without its `O_NOFOLLOW`, a symlink planted at a sheep's `out_file`
/// would be followed, and every line the sheep writes would land in
/// whatever it points at. No other case here can see this, since every
/// other one opens a real file.
///
/// Asserts the target's bytes are unchanged, the discriminator from a
/// followed link, and asserts the error message, which
/// `LogFile::reopen` hands to an operator.
///
/// `cfg(unix)`: needs `std::os::unix::fs::symlink`. `O_NOFOLLOW` has no
/// Windows counterpart wired up yet, a gap named in the operator docs.
#[cfg(unix)]
#[tokio::test]
async fn opening_a_symlinked_log_path_is_refused_rather_than_followed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("someone-elses.conf");
    let link = dir.path().join("web-out.log");
    fs::write(&target, b"original").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let error = open_append(&link)
        .await
        .expect_err("a symlinked log path must not be opened for appending");

    assert_eq!(
        fs::read(&target).unwrap(),
        b"original",
        "a refused open must not have reached the symlink's target at all"
    );
    assert_eq!(
        error.to_string(),
        crate::runner::SYMLINK_REFUSED,
        "the operator who reads this needs the word symlink, not ELOOP's \
         own wording about levels of them"
    );
}

/// Fails if the control channel is only consulted after a line arrives
/// (a sequential `next_line().await` and then a check, rather than a
/// `select!`): a sheep that has gone quiet would never reopen at all,
/// which is the failure a pushed message exists to rule out.
#[tokio::test]
async fn a_reopen_is_answered_while_both_streams_are_idle() {
    let pump = PumpHarness::start();
    // The pump opens both paths as it starts, on its own task. This
    // first acknowledgement is only a barrier proving it got that far,
    // so the removal below cannot race the initial open.
    pump.reopen().await;

    // Deleted rather than renamed, so the reopen under test is the only
    // thing that could put these paths back. Not one byte has been
    // written to either stream, and none is written before the
    // acknowledgement.
    fs::remove_file(&pump.out_path).unwrap();
    fs::remove_file(&pump.err_path).unwrap();

    pump.reopen().await;

    assert!(pump.out_path.exists(), "stdout's path must be back");
    assert!(pump.err_path.exists(), "stderr's path must be back");
}

/// Fails if the pump waits for room on `logs` with a bare
/// `logs_tx.send(...).await` inside the `select!` handler: a handler is
/// not cancellable, so the control channel goes unpolled for as long as
/// that wait lasts, and with nothing draining `logs` it lasts forever.
///
/// One layer up this is the cycle `Actor::claim_manual` documents: the
/// party that drains `logs` is the sheep task, so anything that makes
/// the sheep task wait on an acknowledgement closes the loop: actor
/// waiting on the ack, sheep task waiting on the actor, pump waiting on
/// the sheep task.
#[tokio::test]
async fn a_reopen_is_answered_while_the_logs_channel_is_full() {
    let mut pump = PumpHarness::start();

    // One line more than `logs` can hold, and nothing here drains it:
    // the pump appends every line to the file, hands CHANNEL_CAPACITY of
    // them to the channel, and is left waiting for room for the last.
    let flooded = CHANNEL_CAPACITY + 1;
    let mut written = String::new();
    for n in 0..flooded {
        let line = format!("line-{n}\n");
        pump.out_writer.write_all(line.as_bytes()).await.unwrap();
        written.push_str(&line);
    }

    // The append comes before the send, so a file holding every line is
    // proof the pump has read the last one and is parked on its send,
    // so the reopen below lands on a pump already waiting, not one
    // that merely might be.
    assert_file_settles(&pump.out_path, &written).await;

    // Deleted rather than renamed, so the reopen under test is the only
    // thing that could put these paths back.
    fs::remove_file(&pump.out_path).unwrap();
    fs::remove_file(&pump.err_path).unwrap();

    pump.reopen().await;

    assert!(pump.out_path.exists(), "stdout's path must be back");
    assert!(pump.err_path.exists(), "stderr's path must be back");

    // The line the pump was holding is owed to the receiver, not
    // dropped: serving the reopen must not have cost the send its place.
    for n in 0..flooded {
        let observed = timeout(PUMP_DEADLINE, pump.logs.recv())
            .await
            .expect("the pump must resume once `logs` has room")
            .expect("the pump must not end while its streams are open");
        assert_eq!(observed.line, format!("line-{n}"));
    }
}

/// Fails if the pump lets go of a stream's [`LogFile`] once that stream
/// ends. A sheep whose stdout closes while stderr runs on would then
/// never get its stdout log back from a rotation, and no other case
/// here would notice, since every other one reopens with both streams
/// still live.
#[tokio::test]
async fn a_stream_that_has_ended_is_still_reopened() {
    let mut pump = PumpHarness::start();
    pump.feed(false, "before-out").await;
    pump.feed(true, "before-err").await;

    // stdout at EOF with stderr still live: what a child that closes one
    // stream early leaves behind, and what the pump holds in between the
    // two EOFs of an ordinary exit.
    drop(pump.out_writer);
    let_the_pump_settle().await;

    // Deleted rather than renamed, so the reopen is the only thing that
    // could put either path back.
    fs::remove_file(&pump.out_path).unwrap();
    fs::remove_file(&pump.err_path).unwrap();

    let (done, ack) = oneshot::channel();
    pump.ctl
        .send(LogCtl::Reopen { done })
        .await
        .expect("a pump with one live stream still reads its control channel");
    let outcome = timeout(PUMP_DEADLINE, ack)
        .await
        .expect("a reopen must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement");
    assert_eq!(
        outcome,
        Ok(()),
        "an ended stream's path is as openable as a live one's"
    );

    assert!(
        pump.out_path.exists(),
        "the ended stream's path must come back too"
    );
    assert!(
        pump.err_path.exists(),
        "the live stream's path must be back"
    );
}

/// Fails if a pump that has ended leaves its control channel reachable.
/// The failed send is what `ProcIo::log_ctl` promises callers as the
/// signal that the pump is already gone, and it is what lets a reopen
/// aimed at a stopped sheep be a no-op rather than an error worth
/// reporting to whoever asked for it.
#[tokio::test]
async fn a_send_fails_once_the_pump_has_ended() {
    let mut pump = PumpHarness::start();
    pump.feed(false, "before-out").await;
    pump.feed(true, "before-err").await;

    // Both writers gone = both streams at EOF, which is what a child
    // exiting looks like from inside the pump.
    drop(pump.out_writer);
    drop(pump.err_writer);

    // The pump's `logs` sender drops with the task, so this `None` is a
    // bounded wait for the task to have finished rather than a guess
    // that it already has.
    let ended = timeout(PUMP_DEADLINE, pump.logs.recv())
        .await
        .expect("the pump must end once both streams reach EOF");
    assert!(ended.is_none(), "a pump that has ended sends nothing more");

    let (done, ack) = oneshot::channel();
    assert!(
        pump.ctl.send(LogCtl::Reopen { done }).await.is_err(),
        "a reopen aimed at an ended pump must fail to send"
    );
    // The rejected request takes its acknowledgement down with it, so a
    // caller that had already started awaiting one is told the same
    // thing rather than left pending.
    assert!(ack.await.is_err());
}

/// Fails if the pump acknowledges a reopen it could not carry out.
///
/// A path the pump cannot open leaves that stream with no file at all
/// and every later line dropped, so answering `Ok` tells a rotator its
/// rotation worked while the sheep logs into nothing, the same silent
/// failure a reopen exists to end, moved one layer up.
///
/// A directory in the log's place is the failure with no permission
/// games in it: `open(2)` on a directory fails for every uid, root
/// included, so this cannot pass for the wrong reason on a privileged
/// runner.
#[tokio::test]
async fn a_reopen_that_cannot_open_a_path_again_answers_with_the_failure() {
    let pump = PumpHarness::start();
    // A barrier proving both initial opens are done, so what follows
    // cannot race them.
    pump.reopen().await;

    // The rotator's rename, and then something in stdout's way. stderr
    // is merely deleted, so its own reopen is the only thing that can
    // put it back.
    fs::rename(&pump.out_path, pump.dir.path().join("out.log.1")).unwrap();
    fs::create_dir(&pump.out_path).unwrap();
    fs::remove_file(&pump.err_path).unwrap();

    let error = pump
        .reopen_for_answer()
        .await
        .expect_err("a reopen that could not open stdout's path must say so");
    assert!(
        error.message.contains(pump.out_path.to_str().unwrap()),
        "the failure must name the path it could not open: {error}"
    );
    assert!(
        !error.message.contains(pump.err_path.to_str().unwrap()),
        "stderr's path opened fine and must not be reported: {error}"
    );

    // The other half of the answer: a failed open on one stream must not
    // cost the other its handle. Without this the case would pass
    // against a `serve` that gave up at the first failure, taking a
    // sheep's working stream offline over its broken one.
    assert!(
        pump.err_path.exists(),
        "stderr must be reopened even though stdout's open failed"
    );
}

/// Fails if the `Flush` arm never reaches the files, never answers, or
/// drops a handle the way `Reopen` does.
///
/// No polling: an answered flush means the files hold every line
/// already handed to them, and a `Flush` that dropped a handle would
/// leave the pump writing the stream nowhere from then on.
///
/// Does not catch a `LogFile::flush` that answers without asking its
/// file: on a write this small the content assertion would win that
/// race anyway. `a_flush_reports_the_write_its_file_never_took` pins
/// that leg deterministically instead.
#[tokio::test]
async fn a_flush_lands_both_streams_and_keeps_writing_afterwards() {
    let mut pump = PumpHarness::start();
    pump.feed(false, "out-before").await;
    pump.feed(true, "err-before").await;

    pump.flush().await;

    // No polling: the acknowledgement is the barrier this half of `shep
    // flush` exists to provide, and a test that polled for the content
    // would pass against a pump that never provided it.
    assert_eq!(log_text(&pump.out_path), "out-before\n");
    assert_eq!(log_text(&pump.err_path), "err-before\n");

    // A flush keeps the handle; a reopen replaces it. The next line
    // appends to what is already there rather than starting a file.
    pump.feed(false, "out-after").await;
    assert_file_settles(&pump.out_path, "out-before\nout-after\n").await;
}

/// Fails if [`LogFile::flush`] stops asking the file anything: an early
/// `return Ok(())`, or a `map_err` traded for `.ok()`.
///
/// `tokio::fs::File` reports a failed write on the next operation
/// rather than the one that failed, since `write_all` returns once the
/// real `write(2)` is queued. `flush` is the one that asks, so a flush
/// that answered without asking would swallow the only signal a
/// sheep's log went unwritten.
///
/// Driven against a [`LogFile`] directly, not [`PumpHarness`]: a
/// read-only handle makes the write fail deterministically, since
/// `write(2)` on `O_RDONLY` is `EBADF` for every uid.
#[tokio::test]
async fn a_flush_reports_the_write_its_file_never_took() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.log");
    fs::write(&path, "").unwrap();
    let mut log = LogFile {
        record: record_lock(std::path::Path::new("test")),
        path: path.clone(),
        handle: Some(BufWriter::with_capacity(
            LOG_BUFFER,
            tokio::fs::File::open(&path).await.unwrap(),
        )),
        buffered_since: None,
        stamp: String::new(),
    };

    // Swallowed by design: the pump keeps draining a child whose log
    // it cannot write. The failure is owed at the flush instead.
    log.append("never-lands").await;

    let error = log
        .flush()
        .await
        .expect_err("a write that never reached the file must fail the flush");
    assert!(
        error.message.starts_with(&format!("{}: ", path.display())),
        "the failure must name the file it belongs to: {error}"
    );
}

/// Fails if a log handle carried across a handover writes anywhere but
/// the end of the file.
///
/// `O_APPEND` is a file status flag on the open file description, so it
/// crosses an exec with the descriptor. A handle that lost it writes at
/// its own tracked offset instead, overwriting the first line here, or
/// leaving a sparse hole after a `copytruncate` rotation. Reading both
/// lines is what tells the two apart, not just checking non-empty.
///
/// `cfg(unix)` alongside the constructor it drives, and the whole
/// handover with it: Windows has no `execve`, so no image there is ever
/// handed a log handle it did not open.
#[cfg(unix)]
#[tokio::test]
async fn a_log_file_from_an_open_handle_still_appends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.log");
    fs::write(&path, "first\n").unwrap();

    // Opened exactly as a predecessor's pump had it, and handed over
    // rather than reopened by path.
    let handle = open_append(&path).await.unwrap();
    let mut log = LogFile::from_file(path.clone(), handle);

    log.append("second").await;
    log.flush().await.expect("the carried handle must be live");

    assert_eq!(
        log_text(&path),
        "first\nsecond\n",
        "a carried handle must append, not write at its own offset"
    );
}

/// Fails if the pump can only notice a departed `logs` receiver from
/// inside `deliver_line`, that is, if it has no `select!` branch of its
/// own for it.
///
/// Without that branch a pump ends on a line it cannot deliver, on both
/// streams reaching EOF, or on its last control sender dropping. A child
/// that forked a lamb holding its pipes open satisfies none of the
/// three: the lamb keeps both streams from EOF whether or not it ever
/// writes, and the supervisor keeps a control sender for as long as the
/// sheep stays registered (`SheepSlot::log_ctl`). The pump task, both
/// `LogFile` handles and both pipe read ends would then live until that
/// sheep was deleted or the daemon exited.
#[tokio::test]
async fn dropping_the_logs_receiver_ends_a_pump_with_no_line_to_carry_the_news() {
    let pump = PumpHarness::start();
    // A barrier proving the pump is up and serving control requests, so
    // the drop below cannot race its initial open.
    pump.reopen().await;

    drop(pump.logs); // the sheep task returning

    // Both writers held (no stream at EOF) and `pump.ctl` alive
    // (control channel open), so dropping `logs` is the only thing
    // that can end this pump. `closed()` resolves when the pump's own
    // `ctl_rx` drops with its task: a bounded wait, not a guess.
    timeout(PUMP_DEADLINE, pump.ctl.closed())
        .await
        .expect("a pump whose `logs` receiver is gone must end");
}

/// Fails if the pump treats a closed control channel as nothing to do:
/// `ProcIo::log_ctl` documents that dropping the sender ends the pump,
/// and an arm that ignored the `None` would spin on it forever, since a
/// closed `mpsc::Receiver` is ready on every poll.
#[tokio::test]
async fn dropping_the_control_sender_ends_the_pump() {
    let mut pump = PumpHarness::start();
    pump.feed(false, "still-here").await;

    drop(pump.ctl);

    let after = timeout(PUMP_DEADLINE, pump.logs.recv())
        .await
        .expect("the pump must end once nothing can control it");
    assert!(after.is_none(), "a pump that has ended sends nothing more");
}
