//! Log-file operations that run off the actor loop.
//!
//! Reopening after a rotation, flushing, and truncating all mean talking to a
//! sheep's log pump and waiting for it to answer. Each spawns a task so a
//! pump that has stopped reading cannot stall the actor.

use super::*;

/// Spawns the task that carries out one `Reopen` and answers its caller; must
/// be called from within a Tokio runtime context.
///
/// Every await a reopen needs lives in here, off the actor loop; see
/// [`Actor::handle_reopen`] for the cycle that rules out doing it inline, and
/// for why `pumps` is every writer to a path a sheep in `matched` writes to,
/// a wider set than `matched`, while the reply is not.
///
/// Visited one after another, unlike [`spawn_handover_task`]: the caller
/// carries `rpc`'s own per-request budget.
pub(super) fn spawn_reopen_task(
    matched: Vec<ProcessInfo>,
    pumps: Vec<(ProcessInfo, mpsc::Sender<LogCtl>)>,
    reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
) {
    tokio::spawn(async move {
        let mut failures = Vec::new();
        for (info, log_ctl) in &pumps {
            if let Err(error) = reopen_logs(log_ctl).await {
                // Named and id'd: the reply that would have said which sheep
                // these are is the one being replaced, and a widened set can
                // fail on a sheep the operator never named.
                failures.push(format!("{} (id {}): {error}", info.name, info.id));
            }
        }
        // Every pump is visited before anything is reported: one sheep whose
        // log directory is gone must not stop the rest being reopened.
        let _ = reply.send(if failures.is_empty() {
            Ok(matched)
        } else {
            Err(SupervisorError::ReopenFailed(failures.join("; ")))
        });
    });
}

/// Asks one sheep's log pump to reopen both of its files and waits for the
/// acknowledgement.
///
/// Not reaching a pump at all is a success: a failed send or a dropped
/// acknowledgement both mean there was no pump left to reopen anything.
///
/// # Errors
///
/// [`ReopenError`] if a pump answered and at least one of its two paths could
/// not be opened again. That sheep is now writing a stream nowhere.
pub(super) async fn reopen_logs(log_ctl: &mpsc::Sender<LogCtl>) -> Result<(), ReopenError> {
    let (done, ack) = oneshot::channel();
    if log_ctl.send(LogCtl::Reopen { done }).await.is_err() {
        return Ok(());
    }
    ack.await.unwrap_or(Ok(()))
}

/// Spawns the task that carries out one `Flush` and answers its caller; must
/// be called from within a Tokio runtime context.
///
/// Every pump in `pumps` is flushed before any path in `paths` is truncated:
/// `write_all` on a [`tokio::fs::File`] returns once the real `write(2)` is
/// queued, so a dispatched line can land at offset 0 of a file truncated in
/// between. One barrier is also the only ordering that stays correct when
/// several sheep share one path.
///
/// `pumps` is every writer to a path in `paths`, a wider set than `matched`;
/// see [`Actor::handle_flush`]. All of them are visited before anything is
/// reported.
pub(super) fn spawn_flush_task(
    matched: Vec<ProcessInfo>,
    pumps: Vec<mpsc::Sender<LogCtl>>,
    paths: BTreeSet<PathBuf>,
    reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
) {
    tokio::spawn(async move {
        let mut failures = Vec::new();

        for log_ctl in &pumps {
            if let Err(error) = flush_logs(log_ctl).await {
                failures.push(error.message);
            }
        }

        for path in paths {
            if let Err(error) = truncate_log(&path).await {
                failures.push(error.message);
            }
        }

        let _ = reply.send(if failures.is_empty() {
            Ok(matched)
        } else {
            Err(SupervisorError::FlushFailed(failures.join("; ")))
        });
    });
}

/// Asks one sheep's log pump to land everything it still owes both of its
/// files, and waits for the acknowledgement.
///
/// Not reaching a pump at all is a success, as in [`reopen_logs`]: a pump that
/// is gone owes no bytes. The truncate still runs, which is how a stopped
/// sheep's logs get emptied.
///
/// # Errors
///
/// [`FlushError`] if a pump answered and at least one stream's owed bytes
/// never reached its file. The truncate that follows runs regardless.
pub(super) async fn flush_logs(log_ctl: &mpsc::Sender<LogCtl>) -> Result<(), FlushError> {
    let (done, ack) = oneshot::channel();
    if log_ctl.send(LogCtl::Flush { done }).await.is_err() {
        return Ok(());
    }
    ack.await.unwrap_or(Ok(()))
}

/// Truncates the log file at `path` to zero length.
///
/// Exactly the path the Flockfile named, with no check on where it points: a
/// flush empties whatever an `out_file` names, under the daemon's own
/// privileges. The open goes through [`open_log_path`], which refuses a
/// symlink at the path. Not `create(true)`, so a missing path is a no-op.
///
/// # Errors
///
/// [`FlushError`] if the path could not be opened: an ancestry a privileged
/// shepherd will not write below, a symlink at the path, an unwritable mode,
/// a read-only filesystem, an IO error.
pub(super) async fn truncate_log(path: &Path) -> Result<(), FlushError> {
    let refused = |error: &dyn fmt::Display| FlushError {
        message: format!("{}: {error}", path.display()),
    };
    if let Err(error) = check_log_ancestry(path) {
        return Err(refused(&error));
    }

    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).truncate(true);
    match open_log_path(&mut options, path).await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(refused(&error)),
    }
}
