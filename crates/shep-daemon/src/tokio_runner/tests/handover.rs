//! FD reporting, drain-on-resume, pipe survival and dual-pump isolation.
//!
//! Everything here is `cfg(unix)`: a handover's descriptor report has no
//! Windows counterpart.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::time::Duration;

use tokio::io::DuplexStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use shep_core::logstamp::LOG_STAMP_BYTES;

use crate::runner::LogCtl;

use super::super::log_file::{LogSink, PipeFds};
use super::super::pump::spawn_log_pump;
use super::super::{CHANNEL_CAPACITY, FINAL_DRAIN, READ_BUFFER};
use super::*;

/// Fails if a descriptor report answers with anything but the four
/// numbers the pump is really holding.
///
/// Checks exact equality against the harness's own numbers, not merely
/// that something is open, which a wrong-but-open descriptor would pass.
#[tokio::test]
async fn a_pump_reports_the_descriptors_it_holds() {
    let pump = PumpHarness::start_over_pipes();
    let fds = pump.report_fds().await;

    assert_eq!(fds.out_pipe, pump.pipes.out, "stdout's read end");
    assert_eq!(fds.err_pipe, pump.pipes.err, "stderr's read end");
    assert!(
        fds.out_log.is_some() && fds.err_log.is_some(),
        "a pump that opened both log files must name both handles: {fds:?}"
    );

    let named: Vec<RawFd> = fds.all().into_iter().flatten().collect();
    assert_eq!(named.len(), 4, "a running sheep has all four: {fds:?}");
    for fd in &named {
        assert!(
            is_open(*fd),
            "the blob would name a closed descriptor: {fd}"
        );
    }
    let distinct: BTreeSet<RawFd> = named.iter().copied().collect();
    assert_eq!(distinct.len(), 4, "four descriptors, four numbers: {fds:?}");
}

/// Fails if both pipes filled to capacity cannot be drained.
///
/// The bound [`super::super::FINAL_DRAIN`] rests on: a reaped child cannot
/// leave more than its pipes' capacity behind, so two full pipes is the
/// worst case. Both streams, since `final_drain` selects between them.
/// Runs over real pipes, since the kernel's capacity is the one that
/// matters; the line count is read off the fill rather than assumed.
///
/// Waits [`PUMP_DEADLINE`], not [`super::super::FINAL_DRAIN`]: binding to
/// the latter would assert this machine's scheduling speed rather than
/// the drain's completeness.
#[tokio::test]
async fn both_pipes_filled_to_capacity_drain_inside_the_budget() {
    let pump = PumpHarness::start_over_pipes();
    // 64 bytes a line: short enough to be a round fraction of a pipe,
    // long enough that filling one does not take many thousands of
    // syscalls.
    let line = format!("{}\n", "x".repeat(63));
    let out_lines = fill_pipe(&pump.out_writer, &line).await;
    let err_lines = fill_pipe(&pump.err_writer, &line).await;
    assert!(
        out_lines > 0 && err_lines > 0,
        "neither pipe took a whole line: stdout={out_lines} stderr={err_lines}"
    );

    drop(pump.out_writer);
    drop(pump.err_writer);
    drop(pump.logs);

    for (path, want, stream) in [
        (&pump.out_path, out_lines, "stdout"),
        (&pump.err_path, err_lines, "stderr"),
    ] {
        let settled = timeout(PUMP_DEADLINE, async {
            while fs::read_to_string(path).unwrap_or_default().lines().count() < want {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        assert!(
            settled.is_ok(),
            "a full {stream} pipe did not drain inside {FINAL_DRAIN:?}: {want} lines \
             written, {} landed",
            fs::read_to_string(path).unwrap_or_default().lines().count()
        );
    }
}

/// Fails if two instances of one `merge_logs` app end up naming one
/// descriptor number for the file they share.
///
/// `handover::adopt::refuse_repeated_fds` rejects the entire handover on
/// any repeated number, so a shared descriptor here would fail every
/// reload of every flock containing the app, not just this one.
///
/// One inode, two `open`s, two numbers: each instance runs its own
/// `LogFile::open` rather than sharing a handle. `merge_logs` makes
/// both instances resolve to the same two paths, since `assemble` drops
/// the `-<instance>` suffix.
#[tokio::test]
async fn two_pumps_on_one_log_path_report_different_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("merged-out.log");
    let err_path = dir.path().join("merged-err.log");

    // Held for the whole case: a reading end is only a valid number
    // while its writing end is alive, and a pump that reached EOF clears
    // the number it would otherwise report.
    let mut writers = Vec::new();
    let mut reports = Vec::new();
    for _ in 0..2 {
        let (out_writer, out_reader) = tokio::net::unix::pipe::pipe().unwrap();
        let (err_writer, err_reader) = tokio::net::unix::pipe::pipe().unwrap();
        let pipes = PipeFds {
            out: Some(out_reader.as_raw_fd()),
            err: Some(err_reader.as_raw_fd()),
            stdin: None,
            channel: None,
        };
        let (logs_tx, _logs) = mpsc::channel(CHANNEL_CAPACITY);
        let (ctl, ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
        spawn_log_pump(
            Some(out_reader),
            Some(err_reader),
            LogSink::Path(out_path.clone()),
            LogSink::Path(err_path.clone()),
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
        // `_logs` and `ctl` are kept alive alongside the writers: a pump
        // whose control channel closed would end and close the very
        // handles whose numbers are being compared.
        writers.push((out_writer, err_writer, ctl, _logs));
        reports.push(fds);
    }

    let named: Vec<RawFd> = reports
        .iter()
        .flat_map(|fds| fds.all().into_iter().flatten())
        .collect();
    assert_eq!(
        named.len(),
        8,
        "two running instances, four descriptors each: {reports:?}"
    );
    let distinct: BTreeSet<RawFd> = named.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        8,
        "a repeat here refuses the whole handover, not just this app: {reports:?}"
    );
    // The pointed half of the assertion above: the two log handles are
    // the pair that could plausibly have been shared, since they are the
    // only two of the eight opened on the same path.
    assert_ne!(
        reports[0].out_log, reports[1].out_log,
        "two instances sharing one log file must still hold two handles"
    );
    assert_ne!(reports[0].err_log, reports[1].err_log);
    drop(writers);
}

/// Fails if a report still names a stream whose descriptor the pump has
/// already let go of.
///
/// Exercises the `files.pipes.out = None` clearing in the `StreamEnded`
/// arm: [`a_pump_reports_the_descriptors_it_holds`] reports while both
/// streams are live, so it passes with or without that clearing.
///
/// A closed number is free for the next `open` in this process, so a
/// stale report risks the successor adopting an unrelated file as this
/// sheep's stdout. `err_pipe` staying at its own number is what keeps
/// this a case about the ended stream, not a dead pump.
#[tokio::test]
async fn a_report_after_a_stream_ends_names_no_descriptor_for_it() {
    let mut pump = PumpHarness::start_over_pipes();
    pump.feed(false, "before-the-eof").await;

    drop(pump.out_writer);
    let_the_pump_settle().await;

    // Sent through the field, not `report_fds`: dropping the writer
    // above moves out of the harness, so `&self` cannot be called.
    let (done, ack) = oneshot::channel();
    pump.ctl
        .send(LogCtl::ReportFds { done })
        .await
        .expect("a pump with one live stream still reads its control channel");
    let fds = timeout(PUMP_DEADLINE, ack)
        .await
        .expect("a descriptor report must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement");

    assert_eq!(
        fds.out_pipe, None,
        "stdout is at EOF and its descriptor is gone: {fds:?}"
    );
    assert_eq!(
        fds.err_pipe, pump.pipes.err,
        "stderr is still live and keeps its own number: {fds:?}"
    );
    assert!(
        fds.out_log.is_some() && fds.err_log.is_some(),
        "both log handles are held whichever stream ended: {fds:?}"
    );
}

/// Fails if a report is answered before what the pump is holding has
/// reached the file.
///
/// Written before the report, readable on disk after it, with no
/// settling in between: not [`super::assert_file_settles`], since
/// polling would let `IDLE_FLUSH` pass a case the report's own flush
/// should have covered. A blob whose descriptors are ready but whose
/// bytes are not is a log gap the successor cannot repair, since the
/// bytes died with the image at the exec.
#[tokio::test]
async fn reporting_flushes_first() {
    let mut pump = PumpHarness::start_over_pipes();
    pump.feed(false, "before-the-blob").await;
    pump.feed(true, "and-on-stderr").await;

    let _ = pump.report_fds().await;

    assert_eq!(log_text(&pump.out_path), "before-the-blob\n");
    assert_eq!(log_text(&pump.err_path), "and-on-stderr\n");
}

/// Fails if a pump goes on reading its sheep's streams after it has
/// reported.
///
/// A report is a snapshot: descriptors, and a flush that empties the
/// write buffer behind them, taken before the exec that consumes it.
/// Anything read afterward is written by an image about to be replaced,
/// landing in neither the pipe the successor inherits nor the buffer it
/// could have been handed. Parking keeps the snapshot true until used.
#[tokio::test]
async fn a_pump_that_has_reported_stops_reading_until_it_is_resumed() {
    let mut pump = PumpHarness::start();
    pump.feed(false, "before-the-report").await;

    let _ = pump.report_fds().await;

    // The sheep does not stop writing because its shepherd is being
    // replaced. Every one of these has to still be in the pipe at the
    // exec, which is the same claim as the file not growing.
    pump.out_writer
        .write_all(b"after-1\nafter-2\n")
        .await
        .unwrap();
    assert_file_holds_still(&pump.out_path, "before-the-report\n").await;

    pump.resume().await;
    assert_file_settles(&pump.out_path, "before-the-report\nafter-1\nafter-2\n").await;
}

/// Fails if a report leaves lines stranded in the pump's reader.
///
/// A quiet sheep leaves the reader empty at every instant a report
/// could land, so this needs a busy one: filling `logs` first leaves
/// everything past that sitting in a userspace buffer the exec destroys
/// and no descriptor carries.
///
/// Asserted with no settling: the report's own flush is the barrier.
#[tokio::test]
async fn a_report_lands_what_the_reader_was_holding() {
    let mut pump = PumpHarness::start();
    let burst: String = (1..=BURST).map(|n| format!("{n}\n")).collect();
    pump.out_writer.write_all(burst.as_bytes()).await.unwrap();
    wait_for_a_full_logs_channel(&pump.logs).await;

    let _ = pump.report_fds().await;

    assert_eq!(
        log_text(&pump.out_path),
        burst,
        "every line the reader held must be on disk once, in order, \
         before the report is answered"
    );
}

/// Fails if a report takes bytes off a pipe that it does not write.
///
/// Every byte the sheep wrote must be in the log file or still in the
/// pipe when the report answers, since the successor inherits the pipe
/// by descriptor number. A byte in neither is destroyed at the `execve`.
///
/// The inherited handle is a `try_clone` sharing one open file
/// description with the reader, so it sees what the successor would.
///
/// `drain_ready`'s documented residual is the one line of slack: a
/// part-way line splits between the reader's buffer and `Lines`'
/// accumulator, unreachable here. Fails at two lines lost, not one.
#[tokio::test]
async fn a_report_leaves_in_the_pipe_everything_it_did_not_write() {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("out.log");
    let (reader, mut writer) = std::io::pipe().unwrap();
    // The successor's view, taken before the pump owns the reader.
    let inherited = reader.try_clone().unwrap();
    let reader = tokio::net::unix::pipe::Receiver::from_owned_fd(OwnedFd::from(reader)).unwrap();
    let (logs_tx, logs) = mpsc::channel(CHANNEL_CAPACITY);
    let (ctl, ctl_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let pipes = PipeFds {
        out: Some(reader.as_raw_fd()),
        err: None,
        stdin: None,
        channel: None,
    };
    spawn_log_pump(
        Some(reader),
        None::<DuplexStream>,
        LogSink::Path(out_path.clone()),
        LogSink::Path(dir.path().join("err.log")),
        logs_tx,
        ctl_rx,
        pipes,
    );

    // More than one `READ_BUFFER`, so the reader is full and the pipe
    // still holds the rest; under `SMALLEST_PIPE`, so this write does
    // not park against a pump that has stopped draining.
    let lines = u32::try_from(SMALLEST_PIPE * 3 / 4 / RUN_LINE).unwrap();
    let run: String = (1..=lines).map(|n| format!("{n:07}\n")).collect();
    assert!(run.len() > READ_BUFFER, "the reader must fill");
    std::io::Write::write_all(&mut writer, run.as_bytes()).unwrap();
    wait_for_a_full_logs_channel(&logs).await;

    let (done, ack) = oneshot::channel();
    ctl.send(LogCtl::ReportFds { done }).await.unwrap();
    timeout(PUMP_DEADLINE, ack)
        .await
        .expect("a descriptor report must be acknowledged")
        .expect("the pump must answer rather than drop the acknowledgement");

    // The sheep goes quiet, so the inherited handle reaches EOF rather
    // than waiting for a writer that outlives the test.
    drop(writer);
    let rest = read_to_eof(inherited).await;
    let written = log_text(&out_path);
    assert!(
        run.starts_with(&written),
        "the pump must write the run's own bytes, in order"
    );
    assert!(
        run.ends_with(&rest),
        "what is left in the pipe must be the run's own tail"
    );
    assert!(
        written.len() + rest.len() >= run.len() - RUN_LINE,
        "{} of {} bytes reached neither the file ({}) nor the pipe ({}): the report read \
         them out of the pipe and then dropped them",
        run.len() - written.len() - rest.len(),
        run.len(),
        written.len(),
        rest.len()
    );
}

/// Fails if a report follows a sheep that is still writing instead of
/// rescuing what its reader already held.
///
/// A reader can strand at most one [`super::super::READ_BUFFER`], and
/// the drain writes whole lines out of that buffer without reading the
/// pipe behind it, so it cannot write more than the buffer held however
/// fast the sheep is writing. The slack is one line: `tokio::io::Lines`
/// may have been part-way through one before the buffer was filled.
#[tokio::test]
async fn a_report_drains_at_most_one_bufferful() {
    let mut pump = PumpHarness::start_over_pipes();
    // Three quarters of the smallest pipe: over `MAX_DRAIN`, so the
    // bound is really reached, and under the pipe, so the writer never
    // parks.
    let lines = u32::try_from(SMALLEST_PIPE * 3 / 4 / RUN_LINE).unwrap();
    let run: String = (1..=lines).map(|n| format!("{n:07}\n")).collect();
    assert!(run.len() > READ_BUFFER, "the run must reach the bound");
    pump.out_writer.write_all(run.as_bytes()).await.unwrap();
    wait_for_a_full_logs_channel(&pump.logs).await;
    pump.flush().await;
    let before = fs::metadata(&pump.out_path).unwrap().len();

    let _ = pump.report_fds().await;

    let drained = fs::metadata(&pump.out_path).unwrap().len() - before;
    assert!(
        drained > 0,
        "the report must rescue what the reader was holding"
    );
    // Bound is on the sheep's bytes, one bufferful plus a part-way
    // line, but measured on the file where each line carries a
    // `LOG_STAMP_BYTES` stamp. Counting the stamp in keeps this a bound
    // on the drain, not on stamp width.
    const RESCUED: usize = READ_BUFFER + RUN_LINE;
    let ceiling = u64::try_from(RESCUED + RESCUED / RUN_LINE * LOG_STAMP_BYTES).unwrap();
    assert!(
        drained <= ceiling,
        "the report drained {drained} bytes against a ceiling of {ceiling}, which is \
         more than one bufferful: it is following the sheep rather than catching up"
    );
}
