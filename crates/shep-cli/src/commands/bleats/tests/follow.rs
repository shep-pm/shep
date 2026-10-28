//! The bus-subscribed path: everything `--follow` (the default) delivers
//! through a live [`shep_client::EventStream`], from name resolution and
//! selector filtering through the shutdown, lag and dropped notices a
//! follow can end on.
//!
//! Every run here goes through [`run_bleats`], or [`run_bleats_quiet`]
//! where the test is about `quiet`. The two that cannot are the `ctrl_c`
//! test, which ends its follow with an injected interrupt rather than a
//! closed connection, and the broken-pipe test, which hands `Streams` a
//! writer that always fails.

use super::*;

/// `daemon.close_after_subscribe()`, not `daemon.close()`: it ends the
/// connection only after the real `Subscribe` this test's `bleats` call
/// issues has been served and every `push`ed event flushed, so a follow
/// running to end-of-stream observes everything in order regardless of
/// scheduling.
#[tokio::test]
async fn ids_resolve_to_names_from_one_listing_and_unknown_ids_render_bare() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "hello".into(),
        })
        .await;
    daemon
        .push(BusEvent::LogOut {
            id: 9,
            line: "orphan".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("all"), Format::Table).await;

    assert!(out.contains("web") && out.contains("hello"));
    assert!(
        out.contains('9') && out.contains("orphan"),
        "an unknown id renders bare, not blocked on: {out}"
    );
    assert_eq!(
        daemon.list_flock_count(),
        1,
        "one listing, not one per unknown line"
    );
}

/// Same `close_after_subscribe` reasoning as the test above.
#[tokio::test]
async fn err_and_out_filter_the_two_streams() {
    for (args, kept, gone) in [
        (follow_args_err("all"), "to-stderr", "to-stdout"),
        (follow_args_out("all"), "to-stdout", "to-stderr"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_push(&path).await;
        daemon.reply_to_list(vec![info(1, "web")]);
        daemon
            .push(BusEvent::LogOut {
                id: 1,
                line: "to-stdout".into(),
            })
            .await;
        daemon
            .push(BusEvent::LogErr {
                id: 1,
                line: "to-stderr".into(),
            })
            .await;
        daemon.close_after_subscribe().await;

        let (_code, rendered, _err) = run_bleats(&client, &args, Format::Table).await;

        assert!(
            rendered.contains(kept),
            "{kept} should have survived: {rendered}"
        );
        assert!(
            !rendered.contains(gone),
            "{gone} should have been filtered: {rendered}"
        );
    }
}

/// The daemon's topic filter globs on `log.out` / `log.err`, which
/// carry no sheep identity, so this filtering must happen client-side.
#[tokio::test]
async fn a_selector_filters_client_side_on_the_resolved_id_set() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web"), info(2, "worker")]);
    // The fake queues BOTH; only the selector may narrow them.
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "from-web".into(),
        })
        .await;
    daemon
        .push(BusEvent::LogOut {
            id: 2,
            line: "from-worker".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("web"), Format::Table).await;

    assert!(out.contains("from-web"));
    assert!(
        !out.contains("from-worker"),
        "the selector must narrow the resolved id set: {out}"
    );
}

/// A follow always knows which sheep wrote a line, the daemon emitting
/// `BusEvent::LogOut` per sheep, so it labels a multi-instance app's
/// lines with their slot even though the two rows here would share a
/// file on the backlog path.
#[tokio::test]
async fn a_multi_instance_apps_follow_labels_its_lines_with_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![
        info_with_instance(1, "web", 0),
        info_with_instance(2, "web", 1),
    ]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "from-slot-0".into(),
        })
        .await;
    daemon
        .push(BusEvent::LogOut {
            id: 2,
            line: "from-slot-1".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("web"), Format::Table).await;

    assert!(
        out.contains("web:0 | from-slot-0"),
        "a followed line must carry its slot: {out}"
    );
    assert!(
        out.contains("web:1 | from-slot-1"),
        "a followed line must carry its slot: {out}"
    );
}

/// A selector narrowed to one instance must not change how it is
/// labelled: `instance_count` counts over the whole cache, so `web:0`
/// still prints `web:0` even though the cache holds a `web:1` this
/// selector excludes.
#[tokio::test]
async fn a_selector_narrowed_to_one_instance_does_not_change_its_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![
        info_with_instance(1, "web", 0),
        info_with_instance(2, "web", 1),
    ]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "from-slot-0".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("web:0"), Format::Table).await;

    assert!(
        out.contains("web:0 | from-slot-0"),
        "a selector narrowed to one instance must not strip its label: {out}"
    );
}

/// Guards `resolved_instance`'s "more than one instance registered"
/// check: this row carries `.instance(Some(0))`, so returning it
/// unconditionally would still print `web:0` here.
#[tokio::test]
async fn a_single_instance_app_with_a_slot_on_its_row_still_follows_bare() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info_with_instance(1, "web", 0)]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "hello".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("web"), Format::Table).await;

    assert!(
        out.contains("web | hello"),
        "one registered instance means no slot to report, even though this row \
             carries one: {out}"
    );
    assert!(
        !out.contains("web:0"),
        "a slot must not leak onto a single-instance app's followed output: {out}"
    );
}

/// Guards `handle_event`'s `LogErr` arm specifically: it threads
/// `instance` on its own, separately from `LogOut`'s. A follow that
/// labelled stdout but not stderr passes every other test in this
/// module and fails only this one.
#[tokio::test]
async fn a_multi_instance_apps_followed_stderr_line_carries_its_slot_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![
        info_with_instance(1, "web", 0),
        info_with_instance(2, "web", 1),
    ]);
    daemon
        .push(BusEvent::LogErr {
            id: 2,
            line: "boom".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("web"), Format::Table).await;

    assert!(
        out.contains("web:1 | boom"),
        "a followed stderr line must carry its slot exactly as stdout does: {out}"
    );
}

/// The map a follow builds once must answer every name exactly as
/// [`instance_count`] does, the scan the backlog path still calls per
/// file: three rows for `web`, one for `worker`, one for a dog.
#[test]
fn instance_counts_agree_with_instance_count_for_every_name() {
    let rows = [
        info_with_instance(1, "web", 0),
        info_with_instance(2, "web", 1),
        info_with_instance(3, "web", 2),
        info(4, "worker"),
        ProcessInfo::builder(5, "metrics", ProcStatus::Online)
            .dog(Some(shep_core::protocol::DogSource::BuiltIn {
                grants: None,
            }))
            .build(),
    ];
    let cache: HashMap<u32, ProcessInfo> = rows.iter().map(|row| (row.id, row.clone())).collect();
    let counts = instance_counts(&cache);

    assert_eq!(counts.len(), 3, "one entry per distinct name");
    for (name, want) in [("web", 3usize), ("worker", 1), ("metrics", 1)] {
        assert_eq!(
            counts.get(name).copied(),
            Some(want),
            "{name} carries {want} rows of this cache: {counts:?}"
        );
        assert_eq!(
            counts.get(name).copied(),
            Some(instance_count(&cache, name)),
            "the map must answer {name} the way a scan of the cache does: {counts:?}"
        );
    }
    assert_eq!(
        (counts.get("api"), instance_count(&cache, "api")),
        (None, 0),
        "a name the cache holds no row for is absent from the map, not zero"
    );
    assert_eq!(
        (counts.get("api"), instance_count(&cache, "api")),
        (None, 0),
        "a name the cache holds no row for is absent from the map, not zero"
    );
}

/// An id the one listing never saw has no row to count, so it gets no slot
/// either: [`resolved_instance`] gives up on the same lookup
/// [`resolved_name`] does, even though the cache it consults is full of
/// multi-instance rows.
#[tokio::test]
async fn a_name_missing_from_the_cache_has_no_instance_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![
        info_with_instance(1, "web", 0),
        info_with_instance(2, "web", 1),
    ]);
    daemon
        .push(BusEvent::LogOut {
            id: 99,
            line: "orphan".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("all"), Format::Table).await;

    assert!(
        out.contains("99 | orphan"),
        "an unlisted id renders bare, with no slot to borrow from the \
             multi-instance cache beside it: {out}"
    );
    assert!(
        !out.contains("99:"),
        "a label is a slot this row was never registered with: {out}"
    );
}

/// The stream stays open for the whole test, so only the injected
/// interrupt can end this follow; ignoring it hangs and the timeout
/// fails the test.
///
/// Not on [`run_bleats`]: `bleats_with_signal` and `join!` hold the
/// buffers while the interrupt arm races the follow.
#[tokio::test]
async fn ctrl_c_during_a_follow_exits_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "still running".into(),
        })
        .await;

    let (interrupt_tx, interrupt_rx) = tokio::sync::oneshot::channel::<()>();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams::bare(&mut out, &mut err, Format::Table);

    let args = follow_args("all");
    let follow = bleats_with_signal(&client, &mut streams, false, &args, async {
        let _ = interrupt_rx.await;
    });
    let (_, code) = tokio::join!(
        async {
            tokio::task::yield_now().await;
            let _ = interrupt_tx.send(()); // a oneshot stays ready once sent
        },
        tokio::time::timeout(RUN_TIMEOUT, follow),
    );
    assert_eq!(
        code.expect("the interrupt arm must end the follow"),
        ExitCode::Success,
        "a user ending a follow deliberately has not failed"
    );
}

/// Both end in `DaemonUnreachable`, so the exit code alone discriminates
/// nothing; the notice text is the behaviour under test.
///
/// `close_after_subscribe`, not `close()`: this test needs the
/// connection to end mid-follow, deterministically, right after
/// `Subscribe` is served.
#[tokio::test]
async fn a_daemon_shutdown_mid_follow_is_announced_before_the_stream_ends() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon.push(BusEvent::DaemonShutdown).await; // scripted: emitted after Subscribe
    daemon.close_after_subscribe().await; // scripted: after Subscribe is served

    let (code, _out, err) = run_bleats(&client, &follow_args("all"), Format::Table).await;

    assert_eq!(code, ExitCode::DaemonUnreachable);
    assert!(
        err.contains("shutting down"),
        "the shutdown notice is what distinguishes this from the connection simply ending"
    );
}

#[tokio::test]
async fn a_stream_that_just_ends_reports_no_shutdown_notice() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon.close_after_subscribe().await; // no DaemonShutdown event at all

    let (code, _out, err) = run_bleats(&client, &follow_args("all"), Format::Table).await;

    assert_eq!(code, ExitCode::DaemonUnreachable);
    assert!(
        !err.contains("shutting down"),
        "a notice the daemon never sent must not be invented"
    );
}

/// Same shutdown scenario as
/// [`a_daemon_shutdown_mid_follow_is_announced_before_the_stream_ends`],
/// with `quiet: true`: the exit code must not move, only the notice
/// text.
#[tokio::test]
async fn quiet_suppresses_the_daemon_shutdown_notice_but_not_the_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon.push(BusEvent::DaemonShutdown).await;
    daemon.close_after_subscribe().await;

    let (code, _out, err) =
        run_bleats_quiet(&client, &follow_args("all"), Format::Table, true).await;

    assert_eq!(
        code,
        ExitCode::DaemonUnreachable,
        "quiet must not change the exit code, only whether the notice prints"
    );
    assert!(
        err.is_empty(),
        "quiet must suppress the shutdown notice entirely"
    );
}

/// `resolved_name`/`write_line` never see `quiet` (their call sites are
/// unconditional in `handle_event`), so this checks end-to-end that a
/// sheep's own line still reaches `streams.out` under `quiet: true`.
#[tokio::test]
async fn quiet_does_not_suppress_a_sheeps_own_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "hello".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) =
        run_bleats_quiet(&client, &follow_args("all"), Format::Table, true).await;

    assert!(
        out.contains("web") && out.contains("hello"),
        "quiet must never touch a sheep's own line: {out}"
    );
}

/// Depends on the default current-thread runtime: `overrun_by` pushes
/// `EVENT_CHANNEL_CAPACITY + n` events in one burst, and only a receiver
/// that has not yet been scheduled falls behind enough to see a `Lagged`.
///
/// `cfg(unix)`: a named pipe wakes its reader on a different schedule, so
/// on Windows the receiver keeps pace and the lag never triggers.
#[cfg(unix)]
#[tokio::test]
async fn a_lag_notice_reaches_stderr_and_the_follow_continues() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon.overrun_by(8).await; // forces a Lagged item
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "after".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, err) = run_bleats(&client, &follow_args("all"), Format::Table).await;

    assert!(
        err.contains("dropped") || err.contains("lagged"),
        "a lag must be told, not swallowed: {err}"
    );
    assert!(out.contains("after"), "a lag ends the gap, not the follow");
}

/// Fails if `BusEvent::Dropped` falls into `handle_event`'s `_ =>
/// Ok(())` catch-all and vanishes.
///
/// `Dropped` (the daemon's queue) and `Lagged` (this client's receiver
/// falling behind) must read differently, so this asserts the
/// daemon-side wording specifically: a bare `stderr.contains("dropped")`
/// would also pass if the `Lagged` arm's wording were reused by mistake.
#[tokio::test]
async fn a_dropped_notice_reaches_stderr_worded_for_the_daemon_side_cause() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon.push(BusEvent::Dropped { count: 5 }).await;
    daemon.close_after_subscribe().await;

    let (_code, _out, err) = run_bleats(&client, &follow_args("all"), Format::Table).await;

    assert!(
        err.contains("daemon") && err.contains('5'),
        "a daemon-side Dropped must not be silently swallowed: {err}"
    );
    assert!(
        !err.contains("locally"),
        "Dropped is the daemon's queue overflowing, not this client \
             falling behind reading its own socket — reusing the `Lagged` \
             arm's wording would blame the wrong side: {err}"
    );
}

/// Every other test in this module uses `Format::Table`, so a JSON
/// line shape change (a renamed field, or rendering table rows under
/// `--format json`) would leave every other test green.
#[tokio::test]
async fn json_format_renders_the_pinned_six_key_line_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon
        .push(BusEvent::LogErr {
            id: 1,
            line: "boom".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let (_code, out, _err) = run_bleats(&client, &follow_args("all"), Format::Json).await;

    let line = out.lines().next().expect("one JSON line was rendered");
    let json: serde_json::Value = serde_json::from_str(line).unwrap();
    let obj = json.as_object().expect("a bleats JSON line is an object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["id", "instance", "line", "name", "schema_version", "stream"],
        "the bleats JSON line shape is a stability surface: {out}"
    );
    assert_eq!(json["stream"], "err", "the stream this line came from");
    assert_eq!(
        json["instance"],
        serde_json::Value::Null,
        "one registered instance means no slot to report"
    );
}

/// A writer that always fails with `BrokenPipe`: `shep bleats | head`
/// closing the reading end is the normal way this streaming verb ends,
/// not an error.
struct BrokenPipeWriter;

impl io::Write for BrokenPipeWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `write_outcome` already treats a `BrokenPipe` write as
/// [`ExitCode::Success`]; this exercises that path through an actual
/// write failure, on the very first event.
///
/// Not on [`run_bleats`]: this run's stdout is the always-failing writer,
/// not a buffer.
#[tokio::test]
async fn a_broken_pipe_while_writing_a_line_exits_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&path).await;
    daemon.reply_to_list(vec![info(1, "web")]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "hello".into(),
        })
        .await;
    daemon.close_after_subscribe().await;

    let mut out = BrokenPipeWriter;
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &follow_args("all")),
        )
        .await
        .expect("the scripted run ends on its own, not by hanging")
    };

    assert_eq!(
        code,
        ExitCode::Success,
        "a reader closing the pipe is not a failed command"
    );
}
