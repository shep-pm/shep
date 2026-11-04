
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ratatui::Terminal;

use super::super::app::{App, Control, Msg};
use tokio::sync::mpsc;

use super::super::theme::Palette;

use crate::style::{StyleLevel, StyleSource};

use super::*;
use crate::lookout::app::KeyPress;
use futures_util::stream;
use ratatui::backend::TestBackend;
use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

use super::super::testing::*;

/// The property, not the mechanism: with a write in flight and unable to
/// finish, the loop still processes what comes after it.
///
/// Held up by taking `shep.toml`'s lock from a second file descriptor:
/// `flock(2)` excludes per open file description, not per process. Unix
/// only. No `start_paused`: a thread parked on a lock is never idle, so a
/// paused clock would never advance the timeout.
#[cfg(unix)]
#[tokio::test]
async fn the_loop_keeps_running_while_a_settings_write_is_stuck() {
    use nix::fcntl::{Flock, FlockArg};

    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("shep.toml");
    std::fs::write(&config, "[daemon]\nlog_level = \"info\"\n").unwrap();
    let socket_default = dir.path().join("run/shep.sock");

    // Held for the whole of `run_ui` below, and released only once it
    // has returned.
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("shep.toml.lock"))
        .unwrap();
    let held = Flock::lock(lock_file, FlockArg::LockExclusive).unwrap();

    let (msg_tx, msg_rx) = mpsc::channel(16);
    let (poll_tx, _poll_rx) = mpsc::channel(4);
    let (request_tx, _request_rx) = mpsc::channel(4);

    // The screen is opened by handing the reducer the read it would
    // otherwise have asked for, so the four messages below arrive in a
    // fixed order.
    let snapshot = crate::commands::settings::load_settings(
        &config,
        &socket_default,
        (StyleLevel::Full, StyleSource::Default),
    )
    .unwrap();
    msg_tx
        .send(Msg::Settings {
            result: Ok(snapshot),
        })
        .await
        .unwrap();
    // Four `j` reach `[daemon] log_level` (under `[daemon]`, after
    // `boot_first_dogs`, `environment` and `log_json`), `space` arms it,
    // `Enter` sends it, and `q` is the key that must still be answered.
    for _ in 0..4 {
        msg_tx.send(Msg::Key(KeyPress::SelectDown)).await.unwrap();
    }
    msg_tx.send(Msg::Key(KeyPress::Cycle)).await.unwrap();
    msg_tx.send(Msg::Key(KeyPress::Confirm)).await.unwrap();
    msg_tx.send(Msg::Key(KeyPress::Quit)).await.unwrap();
    drop(msg_tx);

    let app = App::new(
        Palette::detect(None, None, None),
        Control::Allowed,
        dir.path().display().to_string(),
        Instant::now(),
    );
    let terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        run_ui(
            app,
            terminal,
            stream::empty(),
            msg_rx,
            poll_tx,
            request_tx,
            test_paths(dir.path()),
            FakeLocal::default(),
            DogRunner::Real,
        ),
    )
    .await
    .expect("the loop answered `q` with a write still in flight");

    // The other half of the same property: the write was not cancelled by
    // the loop leaving. `spawn_blocking` runs its closure to completion
    // whatever happens to the handle. The bound is so a lost write reports
    // rather than hangs, not a claim about speed.
    const WRITE_ATTEMPTS: usize = 3_000;
    const WRITE_POLL: Duration = Duration::from_millis(10);
    drop(held);
    let mut written = false;
    for _ in 0..WRITE_ATTEMPTS {
        if std::fs::read_to_string(&config).unwrap().contains("debug") {
            written = true;
            break;
        }
        std::thread::sleep(WRITE_POLL);
    }
    assert!(written, "the write that was in flight still landed");
}

/// Drives `Effect::LoadSecrets` end to end, the only test that does: the
/// others call `App::update` and inspect the returned `Effect` as a value.
///
/// `reader-app` is filtered off the dashboard by name before `S` opens the
/// pane, but still names `API_KEY` in the muster roll, so readers must
/// come from `App::all_rows()` and not `App::rows()`.
#[tokio::test]
async fn load_secrets_counts_a_reader_the_dashboard_filter_has_hidden() {
    // Short, not the default `$TMPDIR`: a long `$SHEP_HOME` overflows
    // `SUN_LEN` for the control socket path this builds, even though
    // this test never dials it.
    let dir = tempfile::Builder::new().prefix("s").tempdir().unwrap();
    let paths = crate::secret_readers::test_support::paths_under(dir.path());
    shep_core::secrets::set(&paths.secrets, "API_KEY", "production", "hunter2").unwrap();

    let mut reader_app = shep_core::config::AppConfig::minimal("reader-app", "./srv");
    reader_app
        .env
        .insert("A".to_string(), "{{secret:API_KEY}}".to_string());
    crate::secret_readers::test_support::write_roll(&paths, &[reader_app]);

    let mut app = App::new(
        Palette::detect(None, None, None),
        Control::ReadOnly,
        dir.path().display().to_string(),
        Instant::now(),
    );
    app.update(Msg::Snapshot {
        rows: vec![
            ProcessInfo::builder(1, "keeper", ProcStatus::Online).build(),
            ProcessInfo::builder(2, "reader-app", ProcStatus::Online).build(),
        ],
        at: Instant::now(),
    });
    app.set_filter_for_tests("keeper");
    assert!(
        app.rows().iter().all(|row| row.info.name != "reader-app"),
        "the filter must actually hide reader-app, or this test proves nothing"
    );

    let (msg_tx, msg_rx) = mpsc::channel(16);
    let (poll_tx, _poll_rx) = mpsc::channel(4);
    let (request_tx, _request_rx) = mpsc::channel(2);
    msg_tx.send(Msg::Key(KeyPress::Secrets)).await.unwrap();
    // The sleep, not an immediate `Quit`: `Effect::LoadSecrets` answers
    // off `spawn_blocking`, and `Msg::Secrets` is dropped once it lands
    // if `self.body` has already left `Body::Secrets`: quitting before
    // the read comes back would draw the pane still empty.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2000)).await;
        let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
    });

    let terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    let terminal = tokio::time::timeout(
        Duration::from_secs(10),
        run_ui(
            app,
            terminal,
            stream::empty(),
            msg_rx,
            poll_tx,
            request_tx,
            paths.clone(),
            FakeLocal::default(),
            DogRunner::Real,
        ),
    )
    .await
    .expect("the loop left within ten seconds");

    let frame = crate::lookout::frames::render_text(terminal.backend().buffer());
    assert!(
        frame.contains("API_KEY"),
        "the seeded key is drawn: {frame}"
    );
    let row = frame
        .lines()
        .find(|line| line.contains("API_KEY"))
        .expect("API_KEY's own row");
    assert!(
        row.contains("1 (1 online)"),
        "reader-app must still be counted in READ BY: {row:?}"
    );
}

/// What `local` was asked while a lookout ran: the cursor leaves the
/// sheep and comes back, which asks for its feed and its log size, and
/// the first heartbeat asks for a host sample.
async fn reads_taken(remote: bool) -> (usize, usize, usize) {
    let sheep = ProcessInfo::builder(7, "web", ProcStatus::Online)
        .out_file(Some("/var/log/remote/web-out.log".to_string()))
        .err_file(Some("/var/log/remote/web-err.log".to_string()))
        .build();
    let mut app = crate::lookout::view::fixtures::with_selection(sheep);
    if remote {
        app.set_remote_host("build-01");
    }
    let local = FakeLocal::default();
    let (tails, sizes, hosts) = (
        Arc::clone(&local.tails),
        Arc::clone(&local.log_sizes),
        Arc::clone(&local.hosts),
    );

    let (msg_tx, msg_rx) = mpsc::channel(16);
    let (poll_tx, _poll_rx) = mpsc::channel(4);
    let (request_tx, _request_rx) = mpsc::channel(2);
    // Real time between the keys: the feed and the log size are read
    // only once `MIN_REDRAW` has passed since the last draw.
    tokio::spawn(async move {
        for key in [KeyPress::SelectUp, KeyPress::SelectDown] {
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Key(key)).await;
        }
        tokio::time::sleep(MIN_REDRAW * 3).await;
        let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
    });

    let terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        run_ui(
            app,
            terminal,
            stream::empty(),
            msg_rx,
            poll_tx,
            request_tx,
            shared_test_paths(),
            local,
            DogRunner::Real,
        ),
    )
    .await
    .expect("the loop left within ten seconds");
    (
        tails.load(Ordering::Relaxed),
        sizes.load(Ordering::Relaxed),
        hosts.load(Ordering::Relaxed),
    )
}

/// The log paths on a remote row name files on the host. Read here they
/// would be this machine's files under the host's name, or nothing, which
/// reads as a sheep that wrote nothing.
#[tokio::test]
async fn a_host_lookout_reads_no_log_file_named_by_a_remote_row() {
    // The same script on this machine's own shepherd reads all three, or
    // the zeros below would prove nothing.
    let (tails, sizes, hosts) = reads_taken(false).await;
    assert!(
        tails > 0 && sizes > 0 && hosts > 0,
        "{tails} {sizes} {hosts}"
    );

    assert_eq!(reads_taken(true).await, (0, 0, 0));
}

/// A `cargo` that records its own pid and then replaces the shell with a
/// `sleep`, so the recorded pid is the one that is still building when the
/// operator quits.
#[cfg(unix)]
fn sleeping_cargo(pid_file: &Path) -> String {
    format!(
        "#!/bin/sh\necho $$ > {}\nexec sleep 30\n",
        pid_file.display()
    )
}

/// Whether `pid` is no longer running: reaped already (`ECHILD`), or reaped
/// here. A child still alive is the thing this test is looking for.
///
/// `WNOHANG` rather than `kill(pid, 0)`: a killed child tokio never reaped
/// is still a pid, and a zombie is not a running `cargo`.
#[cfg(unix)]
#[track_caller]
fn is_gone(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    match waitpid(Pid::from_raw(cast_pid(pid)), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => false,
        Ok(_) | Err(Errno::ECHILD) => true,
        Err(err) => panic!("waitpid({pid}): {err}"),
    }
}

#[cfg(unix)]
fn cast_pid(pid: u32) -> i32 {
    i32::try_from(pid).expect("a pid fits an i32")
}

/// Drives `Effect::StartDogJob` to a quit, the only test that does: the
/// job runs `cargo` on its own thread, and only the loop holds the handle
/// that ends it.
///
/// The quit answers the `Confirm` lookout asks first, and the yes drops
/// that thread's runtime, which drops the `cargo install` future spawned
/// with `kill_on_drop`. Step 3 writes the receipt last, so a build cut off
/// mid-way adopts nothing.
///
/// No `start_paused`: the job runs on its own thread against its own real
/// clock, and the wait for its pid file is a real wait.
#[cfg(unix)]
#[tokio::test]
async fn a_quit_mid_build_leaves_no_cargo_and_no_receipt() {
    // Short, not the default `$TMPDIR`: a long `$SHEP_HOME` overflows
    // `SUN_LEN` for the control socket path this home names.
    let dir = tempfile::Builder::new().prefix("s").tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (bin, pid_file) = (dir.path().join("bin"), dir.path().join("cargo.pid"));
    std::fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, sleeping_cargo(&pid_file)).unwrap();
    std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    crate::dog_index::write_cache(
        &paths.dog_index_cache,
        &crate::dog_index::index_url(),
        serde_json::json!({
            "version": 1,
            "dogs": [{
                "name": "Otel", "package": "shep-otel", "adopt_as": "otel",
                "description": "exports traces",
                "repo": "https://github.com/shep-pm/shep-otel",
                "license": "MIT", "category": "metrics",
                "source": { "kind": "cargo", "version": "0.2.4" }
            }]
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();

    // The screen is opened by handing the reducer the read it would
    // otherwise have asked for, so no arm of the loop reaches the network
    // and the two `enter` keys below land on a row that is already there.
    let rows =
        super::super::app::catalogue_read_cache(&paths, &crate::dog_index::index_url(), false)
            .expect("the cached index reads");
    assert_eq!(rows.rows.len(), 1, "{:?}", rows.rows);
    let mut app = App::new(
        Palette::detect(None, None, None),
        Control::Allowed,
        dir.path().display().to_string(),
        Instant::now(),
    );
    app.open_catalogue();
    app.update(Msg::Catalogue { result: Ok(rows) });

    let (msg_tx, msg_rx) = mpsc::channel(16);
    let (poll_tx, _poll_rx) = mpsc::channel(4);
    let (request_tx, _request_rx) = mpsc::channel(4);

    // Held before `paths` moves into the loop, for the assertion after it.
    let receipt = paths.dogs_dir.join("otel/receipt.json");
    // `enter` on the list opens the entry, and `enter` on the entry
    // installs it.
    for key in [KeyPress::Confirm, KeyPress::Confirm] {
        msg_tx.send(Msg::Key(key)).await.unwrap();
    }
    let driver = async {
        // The quit waits for a build that is really running: the pid file
        // is the stub's own first act, so its presence says `cargo` is in
        // the loop's hands and not yet done.
        const TAKEN_ATTEMPTS: usize = 1_500;
        const TAKEN_POLL: Duration = Duration::from_millis(20);
        for _ in 0..TAKEN_ATTEMPTS {
            if std::fs::read_to_string(&pid_file).is_ok() {
                break;
            }
            tokio::time::sleep(TAKEN_POLL).await;
        }
        assert!(
            std::fs::read_to_string(&pid_file).is_ok(),
            "the stub cargo never started"
        );
        msg_tx.send(Msg::Key(KeyPress::Quit)).await.unwrap();
        // Enter is the yes to the quit question the loop asks first.
        msg_tx.send(Msg::Key(KeyPress::Confirm)).await.unwrap();
        drop(msg_tx);
    };

    let terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let (terminal, ()) = tokio::time::timeout(Duration::from_secs(30), async move {
        tokio::join!(
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                paths.clone(),
                FakeLocal::default(),
                DogRunner::Stub(cargo.clone()),
            ),
            driver,
        )
    })
    .await
    .expect("the loop asked before quitting");

    // The quit did ask, and named the dog it was about to stop.
    let last = crate::lookout::frames::render_text(terminal.backend().buffer());
    assert!(
        last.contains("stop installing otel"),
        "the quit question is not on screen:\n{last}"
    );

    let written = std::fs::read_to_string(&pid_file).unwrap();
    let pid: u32 = written.trim().parse().expect("the stub wrote a pid");
    const GONE_ATTEMPTS: usize = 500;
    const GONE_POLL: Duration = Duration::from_millis(20);
    let mut gone = false;
    for _ in 0..GONE_ATTEMPTS {
        if is_gone(pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(GONE_POLL).await;
    }
    assert!(gone, "cargo {pid} is still running");
    assert!(!receipt.exists(), "a build cut off mid-way adopted a dog");
}
