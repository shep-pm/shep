//! The reducer itself: a `Msg` in, an `Effect` out, and no I/O in between.

use super::*;

impl App {
    /// A dashboard with an empty flock, a live link, and no notice.
    #[must_use]
    pub fn new(palette: Palette, control: Control, home: String, now: Instant) -> Self {
        Self {
            flock: BTreeMap::new(),
            selected: None,
            filter: String::new(),
            mode: InputMode::Normal,
            next_write_ticket: 0,
            link: Link::Live,
            notice: None,
            palette,
            control,
            home,
            now,
            froze_at: None,
            frozen_for: Duration::ZERO,
            host: None,
            host_unsupported: false,
            feed: crate::lookout::tail::Tail::default(),
            log_size: None,
            lambs: None,
            action: None,
            body: Body::FlockTable,
            config_target: None,
            config_for: None,
            dog_target: None,
            close_dialog: None,
            held: None,
            style: (StyleLevel::Full, StyleSource::Default),
            cpu_history: HashMap::new(),
            flock_cpu: VecDeque::new(),
            rss_history: HashMap::new(),
            cpu_last: HashMap::new(),
            grouping: Grouping::Flat,
            collapsed_folds: HashSet::new(),
            keymap_open: false,
        }
    }

    /// Applies one message and reports what the caller must do next.
    pub fn update(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::Snapshot { rows, at } => {
                // The link task has ended, so nothing is left to produce a
                // snapshot. Accepting one would un-freeze the dashboard.
                if matches!(self.link, Link::Lost { .. }) {
                    return Effect::None;
                }
                let previous = self.selected_index();
                self.flock = rows
                    .into_iter()
                    .map(|info| (info.id, Row { info, anchor: at }))
                    .collect();
                self.record_samples(at);
                self.reseat(previous);
                self.forget_missing_target();
                // Unconditional: the selected row's log paths can change even
                // when the selection does not, and this is the feed's cadence.
                Effect::RefreshFeed
            }
            Msg::Event(event) => self.on_event(event),
            Msg::BusLagged { count } => {
                self.notice = Some(Notice {
                    text: format!(
                        "lookout fell behind and lost {count} events; re-reading the flock"
                    ),
                    grave: false,
                });
                Effect::PollNow
            }
            Msg::Retrying { attempt } => {
                if !matches!(self.link, Link::Lost { .. }) {
                    self.link = Link::Retrying { attempt };
                    self.disarm_on_link_change();
                }
                Effect::None
            }
            Msg::Relinked => {
                if !matches!(self.link, Link::Lost { .. }) {
                    self.link = Link::Live;
                }
                Effect::None
            }
            Msg::Frozen { at_local, why } => {
                self.link = Link::Lost { at_local, why };
                self.froze_at = Some(self.now);
                self.frozen_for = Duration::ZERO;
                self.disarm_on_link_change();
                // Every notice is about a shepherd that no longer exists,
                // and none of them can be acted on. Left standing, the last
                // one outranks the key hint for the rest of the session
                // (`view::status::status_line`'s own ordering), so an
                // operator reads `the shepherd is shutting down` where the
                // bar should be naming the keys that still work. Whether the
                // shutdown was clean is on the screen either way: the link
                // panel quotes an error that says the socket was removed
                // rather than refusing.
                self.notice = None;
                Effect::None
            }
            Msg::Tick { now } => {
                if !matches!(self.link, Link::Lost { .. }) {
                    self.now = now;
                    let expired = self.action.as_ref().is_some_and(|action| {
                        action.stage == Stage::Armed
                            && now.saturating_duration_since(action.at) >= CONFIRM_EXPIRY
                    });
                    if expired {
                        self.action = None;
                    }
                    let stale = self.close_dialog.as_ref().is_some_and(|dialog| {
                        now.saturating_duration_since(dialog.at()) >= CONFIRM_EXPIRY
                    });
                    if stale {
                        self.close_dialog = None;
                    }
                    // A reply that never comes cannot strand the verb: it
                    // rides the same clock as the dialog it followed from.
                    let stale_held = self.held.as_ref().is_some_and(|held| {
                        now.saturating_duration_since(held.at) >= CONFIRM_EXPIRY
                    });
                    if stale_held {
                        self.held = None;
                    }
                }
                // The tick's own `now` again, for the same reason: how long
                // the shepherd has been gone is the one number a frozen
                // dashboard keeps counting.
                if let Some(at) = self.froze_at {
                    self.frozen_for = now.saturating_duration_since(at);
                }
                // Against the tick's own `now`, not `self.now`, which stops on
                // a dead link: a settings edit describes a local file that is
                // no staler for the shepherd being gone.
                if let Some(settings) = self.settings_mut() {
                    let expired = matches!(
                        settings.pending,
                        Some(Pending::Armed { at, .. } | Pending::DogArmed { at, .. })
                            if now.saturating_duration_since(at) >= CONFIRM_EXPIRY
                    );
                    if expired {
                        settings.pending = None;
                    }
                }
                // The config pane has no expiry of its own any more:
                // nothing on it is a question waiting for an answer. Its
                // edits sit until the operator writes them or undoes them,
                // and a set that timed out would throw work away silently.
                // Outside the link guard for the same reason, and one more:
                // `until` was set off `self.now`, which a dead link stops
                // advancing, so the tick's own `now` is what expires a
                // reveal at all once the link has gone.
                if let Some(pane) = self.secrets_pane_mut()
                    && pane
                        .reveal
                        .as_ref()
                        .is_some_and(|reveal| now >= reveal.until)
                {
                    pane.hide();
                }
                // Outside the link guard for the same reason as the reveal
                // above: an armed delete that can never be sent is no less
                // stale for the shepherd being gone. `now`, not `self.now`.
                if let Some(pane) = self.secrets_pane_mut()
                    && pane
                        .armed
                        .as_ref()
                        .is_some_and(|a| now.saturating_duration_since(a.at) >= CONFIRM_EXPIRY)
                {
                    pane.armed = None;
                }
                // Neither full-screen feed has a timer of its own; each
                // rides every tick instead of the dashboard's own cadence,
                // which is fixed for the connection's lifetime (see
                // `RefreshFeed`'s own doc). The dashboard raises nothing
                // here, or every lookout would poll twice as often for
                // nothing. `Link::Lost` too: `Msg::Bleats` throws the tail
                // away while the link is down, so every read would be work
                // done and discarded once a second. `Msg::Snapshot` and
                // `select_at` guard on the same thing, and
                // `a_frozen_dashboard_does_not_re_read_anything` states the
                // rule.
                if matches!(self.body, Body::Bleats(_) | Body::Sheep(_))
                    && !matches!(self.link, Link::Lost { .. })
                {
                    Effect::RefreshFeed
                } else {
                    Effect::None
                }
            }
            Msg::Resize => Effect::None,
            Msg::Key(key) => self.on_key(key),
            Msg::Host { sample } => {
                // A strip ticking over under a banner saying the values are
                // frozen contradicts it on one frame.
                if matches!(self.link, Link::Lost { .. }) {
                    return Effect::None;
                }
                self.host_unsupported = sample.is_none();
                self.host = sample;
                Effect::None
            }
            // `Effect::None` for `Msg::Bleats`' reason: answering a local read
            // with another refresh would spin the UI task.
            Msg::LogSize { id, total_bytes } => {
                self.log_size = Some(LogSize { id, total_bytes });
                Effect::None
            }
            // Always `Effect::None`: answering a feed update with another
            // refresh would spin the UI task. The guard catches a read `run_ui`
            // armed before the freeze landed.
            Msg::Bleats { tail } => {
                if matches!(self.link, Link::Lost { .. }) {
                    return Effect::None;
                }
                self.feed = tail;
                Effect::None
            }
            Msg::Replied { sent, result } => match sent {
                Sent::Lambs { id } => self.on_lambs(id, result),
                Sent::Action { verb, target, name } => {
                    self.on_action_reply(verb, target, &name, result)
                }
                Sent::Dog {
                    name,
                    enable,
                    ticket,
                    ..
                } => self.on_dog_reply(name, enable, ticket, result),
                Sent::SheepConfig { name } => self.on_sheep_config(&name, result),
                Sent::DogSection { name } => self.on_dog_section(&name, result),
                Sent::SetDogSection { name, .. } => self.on_dog_section_set(&name, result),
                Sent::ApplyField {
                    name,
                    ticket,
                    key,
                    value,
                    ..
                } => {
                    let landed = result.is_ok();
                    let effect = self.on_field_applied(&name, &key, &value, result);
                    self.resolve_held_write(ticket, landed).unwrap_or(effect)
                }
                Sent::SetEnv {
                    name,
                    ticket,
                    key,
                    value,
                    ..
                } => {
                    let landed = result.is_ok();
                    let was_set = value.is_some();
                    let effect = self.on_env_set(&name, &key, was_set, result);
                    self.resolve_held_write(ticket, landed).unwrap_or(effect)
                }
            },
            Msg::Unsent { sent } => match sent {
                Sent::Action { verb, target, name } => {
                    self.action = None;
                    self.notice = Some(Notice {
                        // No cause: `Full` is reachable while the shepherd is
                        // merely slow, so naming one would invent it.
                        text: format!("{}: it was not sent", target_prefix(verb, &target, &name)),
                        grave: true,
                    });
                    Effect::None
                }
                // A dropped lamb fetch already reads as "not read yet".
                Sent::Lambs { .. } => Effect::None,
                // A config read nobody took, reported rather than
                // swallowed: silence here looks like a key that is not
                // bound. Nothing was armed, so this is the whole report.
                Sent::SheepConfig { name } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its config was not asked for"),
                        grave: true,
                    });
                    Effect::None
                }
                // Both write arms name the field, and neither reaches for
                // the pane: a close sends the whole set and leaves, so
                // every one of these lands with no pane on screen. The
                // notice is the whole report.
                Sent::ApplyField { name, key, .. } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: {key} was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
                Sent::SetEnv { name, key, .. } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: env {key} was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
                // The dog twins of the two arms above: a read nobody took
                // is reported, and so is a write nobody took.
                Sent::DogSection { name } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its config was not asked for"),
                        grave: true,
                    });
                    Effect::None
                }
                Sent::SetDogSection { name, .. } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its config was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
                // The arm above, against the settings screen's pending line.
                Sent::Dog {
                    name,
                    enable,
                    ticket,
                    ..
                } => {
                    if let Some(settings) = self.settings_mut() {
                        settings.resolve(ticket);
                    }
                    let verb = if enable { "enable" } else { "disable" };
                    self.notice = Some(Notice {
                        text: format!("{verb} {name}: it was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
            },
            // Delegates to `Msg::Unsent`'s own match rather than repeating
            // it: every arm there already reports the right notice for the
            // `Sent` it carries, and returns `Effect::None`.
            Msg::BatchSent { unsent } => match unsent {
                Some(sent) => self.update(Msg::Unsent { sent }),
                None => Effect::None,
            },
            // The screen opens on what this read found; a failed read leaves
            // the dashboard up. A landed write's re-read and `r` land here too,
            // with `body` already `Body::Settings`, so `opening` is false and
            // the cursor survives.
            Msg::Settings { result } => {
                let opening = self.settings().is_none();
                match result {
                    // A config pane opened while this read was in flight, so
                    // the operator asked for the pane AFTER asking for
                    // settings and this reply is the stale one. Adopting it
                    // would replace the pane with a settings screen the
                    // operator has moved on from, and leave `config_target`
                    // and `close_dialog` describing a screen that is no longer
                    // up. The `Body` enum stops the two coexisting; it does
                    // not stop this handler overwriting one with the other,
                    // which is the same race `on_sheep_config` had in the
                    // opposite direction.
                    Ok(_) if self.config_pane().is_some() => {}
                    Ok(snapshot) => {
                        // An action armed while the read was in flight: once
                        // the screen is up, `on_settings_key` no-ops `Confirm`
                        // and the prompt would be unreachable.
                        self.action = None;
                        // A filter box left open would keep eating every
                        // keystroke the settings keymap owns: `on_key` checks
                        // the mode first. The query itself is kept.
                        self.mode = InputMode::Normal;
                        // Reset to the top only while `opening`.
                        // `Settings::cursor` clamps on every read, so a
                        // preserved `Viewport` past a shorter dogs list
                        // still lands somewhere real.
                        let view = self.settings().map(|settings| settings.view.clone());
                        let mut settings = Settings::new(snapshot);
                        if !opening {
                            if let Some(view) = view {
                                settings.view = view;
                            }
                            let len = settings.rows().len();
                            settings.view.clamp(len);
                        }
                        self.body = Body::Settings(settings);
                    }
                    Err(message) => {
                        self.notice = Some(Notice {
                            text: message,
                            grave: true,
                        });
                    }
                }
                Effect::None
            }
            // `Ok` re-reads rather than folding the write into the row, which
            // covers `Unset` too. `Err` reopens the editor for the two
            // free-text fields, so a long path need not be retyped.
            //
            // Both arms act on the screen only when this reply is the one it
            // is waiting on. `Settings::resolve` answers that; a reply it
            // refuses still reports itself and still re-reads, but leaves
            // whatever is on screen alone. A dog section can also have
            // replaced the settings screen with a config pane while the
            // write was in flight, which `resolve` refuses for the same
            // reason: `InputMode::Text` with no editor behind it sends every
            // later keystroke to a text handler that owns nothing.
            Msg::SettingWritten {
                edit,
                ticket,
                result,
            } => {
                let mine = self
                    .settings_mut()
                    .is_some_and(|settings| settings.resolve(ticket));
                match result {
                    Ok(()) => self.reread_settings(),
                    Err(message) => {
                        // Split so no borrow of `self.body` is held across
                        // the `self.notice` assignment below.
                        if mine && let Some((field, buffer)) = typed_text_of(&edit) {
                            if let Some(settings) = self.settings_mut() {
                                settings.pending = Some(Pending::Typing { field, buffer });
                            }
                            // The overlay closes rather than the editor
                            // reopening beneath it. `on_key` checks text mode
                            // ahead of `keymap_open`, deliberately, so an `h`
                            // typed into a filter box stays a letter; the cost
                            // is that text mode restored from a MESSAGE would
                            // take the keyboard while the box is still drawn,
                            // and every key would reach a socket path the box
                            // hides. Reachable because `is_armed` does not
                            // cover `Pending::Sent`, so `h` with a write in
                            // flight opens the overlay instead of cancelling.
                            //
                            // Closing it is the lesser surprise: the operator
                            // asked for a key list, and what they get instead
                            // is their refused write, the grave notice saying
                            // why, and their own typed text back. `h` reopens
                            // the box.
                            self.keymap_open = false;
                            self.mode = InputMode::Text;
                        }
                        self.notice = Some(Notice {
                            text: message,
                            grave: true,
                        });
                        Effect::None
                    }
                }
            }
            // A dog's schema probe answered. `Ok` parks the schema and asks
            // the shepherd for the section; the pane is built once that
            // lands. `Err` gets no pane, and the refusal names the file to
            // edit instead. The settings screen stays open until then.
            Msg::DogPane {
                name,
                adopted_path,
                result,
            } => match result {
                Ok(schema) => {
                    self.dog_target = Some(DogProbe {
                        name: name.clone(),
                        adopted_path,
                        schema,
                    });
                    Effect::Send(Sent::DogSection { name })
                }
                Err(message) => {
                    self.notice = Some(Notice {
                        text: message,
                        grave: true,
                    });
                    Effect::None
                }
            },
            // `Ok` raises the daemon half: `Cycle` arms, `Confirm` writes the
            // file, this arm asks the shepherd. `Err` never reaches it, since
            // there is nothing for the daemon half to agree with.
            //
            // `Ok` asks the shepherd whether or not the screen is still
            // waiting on this ticket: the file already says the dog is on or
            // off, so a daemon half dropped here would leave the two
            // disagreeing. `Err` clears only the prompt this ticket raised.
            Msg::DogWritten {
                edit,
                ticket,
                result,
            } => match result {
                Ok(source) => Effect::Send(Sent::Dog {
                    name: edit.name,
                    enable: edit.enable,
                    source,
                    ticket,
                }),
                Err(message) => {
                    if let Some(settings) = self.settings_mut() {
                        settings.resolve(ticket);
                    }
                    self.notice = Some(Notice {
                        text: message,
                        grave: true,
                    });
                    Effect::None
                }
            },
            // Dropped when the pane has since closed, the way a settings
            // read that outraced a config pane is: adopting it would reopen
            // a screen the operator has already left.
            Msg::Secrets {
                environment,
                result,
            } => {
                if let Body::Secrets(pane) = &mut self.body {
                    match result {
                        Ok(model) => {
                            // A fresh read describes the store as it is now;
                            // an arm from before it landed named a row this
                            // model may no longer even have.
                            pane.armed = None;
                            // Only on the very first load, where the pane's
                            // model is still the empty default and so has no
                            // tab yet: the daemon's own default environment
                            // wins the tab it lands on.
                            let first_load = pane.model.environments.is_empty();
                            if first_load {
                                pane.tab = model
                                    .environments
                                    .iter()
                                    .position(|candidate| candidate == &environment)
                                    .unwrap_or(0);
                            }
                            // The environments list is a union recomputed on
                            // every load and can shrink, so clamp a tab past
                            // the new end onto the last surviving one
                            // instead of dangling.
                            pane.tab = pane.tab.min(model.environments.len().saturating_sub(1));
                            // A selection on the trailing `+ new key`
                            // sentinel stays on it under the fresh model's
                            // own row count, rather than the ordinary clamp
                            // below, which would otherwise land it on the
                            // new model's last real row. Excludes the first
                            // load, whose own empty model reads `selected`
                            // (`0`) as that same sentinel by coincidence,
                            // having no rows yet either.
                            pane.selected = if !first_load && pane.selected_is_new_key_row() {
                                model.rows.len()
                            } else {
                                pane.selected.min(model.rows.len().saturating_sub(1))
                            };
                            pane.model = model;
                            // The row count clamp above says nothing about
                            // collapse state, and `collapsed` survives a
                            // reload: the surviving index can still name a
                            // row a still-folded namespace hides. Same
                            // fallback the `Collapse` arm uses.
                            if pane
                                .model
                                .rows
                                .get(pane.selected)
                                .is_some_and(|row| pane.is_collapsed(&row.source))
                            {
                                pane.move_by(0);
                            }
                        }
                        Err(message) => {
                            self.notice = Some(Notice {
                                text: message,
                                grave: true,
                            });
                        }
                    }
                }
                Effect::None
            }
            Msg::Revealed {
                key,
                environment,
                value,
            } => {
                self.on_revealed(&key, &environment, value);
                Effect::None
            }
            // `Ok` re-reads (like `Msg::SettingWritten`) so the table shows
            // the file's new contents rather than what was typed, and clears
            // any revealed value, which belonged to the prior store. `Err`
            // raises no reload, so the table keeps its last known-good read.
            Msg::SecretWritten { result } => match result {
                Ok(true) => {
                    self.hide_revealed();
                    Effect::LoadSecrets
                }
                // `secrets::unset` found no slot. Nothing changed, so
                // nothing is re-read, and the operator hears about it: a
                // destructive action reporting success over a no-op is the
                // one answer this pane must never give.
                Ok(false) => {
                    self.notice = Some(Notice {
                        text: NOTHING_REMOVED.to_string(),
                        grave: true,
                    });
                    Effect::None
                }
                Err(message) => {
                    self.notice = Some(Notice {
                        text: message,
                        grave: true,
                    });
                    Effect::None
                }
            },
        }
    }

    fn on_event(&mut self, event: BusEvent) -> Effect {
        match event {
            BusEvent::Process { event, info, .. } => {
                if matches!(event, ProcessEventKind::Delete) {
                    let previous = self.selected_index();
                    self.flock.remove(&info.id);
                    self.forget_missing_target();
                    return if self.reseat(previous) {
                        Effect::RefreshSelected
                    } else {
                        Effect::None
                    };
                }
                // An upsert can orphan the selection from `visible_rows()`
                // without touching `self.flock`: a rename can move the selected
                // row out of the filter. `reseat` is a no-op read while the
                // selection is still seated.
                let previous = self.selected_index();
                let anchor = self.now;
                self.flock.insert(info.id, Row { info, anchor });
                if self.reseat(previous) {
                    return Effect::RefreshSelected;
                }
                Effect::None
            }
            // The shepherd's own outbound queue overflowed. Worded differently
            // from `Msg::BusLagged`: an operator cannot tell which end of the
            // connection to investigate if the two read the same.
            BusEvent::Dropped { count } => {
                self.notice = Some(Notice {
                    text: format!("the shepherd dropped {count} events; re-reading the flock"),
                    grave: false,
                });
                Effect::PollNow
            }
            // A notice, not an exit: a dashboard that vanished would take the
            // last known state with it.
            BusEvent::DaemonShutdown => {
                self.notice = Some(Notice {
                    text: "the shepherd is shutting down".to_string(),
                    grave: true,
                });
                Effect::None
            }
            // `BusEvent` is `#[non_exhaustive]`: a newer shepherd's variant
            // must not take the dashboard down.
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use shep_core::protocol::ProcessEventKind;

    #[test]
    fn a_snapshot_replaces_the_flock_wholesale() {
        let (mut app, t0) = started();
        app.update(Msg::Event(BusEvent::Process {
            event: ProcessEventKind::Start,
            info: sheep(9, "ghost", ProcStatus::Starting),
            manually: true,
            at_ms: 0,
        }));
        assert_eq!(app.rows().len(), 4, "the bus event upserted");

        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(app.rows().len(), 1);
        assert!(app.rows().iter().all(|row| row.info.id == 1));
    }

    #[test]
    fn a_snapshot_that_shrinks_the_flock_pulls_the_selection_back() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected_index(), Some(3), "past the flock header");

        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(
            app.selected_index(),
            Some(1),
            "the selection came back with the flock, past the header"
        );

        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert_eq!(app.selected_index(), None, "an empty flock selects nothing");
    }

    #[test]
    fn the_selection_follows_the_sheep_and_not_the_row_number() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectDown));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(3)),
            "the third row, worker"
        );

        // Sheep 1 goes away. `worker` is now row 1 rather than row 2, where
        // an index cursor would be pointing at `api`.
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "api", ProcStatus::Errored),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)), "still worker");
        assert_eq!(
            app.selected_index(),
            Some(2),
            "which is now row 2, past the header"
        );
    }

    #[test]
    fn a_deleted_selection_falls_to_the_row_that_took_its_place() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "web, at index 1 by name"
        );

        // web dies; api and worker remain. Index 1 is now worker.
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "api", ProcStatus::Online),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(3)),
            "the row that took index 1"
        );

        // The last row dying clamps rather than leaving the cursor past the end.
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)));
        app.update(Msg::Snapshot {
            rows: vec![sheep(2, "api", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(app.selected(), Some(RowKey::Sheep(2)));

        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert_eq!(app.selected(), None);
        assert_eq!(app.selected_index(), None);
    }

    #[test]
    fn a_selection_that_moves_refreshes_the_feed_and_one_that_cannot_does_not() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectFirst)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectUp)),
            Effect::None,
            "already at the top: nothing moved, so nothing is re-read"
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectLast)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::None,
            "already at the bottom"
        );
    }

    #[test]
    fn moving_the_selection_asks_for_lambs() {
        let (mut app, _t0) = started();
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::RefreshSelected
        );
    }

    /// `ListFlock` declines the lamb walk: a full machine enumeration every two
    /// seconds, times every open lookout.
    #[test]
    fn a_snapshot_refreshes_the_feed_and_does_not_ask_for_lambs() {
        let (mut app, t0) = started();
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0,
            }),
            Effect::RefreshFeed
        );
    }

    #[test]
    fn nothing_is_requested_while_the_link_is_lost() {
        let (mut app, _t0) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-16 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
    }

    #[test]
    fn a_snapshot_refreshes_the_feed_unless_the_link_is_frozen() {
        let (mut app, t0) = started();
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0
            }),
            Effect::RefreshFeed
        );
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0
            }),
            Effect::None,
            "a frozen dashboard does not re-read anything"
        );
    }

    /// The cursor still moves: re-rendering the detail pane from the frozen
    /// listing is data already on the frame. Touching the disk is not.
    #[test]
    fn a_frozen_dashboard_moves_the_cursor_without_touching_a_file() {
        let (mut app, _) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::None,
            "no file is read once the link is lost"
        );
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "but the cursor moved anyway"
        );
        assert_eq!(app.update(Msg::Key(KeyPress::SelectLast)), Effect::None);
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)));
    }

    #[test]
    fn a_drop_and_a_lag_both_ask_for_an_immediate_poll() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Event(BusEvent::Dropped { count: 12 })),
            Effect::PollNow
        );
        assert_eq!(app.update(Msg::BusLagged { count: 3 }), Effect::PollNow);
        assert_eq!(
            app.update(Msg::Event(BusEvent::Process {
                event: ProcessEventKind::Online,
                info: sheep(1, "web", ProcStatus::Online),
                manually: false,
                at_ms: 0,
            })),
            Effect::None,
            "an ordinary event needs no repair"
        );
    }

    #[test]
    fn a_shepherd_side_drop_and_a_local_lag_read_differently() {
        let (mut app, _) = started();
        app.update(Msg::Event(BusEvent::Dropped { count: 12 }));
        let shepherd_side = app.notice().expect("a drop leaves a notice").to_string();
        app.update(Msg::BusLagged { count: 3 });
        let local = app.notice().expect("a lag leaves a notice").to_string();

        assert!(shepherd_side.contains("the shepherd dropped"));
        assert!(local.contains("lookout fell behind"));
        assert_ne!(shepherd_side, local);
    }

    #[test]
    fn a_running_sheeps_uptime_advances_with_the_heartbeat() {
        let (mut app, t0) = started();
        assert_eq!(app.uptime_ms(app.rows()[0].info.id), Some(60_000));
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(5),
        });
        assert_eq!(app.uptime_ms(1), Some(65_000));
    }

    #[test]
    fn a_frozen_dashboard_stops_the_uptime_clock() {
        let (mut app, t0) = started();
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(5),
        });
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let at_freeze = app.uptime_ms(1);
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(400),
        });
        assert_eq!(
            app.uptime_ms(1),
            at_freeze,
            "the clock stopped with the link"
        );
        assert_eq!(at_freeze, Some(65_000));
    }

    #[test]
    fn a_stopped_sheeps_uptime_does_not_advance() {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Stopped)],
            at: t0,
        });
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(30),
        });
        assert_eq!(app.uptime_ms(1), Some(60_000));
    }

    #[test]
    fn every_action_key_refuses_while_the_gate_is_closed() {
        for verb in [ActionVerb::Stop, ActionVerb::Restart, ActionVerb::Reload] {
            let (mut app, _t0) = started();
            app.update(Msg::Key(KeyPress::Action(verb)));
            assert!(
                app.action().is_none(),
                "{verb:?} armed behind a closed gate"
            );
            assert_eq!(
                app.notice().map(ToString::to_string).as_deref(),
                Some("read-only: from --read-only or lookout.allow_control"),
                "{verb:?}"
            );
        }
    }

    #[test]
    fn the_link_state_walks_live_to_retrying_to_lost_and_back() {
        let (mut app, t0) = started();
        assert_eq!(app.link(), &Link::Live);

        app.update(Msg::Retrying { attempt: 1 });
        assert_eq!(app.link(), &Link::Retrying { attempt: 1 });

        app.update(Msg::Relinked);
        assert_eq!(app.link(), &Link::Live);

        app.update(Msg::Retrying { attempt: 5 });
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.link(),
            &Link::Lost {
                at_local: "2026-08-14 14:32:07".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            }
        );

        // A late snapshot must not unfreeze it.
        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert!(matches!(app.link(), Link::Lost { .. }));
    }

    #[test]
    fn refresh_polls_while_live_and_says_why_it_cannot_once_frozen() {
        let (mut app, _) = started();
        assert_eq!(app.update(Msg::Key(KeyPress::Refresh)), Effect::PollNow);

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::None,
            "there is no link task left to ask"
        );
        let notice = app.notice().expect("a refusal is a notice").to_string();
        assert!(notice.contains("the shepherd is gone"));
        assert!(notice.contains("nothing left to ask"));
    }

    #[test]
    fn the_next_keypress_clears_the_notice() {
        let (mut app, _) = started();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.notice().is_some());
        app.update(Msg::Key(KeyPress::SelectDown));
        assert!(app.notice().is_none());
    }

    /// The strip reads this machine, which lookout can still see after the
    /// shepherd dies, so it is the one pane that could keep ticking under a
    /// banner saying the values are frozen.
    #[test]
    fn a_frozen_dashboard_ignores_a_host_sample() {
        let (mut app, _) = started();
        app.update(Msg::Host {
            sample: Some(crate::lookout::source::HostSample {
                load: (2.31, 4.10, 3.88),
                cores: Some(10),
                memory_total_bytes: 32 << 30,
                memory_used_bytes: 12 << 30,
                uptime_seconds: 600,
            }),
        });
        assert!(app.host().is_some(), "a live dashboard takes the sample");

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let frozen = app.host();
        assert_eq!(app.update(Msg::Host { sample: None }), Effect::None);
        assert_eq!(app.host(), frozen, "the last values stay, unchanged");
        assert!(
            !app.host_unsupported(),
            "and a refused sample changes no flag"
        );
    }

    #[test]
    fn applying_a_tail_does_not_ask_for_another_one() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Bleats {
                tail: crate::lookout::tail::Tail::default()
            }),
            Effect::None
        );
    }

    /// `run_ui`'s coalesced read is armed before the freeze, so a read can
    /// still be in flight when `Msg::Frozen` lands.
    #[test]
    fn a_frozen_dashboard_ignores_a_bleats_tail_in_flight_at_the_freeze() {
        let (mut app, _) = started();
        let live_tail = crate::lookout::tail::Tail {
            lines: vec![crate::lookout::tail::TailLine {
                stream: crate::lookout::tail::Stream::Out,
                text: "read before the freeze".to_string(),
            }],
            ..Default::default()
        };
        app.update(Msg::Bleats {
            tail: live_tail.clone(),
        });
        assert_eq!(app.feed(), &live_tail, "a live dashboard takes the tail");

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        let in_flight_tail = crate::lookout::tail::Tail {
            lines: vec![crate::lookout::tail::TailLine {
                stream: crate::lookout::tail::Stream::Out,
                text: "read after the freeze".to_string(),
            }],
            ..Default::default()
        };
        assert_eq!(
            app.update(Msg::Bleats {
                tail: in_flight_tail
            }),
            Effect::None
        );
        assert_eq!(
            app.feed(),
            &live_tail,
            "the tail read after the freeze must not reach the rendered frame"
        );
    }
}
