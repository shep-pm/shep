//! Tests for signals and stdin writes.
//!
//! A signal goes to the sheep's own process and not its group. A line only
//! reaches a sheep that asked for stdin, and a flock of wedged sheep is bounded
//! once rather than once each.

use super::*;

/// fails if `signal` reaches the group instead of the process. A supervisor
/// calling `signal` rather than `signal_process` would look correct in
/// every other respect and deliver SIGHUP to every lamb.
#[tokio::test(start_paused = true)]
async fn a_signal_reaches_the_sheeps_own_process_and_not_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, _events) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits()],
    )
    .await;

    let rows = handle
        .signal(ProcessSelector::Id(0), OperatorSignal::Hup)
        .await
        .unwrap();

    assert_eq!(
        rows,
        vec![SignalReply {
            id: 0,
            name: "web".to_string(),
            outcome: SignalOutcome::Delivered,
        }]
    );
    assert_eq!(runner.process_signals(0), vec![OperatorSignal::Hup]);
    assert!(
        runner.signals(0).is_empty(),
        "shep signal must not reach the process group"
    );
}

/// fails if a registered-but-dead sheep is reported as delivered.
/// `Delivered` is the only outcome that claims the kernel took the signal.
#[tokio::test(start_paused = true)]
async fn a_stopped_sheep_answers_not_running_rather_than_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, _events) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits()],
    )
    .await;
    handle.stop(ProcessSelector::Id(0)).await.unwrap();

    let rows = handle
        .signal(ProcessSelector::Id(0), OperatorSignal::Hup)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, SignalOutcome::NotRunning);
}

/// fails if a selector matching nothing is answered with an empty success.
/// It is `NotFound`, as for every other selector-taking verb.
#[tokio::test(start_paused = true)]
async fn a_selector_that_matches_nothing_is_not_found() {
    let h = harness(vec![]);
    let err = h
        .ctx
        .supervisor
        .signal(
            ProcessSelector::Name("ghost".to_string()),
            OperatorSignal::Hup,
        )
        .await
        .unwrap_err();
    assert_eq!(err, SupervisorError::NotFound);
}

/// fails if a reload drainee is skipped. `begin_action` skips one because
/// an action expects a reply; a signal expects nothing back, and the
/// drainee is a live process the selector matched. Actor-tier:
/// `ProcessEntry::reload` is crate-internal.
#[tokio::test(start_paused = true)]
async fn a_reload_drainee_is_signalled_like_any_other_live_sheep() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut signal_rx) = actor_with_a_drainee_holding_a_signal_mailbox(&dir);

    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Signal {
        selector: ProcessSelector::Id(0),
        sig: OperatorSignal::Hup,
        reply,
    });

    // The request really left the actor for the sheep task's mailbox.
    let request = tokio::time::timeout(ACTION_WINDOW, signal_rx.recv())
        .await
        .expect("no signal reached the drainee's mailbox within the window")
        .expect("the drainee's signal mailbox closed");
    assert_eq!(request.sig, OperatorSignal::Hup);
    // Answer it as a live sheep task would, so the fan-out can settle.
    let _ = request.done.send(Ok(()));

    let rows = tokio::time::timeout(ACTION_WINDOW, answer)
        .await
        .expect("the signal reported nothing within the window")
        .expect("the signal's reply channel was dropped")
        .unwrap();
    assert_eq!(rows[0].outcome, SignalOutcome::Delivered);
}

/// fails if a line does not reach the sheep's pipe. The fake records what
/// it was handed, so this asserts the line itself.
#[tokio::test(start_paused = true)]
async fn a_line_reaches_a_sheep_that_asked_for_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("repl", "./repl");
    app.stdin = true;
    let (handle, runner, _events) = started(&dir, app, vec![ProcScript::never_exits()]).await;

    let rows = handle
        .send_line(ProcessSelector::Id(0), "reload-config".to_string())
        .await
        .unwrap();

    assert_eq!(
        rows,
        vec![LineReply {
            id: 0,
            name: "repl".to_string(),
            outcome: LineOutcome::Sent,
        }]
    );
    assert_eq!(runner.stdin_lines(0), vec!["reload-config".to_string()]);
}

/// fails if a sheep without `stdin = true` is answered anything but
/// `no_stdin`, `Sent` above all: there is no pipe for a line to land in.
#[tokio::test(start_paused = true)]
async fn a_sheep_without_stdin_answers_no_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, _events) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits()],
    )
    .await;

    let rows = handle
        .send_line(ProcessSelector::Id(0), "hello".to_string())
        .await
        .unwrap();

    assert_eq!(rows[0].outcome, LineOutcome::NoStdin);
}

/// fails if a mixed flock is refused as a whole. Half the sheep having a
/// pipe is the normal case under `all`. `Reopen`, `Flush`, `Trigger` and
/// `Signal` follow the same rule.
#[tokio::test(start_paused = true)]
async fn a_mixed_flock_reports_per_sheep_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    let mut piped = AppConfig::minimal("repl", "./repl");
    piped.stdin = true;
    // `started` starts one app, the handle the second: the ids are 0 and 1.
    let (handle, runner, _events) = started(
        &dir,
        piped,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;
    handle
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let rows = handle
        .send_line(ProcessSelector::All, "hello".to_string())
        .await
        .unwrap();

    let outcome = |id| rows.iter().find(|r| r.id == id).unwrap().outcome.clone();
    assert_eq!(outcome(0), LineOutcome::Sent);
    assert_eq!(outcome(1), LineOutcome::NoStdin);
    assert_eq!(runner.stdin_lines(1), Vec::<String>::new());
    // id-sorted, like every other row-shaped reply.
    assert!(rows.windows(2).all(|w| w[0].id < w[1].id));
}

/// fails if a wait on an app that never reads its stdin has no bound. The
/// outcome names the bound: "the app is not reading" and "the pipe broke"
/// have different fixes.
#[tokio::test(start_paused = true)]
async fn a_write_that_never_lands_times_out_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("stuck", "./stuck");
    app.stdin = true;
    // `never_reads_its_stdin` accepts the write and answers nothing, which
    // is what a full pipe looks like from this side.
    let (handle, _runner, _events) =
        started(&dir, app, vec![ProcScript::never_reads_its_stdin()]).await;

    let rows = tokio::time::timeout(
        STDIN_WRITE_TIMEOUT * 4,
        handle.send_line(ProcessSelector::Id(0), "hello".to_string()),
    )
    .await
    .expect("send_line did not honour its own bound")
    .unwrap();

    let LineOutcome::NotWritten { reason } = rows[0].outcome.clone() else {
        panic!("expected NotWritten, got {:?}", rows[0].outcome);
    };
    assert!(reason.contains("read"), "{reason}");
}

/// fails if a flock of wedged sheep costs `STDIN_WRITE_TIMEOUT` each.
///
/// Two seconds sits under the 5s an RPC caller gets by default, and that
/// only holds if the waits run concurrently. Awaited in a `for` loop, three
/// wedged sheep cost six seconds and `shep whisper all` answers
/// `DeadlineExceeded` instead of three `not_written` rows.
#[tokio::test(start_paused = true)]
async fn a_flock_of_wedged_sheep_is_bounded_once_and_not_once_each() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("stuck", "./stuck");
    app.stdin = true;
    app.instances = 3;
    let (handle, _runner, _events) =
        started(&dir, app, vec![ProcScript::never_reads_its_stdin(); 3]).await;

    let started_at = tokio::time::Instant::now();
    let rows = handle
        .send_line(ProcessSelector::All, "hello".to_string())
        .await
        .unwrap();
    let elapsed = started_at.elapsed();

    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|row| matches!(row.outcome, LineOutcome::NotWritten { .. })),
        "{rows:?}"
    );
    // Under the paused clock the auto-advance is exact: sequential waits
    // would read 6s.
    assert!(
        elapsed < STDIN_WRITE_TIMEOUT * 2,
        "three wedged sheep cost {elapsed:?}; the bound is per-CALL, not per-sheep"
    );
}

/// fails if a selector matching nothing is answered with an empty
/// success.
#[tokio::test(start_paused = true)]
async fn a_selector_that_matches_nothing_is_not_found_for_send_line() {
    let h = harness(vec![]);
    assert_eq!(
        h.ctx
            .supervisor
            .send_line(ProcessSelector::Name("ghost".to_string()), "x".to_string())
            .await
            .unwrap_err(),
        SupervisorError::NotFound
    );
}
