//! The `--no-follow` path: tailing each matched sheep's log files on disk
//! and never touching the bus, plus the backlog a `--follow` prints before
//! it subscribes.

use super::*;

// --- `--no-follow` reads the log files ---
// None of these subscribe, so there is nothing for `RUN_TIMEOUT` to
// guard but a bounded file read.

/// A `--no-follow` still wired to the bus fails the second assertion; one
/// wired to neither fails the first. This is the test that tells the two
/// apart from a `--no-follow` that reads the right files.
#[tokio::test]
async fn no_follow_reads_the_files_and_never_the_bus() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let out_path = write_log(dir.path(), "web-out.log", "from-the-file\n");

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = Some(out_path);
    daemon.reply_to_list(vec![sheep]);
    daemon
        .push(BusEvent::LogOut {
            id: 1,
            line: "from-the-bus".into(),
        })
        .await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own")
    };
    let rendered = String::from_utf8(out).unwrap();

    assert_eq!(code, ExitCode::Success);
    assert!(rendered.contains("from-the-file"));
    assert!(
        !rendered.contains("from-the-bus"),
        "the file path must never consult the bus: {rendered}"
    );
}

/// A sheep that already crashed before `shep bleats <name>` runs: the
/// backlog is what makes its last output reachable without starting it
/// again in a second window.
#[tokio::test]
async fn following_prints_the_existing_log_before_it_follows() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let out_path = write_log(
        dir.path(),
        "web-out.log",
        "boot: reading config\nFATAL: port 19999 already in use\n",
    );

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = Some(out_path);
    daemon.reply_to_list(vec![sheep]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &follow_args_out("all")),
        )
        .await;
    }
    let rendered = String::from_utf8(out).unwrap();
    assert!(
        rendered.contains("FATAL: port 19999 already in use"),
        "a follow must carry the reason a dead sheep died: {rendered}"
    );
}

/// `--lines 0` is the escape hatch for someone who genuinely wants only
/// what arrives next, and it is what the foreground runner passes.
#[tokio::test]
async fn lines_zero_follows_without_replaying_anything() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let out_path = write_log(dir.path(), "web-out.log", "OLD-HISTORY\n");

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = Some(out_path);
    daemon.reply_to_list(vec![sheep]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(
                &client,
                &mut streams,
                false,
                &BleatsArgs {
                    lines: 0,
                    ..follow_args_out("all")
                },
            ),
        )
        .await;
    }
    let rendered = String::from_utf8(out).unwrap();
    assert!(
        !rendered.contains("OLD-HISTORY"),
        "--lines 0 must replay nothing: {rendered}"
    );
}

/// A `read_to_string`-style implementation prints line 1 and fails this.
#[tokio::test]
async fn the_tail_is_bounded_by_lines() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    const CAP: usize = 50;
    let total = CAP + 20;
    let content: String = (1..=total).map(|n| format!("line-{n}\n")).collect();
    let out_path = write_log(dir.path(), "web-out.log", &content);

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = Some(out_path);
    daemon.reply_to_list(vec![sheep]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(
                &client,
                &mut streams,
                false,
                &BleatsArgs {
                    lines: CAP,
                    ..no_follow_args_out("all")
                },
            ),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own");
    }
    let rendered = String::from_utf8(out).unwrap();

    assert!(
        !rendered.lines().any(|line| line == "web | line-1"),
        "the first line must fall outside the tail: {rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line == format!("web | line-{total}")),
        "the last line must be present: {rendered}"
    );
    assert_eq!(
        rendered.lines().count(),
        CAP,
        "exactly CAP lines must reach stdout: {rendered}"
    );
}

/// Guards the window and the discard-the-partial-first-line rule
/// together: an implementation that keeps the partial head emits a
/// quarter-megabyte fragment and fails this.
#[tokio::test]
async fn the_tail_is_bounded_by_bytes_and_never_shows_half_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let long_line = "x".repeat(usize::try_from(TAIL_WINDOW_BYTES).unwrap() + 1024);
    let content = format!("{long_line}\nshort\n");
    let out_path = write_log(dir.path(), "web-out.log", &content);

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = Some(out_path);
    daemon.reply_to_list(vec![sheep]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own");
    }
    let rendered = String::from_utf8(out).unwrap();

    assert_eq!(
        rendered,
        "web | short\n",
        "no fragment of the long line may reach stdout ({} bytes rendered)",
        rendered.len()
    );
}

/// Two instances sharing one log file (a `merge_logs` app, or any app
/// with an explicit `out_file`) must be read once, not once per
/// instance: reading per row printed the whole file once per instance
/// pointing at it.
#[test]
fn instances_sharing_one_log_file_are_read_once_not_once_each() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("talker-out.log");
    std::fs::write(&shared, "line one\nline two\n").expect("write");

    let shared_path = shared.to_string_lossy().to_string();
    let mut cache = HashMap::new();
    for id in 0..2u32 {
        cache.insert(
            id,
            ProcessInfo::builder(id, "talker", ProcStatus::Online)
                .out_file(Some(shared_path.clone()))
                .err_file(Some(shared_path.clone()))
                .build(),
        );
    }

    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let args = no_follow_args_out("talker");
    let selector = ProcessSelector::parse("talker").expect("selector");

    tail_log_files(&mut streams, false, &cache, &selector, &args);

    let printed = String::from_utf8(out).expect("utf8");
    assert_eq!(
        printed.matches("line one").count(),
        1,
        "one file, one read, however many instances point at it:\n{printed}"
    );
}

/// Builds a cache of `count` rows for one app, and returns it with the
/// printed backlog. `shared` puts every instance on one file, the way
/// `merge_logs` does.
fn backlog_of(dir: &Path, app: &str, count: u32, shared: bool) -> String {
    let mut cache = HashMap::new();
    for slot in 0..count {
        let stem = if shared {
            format!("{app}-out.log")
        } else {
            format!("{app}-{slot}-out.log")
        };
        let path = dir.join(&stem);
        std::fs::write(&path, format!("hello from {slot}\n")).expect("write");
        cache.insert(
            slot,
            ProcessInfo::builder(slot, app, ProcStatus::Online)
                .instance(Some(slot))
                .out_file(Some(path.to_string_lossy().to_string()))
                .build(),
        );
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let args = no_follow_args_out(app);
    let selector = ProcessSelector::parse(app).expect("selector");
    tail_log_files(&mut streams, false, &cache, &selector, &args);
    String::from_utf8(out).expect("utf8")
}

#[test]
fn a_multi_instance_app_labels_its_backlog_lines_with_the_slot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let printed = backlog_of(dir.path(), "web", 2, false);
    assert!(printed.contains("web:0 |"), "{printed}");
    assert!(printed.contains("web:1 |"), "{printed}");
}

#[test]
fn a_single_instance_app_keeps_the_bare_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let printed = backlog_of(dir.path(), "solo", 1, false);
    assert!(printed.contains("solo |"), "{printed}");
    assert!(
        !printed.contains("solo:0"),
        "no suffix for one instance: {printed}"
    );
}

#[test]
fn a_shared_backlog_file_is_labelled_with_the_app_not_a_slot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let printed = backlog_of(dir.path(), "talker", 2, true);
    assert!(printed.contains("talker |"), "{printed}");
    assert!(
        !printed.contains("talker:"),
        "one file holds both instances, and no line says which wrote it: {printed}"
    );
}

/// `truncated` must report `true` when the byte window alone cut the
/// tail short: a sheep logging a few long, structured lines can fill
/// `TAIL_WINDOW_BYTES` in far fewer than `limit` lines.
#[test]
fn read_tail_reports_truncated_on_a_byte_window_cut_alone() {
    let dir = tempfile::tempdir().unwrap();
    let long_line = "x".repeat(usize::try_from(TAIL_WINDOW_BYTES).unwrap() + 1024);
    let content = format!("{long_line}\nshort\n");
    let path = dir.path().join("web-out.log");
    std::fs::write(&path, &content).unwrap();

    let (lines, truncated) = read_tail(&path, 50).unwrap();

    assert_eq!(lines, vec!["short".to_string()]);
    assert!(
        truncated,
        "the file exceeds the byte window, so this tail is not the whole log"
    );
}

/// Three ways: `--out` (out lines only), `--err` (err lines only),
/// neither (out lines, then err lines, this module's own within-sheep
/// ordering).
#[tokio::test]
async fn out_and_err_select_which_file_is_read() {
    async fn run(args: BleatsArgs) -> String {
        let dir = tempfile::tempdir().unwrap();
        let sock = shep_client::testing::control_address(dir.path());
        let out_path = write_log(dir.path(), "web-out.log", "stdout-line\n");
        let err_path = write_log(dir.path(), "web-err.log", "stderr-line\n");

        let (client, daemon) = fake_client_with_push(&sock).await;
        let mut sheep = info(1, "web");
        sheep.out_file = Some(out_path);
        sheep.err_file = Some(err_path);
        daemon.reply_to_list(vec![sheep]);

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            tokio::time::timeout(RUN_TIMEOUT, bleats(&client, &mut streams, false, &args))
                .await
                .expect("--no-follow never subscribes, so it must terminate on its own");
        }
        String::from_utf8(out).unwrap()
    }

    let out_only = run(no_follow_args_out("all")).await;
    assert!(out_only.contains("stdout-line") && !out_only.contains("stderr-line"));

    let err_only = run(no_follow_args_err("all")).await;
    assert!(err_only.contains("stderr-line") && !err_only.contains("stdout-line"));

    let both = run(no_follow_args("all")).await;
    let out_pos = both
        .find("stdout-line")
        .expect("the stdout line is present");
    let err_pos = both
        .find("stderr-line")
        .expect("the stderr line is present");
    assert!(
        out_pos < err_pos,
        "out_file must render before err_file within one sheep: {both}"
    );
}

/// Fails if the sheep are tailed in id order rather than name order.
///
/// `b` holds the lower id but sorts after `a` by name, and the listing
/// is scripted in id order, so neither a `HashMap`'s arbitrary order
/// nor id order can make the assertion pass by accident.
#[tokio::test]
async fn files_are_printed_in_name_order() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let a_path = write_log(dir.path(), "a-out.log", "line-from-a\n");
    let b_path = write_log(dir.path(), "b-out.log", "line-from-b\n");

    let (client, daemon) = fake_client_with_push(&sock).await;
    // `b` takes the LOWER id, so id order and name order disagree.
    let mut sheep_b = info(1, "b");
    sheep_b.out_file = Some(b_path);
    let mut sheep_a = info(2, "a");
    sheep_a.out_file = Some(a_path);
    daemon.reply_to_list(vec![sheep_b, sheep_a]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own");
    }
    let rendered = String::from_utf8(out).unwrap();

    let a_pos = rendered.find("line-from-a").expect("a's line is present");
    let b_pos = rendered.find("line-from-b").expect("b's line is present");
    assert!(
        a_pos < b_pos,
        "name order puts `a` (id 2) before `b` (id 1): {rendered}"
    );
}

/// Fails if one app's instances are tailed in id order rather than
/// slot order: a reload gives slot 0 a fresh high id, so id order
/// alone would read 1, 2, 0.
///
/// Slot 0 is given the highest id here and the listing is scripted in
/// slot order, so neither id order nor a `HashMap`'s arbitrary order
/// can make the assertion pass by accident.
#[tokio::test]
async fn one_apps_instances_are_printed_in_slot_order() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut listing = Vec::new();
    // Slot 0 reloaded, so it holds id 9 while slots 1 and 2 kept 1 and 2.
    for (id, slot) in [(9_u32, 0_u32), (1, 1), (2, 2)] {
        let mut sheep = info(id, "web");
        sheep.instance = Some(slot);
        sheep.out_file = Some(write_log(
            dir.path(),
            &format!("web-{slot}-out.log"),
            &format!("line-from-slot-{slot}\n"),
        ));
        listing.push(sheep);
    }
    daemon.reply_to_list(listing);

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own");
    }
    let rendered = String::from_utf8(out).unwrap();

    let at = |slot: u32| {
        rendered
            .find(&format!("line-from-slot-{slot}"))
            .unwrap_or_else(|| panic!("slot {slot}'s line is present: {rendered}"))
    };
    assert!(
        at(0) < at(1) && at(1) < at(2),
        "slot order, not id order (which would read 1, 2, 0): {rendered}"
    );
}

/// The daemon creates both files at spawn, so a missing one means this
/// sheep has never run in this `$SHEP_HOME`. Said rather than exited on:
/// an empty tail and an empty log read the same on a terminal.
#[tokio::test]
async fn a_missing_file_is_noticed_and_the_rest_still_print() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let real_path = write_log(dir.path(), "web-out.log", "still-here\n");
    let missing_path = dir
        .path()
        .join("never-written.log")
        .to_str()
        .unwrap()
        .to_string();

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut ghost = info(1, "ghost");
    ghost.out_file = Some(missing_path.clone());
    let mut real = info(2, "web");
    real.out_file = Some(real_path);
    daemon.reply_to_list(vec![ghost, real]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own")
    };

    assert_eq!(
        code,
        ExitCode::Success,
        "a sheep that has never run is not a failed run"
    );
    assert!(String::from_utf8(out).unwrap().contains("still-here"));
    let stderr = String::from_utf8(err).unwrap();
    assert!(
        stderr.contains(&format!(
            "notice[log_missing]: ghost: no out log at {missing_path} yet"
        )),
        "the notice must name the sheep, the stream and the file: {stderr}"
    );
}

/// A relative `out_file` is resolved against the shepherd's directory
/// by the shepherd and against this one here, so the two name different
/// files and the silent read was of the wrong one.
#[tokio::test]
async fn a_relative_path_names_the_file_this_process_actually_tried() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = Some("logs/out.log".to_string());
    daemon.reply_to_list(vec![sheep]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own")
    };

    assert_eq!(code, ExitCode::Success);
    assert!(out.is_empty());
    let stderr = String::from_utf8(err).unwrap();
    // Asserted against the un-resolved spelling rather than against a
    // second `absolute` call: a sibling test moves the process cwd, so
    // the expected string cannot be recomputed here.
    assert!(
        stderr.contains("out.log") && !stderr.contains("at logs"),
        "the notice must name the absolute path this process read: {stderr}"
    );
    assert!(
        stderr.contains("out_file is relative"),
        "the notice must say why the shepherd's file is a different one: {stderr}"
    );
}

/// Points `out_file` at a directory, not a `chmod 000` file: opening a
/// directory succeeds on unix and the read fails `EISDIR`
/// deterministically, including as root, where a `000` file would still
/// be readable.
#[tokio::test]
async fn an_unreadable_file_is_noticed_and_exits_failure_with_the_rest_still_printed() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let bad_dir = dir.path().join("a-directory");
    std::fs::create_dir(&bad_dir).unwrap();
    let bad_dir = bad_dir.to_str().unwrap().to_string();
    let real_path = write_log(dir.path(), "web-out.log", "still-here\n");

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut bad = info(1, "bad");
    bad.out_file = Some(bad_dir.clone());
    let mut real = info(2, "web");
    real.out_file = Some(real_path);
    daemon.reply_to_list(vec![bad, real]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own")
    };

    assert_eq!(code, ExitCode::Failure);
    assert!(
        String::from_utf8(out).unwrap().contains("still-here"),
        "one sheep's unreadable file must not hide the rest of the flock's lines"
    );
    let stderr = String::from_utf8(err).unwrap();
    assert!(
        stderr.contains(&bad_dir),
        "the notice must name the unreadable path: {stderr}"
    );
}

/// An implementation that skips a `None` path in silence passes every
/// other test here and fails this one.
#[tokio::test]
async fn a_daemon_that_reported_no_path_is_noticed_not_silently_empty() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = None;
    sheep.err_file = None;
    daemon.reply_to_list(vec![sheep]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own")
    };

    assert_eq!(
        code,
        ExitCode::Success,
        "version skew is not a fault in this run"
    );
    let stderr = String::from_utf8(err).unwrap();
    assert!(
        stderr.contains("log_path_unknown"),
        "a None path must be noticed, not silently empty: {stderr}"
    );
}

/// Sits beside `json_format_renders_the_pinned_six_key_line_shape`:
/// renaming a field of `BleatLine` must now fail both.
#[tokio::test]
async fn a_file_sourced_json_line_is_the_same_six_key_shape_as_a_bus_sourced_one() {
    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let out_path = write_log(dir.path(), "web-out.log", "hello-from-disk\n");

    let (client, daemon) = fake_client_with_push(&sock).await;
    let mut sheep = info(1, "web");
    sheep.out_file = Some(out_path);
    daemon.reply_to_list(vec![sheep]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Json,
        };
        tokio::time::timeout(
            RUN_TIMEOUT,
            bleats(&client, &mut streams, false, &no_follow_args_out("all")),
        )
        .await
        .expect("--no-follow never subscribes, so it must terminate on its own");
    }
    let out = String::from_utf8(out).unwrap();
    let line = out.lines().next().expect("one JSON line was rendered");
    let json: serde_json::Value = serde_json::from_str(line).unwrap();
    let obj = json.as_object().expect("a bleats JSON line is an object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();

    assert_eq!(
        keys,
        ["id", "instance", "line", "name", "schema_version", "stream"],
        "a file-sourced line must be the same shape as a bus-sourced one: {out}"
    );
    assert_eq!(json["id"], 1);
    assert_eq!(json["name"], "web");
    assert_eq!(json["stream"], "out");
    assert_eq!(
        json["instance"],
        serde_json::Value::Null,
        "one registered instance means no slot to report"
    );
    assert_eq!(json["line"], "hello-from-disk");
    assert_eq!(json["schema_version"], output::SCHEMA_VERSION);
}
