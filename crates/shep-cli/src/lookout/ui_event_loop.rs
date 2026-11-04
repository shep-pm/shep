use super::MIN_REDRAW;
use super::app::{
    App, Body, DogRun, DogRunner, Effect, InputMode, Msg, Pasted, RevealedValue, RowKey, Sent,
};
use super::batch_dispatch::{enable_refusal_message, send_batch};
use super::pane::{DeclaredPane, SheepDogEntry};
use super::wiring::{HEARTBEAT, HINT_EVERY};
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
use tokio::task::JoinHandle;

/// The UI loop, generic over the backend and the key source so a test can
/// drive it with a `TestBackend` and get the terminal back.
///
/// An exhausted key source is `Ready` forever, so every arm above the
/// heartbeat is disabled once it runs dry; arm 4's is live, since an empty
/// `FuturesUnordered` fills again.
#[allow(clippy::too_many_arguments)]
pub async fn run_ui<B: Backend, S, L>(
    mut app: App,
    mut terminal: Terminal<B>,
    events: S,
    mut msgs: mpsc::Receiver<Msg>,
    polls: mpsc::Sender<()>,
    requests: mpsc::Sender<super::app::Sent>,
    // `paths.home` is `$SHEP_HOME` as this invocation resolved it, which
    // `Effect::LoadDogPane` hands the schema probe: `commands::dogs::ask`
    // sets `SHEP_HOME` for the probed candidate, so a home other than
    // `--home`'s could point a probe at the live daemon's socket instead.
    paths: ShepPaths,
    mut local: L,
    // Which `cargo` a dog job runs, and which crates.io it asks.
    dogs: DogRunner,
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
    // Set wherever `feed_dirty` is, cleared once the read below has run. The
    // feed's cadence, not the lamb walk's: a selected row's log paths can
    // change while the selection does not, so this rides
    // `Effect::RefreshFeed` rather than `lambs_dirty`.
    let mut log_size_dirty = false;
    // The settings screen's file I/O, in flight. Each entry resolves to the
    // `Msg` its result belongs in. A set, not a slot: a second write can be
    // raised while the first still runs. Dropping the set cancels nothing, so
    // a quit leaves no half-written file.
    let mut inflight: FuturesUnordered<BoxFuture<'static, Msg>> = FuturesUnordered::new();
    // `Option`, not `Instant::now() - MIN_REDRAW`: subtracting from a fresh
    // `Instant` can panic, and "has never drawn" is what this means.
    let mut last_draw: Option<Instant> = None;
    // The one dog job running, if any: what its thread reports, and the
    // handles that end the run. Ended after the loop rather than by dropping
    // here, so the loop waits for the thread that holds the `cargo`.
    let mut dog: Option<DogRun> = None;
    // Whether a snapshot has landed, and when the hint was last read. On the
    // tokio clock so a paused one can drive the cadence.
    let mut flock_seen = false;
    let mut hint_read: Option<tokio::time::Instant> = None;

    loop {
        // One gate, read once, so the feed is refreshed before the frame that
        // shows it and never on a frame that is not about to be drawn. A
        // modal is drawn past the throttle: it answers nothing until a draw
        // has told it its reach, and the loop only reaches one when woken.
        let may_draw =
            last_draw.is_none_or(|at| at.elapsed() >= MIN_REDRAW) || app.modal().is_some();
        if feed_dirty && may_draw {
            // `feed_row`, not `selected_row`: both feeds pin a sheep, and the
            // selection can move out from under either, so reading the
            // selection would draw another sheep's lines under a title naming
            // the pinned one. Over a host, the paths name unreadable files.
            let tail = match app.feed_row() {
                Some(_) if app.remote_host().is_some() => super::tail::Tail::default(),
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
            // The detail pane's log size. Read here for the same two reasons
            // as the walk above: `run_ui` owns the reader, and a terminal too
            // short to draw the pane must not pay for a read it cannot show.
            let height = terminal.size().map_or(0, |size| size.height);
            if super::view::panes_for(height).detail
                && app.remote_host().is_none()
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
                app.note_modal_reach(super::view::scroll_reach(&app, area));
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
                // Only while a box is open; elsewhere a paste is nothing, and
                // the reducer would say so too.
                Some(Ok(crossterm::event::Event::Paste(text))) => {
                    (app.key_mode() == InputMode::Text).then(|| Msg::Paste(Pasted::new(text)))
                }
                Some(Ok(event)) => super::input::map_key(&event, app.key_mode()).map(Msg::Key),
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
            // The running dog job's own thread reports here. `None` retires
            // the arm until the next job, and drops the cancel handle with
            // the run that ended.
            dog_msg = async {
                match dog.as_mut() {
                    Some(run) => run.msgs.recv().await,
                    // No job running: this arm must never complete, rather
                    // than spin the loop above the heartbeat.
                    None => std::future::pending().await,
                }
            } => match dog_msg {
                Some(msg) => Some(msg),
                None => {
                    dog = None;
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
                // process-table walk. Not over a host: this machine's load is
                // not the host's.
                if app.remote_host().is_none() {
                    let _ = app.update(Msg::Host { sample: local.host() });
                }
                // Once after the first snapshot, then at most every
                // `HINT_EVERY`: it stats every sheep's log and reads
                // `hints.json`, which one-second beats would repeat for
                // nothing. Not over a host: its flock is not this machine's.
                if flock_seen
                    && app.remote_host().is_none()
                    && hint_read.is_none_or(|at| at.elapsed() >= HINT_EVERY)
                {
                    hint_read = Some(tokio::time::Instant::now());
                    let flock: Vec<_> = app.flock_infos().cloned().collect();
                    let _ = app.update(Msg::Hint { line: local.hint(&flock) });
                }
                Some(Msg::Tick { now: Instant::now() })
            }
        };

        // Nothing to apply, and not a spin risk: an unbound keypress needs a
        // fresh keystroke, and each source retires once, ever.
        let Some(msg) = msg else { continue };
        // `Quit` leaves the loop below and nothing reads this after it.
        dirty = true;
        flock_seen |= matches!(msg, Msg::Snapshot { .. });

        match app.update(msg) {
            // The job this left running is ended after the loop, where no
            // way out of the loop can step over it.
            Effect::Quit => break,
            Effect::PollNow => {
                // `try_send`, not `send`: a full poll channel means a repair
                // is already queued, and blocking the UI on it would stall the
                // screen. A closed one means the link ended, which the reducer
                // handles by refusing `r` once the link is `Lost`.
                let _ = polls.try_send(());
                // `r` means "tell me again", so it refreshes the panes too.
                lambs_dirty = true;
            }
            // Not the read. A held `j` reaches an ordinary terminal as twenty
            // to thirty Press events a second, so a synchronous 128 KiB read
            // and a `Describe` here would sit behind every repeat, on the task
            // that also owns the redraw. Coalesced onto `MIN_REDRAW` instead.
            Effect::RefreshFeed => {
                feed_dirty = true;
                log_size_dirty = true;
            }
            Effect::RefreshSelected => {
                feed_dirty = true;
                lambs_dirty = true;
                log_size_dirty = true;
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
            }
            // Off this task, unlike the arm above: `try_send` would drop the
            // tail of a batch deeper than the channel, and this task has no
            // `.await` point in a bare loop to let the link task drain it
            // through. `batch_dispatch::send_batch` awaits each entry in turn.
            Effect::SendAll(batch) => {
                inflight.push(Box::pin(send_batch(requests.clone(), batch)));
            }
            // Off this task: `spawn_blocking` even though the read takes no
            // lock, and pushed into `inflight` rather than awaited, as
            // `Effect::WriteSetting` does. The style is `app.style()`, already
            // resolved by `run_argv`, so the STYLE LEVEL row agrees.
            Effect::LoadSettings => {
                let path = paths.daemon_config.clone();
                let socket_default = paths.socket.clone();
                let style = app.style();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::commands::settings::load_settings(&path, &socket_default, style)
                });
                // Pushed, not awaited. The `Msg` is built inside the wrapper
                // so the arm that drains `inflight` stays one line.
                inflight.push(Box::pin(async move {
                    let result = join_blocking(handle).await;
                    Msg::Settings { result }
                }));
            }
            // Off this task for `Effect::LoadSettings`' reason. The cache read
            // never waits on the network: a fetch it finds due comes back as
            // `Effect::FetchCatalogue`.
            Effect::LoadCatalogue { fetch } => {
                let paths = paths.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    super::app::catalogue_read_cache(&paths, &crate::dog_index::index_url(), fetch)
                });
                inflight.push(Box::pin(async move {
                    Msg::Catalogue {
                        result: join_blocking(handle).await,
                    }
                }));
            }
            // The only effect that reaches the network. In `inflight`, so the
            // screen draws and keys move while the index is on its way.
            Effect::FetchCatalogue => {
                let paths = paths.clone();
                inflight.push(Box::pin(async move {
                    let url = crate::dog_index::index_url();
                    Msg::Catalogue {
                        result: super::app::catalogue_fetch(paths, url).await,
                    }
                }));
            }
            // Off this task for `Effect::WriteSetting`'s reason: the store's
            // own lock acquires with no deadline. The environment is the
            // current tab's, or the daemon's configured default before the
            // first load lands; `Msg::Secrets` echoes back whichever it used.
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
            }
            // Off this task for `Effect::LoadSettings`'s reason: a read that
            // takes no lock still stalls the redraw, the tick and the bus
            // drain while it opens and parses a file. The row and the
            // environment ride back out on the `Msg` for the pane to match.
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
                let path = paths.daemon_config.clone();
                let for_msg = edit.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::commands::settings::apply_setting(&path, &edit)
                });
                inflight.push(Box::pin(async move {
                    let result = join_blocking(handle).await;
                    Msg::SettingWritten {
                        edit: for_msg,
                        ticket,
                        result,
                    }
                }));
            }
            // Off this task: an adopted dog's schema probe spawns its own
            // binary and can block up to `VERSION_BUDGET`, which would
            // freeze the redraw and bus drain if awaited inline. `Silent`
            // and `Unreadable` both mean no pane here.
            Effect::LoadDogPane { name, adopted_path } => {
                let home = paths.home.clone();
                let config = paths.daemon_config.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    // Read under the same blocking half as the probe: the
                    // pane's env rows name the keys `shep.toml` sets, and a
                    // value never comes here at all.
                    let env_keys = crate::commands::shep_toml::ShepToml::read_only(&config)
                        .map(|cfg| cfg.adopted_dog_env(&name).keys().cloned().collect())
                        .unwrap_or_default();
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
                        env_keys,
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
                        env_keys: Vec::new(),
                        result: Err(format!("the schema probe failed: {err}")),
                    })
                }));
            }
            // The cache only, never the binary: this spawns no dog. Off this
            // task because the cache is keyed on a digest of the binary.
            // A read that panicked declares no pane.
            Effect::ReadDogPane {
                name,
                opening,
                adopted_path,
            } => {
                let home = paths.home.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::commands::dogs::cached_manifest(&adopted_path, &home)
                        .filter(|manifest| manifest.pane)
                        .map(|manifest| DeclaredPane {
                            actions: manifest.actions.into_iter().map(|a| a.name).collect(),
                            events: manifest.events.into_iter().map(|e| e.kind).collect(),
                        })
                });
                inflight.push(Box::pin(async move {
                    Msg::DogDeclaresPane {
                        name,
                        opening,
                        declared: handle.await.ok().flatten(),
                    }
                }));
            }
            // Off this task for `Effect::LoadDogPane`'s reason. A listing
            // that panicked lists no dog, which leaves every table read-only
            // rather than editable through a schema nobody read.
            Effect::ListSheepDogs { sheep, ask } => {
                let paths = paths.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    super::probe_sheep_dogs::list_sheep_dogs(&paths)
                });
                inflight.push(Box::pin(async move {
                    let dogs = handle.await.unwrap_or_default();
                    Msg::SheepDogNames { sheep, ask, dogs }
                }));
            }
            // One dog, asked when its row is opened. A probe that panicked
            // answers with no schema, which leaves the table read-only.
            Effect::LoadSheepDog {
                sheep,
                dog,
                adopted_path,
            } => {
                let paths = paths.clone();
                let name = dog.clone();
                let path = adopted_path.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    let budget = crate::commands::dogs::VERSION_BUDGET;
                    super::probe_sheep_dogs::probe_sheep_dog(&paths, &name, path, budget)
                });
                inflight.push(Box::pin(async move {
                    let entry = handle.await.unwrap_or(SheepDogEntry {
                        name: dog,
                        adopted_path,
                        schema: None,
                        standard: None,
                    });
                    Msg::SheepDog { sheep, entry }
                }));
            }
            // Each standard dog asked again, off this task for the listing's
            // reason. A dog that goes silent answers with no section, which
            // leaves its rows as they were.
            Effect::RefreshSheepDogSections { sheep, dogs } => {
                let paths = paths.clone();
                let handle = tokio::task::spawn_blocking(move || {
                    let budget = crate::commands::dogs::VERSION_BUDGET;
                    dogs.into_iter()
                        .map(|(name, path)| {
                            super::probe_sheep_dogs::probe_sheep_dog(&paths, &name, path, budget)
                        })
                        .collect::<Vec<_>>()
                });
                inflight.push(Box::pin(async move {
                    let entries = handle.await.unwrap_or_default();
                    Msg::SheepDogSections { sheep, entries }
                }));
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
                let path = paths.daemon_config.clone();
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
                    let result = join_blocking(handle).await;
                    Msg::DogWritten {
                        edit: for_msg,
                        ticket,
                        result,
                    }
                }));
            }
            // One dog pane's `env` edits, through the same writer
            // `shep enable` uses, so the entry keeps its path and the two
            // doors cannot write it differently. `_authority` is dropped as
            // in `Effect::WriteSetting`.
            Effect::WriteDogEnv { write, then } => {
                let path = paths.daemon_config.clone();
                let for_msg = write.name.clone();
                let ticket = write.ticket;
                let handle = tokio::task::spawn_blocking(move || write.apply(&path));
                inflight.push(Box::pin(async move {
                    let result = join_blocking(handle).await;
                    Msg::DogEnvWritten {
                        name: for_msg,
                        ticket,
                        then,
                        result,
                    }
                }));
            }
            // One of a dog's other `[daemon] keys, off the settings screen.
            // `spawn_blocking` for `Effect::WriteSetting`'s reason: both halves
            // take `shep.toml`'s lock, and a grant reads the dog's manifest,
            // which can cost up to `VERSION_BUDGET` of a binary starting up.
            Effect::WriteDogTune {
                tune,
                ticket,
                authority: _authority,
            } => {
                let paths = paths.clone();
                let for_msg = tune.clone();
                let handle = tokio::task::spawn_blocking(move || match &tune {
                    super::app::DogTune::BootFirst { name, first } => {
                        crate::commands::dogs::boot_first_in_config(
                            &paths.daemon_config,
                            name,
                            *first,
                        )
                        .map(|()| super::app::DogTuneLanded::BootOrder)
                        .map_err(|err| err.to_string())
                    }
                    super::app::DogTune::Grant {
                        name,
                        permission,
                        grant,
                    } => {
                        let change = if *grant {
                            crate::commands::dogs::Change::Grant
                        } else {
                            crate::commands::dogs::Change::Revoke
                        };
                        crate::commands::dogs::record_change(&paths, change, name, permission)
                            .map(|applied| super::app::DogTuneLanded::Grants {
                                entry: applied.entry,
                                enabled: applied.enabled,
                            })
                            .map_err(|refusal| match refusal {
                                crate::commands::dogs::GrantRefusal::Usage { message, .. } => {
                                    message
                                }
                                crate::commands::dogs::GrantRefusal::Config(err) => err.to_string(),
                            })
                    }
                });
                inflight.push(Box::pin(async move {
                    let result = join_blocking(handle).await;
                    Msg::DogTuneWritten {
                        tune: for_msg,
                        ticket,
                        result,
                    }
                }));
            }
            // `super::secrets::set`/`super::secrets::unset` take `secrets.json.lock`,
            // which acquires with no deadline, for `Effect::WriteSetting`'s
            // reason. `_authority` is dropped as it is there.
            Effect::WriteSecret(edit, _authority) => {
                let store = paths.secrets.clone();
                // The `bool` is `unset`'s own answer: `false` means there was
                // no slot to remove. It reaches `Msg::SecretWritten` rather
                // than being dropped, so a delete that removed nothing cannot
                // arrive looking like one that worked.
                let handle = tokio::task::spawn_blocking(move || match edit.value {
                    Some(value) => {
                        shep_core::secrets::set(&store, &edit.key, &edit.environment, &value)
                            .map(|()| true)
                    }
                    None => shep_core::secrets::unset(&store, &edit.key, &edit.environment),
                });
                inflight.push(Box::pin(async move {
                    let result = join_blocking(handle).await;
                    Msg::SecretWritten { result }
                }));
            }
            // A rename: both halves in one task, so one `Enter` gets one
            // answer and one re-read. Each half still takes the store's own
            // lock, so a rename that fails at the removal leaves the key under
            // two names rather than under none.
            Effect::WriteSecrets(edits, _authority) => {
                let store = paths.secrets.clone();
                let handle = tokio::task::spawn_blocking(
                    move || -> Result<bool, shep_core::secrets::SecretError> {
                        let mut changed = false;
                        for edit in &edits {
                            match &edit.value {
                                Some(value) => {
                                    shep_core::secrets::set(
                                        &store,
                                        &edit.key,
                                        &edit.environment,
                                        value,
                                    )?;
                                    changed = true;
                                }
                                None => {
                                    changed |= shep_core::secrets::unset(
                                        &store,
                                        &edit.key,
                                        &edit.environment,
                                    )?
                                }
                            }
                        }
                        Ok(changed)
                    },
                );
                inflight.push(Box::pin(async move {
                    let result = join_blocking(handle).await;
                    Msg::SecretWritten { result }
                }));
            }
            // Straight to `io::stdout()` through `term`, not `terminal`
            // (`ratatui::Terminal`): a `TestBackend` verifies nothing here.
            // Ignored on failure: nothing sensible to do with a broken
            // pipe, and the pane's wording already says sent, not arrived.
            Effect::CopyToClipboard(value) => {
                let _ = super::term::copy_to_clipboard(&value.0);
            }
            // The tag names which question the job asked, so an answer to
            // one modal cannot answer another.
            Effect::ModalAnswered { tag, yes } => app.answer_grant(tag, yes),
            Effect::None => {}
            // Off this task, on a thread with a current-thread runtime of its
            // own: the grant callback blocks on a modal answer and the run
            // starts `cargo`, and the UI task can hold neither. `_authority`
            // is a proof this arm never reads.
            Effect::StartDogJob {
                job,
                authority: _authority,
            } => {
                dog = Some(dogs.start(&paths, job));
            }
        }
    }

    // A job still running when this loop leaves must end here, before the
    // process does: dropping the run future is what kills a `cargo` mid-build,
    // and a thread killed by process exit runs no destructors.
    if let Some(mut run) = dog.take() {
        // First, or a session parked in its callback never reaches that drop
        // and the wait below runs out on a job that was never going to move.
        app.release_dog_grants();
        // `false` is a build this could not stop, and no screen is left to
        // say so on.
        let _ = run.end();
    }

    terminal
}

/// A `spawn_blocking` handle's result with a join failure and the task's own
/// error both read as a message.
async fn join_blocking<T, E: ToString>(handle: JoinHandle<Result<T, E>>) -> Result<T, String> {
    handle
        .await
        .map_err(|err| err.to_string())
        .and_then(|inner| inner.map_err(|err| err.to_string()))
}

#[cfg(test)]
mod tests;
