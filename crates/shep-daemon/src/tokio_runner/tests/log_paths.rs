//! Where a sheep's log path lands and what happens to it: rotation, reopen,
//! truncation, and the symlink refusals `open_append` makes.

use std::fs;

use super::super::LOG_BUFFER;
use super::super::log_file::open_append;
use super::*;

/// Every line below fits in [`LOG_BUFFER`], so at the rename the file may hold
/// none of them: a reopen that dropped its handle without flushing would lose
/// the lot, and one that flushed after swapping handles would write them into
/// the fresh file.
///
/// Exact equality on both paths tells those apart from a reopen that got it right.
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
/// Fails if [`open_append`] stops opening through `open_log_path`: without its
/// `O_NOFOLLOW`, a symlink planted at a sheep's `out_file` would be followed,
/// and every line the sheep writes would land in whatever it points at. No
/// other case here can see this, since every other one opens a real file.
///
/// Asserts the target's bytes are unchanged. `cfg(unix)`: `O_NOFOLLOW` has no
/// Windows counterpart wired up yet.
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
/// Fails if the resolving open refuses every symlinked parent again: a link
/// the daemon's own user made (`current -> releases/<sha>`, a symlinked home)
/// is trusted by `loose_ancestor`, and a Linux sheep below one must keep its
/// log. The file lands in the directory the link points at.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_log_path_below_a_symlinked_parent_the_daemon_owns_opens_on_linux() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    let link = dir.path().join("logs");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    open_append(&link.join("web-out.log"))
        .await
        .expect("a link the daemon's own user owns is trusted");

    assert!(real.join("web-out.log").exists());
}
/// Fails if the resolving open drops `O_APPEND`: the handle would carry its
/// own offset across an external truncation and write past a sparse hole.
/// The same shape as [`a_reopened_handle_still_appends_so_a_truncation_leaves_no_hole`],
/// for a path that took the resolving open rather than a pump's reopen.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_plain_log_path_still_appends_after_the_resolving_open() {
    use tokio::io::AsyncWriteExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("web-out.log");

    let mut handle = open_append(&path).await.unwrap();
    handle.write_all(b"first line\n").await.unwrap();
    handle.flush().await.unwrap();

    // The copytruncate rotator: truncates in place under the live handle.
    fs::File::create(&path).unwrap();

    handle.write_all(b"second\n").await.unwrap();
    handle.flush().await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"second\n");
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

    // stdout at EOF with stderr still live: what a child that closes one stream
    // early leaves behind, and what the pump holds in between the two EOFs of
    // an ordinary exit. `shutdown` rather than `drop`: the reopen below is a
    // method on the harness, and moving a writer out leaves `pump` partly moved.
    pump.out_writer
        .shutdown()
        .await
        .expect("a duplex stream's write end must close");
    let_the_duplex_pump_settle().await;

    // Deleted rather than renamed, so the reopen is the only thing that
    // could put either path back.
    fs::remove_file(&pump.out_path).unwrap();
    fs::remove_file(&pump.err_path).unwrap();

    let outcome = pump.reopen_for_answer().await;
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
/// A path the pump cannot open leaves that stream with no file at all and
/// every later line dropped, so answering `Ok` tells a rotator its rotation
/// worked while the sheep logs into nothing, the same silent failure a reopen
/// exists to end, moved one layer up.
///
/// A directory in the log's place is the failure with no permission games in
/// it: `open(2)` on a directory fails for every uid, root included.
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
