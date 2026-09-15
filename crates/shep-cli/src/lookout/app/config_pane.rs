//! The config pane: asking for a sheep's config, the keyboard over it, and
//! closing it.

use super::*;

/// Which screen asked for a sheep's config.
///
/// Both [`Sent::SheepConfig`]'s callers send the exact same request, and the
/// reply cannot tell them apart on its own: `e` pressed inside the sheep
/// pane leaves that pane on screen while the reply is in flight, so
/// [`App::on_sheep_config`] cannot route by the current [`Body`] the way it
/// could if only one screen ever asked. This is read instead, and it is set
/// in the same step as [`App::config_target`], by whichever door sent the
/// request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFor {
    /// The editing pane, opened by `e`.
    Editor,
    /// The sheep pane's read-only left column.
    SheepPane,
}

impl App {
    /// One `Request::SheepConfig` reply. Opens the pane, or refreshes an
    /// open one in place.
    ///
    /// The cursor and offset are carried across a refresh rather than reset
    /// to the first field, the same rule `Msg::Settings` follows and for the
    /// same reason: `r` from inside the pane, and every later re-read, must
    /// not throw an operator who was reading `cron_restart` back to `name`.
    /// [`ConfigPane::adopt_view`] clamps what it adopts, so a field list
    /// that came back shorter cannot leave the cursor past its end.
    ///
    /// A failed read leaves whatever is on screen exactly as it was and
    /// raises a grave notice, so a refusal is reported rather than
    /// swallowed, and a refresh that fails does not blank a pane that was
    /// showing something real.
    pub(super) fn on_sheep_config(
        &mut self,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        // A reply nobody is waiting for any more is dropped in silence:
        // nothing went wrong, and the operator asked for nothing that is
        // still outstanding.
        if self.config_target.as_deref() != Some(name) {
            return Effect::None;
        }
        match result {
            Ok(Response::SheepConfig(view)) => match self.config_for {
                Some(ConfigFor::SheepPane) => {
                    if let Some(pane) = self.sheep_pane_mut() {
                        pane.adopt_config(*view);
                    }
                }
                // `None` only if something outside this file set
                // `config_target` without `config_for`, which nothing does:
                // [`Self::ask_for_sheep_config`] is the one place both are
                // set, always together. Falling back to the editor is the
                // pre-[`ConfigFor`] behaviour, not a guess this reply
                // belongs to a screen that never asked for it.
                None | Some(ConfigFor::Editor) => self.open_or_refresh_config_pane(*view),
            },
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{name}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
            }
            // Prefixed, like every other reply handler in this file: the
            // shepherd's refusals mostly name their own subject, but
            // `EngineStopped` renders as `the supervisor engine has
            // stopped`, naming neither the sheep nor the screen it came
            // from.
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {}", err.message),
                    grave: true,
                });
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {other}"),
                    grave: true,
                });
            }
        }
        Effect::None
    }

    /// Opens the config pane on `view`, or refreshes one already open in
    /// place, carrying across everything a rebuild would otherwise drop.
    /// Split out of [`Self::on_sheep_config`] so that method can route a
    /// [`ConfigFor::SheepPane`] reply to [`Self::sheep_pane_mut`] instead
    /// without repeating its guard or its error arms.
    fn open_or_refresh_config_pane(&mut self, view: SheepConfigView) {
        let carried = self.config_pane().map(|pane| pane.view().clone());
        // An env row's own cursor is carried by key, not index: a
        // set re-reads the whole config, and a removal shortens the
        // list, so an index that survived would name a different
        // key. See `ConfigPane::cursor_env_key`.
        let carried_env_key = self.config_pane().and_then(ConfigPane::cursor_env_key);
        // The list sub-screen rides across for the same reason,
        // and by index rather than by name: an element has no
        // name. See `ListPane::adopt_view`.
        let carried_list = self
            .config_pane()
            .and_then(ConfigPane::list)
            .map(|list| (list.key().to_owned(), list.view().clone()));
        // The pending set survives the rebuild: the values are
        // the shepherd's and the edits are the operator's. An open
        // editor is dropped. See `ConfigPane::adopt_edits`.
        let carried_edits = self
            .config_pane()
            .map(|pane| pane.edits().clone())
            .unwrap_or_default();
        let mut pane = ConfigPane::sheep(view);
        pane.adopt_edits(carried_edits);
        if let Some(carried) = carried {
            pane.adopt_view(carried);
        }
        if let Some(env_key) = carried_env_key {
            pane.adopt_env_cursor(env_key.as_deref());
        }
        if let Some((key, carried)) = carried_list {
            pane.adopt_list_view(&key, carried);
        }
        self.body = Body::ConfigPane(pane);
        // The rebuilt pane carries no editor, so the keyboard must not
        // still think one is open.
        self.release_text_mode_if_unowned();
        // An overlay open when this reply landed would otherwise survive
        // the body it was drawn over, and every key from here goes to
        // `on_keymap_key` instead of the pane the operator asked for.
        self.keymap_open = false;
    }

    /// `e`'s own handler: open a dog's pane directly, else ask the shepherd.
    ///
    /// [`Self::selected_row`] rather than [`Self::selected_name`]: a dog
    /// runs one process and is never a group row, but a group row must
    /// still work with `e`.
    pub(super) fn ask_for_config(&mut self) -> Effect {
        if let Some(row) = self.selected_row()
            && let Some(source) = row.info.dog.as_ref()
        {
            let adopted_path = match source {
                DogSource::Adopted { path } => Some(PathBuf::from(path)),
                _ => None,
            };
            return Effect::LoadDogPane {
                name: row.info.name.clone(),
                adopted_path,
            };
        }
        match self.selected_name() {
            Some(name) => self.ask_for_sheep_config(name, ConfigFor::Editor),
            None => Effect::None,
        }
    }

    /// Sends `Request::SheepConfig` for `name`, recording which screen it is
    /// for so [`Self::on_sheep_config`] can route the reply once it lands.
    ///
    /// The one place [`Self::config_target`] and [`Self::config_for`] are
    /// set for a sheep-config read, so the two can never disagree about
    /// which request is outstanding.
    pub(super) fn ask_for_sheep_config(&mut self, name: String, for_screen: ConfigFor) -> Effect {
        self.config_target = Some(name.clone());
        self.config_for = Some(for_screen);
        Effect::Send(Sent::SheepConfig { name })
    }

    /// The config pane's own keymap, in force for as long as
    /// [`Self::config_pane`] is `Some`.
    ///
    /// Movement walks fields, `r` re-reads, `space` cycles the row under
    /// the cursor, `Enter` or `e` edits it, `u` undoes the newest edit,
    /// and `Escape` asks the close dialog's question if there is one to
    /// ask, else writes everything filed and leaves. `h` raises the keymap
    /// overlay; see `KeyPress::Help`'s arm below. Everything else is named
    /// rather than wildcarded, so a stray variant cannot fall silently into
    /// an arm that ignores it.
    ///
    /// Nothing is armed here and no key is eaten. A keystroke that edits
    /// files into the pane's own set and sends nothing, so a stray one
    /// costs an `u` rather than a write to a running sheep.
    pub(super) fn on_pane_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
        if self.close_dialog.is_some() {
            return self.on_close_dialog_key(key);
        }
        if self.config_pane().is_some_and(|pane| pane.list().is_some()) {
            return self.on_list_key(key);
        }
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            // Unreachable: the guard above already returned. Kept rather
            // than folded into a silent group or replaced with a wildcard,
            // because every other arm here is named on purpose (a stray
            // `KeyPress` variant should not fall through unnoticed), and a
            // wildcard would defeat that for every variant, not just this
            // one. If the guard above is ever removed, this is what Quit
            // still does.
            KeyPress::Quit => return Effect::Quit,
            // Backs out one level at a time: the close dialog's own
            // question first, if there is one, else the pane. `Escape`
            // closes rather than cascading to a filter clear or a quit,
            // exactly as it does on the settings screen.
            //
            // The dialog is asked before anything is taken: `esc` used to
            // write first and ask second, which missed the very edit that
            // made this pane's `Escape` worth asking about. Now nothing
            // leaves the pane until the question is answered, one way or
            // another.
            //
            // One press, not two: the field's help draws unconditionally
            // now, so nothing waits behind a second `esc` for it.
            KeyPress::Escape => {
                if let Some(dialog) = self.close_offer() {
                    self.close_dialog = Some(dialog);
                    return Effect::None;
                }
                let writes = self.take_pane_writes();
                self.close_pane();
                if !writes.is_empty() {
                    return Effect::SendAll(writes);
                }
            }
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if let Some(pane) = self.config_pane_mut() {
                    match key {
                        KeyPress::SelectUp => pane.move_by(-1),
                        KeyPress::SelectDown => pane.move_by(1),
                        KeyPress::SelectFirst => pane.move_to_first(),
                        KeyPress::SelectLast => pane.move_to_last(),
                        _ => unreachable!(),
                    }
                }
            }
            // Re-reads the same sheep, so an override applied from another
            // window shows up. The cursor survives it: see
            // `Self::on_sheep_config`.
            KeyPress::Refresh => return self.reread_pane(),
            KeyPress::Cycle => return self.cycle_field(),
            // `e` does exactly what `Enter` does here: an operator who
            // opened the pane with `e` should not have to learn a second
            // key to use it.
            KeyPress::Confirm | KeyPress::Edit => return self.confirm_field(),
            KeyPress::Help => {
                self.open_keymap();
            }
            // `d` restores the field under the cursor to its default. Does
            // nothing on an env row or `+ add a key`: unsetting a key
            // entirely is a different act from restoring a default, and
            // the spec does not ask for it.
            KeyPress::Remove => return self.restore_default(),
            KeyPress::Action(_)
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse
            // Bound only in the close dialog; with none up, `c` is a stray
            // key the same way an action key is.
            | KeyPress::Continue => {}
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats => {}
            // Drops the newest edit and nothing else. No control gate: a
            // key that unfiles something cannot write, and refusing it
            // behind a closed gate would leave an edit the gate already
            // refused to file.
            KeyPress::Undo => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.undo_edit();
                }
            }
            // `tab` walks the groups; a digit jumps straight to one. Both
            // reset the cursor to the group's first field, so `j`/`k` never
            // start on a row the new tab does not draw.
            KeyPress::NextGroup => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.next_group();
                    pane.move_to_first();
                }
            }
            KeyPress::Group(digit) => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.set_group(digit);
                    pane.move_to_first();
                }
            }
        }
        Effect::None
    }

    /// One write ticket, and the counter moved past it.
    ///
    /// [`Self::take_pane_writes`] mints a batch's worth inline rather than
    /// calling this per entry, because it holds a borrow of `self.body`
    /// across the loop.
    pub(super) fn take_write_ticket(&mut self) -> u64 {
        let ticket = self.next_write_ticket;
        self.next_write_ticket += 1;
        ticket
    }

    /// `r` from inside a pane: ask for the same target again, by whichever
    /// of the two doors it came in.
    ///
    /// A dog's schema is not re-probed. It came from the dog's binary at
    /// open and is parked on [`Self::dog_target`]; re-probing would respawn
    /// somebody else's binary on a keystroke whose job is to re-read a file.
    pub(super) fn reread_pane(&mut self) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        let name = pane.target().name().to_owned();
        match pane.target() {
            PaneTarget::Sheep { .. } => Effect::Send(Sent::SheepConfig { name }),
            PaneTarget::Dog { .. } => Effect::Send(Sent::DogSection { name }),
        }
    }

    /// Drops the open pane and everything a reply for it would re-open.
    ///
    /// Both targets are cleared, always, and not only the one the pane
    /// happens to hold: a read for the other kind can be in flight when this
    /// runs (`e` on a dog, then the settings screen closes and `e` opens a
    /// sheep), and a stale one left set is exactly the re-open behind the
    /// operator's back `config_target` exists to prevent.
    ///
    /// This always lands on [`Body::FlockTable`], never on whatever screen
    /// preceded the pane. That used to be reachable the other way: a
    /// settings screen and a config pane were once two independent
    /// `Option`s, so closing the pane only cleared its own field and a
    /// still-`Some` settings screen underneath resurfaced — the dashboard is
    /// what `Escape` is supposed to reach, not a screen the operator asked
    /// for two actions ago. `Body` makes that unrepresentable: there is only
    /// ever one screen to close to, and it is this one.
    pub(super) fn close_pane(&mut self) {
        self.body = Body::FlockTable;
        self.close_dialog = None;
        self.config_target = None;
        self.config_for = None;
        self.dog_target = None;
        self.release_text_mode_if_unowned();
    }

    /// The refusal a locked row gets, in that row's own words.
    ///
    /// Two sentences for two facts, never one for both: shep refusing a
    /// config write is not the same as this pane having no widget for a
    /// shape a Flockfile writes perfectly well.
    ///
    /// A refused Structural field names the verb that moves it instead.
    /// The wildcard keeps a generic sentence for a Structural field this
    /// binary has no remedy for, rather than guessing a verb.
    pub(super) fn lock_refusal(key: &str, lock: Lock) -> String {
        match lock {
            Lock::Refused => match key {
                "instances" => {
                    format!("{key} is not a config write; `shep stock` moves an instance count")
                }
                "name" => {
                    format!("{key} is not a config write; a name change is a different sheep")
                }
                _ => {
                    format!("{key} is not something a config write changes, from here or anywhere")
                }
            },
            Lock::NoWidget => {
                format!("{key} has no editor in this pane; a Flockfile still sets it")
            }
        }
    }

    /// The config pane's own text keymap, in force for as long as one of
    /// its two editors owns [`InputMode::Text`].
    ///
    /// Does not trim the buffer, for the reason
    /// [`Self::on_settings_text_key`]'s own doc gives: this repository does
    /// not widen an accepted input grammar without a basis in the spec.
    ///
    /// Both editors file on `TextApply`, and neither sends: the pane's own
    /// `Escape` writes the whole set.
    pub(super) fn on_pane_text_key(&mut self, key: KeyPress) -> Effect {
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        if self.config_pane().is_some_and(|pane| pane.list().is_some()) {
            return self.on_list_text_key(key);
        }
        if self
            .config_pane()
            .is_some_and(|pane| pane.env_typing().is_some())
        {
            return self.on_env_text_key(key);
        }
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        match key {
            KeyPress::TextChar(typed) => pane.type_char(typed),
            KeyPress::TextBackspace => pane.type_backspace(),
            KeyPress::TextApply => {
                pane.apply_typing();
                // `apply_typing` keeps the editor open on an integer buffer
                // that does not parse, so the mode follows what the pane
                // actually did rather than what the key asked for.
                if pane.typing().is_none() {
                    self.mode = InputMode::Normal;
                }
            }
            KeyPress::TextAbandon => {
                pane.abandon_typing();
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// The [`WriteAuthority`] every settings write path has to hold, or
    /// [`None`] with the refusal already raised.
    ///
    /// The one place [`Control`] is read on this screen: `space` on a scalar or
    /// a dog, and `Enter` opening or applying an edit, all come through here.
    pub(super) fn authorize_write(&mut self) -> Option<WriteAuthority> {
        let authority = WriteAuthority::granted(self);
        if authority.is_none() {
            self.notice = Some(Notice {
                text: READ_ONLY_REFUSAL.to_string(),
                grave: true,
            });
        }
        authority
    }

    /// The open config pane, or `None` while nothing is being edited.
    #[must_use]
    pub fn config_pane(&self) -> Option<&ConfigPane> {
        match &self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::config_pane`]'s mutable twin, for the pane keymap and the
    /// handlers that settle a write or adopt a re-read in place.
    pub(super) fn config_pane_mut(&mut self) -> Option<&mut ConfigPane> {
        match &mut self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::edits::EditKey;
    use shep_core::protocol::RpcError;
    use shep_core::protocol::RpcErrorCode;

    /// `e` raises the read, not the open: the pane shows the shepherd's own
    /// answer or it shows nothing, the same rule `s` and `Msg::Settings`
    /// already follow for the settings screen.
    #[test]
    fn e_asks_for_the_selected_sheeps_config_and_the_reply_opens_the_pane() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let effect = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "web".to_string()
            })
        );
        assert!(app.config_pane().is_none(), "nothing opens on the keypress");

        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        let pane = app.config_pane().expect("the reply opens the pane");
        assert_eq!(pane.target().name(), "web");
        assert_eq!(pane.fields().len(), 42);
    }

    /// `h` before the reply arrives raises the overlay over the dashboard;
    /// the reply then replaces `self.body` with the pane, which must close
    /// the overlay too, or every key past this point goes to
    /// `on_keymap_key` instead of the pane the operator asked for.
    #[test]
    fn a_config_reply_that_lands_with_the_overlay_up_closes_it() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open(), "the overlay did not open");

        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(
            app.config_pane().is_some(),
            "the reply must still open the pane"
        );
        assert!(
            !app.keymap_open(),
            "the overlay survived a body change under it"
        );
    }

    /// `s` then `e` fire two reads; if the settings one lands first it opens
    /// the settings screen, and the config-pane reply that follows replaces
    /// it (`Body` cannot hold both at once). `Escape` from the config pane
    /// must land on the dashboard, not resurrect the settings screen it
    /// walked past on the way in — see the doc note on [`Body`] and on
    /// [`App::close_pane`].
    #[test]
    fn escape_from_a_config_pane_that_outraced_a_settings_read_lands_on_the_dashboard() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(app.settings().is_some(), "the settings reply lands first");
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(
            app.config_pane().is_some(),
            "the config pane reply replaces the settings screen"
        );
        assert!(app.settings().is_none());
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            matches!(app.body(), Body::FlockTable),
            "esc from the pane goes to the dashboard, not back to settings"
        );
        assert!(app.settings().is_none());
    }

    /// The sibling above pins the order where the settings read wins. This
    /// is the other one, and it is the order that used to corrupt state:
    /// the pane's own reply lands first, and the settings read arrives with
    /// the operator two actions past caring about it. `Msg::Settings` wrote
    /// `body` unconditionally, so the reply replaced the pane, reset the
    /// cursor as if opening, and forced `InputMode::Normal` while
    /// `config_target` and `close_dialog` went on describing a pane that was
    /// no longer on screen.
    #[test]
    fn a_settings_read_landing_after_a_config_pane_leaves_the_pane_up() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(app.config_pane().is_some(), "the pane reply lands first");

        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(
            app.config_pane().is_some(),
            "the stale settings reply leaves the pane alone"
        );
        assert!(app.settings().is_none());
    }

    /// `typed_text_of` answers for the two free-text settings fields, and
    /// its `Some` used to arm `InputMode::Text` whether or not the editor
    /// it types into was still there. Opening a dog section replaces the
    /// settings screen, so a refusal landing afterwards armed a text mode
    /// over a pane, and `on_key` then sent every later keystroke to a text
    /// handler owning nothing.
    #[test]
    fn a_refused_settings_write_landing_over_a_dog_pane_does_not_arm_text_mode() {
        let mut app = fixtures::app_in_dog_pane();
        assert!(app.config_pane().is_some(), "the pane is open");

        let _ = app.update(Msg::SettingWritten {
            edit: SettingEdit::Set {
                field: SettingField::MaxCronSleep,
                value: "500ms".to_string(),
            },
            // Any ticket: there is no settings screen to hold one.
            ticket: 0,
            result: Err("max_cron_sleep is 500ms, below the 1s floor".to_string()),
        });

        assert!(app.config_pane().is_some(), "the pane survives the reply");
        assert_ne!(
            app.mode(),
            InputMode::Text,
            "there is no settings editor for the keystrokes to reach"
        );
    }

    #[test]
    fn e_with_nothing_selected_asks_for_nothing() {
        let mut app = fixtures::app_with(Vec::new(), fixtures::plain());
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(app.config_pane().is_none());
    }

    /// A group has no single sheep, so `selected_row` answers `None` for
    /// one. Every instance behind it shares one stored spec, and the group
    /// row is what a multi-instance app shows by default.
    #[test]
    fn e_on_a_group_row_asks_by_the_apps_name() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .instance(Some(0))
                    .build(),
                ProcessInfo::builder(2, "web", ProcStatus::Online)
                    .instance(Some(1))
                    .build(),
            ],
            fixtures::plain(),
        );
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert!(
            app.selected_row().is_none(),
            "the selection is the group row, not one instance"
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::Edit)),
            Effect::Send(Sent::SheepConfig {
                name: "web".to_string()
            })
        );
    }

    #[test]
    fn escape_closes_the_pane_and_does_not_quit() {
        let mut app = fixtures::app_in_sheep_pane();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert!(app.config_pane().is_none());
    }

    /// The wire carries no env value for any key, Flockfile or store, so
    /// every one renders the same way. SheepConfigView::new clears the map.
    #[test]
    fn every_env_value_renders_as_set_and_never_as_itself() {
        let app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "production")]);
        let rows =
            fixtures::config_pane_env_rows_for_tests(app.config_pane().expect("the pane is open"));
        assert!(rows.iter().any(|row| row.contains("NODE_ENV")), "{rows:?}");
        assert!(
            rows.iter().all(|row| !row.contains("production")),
            "an env value reached the pane: {rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("(set)")), "{rows:?}");
    }

    #[test]
    fn one_cursor_walks_from_the_last_field_into_the_env_keys() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "x")]);
        app.update(Msg::Key(KeyPress::SelectLast));
        assert!(matches!(
            app.config_pane().unwrap().rows().last(),
            Some(PaneRow::AddEnv)
        ));
    }

    #[test]
    fn setting_an_env_key_files_an_edit_rather_than_sending() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "x")]);
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_env_key(&mut app, "NODE_ENV");
        app.update(Msg::Key(KeyPress::Confirm));
        for typed in "staging".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let effect = app.update(Msg::Key(KeyPress::TextApply));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    /// An env edit and a config field of the same name are two entries,
    /// which is the whole reason `EditKey` has two arms.
    #[test]
    fn an_env_edit_does_not_collide_with_the_env_config_field() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[("env", "x")]);
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_env_key(&mut app, "env");
        fixtures::type_into_the_open_editor(&mut app, "y");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert!(
            app.config_pane()
                .unwrap()
                .edits()
                .get(&EditKey::Env("env".to_owned()))
                .is_some()
        );
    }

    #[test]
    fn the_add_a_key_row_opens_an_editor() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[]);
        app.set_control_for_tests(Control::Allowed);
        app.update(Msg::Key(KeyPress::SelectLast));
        app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text);
    }

    /// The behaviour change this branch exists for: `e` used to close the
    /// pane, and now it does the field's own edit instead.
    #[test]
    fn e_no_longer_closes_the_pane() {
        let mut app = fixtures::app_in_sheep_pane();
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert!(
            app.config_pane().is_some(),
            "e edits now; it does not close"
        );
    }

    #[test]
    fn e_opens_the_editor_on_a_typed_field_the_same_as_enter() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(app.mode(), InputMode::Text, "e opens the text editor");
        assert_eq!(
            app.config_pane().unwrap().typing().map(|t| t.key.as_str()),
            Some("cwd")
        );
    }

    /// `e` opens the env editor exactly as `Enter` does, and does not close
    /// the pane, the same as it does for any other field row: an env row
    /// walks the same cursor and answers to the same key.
    #[test]
    fn e_opens_the_env_editor_the_same_as_enter_and_does_not_close_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert!(
            app.config_pane().is_some(),
            "e must not close the pane from an env row"
        );
        assert_eq!(app.mode(), InputMode::Text, "e opens the editor here too");
    }

    #[test]
    fn the_pane_owns_the_keyboard_while_it_is_open() {
        let mut app = fixtures::app_in_sheep_pane();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop))),
            Effect::None
        );
        assert!(
            app.action().is_none(),
            "no action arms from inside the pane"
        );
        assert_eq!(app.update(Msg::Key(KeyPress::Settings)), Effect::None);
        assert!(
            app.settings().is_none(),
            "`s` does not open a second screen"
        );
        assert!(app.config_pane().is_some());
    }

    #[test]
    fn the_movement_keys_walk_the_fields() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::SelectDown));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(app.config_pane().unwrap().view().cursor(), 2);
        app.update(Msg::Key(KeyPress::SelectLast));
        // `process`, the group a fresh pane opens on, has ten fields, then
        // the fixture's two env keys and `+ add a key`: thirteen rows,
        // index 12.
        assert_eq!(app.config_pane().unwrap().view().cursor(), 12);
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(app.config_pane().unwrap().view().cursor(), 0);
    }

    #[test]
    fn r_re_reads_the_same_sheep_and_the_cursor_survives_it() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::SelectLast));
        let sent = Sent::SheepConfig {
            name: "web".to_string(),
        };
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::Send(sent.clone())
        );
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        // `process`, the group a fresh pane opens on, has ten fields, then
        // the fixture's two env keys and `+ add a key`: thirteen rows,
        // index 12.
        assert_eq!(app.config_pane().unwrap().view().cursor(), 12);
    }

    #[test]
    fn a_refused_config_read_says_why_and_leaves_the_pane_alone() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: "no sheep named web".to_string(),
                daemon_version: None,
            })),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.contains("no sheep named web"), "got {said:?}");
        assert!(app.notice().unwrap().is_grave());
        assert!(app.config_pane().is_some(), "the pane stays as it was");
    }

    /// Silence looks exactly like a key that is not bound.
    #[test]
    fn a_config_read_that_was_never_sent_says_so() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Unsent {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.contains("web"), "got {said:?}");
        assert!(app.notice().unwrap().is_grave());
    }

    /// It spends its first line on a title naming the sheep, so a viewport
    /// told the full body height would scroll one row late.
    #[test]
    fn the_pane_gets_the_body_height_minus_its_own_title() {
        let mut app = fixtures::app_in_sheep_pane();
        app.note_body_rows(6);
        app.update(Msg::Key(KeyPress::SelectLast));
        // `process`, the group a fresh pane opens on, has ten fields, then
        // the fixture's two env keys and `+ add a key`: thirteen rows.
        assert_eq!(app.config_pane().unwrap().view().offset(), 13 - 5);
    }
}
