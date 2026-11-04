//! The keyboard over an open config pane: the one dispatch that decides which
//! handler answers a key.
//!
//! Moved whole out of `config_pane` under IR-48's 1000-line ceiling: the arms,
//! their order and their comments are what they were, and the handlers this
//! dispatches to live where they already did, in [`crate::lookout::app::close_dialog`],
//! [`crate::lookout::app::pane_list`] and [`crate::lookout::app::pane_dogs`].

use super::*;
use crate::lookout::pane::{Step, Tab};

impl App {
    /// The config pane's own keymap, in force for as long as
    /// [`Self::config_pane`] is `Some`.
    ///
    /// Movement walks fields, `r` re-reads, `space` cycles the row under the
    /// cursor, `Enter` or `e` edits it, `u` undoes the newest edit, and
    /// `Escape` asks the close dialog's question if there is one: see
    /// [`Self::reread_before_closing`]. Nothing is armed here.
    pub(super) fn on_pane_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
        // An armed env delete is cancelled by every key but the confirm and a
        // quit, the secrets pane's rule, and its status line promises exactly
        // that. The arm is taken before the key is dispatched so `Escape` can
        // eat its own press rather than also closing the pane behind it.
        let was_armed = !matches!(key, KeyPress::Confirm | KeyPress::Quit)
            && self
                .config_pane_mut()
                .is_some_and(ConfigPane::disarm_env_delete);
        if self.close_dialog.is_some() {
            return self.on_close_dialog_key(key);
        }
        if self.config_pane().is_some_and(|pane| pane.list().is_some()) {
            return self.on_list_key(key);
        }
        if let Some(effect) = self.on_pane_dog_key(key) {
            return effect;
        }
        if let Some(effect) = self.on_dogs_tab_key(key) {
            return effect;
        }
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            // Unreachable: the guard above already returned. Kept rather
            // than a wildcard, because every other arm here is named on
            // purpose, and a wildcard would defeat that for every variant,
            // not just this one.
            KeyPress::Quit => return Effect::Quit,
            // Backs out one level at a time: the close dialog's own
            // question first, if there is one, else the pane. `Escape`
            // closes rather than cascading to a filter clear or a quit.
            // Nothing leaves the pane until the question is answered.
            KeyPress::Escape => {
                // An armed delete eats the first `Escape`, as it does on the
                // secrets pane: a delete waiting on a confirm is a state the
                // operator should see cleared before anything else moves.
                if was_armed {
                    return Effect::None;
                }
                if let Some(text) = self.config_pane().and_then(ConfigPane::incomplete_union) {
                    self.notice = Some(Notice::grave(text));
                    return Effect::None;
                }
                // Tables are read again first: the dialog's `c`, `R` and `L`
                // write them too. See [`Self::on_close_reread`].
                if let Some(effect) = self.reread_before_closing() {
                    return effect;
                }
                if let Some(dialog) = self.close_offer() {
                    self.close_dialog = Some(dialog);
                    return Effect::None;
                }
                return self.write_and_close();
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
            KeyPress::Confirm => return self.confirm_field(),
            // `e` does exactly what `Enter` does here: an operator who
            // opened the pane with `e` should not have to learn a second
            // key to use it. Except on a table, which `Enter` opens and
            // closes and which holds nothing for `e` to edit.
            KeyPress::Edit => {
                if self.config_pane().is_some_and(|pane| !pane.cursor_on_table()) {
                    return self.confirm_field();
                }
            }
            // On a dog's pane `←` and `→` close and open the table under the
            // cursor; a sheep pane has no tables and they do nothing.
            KeyPress::TabPrev => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.close_table();
                }
            }
            KeyPress::TabNext => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.open_table();
                }
            }
            KeyPress::Help => {
                self.open_keymap();
            }
            // `d` restores the field under the cursor to its default. Does
            // nothing on an env row or `+ add a key`: unsetting a key
            // entirely is a different act from restoring a default, and
            // the spec does not ask for it.
            KeyPress::Remove => return self.restore_default(),
            // `D` arms the removal of the env key under the cursor, `Enter`
            // confirms it, and every other key cancels. The cursor is asked
            // before the gate is: a field row names no key to remove, so `D`
            // is inert there. The gate is checked on the arm, not the confirm.
            KeyPress::ArmDelete => {
                let names_a_key = self
                    .config_pane()
                    .is_some_and(|pane| pane.cursor_env_key_name().is_some());
                if names_a_key {
                    let now = self.now;
                    if self.authorize_write().is_some()
                        && let Some(pane) = self.config_pane_mut()
                    {
                        pane.arm_env_delete(now);
                    }
                }
            }
            KeyPress::Action(_)
            | KeyPress::Settings
            | KeyPress::Catalogue
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextEdit(_)
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::Collapse
            // Bound only in the close dialog; with none up, `c` is a stray
            // key the same way an action key is.
            | KeyPress::Continue => {}
            // The key/value editor's two field steps. `map_key` only raises
            // them in `InputMode::Text`, which `on_key` branches on before
            // this match is reached, so nothing here can see them.
            KeyPress::NextField | KeyPress::PrevField => {}
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
            // `tab` and `shift-tab` walk the groups; a digit jumps straight
            // to one. All reset the cursor to the group's first field, so
            // `j`/`k` never start on a row the new tab does not draw. The
            // `dogs` tab lists its dogs the first time it comes up.
            KeyPress::NextGroup => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.next_group();
                    pane.move_to_first();
                }
                return self.dogs_tab_effect();
            }
            KeyPress::PrevGroup => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.prev_group();
                    pane.move_to_first();
                }
                return self.dogs_tab_effect();
            }
            KeyPress::Group(digit) => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.set_group(digit);
                    pane.move_to_first();
                }
                return self.dogs_tab_effect();
            }
        }
        Effect::None
    }
}

impl App {
    /// A pane dog's tab keys, and its status tab's keymap.
    ///
    /// `tab` and `shift-tab` switch tabs. On the status tab the select keys
    /// move its cursor and a key that only means something on the settings
    /// form does nothing, so it cannot edit a field the operator cannot see.
    /// [`None`] for a dog with no pane, for the settings tab's own keys, and
    /// for the keys both tabs share.
    fn on_pane_dog_key(&mut self, key: KeyPress) -> Option<Effect> {
        let status = self.config_pane_mut()?.status_mut()?;
        if matches!(key, KeyPress::NextGroup | KeyPress::PrevGroup) {
            status.switch_tab();
            return Some(Effect::None);
        }
        if status.tab() != Tab::Status {
            return None;
        }
        match key {
            KeyPress::SelectUp => status.step(Step::Up),
            KeyPress::SelectDown => status.step(Step::Down),
            KeyPress::SelectFirst => status.step(Step::First),
            KeyPress::SelectLast => status.step(Step::Last),
            // A dog's action letter. `r` is one when an action's label gives
            // it, and the pane pulls again on its own after one.
            KeyPress::TextChar(letter) => return Some(self.on_pane_action_key(letter)),
            KeyPress::Quit
            | KeyPress::Escape
            | KeyPress::Refresh
            | KeyPress::Help
            | KeyPress::Continue => return None,
            _other => {}
        }
        Some(Effect::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use shep_core::dogs::pane::{Destructive, Pane, Row};
    use shep_core::protocol::{ActionOutcome, ActionReply, RpcError, RpcErrorCode};

    use crate::lookout::pane::Tab;
    use crate::lookout::view::fixtures;

    fn models() -> Pane {
        let rows = ["a", "b", "c"].map(|key| Row::new(key, [key]));
        Pane::new().table("models", ["MODEL"], rows)
    }

    fn tab_of(app: &App) -> Tab {
        app.config_pane()
            .and_then(ConfigPane::status)
            .map(crate::lookout::pane::DogStatus::tab)
            .expect("the dog declares a pane")
    }

    fn cursor_of(app: &App) -> Option<usize> {
        app.config_pane()
            .and_then(ConfigPane::status)
            .and_then(crate::lookout::pane::DogStatus::cursor)
    }

    /// Whether the config pane holds an armed env delete.
    fn armed(app: &App) -> Option<String> {
        app.config_pane()
            .expect("the pane is open")
            .armed_env()
            .map(|armed| armed.key.clone())
    }

    /// `D` on a row that names no key to remove is inert, and inert here means
    /// silent: the gate's "read-only" sentence answers a write the operator
    /// actually asked for, and no `D` on a field row ever did.
    #[test]
    fn d_on_a_field_row_arms_nothing_and_says_nothing() {
        let mut app = fixtures::app_in_sheep_pane_read_only();
        assert!(
            app.config_pane()
                .expect("the pane is open")
                .cursor_env_key_name()
                .is_none(),
            "sanity: the cursor is on a field"
        );

        let effect = app.update(Msg::Key(KeyPress::ArmDelete));

        assert!(matches!(effect, Effect::None));
        assert!(armed(&app).is_none(), "nothing armed");
        assert!(app.notice().is_none(), "and nothing said");
    }

    /// The same key on a row that does name a key still refuses through the
    /// gate, with the reason the gate gives.
    #[test]
    fn d_on_an_env_row_refuses_through_the_gate_with_its_reason() {
        let mut app = fixtures::app_in_sheep_pane_read_only();
        fixtures::select_env_key(&mut app, "DB_HOST");

        let _ = app.update(Msg::Key(KeyPress::ArmDelete));

        assert!(armed(&app).is_none(), "the gate closed the arm");
        let notice = app.notice().map(ToString::to_string);
        assert!(
            notice
                .as_deref()
                .is_some_and(|notice| notice.contains("read-only")),
            "and named the gate: {notice:?}"
        );
    }

    #[test]
    fn tab_switches_a_pane_dog_between_status_and_settings() {
        let mut app = fixtures::app_in_pane_dog(Some(fixtures::pane_reply(&models())));
        assert_eq!(tab_of(&app), Tab::Status, "a pane dog opens on status");

        let effect = app.update(Msg::Key(KeyPress::NextGroup));
        assert!(matches!(effect, Effect::None));
        assert_eq!(tab_of(&app), Tab::Settings);

        app.update(Msg::Key(KeyPress::PrevGroup));
        assert_eq!(tab_of(&app), Tab::Status, "shift-tab goes back");
        app.update(Msg::Key(KeyPress::PrevGroup));
        assert_eq!(tab_of(&app), Tab::Settings, "and either key switches");
    }

    #[test]
    fn tab_does_nothing_for_a_dog_without_a_pane() {
        let mut app = fixtures::app_in_dog_pane();
        let before = format!("{:?}", app.config_pane());

        for key in [KeyPress::NextGroup, KeyPress::PrevGroup] {
            let effect = app.update(Msg::Key(key));
            assert!(matches!(effect, Effect::None));
        }

        assert!(
            app.config_pane()
                .is_some_and(|pane| pane.status().is_none())
        );
        assert_eq!(format!("{:?}", app.config_pane()), before);
    }

    #[test]
    fn the_select_keys_move_the_status_cursor_and_not_the_settings_form() {
        let mut app = fixtures::app_in_pane_dog(Some(fixtures::pane_reply(&models())));
        let form_cursor = app.config_pane().and_then(ConfigPane::cursor);

        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(cursor_of(&app), Some(1));
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(cursor_of(&app), Some(2));
        app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(cursor_of(&app), Some(1));
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(cursor_of(&app), Some(0));

        assert_eq!(app.config_pane().and_then(ConfigPane::cursor), form_cursor);
    }

    #[test]
    fn the_settings_tab_keeps_its_own_keys() {
        let mut app = fixtures::app_in_pane_dog(Some(fixtures::pane_reply(&models())));
        app.update(Msg::Key(KeyPress::NextGroup));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            cursor_of(&app),
            Some(0),
            "j on settings leaves the status cursor"
        );
    }

    /// Two leases, a destructive and a plain row action, a destructive and a
    /// plain pane action.
    fn leases() -> Pane {
        let rows = ["qwen3-32b", "strata-7b"].map(|key| Row::new(key, [key]));
        Pane::new()
            .table("leases", ["LEASE"], rows)
            .row_action("revoke", "revoke lease", Destructive::Yes)
            .row_action("renew", "renew lease", Destructive::No)
            .action("pause", "pause the board", Destructive::No)
            .action("drain", "drain the board", Destructive::Yes)
    }

    const ACTIONS: [&str; 4] = ["revoke", "renew", "pause", "drain"];

    fn app_over(pane: &Pane, actions: &[&str]) -> App {
        fixtures::app_in_pane_dog_declaring(actions, Some(fixtures::pane_reply(pane)))
    }

    fn press(app: &mut App, letter: char) -> Effect {
        app.update(Msg::Key(KeyPress::TextChar(letter)))
    }

    fn sent_action(action: &str, row: Option<&str>) -> Effect {
        Effect::Send(Sent::DogPaneAction {
            name: "paddock".to_owned(),
            action: action.to_owned(),
            row: row.map(str::to_owned),
        })
    }

    fn said(app: &App) -> Option<String> {
        app.config_pane()
            .and_then(ConfigPane::status)
            .and_then(crate::lookout::pane::DogStatus::said)
            .map(str::to_owned)
    }

    /// The question of the modal that is open, drawn: a modal answers nothing
    /// until it has been on screen.
    fn asked(app: &mut App) -> Option<String> {
        app.note_modal_reach(Some(0));
        match app.modal() {
            Some(crate::lookout::app::Modal::Confirm { question, .. }) => Some(question.clone()),
            _other => None,
        }
    }

    #[test]
    fn a_row_action_runs_on_the_row_under_the_cursor_and_a_pane_action_on_the_pane() {
        let mut app = app_over(&leases(), &ACTIONS);
        app.update(Msg::Key(KeyPress::SelectDown));

        assert_eq!(
            press(&mut app, 'e'),
            sent_action("renew", Some("strata-7b"))
        );
        assert_eq!(press(&mut app, 'p'), sent_action("pause", None));
    }

    #[test]
    fn a_destructive_row_action_asks_first_and_enter_runs_it() {
        let mut app = app_over(&leases(), &ACTIONS);

        let effect = press(&mut app, 'r');

        assert_eq!(effect, Effect::None, "nothing is sent before the answer");
        assert_eq!(
            asked(&mut app).as_deref(),
            Some("Run revoke lease on qwen3-32b?")
        );
        let answer = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(answer, sent_action("revoke", Some("qwen3-32b")));
    }

    #[test]
    fn a_destructive_pane_action_asks_without_a_row() {
        let mut app = app_over(&leases(), &ACTIONS);

        press(&mut app, 'd');

        assert_eq!(asked(&mut app).as_deref(), Some("Run drain the board?"));
        assert_eq!(
            app.update(Msg::Key(KeyPress::Confirm)),
            sent_action("drain", None)
        );
    }

    #[test]
    fn any_key_but_enter_cancels_the_question_and_sends_nothing() {
        for cancel in [
            KeyPress::Escape,
            KeyPress::TextChar('y'),
            KeyPress::SelectDown,
            KeyPress::Quit,
        ] {
            let mut app = app_over(&leases(), &ACTIONS);
            press(&mut app, 'r');
            asked(&mut app);

            let effect = app.update(Msg::Key(cancel));

            assert!(
                !matches!(effect, Effect::Send(_)),
                "{cancel:?} sent {effect:?}"
            );
            assert!(app.modal().is_none(), "{cancel:?} left the question up");
            // And the answer is spent: a later Enter runs nothing.
            assert!(!matches!(
                app.update(Msg::Key(KeyPress::Confirm)),
                Effect::Send(_)
            ));
        }
    }

    #[test]
    fn a_row_action_with_no_row_does_nothing_and_says_so() {
        let empty = Pane::new()
            .table("leases", ["LEASE"], Vec::<Row>::new())
            .row_action("revoke", "revoke lease", Destructive::No);
        let mut app = app_over(&empty, &["revoke"]);

        assert_eq!(press(&mut app, 'r'), Effect::None);

        assert_eq!(said(&app).as_deref(), Some("no row selected"));
        assert!(app.modal().is_none());
    }

    #[test]
    fn an_action_the_manifest_lacks_sends_nothing_and_the_footer_says_so() {
        let mut app = app_over(&leases(), &["pause", "drain"]);

        // `r` is no one's letter, so it falls back to the refresh key.
        assert!(!matches!(
            press(&mut app, 'r'),
            Effect::Send(Sent::DogPaneAction { .. })
        ));
        assert_eq!(press(&mut app, 'e'), Effect::None, "renew has no letter");
        assert!(app.modal().is_none());

        let status = app.config_pane().and_then(ConfigPane::status).unwrap();
        assert_eq!(
            status.undeclared().collect::<Vec<_>>(),
            ["revoke", "renew"],
            "and the footer is told which"
        );
    }

    fn triggered(outcome: ActionOutcome) -> Result<Response, RequestError> {
        Ok(Response::Triggered(vec![ActionReply {
            id: 92,
            name: "paddock".to_owned(),
            outcome,
        }]))
    }

    fn replied(app: &mut App, result: Result<Response, RequestError>) -> Effect {
        app.update(Msg::Replied {
            sent: Sent::DogPaneAction {
                name: "paddock".to_owned(),
                action: "renew".to_owned(),
                row: Some("qwen3-32b".to_owned()),
            },
            result,
        })
    }

    #[test]
    fn a_plain_reply_pulls_the_pane_again_and_shows_its_text() {
        let mut app = app_over(&leases(), &ACTIONS);

        let effect = replied(
            &mut app,
            triggered(ActionOutcome::Replied {
                body: "lease renewed".to_owned(),
            }),
        );

        assert_eq!(
            effect,
            Effect::Send(Sent::DogPane {
                name: "paddock".to_owned(),
                opening: 1,
            })
        );
        assert_eq!(said(&app).as_deref(), Some("lease renewed"));
    }

    #[test]
    fn a_refusal_and_a_timeout_show_their_reasons_and_pull_nothing() {
        let refused = Err(RequestError::Rpc(RpcError {
            code: RpcErrorCode::NotFound,
            message: "no sheep named paddock".into(),
            daemon_version: None,
        }));
        let cases = [
            (refused, "no sheep named paddock"),
            (
                triggered(ActionOutcome::TimedOut),
                "paddock did not answer in time",
            ),
            (
                Err(RequestError::Timeout {
                    after: std::time::Duration::from_secs(60),
                }),
                "no reply within 60s",
            ),
        ];
        for (result, reason) in cases {
            let mut app = app_over(&leases(), &ACTIONS);
            assert_eq!(replied(&mut app, result), Effect::None);
            assert_eq!(said(&app).as_deref(), Some(reason));
        }
    }

    #[test]
    fn a_reply_for_a_pane_the_operator_left_is_dropped() {
        let mut app = app_over(&leases(), &ACTIONS);
        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());

        let effect = replied(
            &mut app,
            triggered(ActionOutcome::Replied {
                body: "lease renewed".to_owned(),
            }),
        );

        assert_eq!(effect, Effect::None);
    }

    #[test]
    fn a_read_only_lookout_runs_no_action() {
        let mut app = app_over(&leases(), &ACTIONS);
        app.set_control_for_tests(Control::ReadOnly);

        assert_eq!(press(&mut app, 'e'), Effect::None);
        assert_eq!(press(&mut app, 'r'), Effect::None);

        assert!(app.modal().is_none());
        assert!(app.notice().is_some_and(|notice| notice.is_grave()));
    }

    #[test]
    fn the_status_tab_reads_keys_in_pane_mode_and_the_settings_tab_in_normal() {
        let mut app = app_over(&leases(), &ACTIONS);
        assert_eq!(app.mode(), InputMode::Pane);

        app.update(Msg::Key(KeyPress::NextGroup));
        assert_eq!(app.mode(), InputMode::Normal);
        app.update(Msg::Key(KeyPress::NextGroup));
        assert_eq!(app.mode(), InputMode::Pane);
    }

    #[test]
    fn r_and_h_keep_refresh_and_help_when_no_action_takes_them() {
        let mut app = app_over(&leases(), &ACTIONS);

        assert!(matches!(press(&mut app, 'r'), Effect::None), "revoke has r");
        assert!(app.modal().is_some(), "and asked first");

        let mut app = app_over(&leases(), &["pause"]);
        assert_eq!(
            press(&mut app, 'r'),
            Effect::Send(Sent::DogSection {
                name: "paddock".to_owned()
            }),
            "r re-reads the dog's section"
        );
        press(&mut app, 'h');
        assert!(app.keymap_open());
    }
}
