//! Start, list and stop a real sheep, and what `reopen` and `flush` do to
//! the log file underneath it.

use super::*;

#[tokio::test]
async fn handshake_then_start_list_and_stop_a_real_sheep() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;
    assert_eq!(client.hello_ack().pid, std::process::id());
    assert_eq!(client.hello_ack().protocol, PROTOCOL_VERSION);

    // Subscribe before starting: the bus delivers from the moment you join.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let app = forever_app("sleeper");
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    assert_eq!(infos.len(), 1);
    let id = infos[0].id;
    let spawned_pid = infos[0].pid.expect("a real spawn reports a real pid");

    let online = client
        .await_process_event(id, ProcessEventKind::Online)
        .await;
    assert_eq!(online.pid, Some(spawned_pid));

    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1);
    assert_eq!(flock[0].status, ProcStatus::Online);
    assert_eq!(flock[0].pid, Some(spawned_pid));

    let stopped = client
        .request(Request::Stop {
            selector: SelectorSpec::All,
        })
        .await;
    let Response::Stopped(gone) = stopped.result.unwrap() else {
        panic!("expected stopped")
    };
    // The reply is deferred until the kill ladder finished: terminal.
    assert_eq!(gone[0].status, ProcStatus::Stopped);
    client.await_process_event(id, ProcessEventKind::Stop).await;

    fixture.shutdown().await;
}

#[tokio::test]
async fn log_lines_reach_a_log_subscriber() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["log.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let app = announce_app("chatty", "hello-flock");
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;

    let line = client.await_log_line(id).await;
    assert_eq!(line, "hello-flock");

    fixture.shutdown().await;
}

/// A log file's contents with the daemon's per-line timestamp taken off.
///
/// A missing or unreadable file reads as the empty string.
fn unstamped_file(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = String::new();
    for line in text.lines() {
        out.push_str(shep_core::logstamp::strip(line));
        out.push('\n');
    }
    out
}

/// Waits for `path` to hold exactly `expected`, failing at [`RECV_TIMEOUT`].
///
/// Polls: a line seen on the bus has had its write issued, not completed,
/// since `tokio::fs` dispatches the real `write(2)` to the blocking pool.
async fn await_file_contents(path: &std::path::Path, expected: &str) {
    let settled = tokio::time::timeout(RECV_TIMEOUT, async {
        while unstamped_file(path) != expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "{}: expected {expected:?}, found {:?}",
        path.display(),
        std::fs::read_to_string(path)
    );
}

/// Both halves are asserted: a pump that opened a second handle without
/// dropping the first would grow the new file too, and only the archive
/// standing still rules that out.
#[tokio::test]
async fn reopen_moves_a_running_sheeps_log_onto_the_recreated_path() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    // Subscribe before starting: a connection gets no events until it does.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["log.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    // The marker makes "after the reopen" a fact rather than a timing bet.
    let marker = fixture.paths.home.join("go");
    let app = gated_announce_app("rotator", &marker);
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;
    let out_file = std::path::PathBuf::from(
        infos[0]
            .out_file
            .clone()
            .expect("this daemon reports its own resolved log paths"),
    );

    assert_eq!(client.await_log_line(id).await, "before");
    await_file_contents(&out_file, "before\n").await;

    let archive = out_file.with_extension("log.1");
    std::fs::rename(&out_file, &archive).unwrap();
    assert!(!out_file.exists(), "sanity: the rename really moved it");

    let reopened = client
        .request(Request::Reopen {
            selector: SelectorSpec::All,
        })
        .await;
    let Response::Reopened(matched) = reopened.result.unwrap() else {
        panic!("expected reopened")
    };
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].id, id);

    // The reply is the barrier: the pump has flushed the old handle and
    // opened the path again, so neither of these polls.
    assert_eq!(unstamped_file(&out_file), "");
    assert_eq!(unstamped_file(&archive), "before\n");

    std::fs::write(&marker, "").unwrap();
    assert_eq!(client.await_log_line(id).await, "after");
    await_file_contents(&out_file, "after\n").await;
    assert_eq!(
        unstamped_file(&archive),
        "before\n",
        "the renamed file must stop growing the moment the handle is swapped"
    );

    fixture.shutdown().await;
}

#[cfg(unix)]
/// The case a rotator that moves the directory aside rather than the files
/// produces.
///
/// Under `umask 0o077` a plain `create_dir_all` lands `0o700` unaided and both
/// implementations look alike here; narrowing that would take a process-wide
/// umask, which is `unsafe` and leaks into every other case in this binary.
#[tokio::test]
async fn reopen_recreates_a_removed_log_directory_owner_only() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    // The marker lives beside the log directory, not inside it: removing that
    // directory must not disturb it.
    let marker = fixture.paths.home.join("go");
    let app = gated_announce_app("rotator", &marker);
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;
    let out_file = std::path::PathBuf::from(
        infos[0]
            .out_file
            .clone()
            .expect("this daemon reports its own resolved log paths"),
    );
    await_file_contents(&out_file, "before\n").await;

    // The whole directory, not the file: `mkdir`'s mode governs only the
    // directories a call creates.
    std::fs::remove_dir_all(&fixture.paths.logs).unwrap();
    assert!(
        !fixture.paths.logs.exists(),
        "sanity: the log directory really is gone"
    );

    let reopened = client
        .request(Request::Reopen {
            selector: SelectorSpec::All,
        })
        .await;
    let Response::Reopened(matched) = reopened.result.unwrap() else {
        panic!("expected reopened")
    };
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].id, id);

    let mode = std::fs::metadata(&fixture.paths.logs)
        .expect("a reopen must put the log directory back")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, DIR_MODE,
        "the recreated log directory must be {DIR_MODE:o}, found {mode:o}"
    );

    // The reply is the barrier: both handles are open on the recreated path,
    // so the next line says the directory is usable and not merely present.
    std::fs::write(&marker, "").unwrap();
    await_file_contents(&out_file, "after\n").await;

    fixture.shutdown().await;
}

/// What the flush case writes at the live log path after renaming the real
/// one away, standing in for the file a `create`-mode rotator leaves behind.
const STRAY_CONTENT: &str = "what the recreated log holds\n";

/// [`STRAY_CONTENT`] keeps the recorded path and the pump's inode
/// distinguishable: without it the live path would simply be missing and the
/// truncate would be the documented no-op.
#[tokio::test]
async fn flush_empties_the_recorded_path_and_leaves_a_renamed_archive_alone() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    // Subscribe before starting: a connection gets no events until it does.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["log.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let app = announce_app("noisy", "before");
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;
    let out_file = std::path::PathBuf::from(
        infos[0]
            .out_file
            .clone()
            .expect("this daemon reports its own resolved log paths"),
    );

    assert_eq!(client.await_log_line(id).await, "before");
    await_file_contents(&out_file, "before\n").await;

    // From here the pump's handle and the recorded path name different files.
    let archive = out_file.with_extension("log.1");
    std::fs::rename(&out_file, &archive).unwrap();
    std::fs::write(&out_file, STRAY_CONTENT).unwrap();

    let flushed = client
        .request(Request::Flush {
            selector: SelectorSpec::All,
        })
        .await;
    let Response::Flushed(matched) = flushed.result.unwrap() else {
        panic!("expected flushed")
    };
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].id, id);

    // The reply is the barrier: every matched pump has answered, so neither
    // of these polls.
    assert_eq!(
        unstamped_file(&out_file),
        "",
        "the recorded path is what a flush empties"
    );
    assert_eq!(
        unstamped_file(&archive),
        "before\n",
        "the renamed file is not the daemon's to empty — a flush that chased \
         the pump's inode would have emptied this one instead"
    );

    fixture.shutdown().await;
}
