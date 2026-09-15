//! Tests for reopening log files after a rotation.
//!
//! A reopen has to reach the pump a restart spawned rather than the one it
//! replaced, and must not stall the actor when a pump has stopped reading or
//! has already gone.

use super::*;

/// Fails if the actor awaits a reopen's acknowledgement inside its own
/// loop. An actor parked on one stops draining its mailbox, so its sheep
/// tasks block sending into it, so nothing drains their `logs`.
///
/// `list` is the probe: it is answered from the actor loop and nowhere
/// else, and the request must reach the pump first for it to mean anything.
#[tokio::test(start_paused = true)]
async fn the_actor_keeps_answering_while_a_reopen_waits_on_a_silent_pump() {
    let (events, _rx) = crate::bus::test_bus(64);
    let (runner, mut requests) = SilentPumpRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let reopening = tokio::spawn({
        let handle = handle.clone();
        async move { handle.reopen(ProcessSelector::All).await }
    });

    tokio::time::timeout(Duration::from_secs(5), requests.wait_for(|seen| *seen == 1))
        .await
        .expect("the reopen must reach the pump")
        .expect("the runner outlives this wait, so its sender cannot have closed");

    let listed = tokio::time::timeout(Duration::from_secs(5), handle.list())
        .await
        .expect("the actor must keep answering while a reopen is outstanding");
    assert_eq!(listed.len(), 1);
    assert!(
        !reopening.is_finished(),
        "sanity: nothing can acknowledge this reopen, so `list` answering \
         above is not just the reopen having finished first"
    );
    reopening.abort();
}

/// Fails if a reopen skips a matched sheep's pump, reaches a sheep the
/// selector never named, or answers with the wrong set.
///
/// The counts are what make this more than a smoke test: three sheep and a
/// selector naming two of them catches both too narrow and too wide.
#[tokio::test(start_paused = true)]
async fn a_reopen_reaches_every_matched_sheep_and_no_others() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Three scripts for three instances: a fourth spawn would land that
    // sheep `Errored` with no pump, which reads like the skip under test.
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);

    let mut web = AppConfig::minimal("web", "./srv");
    web.instances = 2;
    handle
        .start(vec![
            normalize(web).unwrap(),
            normalize(AppConfig::minimal("api", "./api")).unwrap(),
        ])
        .await
        .unwrap();

    let reopened = handle
        .reopen(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();

    assert_eq!(
        reopened.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0, 1],
        "the reply must carry both `web` instances, id-sorted, and no `api`"
    );
    assert_eq!(runner.reopens(0), 1, "web's first instance");
    assert_eq!(runner.reopens(1), 1, "web's second instance");
    assert_eq!(runner.reopens(2), 0, "api was never named");
}

/// Fails if a respawn leaves [`SheepSlot::log_ctl`] pointing at the pump of
/// the process it replaced: `slot.log_ctl = Some(log_ctl);` dropped from
/// [`Actor::respawn`]. The send then fails, a failed send is the documented
/// no-op success, and `shep reopen` exits 0 having reached nothing.
#[tokio::test(start_paused = true)]
async fn a_reopen_after_a_restart_reaches_the_pump_the_restart_spawned() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events);
    handle
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let restarted = handle
        .restart(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();
    // The premise, stated rather than assumed: a reopen aimed at a sheep
    // that never restarted would reach the first pump and prove nothing.
    assert_eq!(
        (restarted[0].restarts, restarted[0].status),
        (1, ProcStatus::Online),
        "the sheep must be back up on a second process before the reopen"
    );

    let reopened =
        tokio::time::timeout(Duration::from_secs(5), handle.reopen(ProcessSelector::All))
            .await
            .expect("a live pump must answer rather than leave the reopen waiting")
            .expect("a running sheep's reopen must succeed");

    assert_eq!(reopened.len(), 1);
    assert_eq!(
        runner.reopens(1),
        1,
        "the reopen must reach the pump the restart spawned"
    );
    assert_eq!(
        runner.reopens(0),
        0,
        "the pre-restart pump belongs to a process that is gone; a reopen \
         sent there reaches nothing and is reported as a success"
    );
}

/// Fails if a selector that matches nothing is answered as a success.
/// `reopen` is the one selector verb with a default, so silence would look
/// like a rotation that worked.
#[tokio::test(start_paused = true)]
async fn a_reopen_matching_nothing_is_not_found() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);

    assert_eq!(
        handle.reopen(ProcessSelector::All).await,
        Err(SupervisorError::NotFound)
    );
}

/// Fails if a stopped sheep makes a reopen error out, or hang waiting for
/// an acknowledgement that cannot come. The fake's control task ends with
/// its proc, so this sheep's pump is gone by the time the reopen is issued.
#[tokio::test(start_paused = true)]
async fn a_stopped_sheep_is_a_no_op_success() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("web", "./srv");
    app.autorestart = false;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();
    handle
        .stop(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();
    assert_eq!(handle.list().await[0].status, ProcStatus::Stopped);

    let reopened =
        tokio::time::timeout(Duration::from_secs(5), handle.reopen(ProcessSelector::All))
            .await
            .expect("a reopen aimed at a stopped sheep must not wait for an acknowledgement")
            .expect("a stopped sheep has nothing to reopen, which is a success");
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].status, ProcStatus::Stopped);
}

/// Fails if [`reopen_logs`] waits on an acknowledgement nobody is left to
/// send: the leg where the pump is gone and the send itself fails.
#[tokio::test(start_paused = true)]
async fn a_reopen_whose_pump_is_already_gone_returns_at_once() {
    let (tx, rx) = mpsc::channel(1);
    drop(rx); // the pump ended before the request was made

    let outcome = tokio::time::timeout(Duration::from_secs(5), reopen_logs(&tx))
        .await
        .expect("a failed send must end the reopen, not leave it waiting");
    assert_eq!(
        outcome,
        Ok(()),
        "a pump that was never reached reopened nothing, which is a no-op \
         success rather than a reopen that failed"
    );
}

/// Fails if [`reopen_logs`] keeps waiting on a dropped acknowledgement: the
/// leg where the pump ends between accepting the request and answering it.
#[tokio::test(start_paused = true)]
async fn a_pump_that_ends_mid_request_still_ends_the_reopen() {
    let (tx, mut rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let request = rx.recv().await.expect("the reopen must reach the pump");
        drop(request); // ends without answering, exactly as a closing pump does
    });

    let outcome = tokio::time::timeout(Duration::from_secs(5), reopen_logs(&tx))
        .await
        .expect("a dropped acknowledgement must end the reopen, not leave it waiting");
    assert_eq!(
        outcome,
        Ok(()),
        "a pump that ended mid-request reopened nothing, which is the same \
         no-op success a failed send is"
    );
}

/// The flush half of
/// [`a_pump_that_ends_mid_request_still_ends_the_reopen`]. A pump that
/// ended mid-request owes no bytes, but the truncate still has to run.
#[tokio::test(start_paused = true)]
async fn a_pump_that_ends_mid_request_still_ends_the_flush() {
    let (tx, mut rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let request = rx.recv().await.expect("the flush must reach the pump");
        drop(request); // ends without answering, exactly as a closing pump does
    });

    let outcome = tokio::time::timeout(Duration::from_secs(5), flush_logs(&tx))
        .await
        .expect("a dropped acknowledgement must end the flush, not leave it waiting");
    assert_eq!(
        outcome,
        Ok(()),
        "a pump that ended mid-request owes no bytes, which is the same \
         no-op success a failed send is"
    );
}

/// Fails if a pump that could not reopen its files is reported as a
/// success. That sheep writes a stream nowhere while `shep reopen` exits 0.
/// The healthy sheep is the second half: a failure must name its own sheep
/// and must not stop the rest of the flock being reopened.
#[tokio::test(start_paused = true)]
async fn a_pump_that_could_not_reopen_fails_the_request_and_names_its_sheep() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Two scripts for two instances: a third spawn would land that sheep
    // `Errored` with no pump at all.
    let scripted = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(
        FailingPumpRunner::new(Arc::clone(&scripted)),
        test_paths(&dir),
        events,
    );
    handle
        .start(vec![
            normalize(AppConfig::minimal("web", "./srv")).unwrap(),
            normalize(AppConfig::minimal("api", "./api")).unwrap(),
        ])
        .await
        .unwrap();

    let error = tokio::time::timeout(Duration::from_secs(5), handle.reopen(ProcessSelector::All))
        .await
        .expect("a pump that answers must not leave the reopen waiting")
        .expect_err("a reopen a pump could not carry out must not answer Ok");

    assert_eq!(
        error,
        SupervisorError::ReopenFailed(format!("web (id 0): could not reopen {PUMP_REFUSAL}")),
        "the failure must carry the sheep and the path, and only the sheep that failed"
    );
    assert_eq!(
        scripted.reopens(1),
        1,
        "the healthy sheep must still have been reopened"
    );
}
