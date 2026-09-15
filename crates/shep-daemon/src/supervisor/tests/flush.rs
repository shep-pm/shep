//! Tests for flushing and truncating log files.
//!
//! Truncation only happens once the pump has answered, or the bytes it was
//! still holding would land in the truncated file. Instances sharing one path
//! have to be flushed together even when the selector names only one of them.

use super::*;

/// Fails if a flush skips a matched sheep's pump, reaches a sheep the
/// selector never named, or answers with the wrong set.
///
/// The counts are what make this more than a smoke test, as in
/// [`super::reopen::a_reopen_reaches_every_matched_sheep_and_no_others`].
#[tokio::test(start_paused = true)]
async fn a_flush_reaches_every_matched_sheep_and_no_others() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Three scripts for three instances: a fourth spawn would land that
    // sheep pumpless, which reads like the skip being looked for.
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

    let flushed = handle
        .flush(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0, 1],
        "the reply must carry both `web` instances, id-sorted, and no `api`"
    );
    assert_eq!(runner.flushes(0), 1, "web's first instance");
    assert_eq!(runner.flushes(1), 1, "web's second instance");
    assert_eq!(runner.flushes(2), 0, "api was never named");
    assert_eq!(
        runner.reopens(0),
        0,
        "a flush must push `LogCtl::Flush`, never `LogCtl::Reopen` — a \
         flush wired to the neighbouring variant would swap the flock's \
         handles and empty nothing"
    );
}

/// Fails if the actor awaits a flush's acknowledgement inside its own loop,
/// the cycle
/// [`super::reopen::the_actor_keeps_answering_while_a_reopen_waits_on_a_silent_pump`]
/// describes, reached through the other verb. `list` is the probe: it is
/// answered from the actor loop and nowhere else.
#[tokio::test(start_paused = true)]
async fn the_actor_keeps_answering_while_a_flush_waits_on_a_silent_pump() {
    let (events, _rx) = crate::bus::test_bus(64);
    let (runner, mut requests) = SilentPumpRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    handle
        .start(vec![normalize(AppConfig::minimal("web", "./srv")).unwrap()])
        .await
        .unwrap();

    let flushing = tokio::spawn({
        let handle = handle.clone();
        async move { handle.flush(ProcessSelector::All).await }
    });

    tokio::time::timeout(Duration::from_secs(5), requests.wait_for(|seen| *seen == 1))
        .await
        .expect("the flush must reach the pump")
        .expect("the runner outlives this wait, so its sender cannot have closed");

    let listed = tokio::time::timeout(Duration::from_secs(5), handle.list())
        .await
        .expect("the actor must keep answering while a flush is outstanding");
    assert_eq!(listed.len(), 1);
    assert!(
        !flushing.is_finished(),
        "sanity: nothing can acknowledge this flush, so `list` answering \
         above is not just the flush having finished first"
    );
    flushing.abort();
}

/// What [`LateWritingPumpRunner`]'s pump appends as it answers a flush. One
/// owner: the cases below assert the file it lands in is empty.
const LATE_LINE: &str = "landed-while-the-flush-was-being-answered\n";

/// The sheep [`LateWritingPumpRunner`] gives a late-writing pump to.
const LATE_WRITING_SHEEP: &str = "latecomer";

/// A [`ScriptedRunner`] whose spawn of [`LATE_WRITING_SHEEP`] gets a pump
/// that appends [`LATE_LINE`] to that sheep's stdout log path as it
/// acknowledges a flush. Every other sheep keeps the fake's own pump.
///
/// A real [`tokio::fs::File`] hands its `write(2)` to the blocking pool, so
/// a `Flush` can arrive with bytes not yet in the file; the acknowledgement
/// is what says they landed.
struct LateWritingPumpRunner {
    inner: ScriptedRunner,
}

impl LateWritingPumpRunner {
    fn new(inner: ScriptedRunner) -> Self {
        Self { inner }
    }
}

impl fmt::Debug for LateWritingPumpRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LateWritingPumpRunner")
            .finish_non_exhaustive()
    }
}

impl ProcessRunner for LateWritingPumpRunner {
    type Proc = crate::fake::FakeProc;

    fn spawn(&self, spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), crate::runner::RunnerError> {
        let (proc, mut io) = self.inner.spawn(spec)?;
        if spec.name != LATE_WRITING_SHEEP {
            return Ok((proc, io));
        }
        let (tx, mut rx) = mpsc::channel(SHEEP_CTL_CAPACITY);
        // Replacing the sender drops the fake's own, ending the control
        // task it spawned. This pump answers in its place.
        io.log_ctl = tx;
        let out_file = spec.out_file.clone();
        tokio::spawn(async move {
            while let Some(ctl) = rx.recv().await {
                match ctl {
                    LogCtl::Flush { done } => {
                        if let Some(parent) = out_file.parent() {
                            std::fs::create_dir_all(parent).unwrap();
                        }
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&out_file)
                            .unwrap();
                        std::io::Write::write_all(&mut file, LATE_LINE.as_bytes()).unwrap();
                        // Answered only once the bytes are on disk.
                        let _ = done.send(Ok(()));
                    }
                    LogCtl::Reopen { done } => {
                        let _ = done.send(Ok(()));
                    }
                    // This runner exists for the flush ordering.
                    #[cfg(unix)]
                    LogCtl::ReportFds { done } => {
                        let _ = done.send(CarriedFds::none());
                    }
                    // Nothing to start reading again: no streams are read.
                    #[cfg(unix)]
                    LogCtl::Resume => {}
                }
            }
        });
        Ok((proc, io))
    }
}

/// Fails if the truncate runs before the pump has acknowledged the flush:
/// the file is emptied with a line still in flight, and the line lands at
/// offset 0 afterwards under `O_APPEND`.
///
/// That the file exists proves the pump was flushed at all, since the
/// truncate does not create a missing path; that it is empty proves the
/// truncate came second.
#[tokio::test(start_paused = true)]
async fn a_flush_truncates_only_after_its_pump_has_answered() {
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(
        LateWritingPumpRunner::new(ScriptedRunner::new(vec![ProcScript::never_exits()])),
        test_paths(&dir),
        events,
    );
    handle
        .start(vec![
            normalize(AppConfig::minimal(LATE_WRITING_SHEEP, "./srv")).unwrap(),
        ])
        .await
        .unwrap();

    // Read off the daemon's own snapshot, so the test cannot disagree with
    // the assembler about the path.
    let out_file = PathBuf::from(
        handle.list().await[0]
            .out_file
            .clone()
            .expect("the daemon reports its own resolved log paths"),
    );

    handle.flush(ProcessSelector::All).await.unwrap();

    assert!(
        out_file.exists(),
        "the pump never wrote, so this flush never reached one: {}",
        out_file.display()
    );
    assert_eq!(
        std::fs::read_to_string(&out_file).unwrap(),
        "",
        "a line the pump landed as it answered the flush must not survive \
         the truncate that follows it"
    );
}

/// Fails if a flush leaves a stopped sheep's log file alone. The operation
/// addresses recorded paths, not open handles, and `shep bleats
/// --no-follow` reads a stopped sheep's logs. The fake's control task ends
/// with its proc, so the truncate is reached through the no-pump leg.
#[tokio::test(start_paused = true)]
async fn a_stopped_sheeps_log_file_is_truncated_too() {
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

    let listed = handle.list().await;
    assert_eq!(listed[0].status, ProcStatus::Stopped);
    let out_file = PathBuf::from(listed[0].out_file.clone().unwrap());
    std::fs::create_dir_all(out_file.parent().unwrap()).unwrap();
    std::fs::write(&out_file, "what the sheep logged before it stopped\n").unwrap();

    let flushed = tokio::time::timeout(Duration::from_secs(5), handle.flush(ProcessSelector::All))
        .await
        .expect("a flush aimed at a stopped sheep must not wait for an acknowledgement")
        .expect("a stopped sheep has no pump to flush, which is not a failure");

    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].status, ProcStatus::Stopped);
    assert_eq!(
        std::fs::read_to_string(&out_file).unwrap(),
        "",
        "a stopped sheep's log is still readable, so it is still emptied"
    );
}

/// Fails if instances sharing one log path answer with one row per file
/// rather than one per sheep, or leave the shared file unemptied.
///
/// The answer is keyed by sheep, since the selector named sheep; the work
/// is keyed by path, since one truncate empties the file for every handle
/// open on it. The shared path is asserted rather than assumed.
#[tokio::test(start_paused = true)]
async fn instances_sharing_one_log_path_answer_one_row_each() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);

    let mut web = AppConfig::minimal("web", "./srv");
    web.instances = 2;
    web.merge_logs = true;
    handle.start(vec![normalize(web).unwrap()]).await.unwrap();

    let listed = handle.list().await;
    assert_eq!(
        listed[0].out_file, listed[1].out_file,
        "fixture check: `merge_logs` must really point both instances at \
         one path, or this case proves nothing"
    );
    let shared = PathBuf::from(listed[0].out_file.clone().unwrap());
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    std::fs::write(&shared, "both instances wrote here\n").unwrap();

    let flushed = handle.flush(ProcessSelector::All).await.unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0, 1],
        "one row per sheep, not per file emptied"
    );
    assert_eq!(std::fs::read_to_string(&shared).unwrap(), "");
}

/// Fails if the set of pumps a flush drains is narrowed back to the sheep
/// the selector matched, leaving a sibling sharing that file unflushed.
///
/// [`LateWritingPumpRunner`] is pointed at the unmatched sheep and the
/// shared file is not created up front, so existence proves the sibling's
/// pump was reached and emptiness that the truncate waited.
#[tokio::test(start_paused = true)]
async fn a_sibling_sharing_a_path_is_flushed_even_when_the_selector_skips_it() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Two scripts for two apps of one instance each: a third spawn would
    // land that sheep pumpless, which reads like the skipped pump.
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(
        LateWritingPumpRunner::new(ScriptedRunner::new(vec![
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ])),
        test_paths(&dir),
        events,
    );

    // Two apps pointed at one file rather than one app's two instances
    // under `merge_logs`: the sibling needs a name to aim the pump at.
    let shared = dir.path().join("shared-out.log");
    let mut named = AppConfig::minimal("web", "./srv");
    named.out_file = Some(shared.display().to_string());
    let mut sibling = AppConfig::minimal(LATE_WRITING_SHEEP, "./api");
    sibling.out_file = Some(shared.display().to_string());
    handle
        .start(vec![normalize(named).unwrap(), normalize(sibling).unwrap()])
        .await
        .unwrap();

    let listed = handle.list().await;
    assert_eq!(
        listed[0].out_file, listed[1].out_file,
        "fixture check: both apps must really resolve to one path, or \
         this case proves nothing"
    );

    let flushed = handle
        .flush(ProcessSelector::Name("web".to_string()))
        .await
        .unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![0],
        "the reply answers the selector: the sibling's file was emptied \
         too, but the operator never named the sibling and it is not a \
         row here"
    );
    assert!(
        shared.exists(),
        "the sibling's pump writes as it answers a flush, so a path that \
         is not there means it was never asked: {}",
        shared.display()
    );
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "",
        "a line the unmatched sibling landed as it answered must not \
         survive the truncate of the path it shares"
    );
}

/// Fails if a pump that could not land what it owed is reported as a
/// success. The failure is keyed by path rather than by sheep: see
/// [`SupervisorError::FlushFailed`]. The healthy sheep is the second half:
/// a failure must not stop the rest of the flock being flushed.
#[tokio::test(start_paused = true)]
async fn a_pump_that_could_not_flush_fails_the_request() {
    let (events, _rx) = crate::bus::test_bus(64);
    // Two scripts for two instances: a third spawn would land that sheep
    // pumpless.
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
            normalize(AppConfig::minimal(REFUSING_SHEEP, "./srv")).unwrap(),
            normalize(AppConfig::minimal("api", "./api")).unwrap(),
        ])
        .await
        .unwrap();

    let error = tokio::time::timeout(Duration::from_secs(5), handle.flush(ProcessSelector::All))
        .await
        .expect("a pump that answers must not leave the flush waiting")
        .expect_err("a flush a pump could not carry out must not answer Ok");

    assert_eq!(
        error,
        SupervisorError::FlushFailed(PUMP_REFUSAL.to_string()),
        "the failure must carry the path, and only the path that failed"
    );
    assert_eq!(
        scripted.flushes(1),
        1,
        "the healthy sheep must still have been flushed"
    );
}

/// Fails if a selector that matches nothing is answered as a success.
/// `flush` demands an explicit selector, so a zero exit would tell the
/// operator the logs they named are empty when nothing was touched.
#[tokio::test(start_paused = true)]
async fn a_flush_matching_nothing_is_not_found() {
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(ScriptedRunner::new(vec![]), test_paths(&dir), events);

    assert_eq!(
        handle.flush(ProcessSelector::All).await,
        Err(SupervisorError::NotFound)
    );
}

/// Fails if [`truncate_log`] gains a `create(true)`, or treats a missing
/// path as an error. A log file that is not there is already empty, and a
/// stray empty log at a path a rotator just renamed away is worse.
#[tokio::test]
async fn truncating_a_path_that_is_not_there_creates_nothing_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("never-started-out.log");

    assert_eq!(truncate_log(&missing).await, Ok(()));
    assert!(
        !missing.exists(),
        "a flush must not create the log file it did not find"
    );
}

/// Fails if [`truncate_log`]'s last arm swallows its error: a `_ => Ok(())`
/// beside the `NotFound` one, or a `NotFound` guard widened to every kind.
/// The case above cannot see it, since a missing path answers `Ok`.
///
/// A directory in the log's place fails `open(2)` for writing for every
/// uid, root included, so this cannot pass for the wrong reason.
#[tokio::test]
async fn truncating_a_path_that_is_a_directory_reports_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("web-out.log");
    std::fs::create_dir(&blocked).unwrap();

    let error = truncate_log(&blocked)
        .await
        .expect_err("a path that could not be truncated must not answer Ok");
    assert!(
        error
            .message
            .starts_with(&format!("{}: ", blocked.display())),
        "the failure must name the path it could not empty: {error}"
    );
}

/// Fails if [`truncate_log`] stops opening through [`open_log_path`]: drop
/// the `O_NOFOLLOW` it adds and `shep flush` empties whatever the symlink
/// points at, with the daemon's privileges.
///
/// The target's bytes prove nothing was emptied, the link still being a
/// link proves the open did not replace it, and the message names the
/// symlink rather than `ELOOP`. `#[cfg(unix)]`: `O_NOFOLLOW` and the
/// refusal are both unix-only.
#[cfg(unix)]
#[tokio::test]
async fn truncating_a_symlinked_log_path_refuses_and_leaves_its_target_alone() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("precious.txt");
    let link = dir.path().join("web-out.log");
    std::fs::write(&target, b"do not empty me").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let error = truncate_log(&link)
        .await
        .expect_err("a symlinked log path must not be truncated through");

    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"do not empty me",
        "the symlink's target must still hold every byte it did"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the refusal must leave the symlink itself in place, not replace it"
    );
    assert_eq!(
        error.message,
        format!("{}: {}", link.display(), crate::runner::SYMLINK_REFUSED),
        "the failure must name the path and say the word symlink: {error}"
    );
}

/// Fails if the set of pumps a flush drains is narrowed back to the sheep
/// the selector matched, leaving a reload's drainee appending to a file
/// being emptied under it. The same mechanism as
/// [`a_sibling_sharing_a_path_is_flushed_even_when_the_selector_skips_it`],
/// reached without configuring anything.
#[tokio::test(start_paused = true)]
async fn a_flush_naming_a_replacement_still_drains_the_drainee_sharing_its_path() {
    let dir = tempfile::tempdir().unwrap();
    // Two scripts: the original spawn and the one replacement. A third
    // would abandon the reload, leaving one entry and no overlap to see.
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    let mid = handle.list().await;
    assert_eq!(
        mid.len(),
        2,
        "fixture check: both halves of the swap must be registered, or \
         there is no shared path to widen to"
    );
    assert_eq!(
        mid[0].out_file, mid[1].out_file,
        "fixture check: one instance slot must really give both entries \
         one out path, or this case proves nothing"
    );
    assert_eq!(
        mid[0].err_file, mid[1].err_file,
        "fixture check: and one err path"
    );

    let flushed = handle.flush(ProcessSelector::Id(1)).await.unwrap();

    assert_eq!(
        flushed.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![1],
        "the reply answers the selector: the drainee's pump was drained \
         too, but the operator named only the replacement"
    );
    assert_eq!(
        runner.flushes(0),
        1,
        "the drainee's pump is what this case exists for — it is still \
         holding the file the truncate is about to empty"
    );
    assert_eq!(
        runner.flushes(1),
        1,
        "the replacement's pump, which the selector did name"
    );
}

/// Fails if a reopen is keyed on the selector alone, leaving a reload's
/// drainee holding the inode an external rotator has just renamed. The
/// drainee goes on appending to the archive while the recreated path takes
/// only the replacement's lines. The counts are the whole case.
#[tokio::test(start_paused = true)]
async fn a_reopen_naming_a_replacement_still_reaches_the_drainee_sharing_its_path() {
    let dir = tempfile::tempdir().unwrap();
    // Two scripts, counted, for the reason the flush case above gives.
    let (handle, runner, mut rx) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    handle
        .reload(ProcessSelector::Name("web".to_string()))
        .await
        .expect("the reload is accepted");
    expect_event(&mut rx, 1, ProcessEventKind::Start).await;

    let mid = handle.list().await;
    assert_eq!(
        mid.len(),
        2,
        "fixture check: both halves of the swap must be registered"
    );
    assert_eq!(
        mid[0].out_file, mid[1].out_file,
        "fixture check: one instance slot must really give both entries \
         one out path, or this case proves nothing"
    );
    assert_eq!(
        mid[0].err_file, mid[1].err_file,
        "fixture check: and one err path"
    );

    let reopened = handle.reopen(ProcessSelector::Id(1)).await.unwrap();

    assert_eq!(
        reopened.iter().map(|info| info.id).collect::<Vec<_>>(),
        vec![1],
        "the reply answers the selector, the same way `flush`'s does"
    );
    assert_eq!(
        runner.reopens(0),
        1,
        "the drainee's pump is what this case exists for — unasked, it \
         keeps the renamed inode open and goes on filling the archive"
    );
    assert_eq!(
        runner.reopens(1),
        1,
        "the replacement's pump, which the selector did name"
    );
    assert_eq!(
        runner.flushes(0),
        0,
        "a reopen must push `LogCtl::Reopen`, never `LogCtl::Flush` — the \
         neighbouring variant would land the drainee's owed bytes and \
         leave it on the renamed inode regardless"
    );
}
