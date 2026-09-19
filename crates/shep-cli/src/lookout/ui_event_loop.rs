use super::MIN_REDRAW;
use super::app::{App, Body, Effect, Msg, RevealedValue, RowKey, Sent};
use super::batch_dispatch::{enable_refusal_message, send_batch};
use super::wiring::HEARTBEAT;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use shep_core::paths::ShepPaths;
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc;

/// The UI loop.
///
/// Generic over the backend and the key source, so a test drives it with a
/// `TestBackend` and a finite `tail::Stream`, and gets the terminal back.
///
/// Five `biased` arms: `SIGTERM`, the keyboard, the link, the settings
/// screen's finished file I/O, the heartbeat. An exhausted source is `Ready`
/// forever, so every arm above the heartbeat is disabled once it runs dry;
/// arm 4's is live, since an empty `FuturesUnordered` fills again. The redraw
/// runs after the `select!`, gated on `dirty` and [`MIN_REDRAW`]; the feed
/// and the lamb fetch ride that same gate. Ten arguments, hence the
/// `#[allow]`.
#[allow(clippy::too_many_arguments)]
pub async fn run_ui<B: Backend, S, L>(
    mut app: App,
    mut terminal: Terminal<B>,
    events: S,
    mut msgs: mpsc::Receiver<Msg>,
    polls: mpsc::Sender<()>,
    requests: mpsc::Sender<super::app::Sent>,
    // The resolved layout, read only by `Effect::LoadSecrets`.
    paths: ShepPaths,
    // `$SHEP_HOME` as this invocation resolved it. Only `Effect::LoadDogPane`
    // reads it: `commands::dogs::ask` sets `SHEP_HOME` for the probed
    // candidate, so a home other than `--home`'s could point a schema probe
    // at the live daemon's socket instead.
    home: std::path::PathBuf,
    // The settings screen's own read target. Two owned `PathBuf`s rather than
    // `&ShepPaths`, so a test can hand this loop an arbitrary pair, and cloned
    // into each `spawn_blocking` closure that outlives this stack frame.
    daemon_config: std::path::PathBuf,
    socket_default: std::path::PathBuf,
    mut local: L,
) -> Terminal<B>
where
    S: Stream<Item = std::io::Result<crossterm::event::Event>> + Unpin,
    L: super::source::Local,
{
    let mut events = events;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut sigterm = crate::shutdown::Terminate::install().ok();

    // Set once each, when their source runs dry.
    let mut keys_done = false;
    let mut link_done = false;

    let mut dirty = true;
    // Set by `Effect::RefreshFeed`, cleared once the coalesced read has run.
    let mut feed_dirty = false;
    // Set by `Effect::RefreshSelected` and by `Effect::PollNow`, cleared once
    // the coalesced request below has gone out.
    let mut lambs_dirty = false;
    // Set wherever `feed_dirty` is, cleared once the read below has run.
    //
    // The feed's cadence, not the lamb walk's: `Msg::Snapshot` answers every
    // ordinary poll with `Effect::RefreshFeed`, and its own comment gives the
    // reason this flag needs the same trigger, that "the selected row's log
    // paths can change even when the selection does not". Hung off
    // `lambs_dirty` this refreshed on a selection change and then never again
    // while an operator sat on one sheep watching it write.
    let mut log_size_dirty = false;
    // The settings screen's file I/O, in flight. Each entry resolves to the
    // `Msg` its result belongs in. A set, not a slot: a second write can be
    // raised while the first still runs. Dropping the set cancels nothing, so
    // a quit leaves no half-written file.
    let mut inflight: FuturesUnordered<BoxFuture<'static, Msg>> = FuturesUnordered::new();
    // `Option`, not `Instant::now() - MIN_REDRAW`: subtracting from a fresh
    // `Instant` can panic, and "has never drawn" is what this means.
    let mut last_draw: Option<Instant> = None;

    loop {
        // One gate, read once, so the feed is refreshed before the frame that
        // shows it and never on a frame that is not about to be drawn.
        let may_draw = last_draw.is_none_or(|at| at.elapsed() >= MIN_REDRAW);
        if feed_dirty && may_draw {
            // `feed_row`, not `selected_row`: both the full-screen pane and
            // the sheep pane's own embedded feed pin a sheep, and the
            // selection can move out from under either, so reading the
            // selection would draw another sheep's lines under a title
            // naming the pinned one.
            //
            // Nothing to read means an empty flock, or a pinned sheep that
            // has left it, and the pane's header says which. `super::tail::read`'s
            // `(None, None)` early return is for a different case, a sheep
            // whose shepherd predates the `out_file`/`err_file` fields.
            let tail = match app.feed_row() {
                None => super::tail::Tail::default(),
                Some(row) => {
                    // Cloned out before `app` is borrowed mutably.
                    let (out, err) = (row.info.out_file.clone(), row.info.err_file.clone());
                    local.tail(out.as_deref().map(Path::new), err.as_deref().map(Path::new))
                }
            };
            // `let _`: `Msg::Bleats` returns `Effect::None` by construction,
            // and acting on an effect here is where this could recurse.
            let _ = app.update(Msg::Bleats { tail });
            feed_dirty = false;
            dirty = true;
        }
        if lambs_dirty && may_draw {
            // Read here and not in the reducer: `run_ui` knows the terminal
            // and `App` does not, and a terminal too short to draw the detail
            // pane must not pay for a process-table walk it cannot show. A
            // size that cannot be read counts as too short.
            let height = terminal.size().map_or(0, |size| size.height);
            if super::view::panes_for(height).detail
                && let Some(RowKey::Sheep(id)) = app.selected()
            {
                // `try_send`, for `Effect::PollNow`'s reason: a full channel
                // means a request is already queued, and a dropped lamb fetch
                // reads as "not read yet".
                let _ = requests.try_send(Sent::Lambs { id });
            }
            lambs_dirty = false;
        }
        if log_size_dirty && may_draw {
            // The detail pane's log size. Read here for the same two reasons as
            // the walk above: `run_ui` owns the reader, and a terminal too
            // short to draw the pane must not pay for a read it cannot show.
            // It used to be two `fs::metadata` calls inside the draw, so up to
            // sixty a second for a number that moves when the sheep writes a
            // line.
            let height = terminal.size().map_or(0, |size| size.height);
            if super::view::panes_for(height).detail
                && let Some(RowKey::Sheep(id)) = app.selected()
            {
                // Paths cloned out before `app` is borrowed mutably, the same
                // as the feed read above.
                let paths = app
                    .row(id)
                    .map(|row| (row.info.out_file.clone(), row.info.err_file.clone()));
                if let Some((out, err)) = paths {
                    let total_bytes = local
                        .log_sizes(out.as_deref().map(Path::new), err.as_deref().map(Path::new));
                    // `let _`: `Msg::LogSize` returns `Effect::None` by
                    // construction.
                    let _ = app.update(Msg::LogSize { id, total_bytes });
                }
            }
            log_size_dirty = false;
        }
        if dirty && may_draw {
            // Told before the draw it is about to feed, not after: a
            // scrolled screen's cursor must never land on a row the
            // terminal that size implies could not have shown.
            if let Ok(size) = terminal.size() {
                let area = Rect::new(0, 0, size.width, size.height);
                app.note_body_rows(super::view::body_rows(area));
                app.note_body_width(area.width);
            }
            let _ = terminal.draw(|frame| super::view::draw(&app, frame));
            dirty = false;
            last_draw = Some(Instant::now());
        }

        let msg = tokio::select! {
            biased;
            () = async {
                match sigterm.as_mut() {
                    Some(signal) => {
                        signal.recv().await;
                    }
                    // No handler could be installed; this arm must then never
                    // complete, rather than spinning the loop.
                    None => std::future::pending().await,
                }
            } => break,
            event = events.next(), if !keys_done => match event {
                Some(Ok(crossterm::event::Event::Resize(..))) => Some(Msg::Resize),
                Some(Ok(event)) => super::input::map_key(&event, app.mode()).map(Msg::Key),
                // A key source that has ended, or has started erroring. Both
                // conditions are permanent, and both retire this arm: one
                // that keeps completing immediately, above the link and the
                // heartbeat, freezes the display and spins the process.
                Some(Err(_)) | None => {
                    keys_done = true;
                    None
                }
            },
            msg = msgs.recv(), if !link_done => match msg {
                Some(msg) => Some(msg),
                // Every sender dropped: the link task ended without freezing,
                // which only happens if it was aborted. Keep the last frame
                // up, and retire this arm.
                None => {
                    link_done = true;
                    None
                }
            },
            // `FuturesUnordered::next` is cancel-safe: the futures live in
            // the set, not in the future this arm polls, so losing a
            // `select!` race loses no progress. The precondition is not
            // optional: an empty set is `Ready(None)` on every poll.
            done = inflight.next(), if !inflight.is_empty() => done,
            _ = heartbeat.tick() => {
                // The host sample rides this arm rather than adding one of
                // its own: memory and a load average cost microseconds and no
                // process-table walk. Sampled unconditionally and refused by
                // the reducer once the link is lost, one enforcement point.
                let _ = app.update(Msg::Host { sample: local.host() });
                Some(Msg::Tick { now: Instant::now() })
            }
        };

        // Nothing to apply, and not a spin risk: an unbound keypress needs a
        // fresh keystroke, and each source retires once, ever.
        let Some(msg) = msg else { continue };

        match app.update(msg) {
            Effect::Quit => break,
            Effect::PollNow => {
                // `try_send`, not `send`: a full poll channel means a repair
                // is already queued, and blocking the UI on it would stall the
                // screen. A closed one means the link ended, which the reducer
                // handles by refusing `r` once the link is `Lost`.
                let _ = polls.try_send(());
                // `r` means "tell me again", so it refreshes the panes too.
                lambs_dirty = true;
                dirty = true;
            }
            // Not the read. A held `j` reaches an ordinary terminal as twenty
            // to thirty Press events a second, so a synchronous 128 KiB read
            // and a `Describe` here would sit behind every repeat, on the task
            // that also owns the redraw. Coalesced onto `MIN_REDRAW` instead.
            Effect::RefreshFeed => {
                feed_dirty = true;
                log_size_dirty = true;
                dirty = true;
            }
            Effect::RefreshSelected => {
                feed_dirty = true;
                lambs_dirty = true;
                log_size_dirty = true;
                dirty = true;
            }
            Effect::Send(sent) => {
                // `try_send`, not `send`: blocking the UI on a full channel
                // would stall the screen. A failure goes back to the reducer,
                // which is already showing an in-flight line about it.
                if let Err(err) = requests.try_send(sent) {
                    let (mpsc::error::TrySendError::Full(sent)
                    | mpsc::error::TrySendError::Closed(sent)) = err;
                    // `let _`: `Msg::Unsent` returns `Effect::None` by
                    // construction.
                    let _ = app.update(Msg::Unsent { sent });
                }
                dirty = true;
            }
            // Off this task, unlike the arm above: `try_send` would drop
            // the tail of a batch deeper than the channel, and this task
            // has no `.await` point in a bare loop to let the link task
            // drain it through. `batch_dispatch::send_batch` owns a cloned sender and
            // awaits each entry in turn, in one task, so order survives
            // and nothing is dropped for merely being full.
            Effect::SendAll(batch) => {
                inflight.push(Box::pin(send_batch(requests.clone(), batch)));
                dirty = true;
            }
            // Off this task: `spawn_blocking` even though the read takes no
            // lock, and pushed into `inflight` rather than awaited, as
            // `Effect::WriteSetting` does. The style is `app.style()`, already
            // resolved by `run_argv`, so the STYLE LEVEL row agrees.
            Effect::LoadSettings => {
                let path = daemon_config.clone();
                let socket_default = socket_default.clone();
                let style = app.style();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::commands::settings::load_settings(&path, &socket_default, style)
                });
                // Pushed, not awaited. The `Msg` is built inside the wrapper
                // so the arm that drains `inflight` stays one line.
                inflight.push(Box::pin(async move {
                    let result = handle
                        .await
                        .map_err(|err| err.to_string())
                        .and_then(|inner| inner.map_err(|err| err.to_string()));
                    Msg::Settings { result }
                }));
                dirty = true;
            }
            // Off this task for the same reason `Effect::WriteSetting` is:
            // the store's own lock (`ShepToml::try_edit`'s cousin over
            // `secrets.json`) acquires with no deadline.
            //
            // The environment is the current tab's, once there is one;
            // before the first load lands there is no tab yet, so this reads
            // the daemon's own configured default instead, and `Msg::Secrets`
            // echoes back whichever it used so the reducer can find that
            // environment's tab once the model arrives.
            Effect::LoadSecrets => {
                let paths = paths.clone();
                // `all_rows`, not the filtered `keymap::rows`: READ BY has to name
                // every sheep that reads a key, not just the ones a dashboard
                // name filter left on screen.
                let procs = app
                    .all_rows()
                    .into_iter()
                    .map(|row| row.info.clone())
                    .collect::<Vec<_>>();
                let environment = match app.body() {
                    Body::Secrets(pane) => pane.environment().map(str::to_string),
                    _ => None,
                }
                .unwrap_or_else(|| {
                    crate::commands::secret::daemon_config(&paths)
                        .daemon
                        .environment
                });
                let for_msg = environment.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::lookout::secrets::model(&paths, &procs, &environment)
                });
                inflight.push(Box::pin(async move {
                    let result = handle.await.map(Box::new).map_err(|err| err.to_string());
                    Msg::Secrets {
                        environment: for_msg,
                        result,
                    }
                }));
                dirty = true;
            }
            // Off this task for `Effect::LoadSettings`'s reason: a read that
            // takes no lock still stalls the redraw, the tick and the bus
            // drain while it opens and parses a file.
            //
            // The row and the environment ride back out on the `Msg`, which
            // is where the pane decides whether the answer is still the one
            // it asked for.
            Effect::RevealSecret {
                store,
                provider_cache,
                row,
                environment,
            } => {
                let key = row.key.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::lookout::secrets::stored_value(&store, &provider_cache, &row)
                });
                inflight.push(Box::pin(async move {
                    Msg::Revealed {
                        key,
                        environment,
                        // A join failure reads as no value, the same as a
                        // slot that has gone: there is nothing to show
                        // either way, and a notice would name a panic the
                        // operator cannot act on.
                        value: handle.await.ok().flatten().map(RevealedValue),
                    }
                }));
                dirty = true;
            }
            // `apply_setting` takes `ShepToml::try_edit`'s lock, which blocks
            // with no deadline, so the handle goes into `inflight` rather than
            // being awaited here. `_authority` is a proof carried by the
            // effect, not a value this arm reads.
            Effect::WriteSetting {
                edit,
                ticket,
                authority: _authority,
            } => {
                let path = daemon_config.clone();
                let for_msg = edit.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::commands::settings::apply_setting(&path, &edit)
                });
                inflight.push(Box::pin(async move {
                    let result = handle
                        .await
                        .map_err(|err| err.to_string())
                        .and_then(|inner| inner.map_err(|err| err.to_string()));
                    Msg::SettingWritten {
                        edit: for_msg,
                        ticket,
                        result,
                    }
                }));
                dirty = true;
            }
            // Off this task: an adopted dog's schema probe spawns its own
            // binary and can block up to `VERSION_BUDGET`, which would
            // freeze the redraw and bus drain if awaited inline. `Silent`
            // and `Unreadable` both mean no pane here.
            Effect::LoadDogPane { name, adopted_path } => {
                let home = home.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    let schema = match (crate::dog::builtin_schema(&name), adopted_path.as_deref())
                    {
                        (Some(schema), _) => Some(schema),
                        (None, Some(path)) => {
                            match crate::commands::dogs::ask_schema(
                                path,
                                &home,
                                &name,
                                crate::commands::dogs::VERSION_BUDGET,
                            ) {
                                crate::commands::dogs::DogSchema::Published(schema) => Some(schema),
                                crate::commands::dogs::DogSchema::Silent
                                | crate::commands::dogs::DogSchema::Unreadable => None,
                            }
                        }
                        (None, None) => None,
                    };
                    let result = schema.ok_or_else(|| {
                        format!("{name} publishes no schema; edit dogs.toml with $EDITOR")
                    });
                    Msg::DogPane {
                        name,
                        adopted_path,
                        result,
                    }
                });
                inflight.push(Box::pin(async move {
                    // A `spawn_blocking` that panicked is the one case this
                    // has no `Msg` for, and it is reported as the same
                    // refusal rather than dropped: a keystroke that produces
                    // silence reads as a key that is not bound.
                    handle.await.unwrap_or_else(|err| Msg::DogPane {
                        name: String::new(),
                        adopted_path: None,
                        result: Err(format!("the schema probe failed: {err}")),
                    })
                }));
                dirty = true;
            }
            // `dogs::enable_in_config`/`dogs::disable_in_config` take
            // `ShepToml`'s own lock, which blocks with no deadline, so this
            // arm does not wait for it either. `_authority` is dropped as in
            // `Effect::WriteSetting`.
            Effect::WriteDog {
                edit,
                ticket,
                authority: _authority,
            } => {
                let path = daemon_config.clone();
                let for_msg = edit.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    if edit.enable {
                        crate::commands::dogs::enable_in_config(&path, &edit.name)
                            .map_err(|err| enable_refusal_message(&err, &edit.name))
                    } else {
                        crate::commands::dogs::disable_in_config(&path, &edit.name)
                            .map_err(|err| err.to_string())
                    }
                });
                // `Msg::DogWritten` answers with `Effect::Send` on the `Ok`
                // arm, and reaches this loop through `inflight` like any other
                // message, so the daemon half of a dog toggle goes out exactly
                // as an ordinary key's `Effect::Send` does.
                inflight.push(Box::pin(async move {
                    let result = handle
                        .await
                        .map_err(|err| err.to_string())
                        .and_then(|inner| inner);
                    Msg::DogWritten {
                        edit: for_msg,
                        ticket,
                        result,
                    }
                }));
                dirty = true;
            }
            // `super::secrets::set`/`super::secrets::unset` take `secrets.json.lock`,
            // which acquires with no deadline, for `Effect::WriteSetting`'s
            // reason. `_authority` is dropped as it is there.
            Effect::WriteSecret(edit, _authority) => {
                let store = paths.secrets.clone();
                // The `bool` is `unset`'s own answer: `false` means there was
                // no slot to remove. It reaches `Msg::SecretWritten` rather
                // than being dropped, so a delete that removed nothing
                // cannot arrive looking like a delete that worked. A `set`
                // always changed the store, so it says `true`.
                let handle = tokio::task::spawn_blocking(move || match edit.value {
                    Some(value) => {
                        shep_core::secrets::set(&store, &edit.key, &edit.environment, &value)
                            .map(|()| true)
                    }
                    None => shep_core::secrets::unset(&store, &edit.key, &edit.environment),
                });
                inflight.push(Box::pin(async move {
                    let result = handle
                        .await
                        .map_err(|err| err.to_string())
                        .and_then(|inner| inner.map_err(|err| err.to_string()));
                    Msg::SecretWritten { result }
                }));
                dirty = true;
            }
            // Straight to `io::stdout()` through `term`, not `terminal`
            // (`ratatui::Terminal`): a `TestBackend` verifies nothing here.
            // Ignored on failure: nothing sensible to do with a broken
            // pipe, and the pane's wording already says sent, not arrived.
            Effect::CopyToClipboard(value) => {
                let _ = super::term::copy_to_clipboard(&value.0);
                dirty = true;
            }
            Effect::None => dirty = true,
        }
    }

    terminal
}

#[cfg(test)]
mod tests {

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
    /// Held up by taking `shep.toml`'s own lock from a second file descriptor.
    /// `flock(2)` excludes per open file description, not per process, so
    /// `ShepToml::try_edit` blocks with no deadline and nothing to wake it.
    /// `q` after the confirm is what has to keep working. Unix only: the
    /// Windows arm of `ConfigLock` polls a `share_mode(0)` open instead.
    ///
    /// The clock is the real one: a paused clock auto-advances only while the
    /// runtime is idle, and a thread parked on a lock is not idle, so the
    /// timeout would never fire.
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
        // `space` arms the first row (`[daemon] log_level`), `Enter` sends
        // it, and `q` is the key that must still be answered.
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
                dir.path().to_path_buf(),
                config.clone(),
                socket_default,
                FakeLocal::default(),
            ),
        )
        .await
        .expect("the loop answered `q` with a write still in flight");

        // The other half of the same property: the write was not cancelled by
        // the loop leaving. `spawn_blocking` runs its closure to completion
        // whatever happens to the handle, so releasing the lock here lets it
        // land.
        // A bound so a lost write reports rather than hangs, 3_000 * 10ms =
        // thirty seconds. Not a claim about speed: when the blocking pool
        // reaches the closure is the runner's business, not shep's.
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

    /// Drives `Effect::LoadSecrets` end to end, the only test that does: every
    /// other secrets-pane test calls `App::update` directly and inspects the
    /// `Effect` it returns as a value, so nothing ever runs the arm that
    /// reads `paths.secrets` and gathers who reads each key.
    ///
    /// `reader-app` is filtered off the dashboard (`App::rows()`) by name
    /// before `S` opens the pane, but it still names `API_KEY` in the muster
    /// roll. `Effect::LoadSecrets` has to gather readers off
    /// `App::all_rows()`, not `App::rows()`, or a name filter would make
    /// `READ BY` lie about who reads a key.
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
                dir.path().to_path_buf(),
                paths.daemon_config.clone(),
                paths.socket.clone(),
                FakeLocal::default(),
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
}
