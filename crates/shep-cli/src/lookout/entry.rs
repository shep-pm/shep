use super::app::{App, Control};
use super::source::Shepherd;
use super::theme::Palette;
use super::ui_event_loop::run_ui;
use crate::cli::LookoutArgs;
use crate::exit::ExitCode;
use crate::output::Streams;
use crate::style::{StyleLevel, StyleSource};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use shep_core::paths::ShepPaths;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc;

/// Runs the dashboard, and returns the [`ExitCode`] to exit with.
///
/// Four refusals, all before a single escape byte is written:
/// [`ExitCode::Usage`] when stdout is not a terminal;
/// [`ExitCode::DaemonUnreachable`] or [`ExitCode::ProtocolMismatch`] when the
/// first connection fails; [`ExitCode::VersionSkew`] when it succeeds against
/// a different crate version; [`ExitCode::Failure`] when the terminal cannot
/// be put into raw mode. After that it never exits on its own.
///
/// `style` is `run_argv`'s already-resolved pair, handed to `App::set_style`
/// so the settings screen reports the layer that actually won.
pub async fn lookout(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &LookoutArgs,
    style: (StyleLevel, StyleSource),
) -> ExitCode {
    // A TUI piped into a file is a usage error, not a rendering mode: the
    // alternative is writing alternate-screen escapes into somebody's log.
    if !std::io::stdout().is_terminal() {
        return streams.fail(
            ExitCode::Usage,
            "lookout needs a terminal; stdout is not one",
        );
    }

    // The first dial, before the palette, the panic hook, raw mode, or
    // anything drawn: a shepherd that was never running gets the same refusal
    // `shep flock` gets. Everything after this point is the running-dashboard
    // case, which is the one the ladder is for.
    let mut shepherd = super::source::UnixShepherd::new(&paths.socket);
    let opened = match shepherd.link().await {
        Ok(opened) => opened,
        Err(err) => {
            let code = err.exit_code();
            return streams.fail(code, &err.to_string());
        }
    };

    // `lookout` drives the daemon for as long as the dashboard stays open, so
    // it can never be one of `RECOVERY_VERBS`. A reconnect on the ladder is
    // not re-checked; a shepherd cannot downgrade itself mid-run.
    if let Err(code) = crate::version_guard::refuse_version_skew(
        streams,
        opened.0.client(),
        crate::version_guard::VersionGuard::Enforce,
    ) {
        return code;
    }

    let palette = Palette::detect(
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("COLORTERM").as_deref(),
    );
    let control = resolve_control(args.read_only, &paths.kv);
    let mut app = App::new(
        palette,
        control,
        paths.home.to_string_lossy().into_owned(),
        Instant::now(),
    );
    app.set_style(style);

    // Hook first, then the guard, then raw mode, then the alternate screen,
    // with nothing that can panic in between.
    super::term::install_panic_hook();
    // Armed before `enter()`: `term::enter` turns raw mode on and then enters the
    // alternate screen, so a failure in the second step would otherwise leave
    // the operator's shell with no echo and no line editing. `restore()` is
    // idempotent and safe outside raw mode.
    let _guard = super::term::RestoreGuard::new();
    let out = match super::term::enter() {
        Ok(out) => out,
        Err(err) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("could not put the terminal into raw mode: {err}"),
            );
        }
    };

    let terminal = match Terminal::new(CrosstermBackend::new(out)) {
        Ok(terminal) => terminal,
        Err(err) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("could not open the terminal: {err}"),
            );
        }
    };

    let (msg_tx, msg_rx) = mpsc::channel(1024);
    let (poll_tx, poll_rx) = mpsc::channel(8);
    // Capacity 2: one action plus one lamb fetch is the most that can be
    // outstanding, because the reducer refuses a second action while one is in
    // flight and the lamb fetch is coalesced onto the redraw gate.
    let (request_tx, request_rx) = mpsc::channel(2);
    // The connection opened above is handed straight in, so the link task
    // never dials for its first one.
    let link = tokio::spawn(super::link::run_link(
        shepherd,
        opened,
        msg_tx,
        super::link::Channels {
            polls: poll_rx,
            requests: request_rx,
        },
        super::link::FLOCK_POLL,
    ));

    let events = crossterm::event::EventStream::new();
    let _ = run_ui(
        app,
        terminal,
        events,
        msg_rx,
        poll_tx,
        request_tx,
        paths.clone(),
        paths.home.clone(),
        paths.daemon_config.clone(),
        paths.socket.clone(),
        super::source::LocalReader::new(),
    )
    .await;
    link.abort();
    ExitCode::Success
}

/// Whether this lookout may act, from `--read-only` or from the KV store.
///
/// Control is on by default. `--read-only` closes it outright; short of
/// that, `shep set lookout.allow_control false` closes it too. The store is
/// `$SHEP_HOME/kv.json` rather than a `shep.toml` section, since this gate is
/// the operator's own, and unreadable it leaves control on: the gate stops
/// an accident, not an attacker.
#[must_use]
pub fn resolve_control(read_only: bool, kv: &Path) -> Control {
    if read_only {
        return Control::ReadOnly;
    }
    match shep_core::kv::get(kv, "lookout.allow_control") {
        Ok(Some(value)) if value == "false" => Control::ReadOnly,
        _ => Control::Allowed,
    }
}

#[cfg(test)]
mod tests {
    use super::super::MIN_REDRAW;
    use super::super::ui_event_loop::run_ui;

    use std::path::Path;
    use std::time::{Duration, Instant};

    use ratatui::Terminal;

    use super::super::app::{App, Body, Control, Msg, Sent};
    use tokio::sync::mpsc;

    use super::super::theme::Palette;

    use super::*;
    use crate::lookout::app::KeyPress;
    use futures_util::stream;
    use ratatui::backend::TestBackend;
    use shep_core::protocol::{BusEvent, ProcessInfo};
    use shep_core::status::ProcStatus;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use super::super::testing::*;

    /// The whole loop with no terminal and no socket: a `TestBackend` for the
    /// screen and a finite `tail::Stream` for the keyboard. Bounded, so a loop that
    /// never sees its quit key fails rather than hangs the suite.
    #[tokio::test(start_paused = true)]
    async fn the_loop_draws_and_quits_on_a_keypress() {
        let (msg_tx, msg_rx) = mpsc::channel(16);
        let (poll_tx, _poll_rx) = mpsc::channel(1);
        let (request_tx, _request_rx) = mpsc::channel(2);
        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/tmp/shep".to_string(),
            Instant::now(),
        );
        let terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let keys = stream::iter(vec![Ok(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ))]);

        drop(msg_tx);
        let done = tokio::time::timeout(
            Duration::from_secs(10),
            run_ui(
                app,
                terminal,
                keys,
                msg_rx,
                poll_tx,
                request_tx,
                test_paths(Path::new("/tmp/shep-lookout-tests")),
                PathBuf::from("/tmp/shep-lookout-tests"),
                PathBuf::from("/tmp/shep-lookout-tests/shep.toml"),
                PathBuf::from("/tmp/shep-lookout-tests/run/shep.sock"),
                FakeLocal::default(),
            ),
        )
        .await;
        let terminal = done.expect("the loop left on `q` within ten seconds");
        let frame = crate::lookout::frames::render_text(terminal.backend().buffer());
        assert!(frame.contains("shep lookout"), "it drew at least once");
    }

    /// The `Effect::PollNow` a drop produces has to reach the link task, or
    /// the repair the link task exists for never happens.
    ///
    /// Also the starvation pin: `stream::empty()` is `Poll::Ready(None)` on
    /// every poll, so an implementation that did not retire the keyboard arm
    /// would win that arm forever and never read either message queued below.
    #[tokio::test(start_paused = true)]
    async fn a_drop_forwards_a_poll_request_to_the_link_task() {
        let (msg_tx, msg_rx) = mpsc::channel(16);
        let (poll_tx, mut poll_rx) = mpsc::channel(4);
        let (request_tx, _request_rx) = mpsc::channel(2);
        msg_tx
            .send(Msg::Event(BusEvent::Dropped { count: 4 }))
            .await
            .unwrap();
        msg_tx.send(Msg::Key(KeyPress::Quit)).await.unwrap();
        drop(msg_tx);

        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/tmp/shep".to_string(),
            Instant::now(),
        );
        let terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                test_paths(Path::new("/tmp/shep-lookout-tests")),
                PathBuf::from("/tmp/shep-lookout-tests"),
                PathBuf::from("/tmp/shep-lookout-tests/shep.toml"),
                PathBuf::from("/tmp/shep-lookout-tests/run/shep.sock"),
                FakeLocal::default(),
            ),
        )
        .await
        .expect("the loop left within ten seconds");

        assert_eq!(poll_rx.try_recv(), Ok(()), "the poll request was forwarded");
    }

    #[test]
    fn control_is_allowed_when_nothing_says_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let kv = dir.path().join("kv.json");
        assert_eq!(resolve_control(false, &kv), Control::Allowed);
    }

    #[test]
    fn the_flag_and_the_key_can_each_ask_for_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let kv = dir.path().join("kv.json");
        assert_eq!(resolve_control(true, &kv), Control::ReadOnly);

        shep_core::kv::set(&kv, "lookout.allow_control", "false").unwrap();
        assert_eq!(resolve_control(false, &kv), Control::ReadOnly);
    }

    #[test]
    fn an_unreadable_store_leaves_control_allowed() {
        // Fails open now, deliberately: the gate stops an accident, not an
        // attacker, and a broken store is not a reason to refuse every key.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_control(false, &dir.path().join("missing.json")),
            Control::Allowed
        );
    }

    /// The strip renders from `App::host`, and every pane test and gallery
    /// frame injects the message directly, so a heartbeat that yielded only
    /// `Msg::Tick` would leave the shipped binary drawing `host  not read
    /// yet` with nothing red on the suite. Asserted on the reader rather than
    /// on a frame; `a_heartbeat_puts_the_host_strip_on_the_frame` is the
    /// other half.
    #[tokio::test(start_paused = true)]
    async fn the_heartbeat_asks_the_local_reader_for_a_host_sample() {
        let (msg_tx, msg_rx) = mpsc::channel(64);
        let (poll_tx, _poll_rx) = mpsc::channel(4);
        let (request_tx, _request_rx) = mpsc::channel(2);
        let local = FakeLocal::default();
        let hosts = Arc::clone(&local.hosts);

        // After the 1-second heartbeat, so the tick lands first.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
        });

        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/tmp/shep".to_string(),
            Instant::now(),
        );
        let terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                test_paths(Path::new("/tmp/shep-lookout-tests")),
                PathBuf::from("/tmp/shep-lookout-tests"),
                PathBuf::from("/tmp/shep-lookout-tests/shep.toml"),
                PathBuf::from("/tmp/shep-lookout-tests/run/shep.sock"),
                local,
            ),
        )
        .await
        .expect("the loop left within ten seconds");

        assert!(
            hosts.load(Ordering::Relaxed) >= 1,
            "the heartbeat fired and never sampled the host"
        );
    }

    /// `Msg::Snapshot` answers every ordinary poll with `Effect::RefreshFeed`,
    /// so the detail pane's log-size read has to hang off that rather than off
    /// the lamb walk's `Effect::RefreshSelected`. Two snapshots with the
    /// selection never moving between them: the second still has to read, or a
    /// size freezes on screen while an operator watches one sheep write.
    ///
    /// Asserted on the reader's own counter, since `FakeLocal` answers `None`
    /// and the pane would draw no size either way. A count of one is the
    /// regression: the first snapshot seats the selection, which raises
    /// `RefreshSelected` too, so hanging the read off the lamb walk still
    /// reads exactly once.
    ///
    /// Not `start_paused`: both reads are gated on `may_draw`, which measures
    /// real `std::time::Instant` against `MIN_REDRAW`, and a paused clock's
    /// virtual sleeps resolve in microseconds of real time, so the second read
    /// would never be allowed to run.
    #[tokio::test]
    async fn every_poll_re_reads_the_selected_sheeps_log_sizes() {
        let (msg_tx, msg_rx) = mpsc::channel(64);
        let (poll_tx, _poll_rx) = mpsc::channel(4);
        let (request_tx, _request_rx) = mpsc::channel(8);
        let local = FakeLocal::default();
        let log_sizes = Arc::clone(&local.log_sizes);

        let rows = vec![
            ProcessInfo::builder(1, "api", ProcStatus::Online)
                .pid(Some(4_001))
                .out_file(Some("/tmp/shep-lookout-tests/api-out.log".to_string()))
                .err_file(Some("/tmp/shep-lookout-tests/api-err.log".to_string()))
                .build(),
        ];

        // Each snapshot needs its own serviced iteration, so each is followed
        // by a wait past `MIN_REDRAW` and a `Msg::Resize` to wake the loop.
        // `may_draw` is only re-checked at the top of the next iteration and
        // the loop only wakes on a message, so two snapshots sent back to back
        // coalesce into one read and the count cannot tell the wirings apart.
        tokio::spawn(async move {
            let _ = msg_tx
                .send(Msg::Snapshot {
                    rows: rows.clone(),
                    at: Instant::now(),
                })
                .await;
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Resize).await;
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx
                .send(Msg::Snapshot {
                    rows,
                    at: Instant::now(),
                })
                .await;
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Resize).await;
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
        });

        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/tmp/shep".to_string(),
            Instant::now(),
        );
        // Tall enough for `view::panes_for` to report a detail pane; a shorter
        // terminal is meant to skip the read entirely.
        let terminal = Terminal::new(TestBackend::new(120, 48)).unwrap();
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                test_paths(Path::new("/tmp/shep-lookout-tests")),
                PathBuf::from("/tmp/shep-lookout-tests"),
                PathBuf::from("/tmp/shep-lookout-tests/shep.toml"),
                PathBuf::from("/tmp/shep-lookout-tests/run/shep.sock"),
                local,
            ),
        )
        .await
        .expect("the loop left within ten seconds");

        let reads = log_sizes.load(Ordering::Relaxed);
        assert!(
            reads >= 2,
            "read {reads} times; an ordinary poll did not refresh the size"
        );
    }

    /// The end-to-end half of
    /// `the_heartbeat_asks_the_local_reader_for_a_host_sample`: a `source::Local` that
    /// reports a sample, one heartbeat, and the numbers on the rendered frame.
    ///
    /// Not `start_paused`: the redraw that carries the sample is gated on
    /// `MIN_REDRAW`, which reads real `std::time::Instant`, and a paused
    /// clock's virtual sleeps resolve in microseconds of real time, so the
    /// gate would never open. The heartbeat's first tick fires regardless of
    /// the clock, so the real wait below need only outlast `MIN_REDRAW`.
    #[tokio::test]
    async fn a_heartbeat_puts_the_host_strip_on_the_frame() {
        let (msg_tx, msg_rx) = mpsc::channel(64);
        let (poll_tx, _poll_rx) = mpsc::channel(4);
        let (request_tx, _request_rx) = mpsc::channel(2);
        let local = FakeLocal {
            sample: Some(crate::lookout::view::fixtures::sample()),
            ..FakeLocal::default()
        };

        tokio::spawn(async move {
            // The nudge, not the quit: `may_draw` is only re-checked at the
            // top of the next iteration, so real time elapsing while the loop
            // sits blocked in `select!` is never observed on its own.
            // `Msg::Resize` wakes it once real time has cleared `MIN_REDRAW`.
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Resize).await;
            tokio::time::sleep(MIN_REDRAW).await;
            let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
        });

        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/tmp/shep".to_string(),
            Instant::now(),
        );
        let terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let terminal = tokio::time::timeout(
            Duration::from_secs(10),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                test_paths(Path::new("/tmp/shep-lookout-tests")),
                PathBuf::from("/tmp/shep-lookout-tests"),
                PathBuf::from("/tmp/shep-lookout-tests/shep.toml"),
                PathBuf::from("/tmp/shep-lookout-tests/run/shep.sock"),
                local,
            ),
        )
        .await
        .expect("the loop left within ten seconds");

        let frame = crate::lookout::frames::render_text(terminal.backend().buffer());
        assert!(
            frame.contains("host  load  ██░░░░░░░░ 2.31 4.10 3.88 / 10 cores"),
            "the strip drew the sample the heartbeat took: {frame}"
        );
        assert!(
            !frame.contains("not read yet"),
            "and not the pre-heartbeat sentence"
        );
    }

    /// Ordinary terminals deliver auto-repeat as twenty to thirty Press
    /// events a second, each moving the selection; without the redraw gate
    /// this would be the fixed-clock process-table walk it exists to avoid,
    /// only faster. One request per redraw window, not one per key.
    #[tokio::test]
    async fn a_burst_of_selection_moves_costs_one_lamb_request() {
        let (msg_tx, msg_rx) = mpsc::channel(64);
        let (poll_tx, _poll_rx) = mpsc::channel(4);
        let (request_tx, mut request_rx) = mpsc::channel(2);
        let local = FakeLocal::default();

        let at = Instant::now();
        msg_tx
            .send(Msg::Snapshot {
                rows: (0..8)
                    .map(|id| {
                        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online).build()
                    })
                    .collect(),
                at,
            })
            .await
            .unwrap();
        for _ in 0..20 {
            msg_tx.send(Msg::Key(KeyPress::SelectDown)).await.unwrap();
        }
        tokio::spawn(async move {
            // The same nudge-then-quit shape as the feed's own burst test:
            // the redraw gate is read once per loop iteration.
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Resize).await;
            tokio::time::sleep(MIN_REDRAW).await;
            let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
        });

        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/tmp/shep".to_string(),
            Instant::now(),
        );
        let terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                test_paths(Path::new("/tmp/shep-lookout-tests")),
                PathBuf::from("/tmp/shep-lookout-tests"),
                PathBuf::from("/tmp/shep-lookout-tests/shep.toml"),
                PathBuf::from("/tmp/shep-lookout-tests/run/shep.sock"),
                local,
            ),
        )
        .await
        .expect("the loop left within five seconds");

        let mut asked = 0;
        while let Ok(sent) = request_rx.try_recv() {
            assert!(matches!(sent, Sent::Lambs { .. }));
            asked += 1;
        }
        assert_eq!(asked, 1, "twenty moves, one Describe");
    }

    /// `ui_event_loop::run_ui` knows the height; the reducer does not, and does not need to.
    #[tokio::test]
    async fn no_lambs_are_requested_when_the_detail_pane_is_not_drawn() {
        let (msg_tx, msg_rx) = mpsc::channel(64);
        let (poll_tx, _poll_rx) = mpsc::channel(4);
        let (request_tx, mut request_rx) = mpsc::channel(2);
        let local = FakeLocal::default();

        let at = Instant::now();
        msg_tx
            .send(Msg::Snapshot {
                rows: (0..8)
                    .map(|id| {
                        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online).build()
                    })
                    .collect(),
                at,
            })
            .await
            .unwrap();
        for _ in 0..20 {
            msg_tx.send(Msg::Key(KeyPress::SelectDown)).await.unwrap();
        }
        tokio::spawn(async move {
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Resize).await;
            tokio::time::sleep(MIN_REDRAW).await;
            let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
        });

        let app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/tmp/shep".to_string(),
            Instant::now(),
        );
        // The 18-row tier: `super::super::view::panes_for(20).detail` is false, so the
        // detail pane is not drawn even though the host strip and feed are.
        let terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                test_paths(Path::new("/tmp/shep-lookout-tests")),
                PathBuf::from("/tmp/shep-lookout-tests"),
                PathBuf::from("/tmp/shep-lookout-tests/shep.toml"),
                PathBuf::from("/tmp/shep-lookout-tests/run/shep.sock"),
                local,
            ),
        )
        .await
        .expect("the loop left within five seconds");

        assert!(
            request_rx.try_recv().is_err(),
            "no lamb request when the detail pane is not drawn"
        );
    }

    /// Drives the `Effect::LoadSecrets` arm itself, the only test that does
    /// for this bug: every other secrets-pane regression lives in
    /// `super::super::app::secrets_keys::tests` and calls `App::update` directly, so
    /// nothing else runs this loop's own read of `pane.tab`.
    ///
    /// `TabNext`'s own clamp (`(tab + 1).min(last)`) re-derives the index
    /// from whatever list is current, so it cannot go stale no matter how
    /// far the environments list has shrunk. `TabPrev` only subtracts one
    /// from `tab`, with no such re-derivation, so it takes two dropped
    /// environments, not one, before it can be handed a `tab` further past
    /// the end than a lone subtraction can walk back: three environments
    /// down to one, sitting on the last tab, `TabPrev` once. Three down to
    /// two self-corrects either direction, which is why the narrower
    /// `a_shrinking_environment_list_leaves_the_tab_somewhere_valid`, over in
    /// `super::super::app::secrets_keys::tests`, cannot exercise this arm: it is the
    /// reducer-level half of this same bug, not this one.
    #[tokio::test]
    async fn a_tab_past_a_shrunk_environment_list_does_not_panic_the_loop() {
        let dir = tempfile::Builder::new().prefix("s").tempdir().unwrap();
        let paths = crate::secret_readers::test_support::paths_under(dir.path());

        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            dir.path().display().to_string(),
            Instant::now(),
        );
        // Built with plain `App::update` calls, off `ui_event_loop::run_ui` entirely: none
        // of this touches disk, so nothing here races the real read below.
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".to_string(),
            result: Ok(Box::new(crate::lookout::secrets::SecretsModel {
                environments: vec!["dev".to_string(), "staging".to_string(), "prod".to_string()],
                ..crate::lookout::secrets::SecretsModel::default()
            })),
        });
        let _ = app.update(Msg::Key(KeyPress::TabNext));
        let _ = app.update(Msg::Key(KeyPress::TabNext));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 2, "sitting on the rightmost tab, `prod`");
        // The environments this pane knows about collapse to one, the way
        // `super::super::secrets::model`'s union does once every operator key naming
        // `staging` and `prod` has been unset out from under it.
        let _ = app.update(Msg::Secrets {
            environment: "prod".to_string(),
            result: Ok(Box::new(crate::lookout::secrets::SecretsModel {
                environments: vec!["dev".to_string()],
                ..crate::lookout::secrets::SecretsModel::default()
            })),
        });

        let (msg_tx, msg_rx) = mpsc::channel(4);
        let (poll_tx, _poll_rx) = mpsc::channel(1);
        let (request_tx, _request_rx) = mpsc::channel(2);
        // `TabPrev` is the reload that would have indexed the stale `tab`
        // straight into the gap; `Quit` right behind it so the loop leaves
        // on its own once that arm has run.
        msg_tx.send(Msg::Key(KeyPress::TabPrev)).await.unwrap();
        msg_tx.send(Msg::Key(KeyPress::Quit)).await.unwrap();

        let terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let done = tokio::time::timeout(
            Duration::from_secs(10),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                paths.clone(),
                dir.path().to_path_buf(),
                paths.daemon_config.clone(),
                paths.socket.clone(),
                FakeLocal::default(),
            ),
        )
        .await;

        assert!(
            done.is_ok(),
            "the loop must reach `Quit` rather than panic on a dangling tab"
        );
    }
}
