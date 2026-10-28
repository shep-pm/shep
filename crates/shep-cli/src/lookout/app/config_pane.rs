//! The config pane: asking for a sheep's config, and closing it. The keyboard
//! over an open pane is [`crate::lookout::app::config_pane_keys`], and the
//! edits it files are in [`crate::lookout::app::config_pane_edits`].

use super::*;

/// Which screen asked for a sheep's config.
///
/// Both [`Sent::SheepConfig`]'s callers send the same request, and the reply
/// cannot tell them apart on its own: `e` inside the sheep pane leaves that
/// pane on screen while the reply is in flight, so [`App::on_sheep_config`]
/// cannot route by the current [`Body`]. It is set in the same step as
/// [`App::config_target`], by whichever door sent the request.
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
    /// The cursor and offset are carried across a refresh rather than reset,
    /// the same rule `Msg::Settings` follows: a re-read must not throw an
    /// operator back to `name`. [`ConfigPane::adopt_view`] clamps what it
    /// adopts. A failed read leaves the screen as it was and raises a notice.
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
        self.config_read_in_flight = false;
        if self.closing {
            return self.on_close_reread(name, result);
        }
        match result {
            Ok(Response::SheepConfig(view)) => match self.config_for {
                Some(ConfigFor::SheepPane) => {
                    if let Some(pane) = self.sheep_pane_mut() {
                        pane.adopt_config(*view);
                    }
                    Effect::None
                }
                // `None` only if something outside this file set
                // `config_target` without `config_for`, which nothing does:
                // [`Self::ask_for_sheep_config`] is the one place both are
                // set, always together.
                None | Some(ConfigFor::Editor) => self.open_or_refresh_config_pane(*view),
            },
            // Prefixed, like every other reply handler in this file: the
            // shepherd's refusals mostly name their own subject, but
            // `EngineStopped` renders as `the supervisor engine has
            // stopped`, naming neither the sheep nor the screen.
            other => {
                // The tab and the dog that were waiting on this read go with
                // it: a pane that never opened spent neither, and left set
                // they would open the next one the operator asked for.
                self.focus_ask = None;
                self.notice = Some(Notice::grave(format!("{name}: {}", failed_reply(other))));
                Effect::None
            }
        }
    }

    /// Opens the config pane on `view`, or refreshes one already open in
    /// place, carrying across everything a rebuild would otherwise drop.
    /// Split out of [`Self::on_sheep_config`] so that method can route a
    /// [`ConfigFor::SheepPane`] reply without repeating its guard.
    ///
    /// A tab the command line asked for is spent here, ahead of the carried
    /// one: the operator named it, and nothing has been read since.
    pub(super) fn open_or_refresh_config_pane(&mut self, view: SheepConfigView) -> Effect {
        let carried = self.config_pane().map(|pane| pane.view().clone());
        // The tab too, so a re-read lands where the operator was reading.
        let carried_group = self.config_pane().map(ConfigPane::group_index);
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
        // The dogs tab rides across too, its open tables refreshed from the
        // new ones with their edits kept, so a removal shows at once.
        let carried_dogs = self
            .config_pane()
            .filter(|pane| matches!(pane.target(), PaneTarget::Sheep { .. }))
            .map(|pane| pane.dogs_tab().clone());
        // The cursor among the dogs is carried by name: a re-read can list a
        // table above it.
        let carried_dog = self.config_pane().and_then(ConfigPane::dog_anchor);
        // The pending set survives the rebuild: the values are
        // the shepherd's and the edits are the operator's. An open
        // editor is dropped. See `ConfigPane::adopt_edits`.
        let carried_edits = self
            .config_pane()
            .map(|pane| pane.edits().clone())
            .unwrap_or_default();
        let mut pane = ConfigPane::sheep(view);
        pane.adopt_edits(carried_edits);
        if let Some(group) = carried_group {
            pane.adopt_group(group);
        }
        // Ahead of the carried group: the command line named this one, and no
        // keystroke has moved a tab since.
        if let Some(tab) = self.take_focus_tab()
            && let Some(index) = shep_core::config::GROUP_ORDER
                .iter()
                .position(|group| *group == tab)
        {
            pane.adopt_group(index);
        }
        if let Some(dogs) = carried_dogs {
            pane.adopt_dogs_tab(dogs);
        }
        if let Some(carried) = carried {
            pane.adopt_view(carried);
        }
        if let Some(dog) = carried_dog {
            pane.restore_dog_anchor(&dog);
        }
        if let Some(env_key) = carried_env_key {
            pane.adopt_env_cursor(env_key.as_deref());
        }
        if let Some((key, carried)) = carried_list {
            pane.adopt_list_view(&key, carried);
        }
        self.body = Body::ConfigPane(Box::new(pane));
        // The rebuilt pane carries no editor, so the keyboard must not
        // still think one is open.
        self.release_text_mode_if_unowned();
        // An overlay open when this reply landed would otherwise survive
        // the body it was drawn over, and every key from here goes to
        // `on_keymap_key` instead of the pane the operator asked for.
        self.keymap_open = false;
        // The `dogs` tab has no rows until its listing lands, and a pane that
        // opened on it needs the ask `tab` would have raised.
        self.dogs_tab_effect()
    }

    /// `e`'s own handler: open a dog's pane directly, else ask the shepherd.
    ///
    /// [`Self::selected_row`] rather than [`Self::selected_name`]: a dog
    /// runs one process and is never a group row, but a group row must
    /// still work with `e`.
    pub(super) fn ask_for_config(&mut self) -> Effect {
        // A dog's pane is read off its binary and `dogs.toml` on this machine.
        if self
            .selected_row()
            .is_some_and(|row| row.info.dog.is_some())
            && self.refuse_over_host()
        {
            return Effect::None;
        }
        if let Some(row) = self.selected_row()
            && let Some(source) = row.info.dog.as_ref()
        {
            let adopted_path = match source {
                DogSource::Adopted { path, .. } => Some(PathBuf::from(path)),
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
    /// The one place [`Self::config_target`] and [`Self::config_for`] are set
    /// for a sheep-config read, so the two can never disagree.
    ///
    /// An ask for one sheep retires the tab and the dog the command line was
    /// waiting on for another; an ask for the same sheep keeps them.
    pub(super) fn ask_for_sheep_config(&mut self, name: String, for_screen: ConfigFor) -> Effect {
        if self.focus_ask.as_ref().is_some_and(|ask| ask.sheep != name) {
            self.focus_ask = None;
        }
        self.config_target = Some(name.clone());
        self.config_for = Some(for_screen);
        Effect::Send(Sent::SheepConfig { name })
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
    /// `None` while a read for this pane is already out.
    pub(super) fn reread_pane(&mut self) -> Effect {
        if self.config_read_in_flight {
            return Effect::None;
        }
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        let name = pane.target().name().to_owned();
        let effect = match pane.target() {
            PaneTarget::Sheep { .. } | PaneTarget::SheepDog { .. } => {
                Effect::Send(Sent::SheepConfig { name })
            }
            PaneTarget::Dog { .. } => Effect::Send(Sent::DogSection { name }),
        };
        self.config_read_in_flight = true;
        effect
    }

    /// Drops the open pane and everything a reply for it would re-open.
    ///
    /// Both targets are cleared, always, and not only the one the pane
    /// holds: a read for the other kind can be in flight when this runs, and
    /// a stale one left set is the re-open behind the operator's back
    /// `config_target` exists to prevent. This always lands on
    /// [`Body::FlockTable`]: there is only ever one screen to close to.
    pub(super) fn close_pane(&mut self) {
        self.body = Body::FlockTable;
        self.close_dialog = None;
        self.config_target = None;
        self.config_for = None;
        self.dog_target = None;
        self.closing = false;
        // A listing still out belongs to this pane. Retired here, not on a
        // refresh, so a pane reopened on the same sheep does not take it.
        self.sheep_dogs_ask += 1;
        // So does the rest of what the command line asked for. The operator
        // closed the pane it was opening; the next one is theirs.
        self.focus_ask = None;
        self.release_text_mode_if_unowned();
    }

    /// The refusal a locked row gets, in that row's own words. Two
    /// sentences for two facts: shep refusing a config write is not the same
    /// as this pane having no widget for a shape a Flockfile writes well.
    /// The wildcard keeps a generic sentence rather than guessing a verb.
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

    /// Refuse the row under the cursor when it is locked: raise the grave
    /// notice in that row's own words, and report whether it did.
    ///
    /// [`Self::cycle_field`] and [`Self::restore_default`] gate on it as
    /// `if self.refuse_if_locked() { return Effect::None; }`.
    pub(super) fn refuse_if_locked(&mut self) -> bool {
        let Some((key, lock)) = self
            .config_pane()
            .and_then(ConfigPane::cursor_lock)
            .map(|(key, lock)| (key.to_owned(), lock))
        else {
            return false;
        };
        self.refuse_locked(&key, lock);
        true
    }

    /// Raise the grave notice for a locked row, in that row's own words.
    ///
    /// For a caller that already holds the cursor's lock for another gate:
    /// [`Self::confirm_field`] needs it before its opens gate, so it keeps
    /// that computation and hands the pair here.
    pub(super) fn refuse_locked(&mut self, key: &str, lock: Lock) {
        self.notice = Some(Notice::grave(Self::lock_refusal(key, lock)));
    }

    /// The [`WriteAuthority`] every settings write path has to hold, or
    /// [`None`] with the refusal already raised.
    ///
    /// The one place [`Control`] is read on this screen: `space` on a scalar or
    /// a dog, and `Enter` opening or applying an edit, all come through here.
    pub(super) fn authorize_write(&mut self) -> Option<WriteAuthority> {
        let authority = WriteAuthority::granted(self);
        if authority.is_none() {
            self.notice = Some(Notice::grave(READ_ONLY_REFUSAL.to_string()));
        }
        authority
    }

    /// The open config pane, or `None` while nothing is being edited:
    /// [`Body::config_pane`] over this app's body.
    #[must_use]
    pub fn config_pane(&self) -> Option<&ConfigPane> {
        self.body.config_pane()
    }

    /// [`Self::config_pane`]'s mutable twin, for the pane keymap and the
    /// handlers that settle a write or adopt a re-read in place.
    ///
    /// A caller that still needs another `App` field while it holds the pane
    /// calls [`Body::config_pane_mut`] on `self.body` instead, which borrows
    /// no more than the body.
    pub(super) fn config_pane_mut(&mut self) -> Option<&mut ConfigPane> {
        self.body.config_pane_mut()
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
        assert_eq!(pane.fields().len(), 44);
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
    /// is the other one: the pane's own reply lands first, and the settings
    /// read arrives with the operator two actions past caring about it.
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
            edit: fixtures::setting_set("daemon.max_cron_sleep", "500ms"),
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

    /// `space`, `Enter` and `d` each refuse a locked row, and all three
    /// refusals are one sentence: one fact, one wording, three doors.
    #[test]
    fn a_locked_row_refuses_cycle_confirm_and_restore_with_the_same_sentence() {
        for key in ["instances", "liveness_probe"] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            let cycled = app.notice().map(ToString::to_string);
            let _ = app.update(Msg::Key(KeyPress::Confirm));
            let confirmed = app.notice().map(ToString::to_string);
            let _ = app.update(Msg::Key(KeyPress::Remove));
            let restored = app.notice().map(ToString::to_string);
            assert_eq!(cycled, confirmed, "{key}");
            assert_eq!(confirmed, restored, "{key}");
            assert!(
                cycled.as_deref().is_some_and(|text| text.contains(key)),
                "{key}: {cycled:?}"
            );
            assert!(app.config_pane().unwrap().edits().is_empty(), "{key}");
        }
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
