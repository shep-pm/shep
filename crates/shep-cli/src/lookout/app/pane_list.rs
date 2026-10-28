//! The two sub-screens under a pane: a dog's array field, and a sheep's
//! environment.

use super::*;
use crate::dog_settings::Refusal;

impl App {
    /// One `Request::SetSheepEnv` reply.
    ///
    /// `was_set` comes off the request rather than the reply: the answer
    /// names the key and deliberately never the value, so it cannot say
    /// which of the two things happened.
    ///
    /// Env is spawn-time in every case (`AppConfig::env` is
    /// `ApplyGroup::NeedsRespawn`), so the sentence says so unconditionally
    /// rather than reading a list that this reply does not carry.
    pub(super) fn on_env_set(
        &mut self,
        name: &str,
        key: &str,
        was_set: bool,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::SheepEnvSet { .. }) => {
                let verb = if was_set { "set" } else { "removed" };
                self.notice = Some(Notice::info(format!(
                    "{name}: env {key} {verb}, and waits for `shep reload {name}`"
                )));
                Effect::Send(Sent::SheepConfig {
                    name: name.to_owned(),
                })
            }
            other => {
                self.notice = Some(Notice::grave(format!(
                    "{name}: env {key}: {}",
                    failed_reply(other)
                )));
                Effect::None
            }
        }
    }

    /// The list sub-screen's own keymap, in force for as long as the pane
    /// holds one.
    ///
    /// `Escape` closes the sub-screen, not the pane, the same
    /// innermost-first rule the env screen follows. `Enter` or `e` opens
    /// the editor on the element under the cursor, or adds one on
    /// `+ new`. `d` removes, `K`/`J` move the element one place, and `h`
    /// opens the keymap overlay, same as everywhere else.
    ///
    /// A removal and a move file the whole array, since that is what the
    /// write carries. Nothing goes out here: the pane's own `Escape` is
    /// what writes the set.
    pub(super) fn on_list_key(&mut self, key: KeyPress) -> Effect {
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            // Unreachable, the same way and for the same reason as
            // `on_pane_key`'s own copy of this arm: the guard above already
            // returned, and this stays instead of a wildcard so a stray
            // `KeyPress` variant added later cannot fall through unnoticed.
            KeyPress::Quit => return Effect::Quit,
            KeyPress::Escape => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.close_list();
                }
                self.release_text_mode_if_unowned();
            }
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if let Some(list) = self.config_pane_mut().and_then(ConfigPane::list_mut) {
                    match key {
                        KeyPress::SelectUp => list.move_by(-1),
                        KeyPress::SelectDown => list.move_by(1),
                        KeyPress::SelectFirst => list.move_to_first(),
                        KeyPress::SelectLast => list.move_to_last(),
                        _ => unreachable!(),
                    }
                }
            }
            KeyPress::Refresh => return self.reread_pane(),
            KeyPress::Confirm | KeyPress::Edit => {
                if self.authorize_write().is_none() {
                    return Effect::None;
                }
                if let Some(list) = self.config_pane_mut().and_then(ConfigPane::list_mut) {
                    list.begin_typing();
                    self.mode = InputMode::Text;
                }
            }
            KeyPress::Remove => {
                if self.authorize_write().is_none() {
                    return Effect::None;
                }
                let refused = self
                    .config_pane_mut()
                    .and_then(|pane| pane.file_list_removal().err());
                self.notice_refusal(refused);
            }
            KeyPress::StepUp | KeyPress::StepDown => {
                if self.authorize_write().is_none() {
                    return Effect::None;
                }
                let delta = if key == KeyPress::StepUp { -1 } else { 1 };
                let refused = self
                    .config_pane_mut()
                    .and_then(|pane| pane.file_list_reorder(delta).err());
                self.notice_refusal(refused);
            }
            KeyPress::Help => {
                self.open_keymap();
            }
            KeyPress::Action(_)
            | KeyPress::Cycle
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextEdit(_)
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::ArmDelete
            | KeyPress::Collapse
            // Bound only in the close dialog; with none up on a list
            // sub-screen, `c` is a stray key the same way an action key is.
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
            // Drops the newest edit, the same key the field list answers
            // and on the same terms: no control gate, since a key that
            // unfiles something cannot write. The sub-screen re-reads the
            // array from what is left filed, so the rows show what was
            // restored. The set holds one entry per field, so this takes
            // the whole array back to the shepherd's rather than one
            // keystroke of it.
            KeyPress::Undo => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.undo_edit();
                }
            }
            // The groups belong to the field list; the sub-screen is one
            // field's own array and has none to walk.
            KeyPress::NextGroup | KeyPress::PrevGroup | KeyPress::Group(_) => {}
            // The key/value editor's two field steps belong to the env
            // editor and the secrets pane: a list sub-screen edits one
            // element at a time and has no second field to step to.
            KeyPress::NextField | KeyPress::PrevField => {}
        }
        Effect::None
    }

    /// Shows `refused`, when there is one, as a grave notice.
    fn notice_refusal(&mut self, refused: Option<Refusal>) {
        if let Some(refused) = refused {
            self.notice = Some(Notice::grave(refused.text));
        }
    }

    /// The list sub-screen's own text keymap.
    ///
    /// `TextApply` files the whole array. An element the item schema refuses
    /// keeps the editor open, which is why the mode follows what the
    /// sub-screen did rather than what the key asked for, and the sentence
    /// naming the rule goes in the status bar: a dog's table is written
    /// without being read, so an operator who is not told now finds out from
    /// the dog's own log, or never.
    pub(super) fn on_list_text_key(&mut self, key: KeyPress) -> Effect {
        let Some(pane) = self.body.config_pane_mut() else {
            return Effect::None;
        };
        let Some(list) = pane.list_mut() else {
            return Effect::None;
        };
        match key {
            KeyPress::TextChar(typed) => list.type_char(typed),
            KeyPress::TextBackspace => list.type_backspace(),
            KeyPress::TextEdit(edit) => list.edit_text(edit),
            KeyPress::TextApply => {
                let applied = list.apply_typing();
                if list.typing().is_none() {
                    self.mode = InputMode::Normal;
                }
                match applied {
                    Ok(Some(text)) => {
                        let refused = pane.file_list_element(text).err();
                        self.notice_refusal(refused);
                    }
                    Ok(None) => {}
                    Err(refused) => self.notice_refusal(Some(refused)),
                }
            }
            KeyPress::TextAbandon => {
                list.abandon_typing();
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// The env editor's own text keymap, in force for as long as
    /// [`ConfigPane::env_typing`] is `Some`. The secrets pane's
    /// [`App::on_secrets_text_key`] answers the same keys the same way,
    /// because both are driving one [`KeyValues`].
    ///
    /// `TextApply` files, exactly as the field editor's does: nothing on an
    /// env row reaches the shepherd before the pane closes, so an env key
    /// typed by mistake costs an `u` rather than an override the operator
    /// cannot read back.
    ///
    /// A refusal keeps the editor open with what was typed still in it, so
    /// the mode follows what the editor did rather than what the key asked
    /// for, the same rule the field editor's own arm above follows.
    pub(super) fn on_env_text_key(&mut self, key: KeyPress) -> Effect {
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            KeyPress::NextField | KeyPress::PrevField => {
                let back = key == KeyPress::PrevField;
                let refused = self
                    .body
                    .config_pane_mut()
                    .and_then(|pane| pane.step_env_field(back));
                if let Some(refused) = refused {
                    self.notice = Some(Notice::grave(refused));
                }
            }
            KeyPress::TextApply => {
                let submitted = self
                    .body
                    .config_pane_mut()
                    .map_or(EnvSubmit::Filed, ConfigPane::submit_env_typing);
                match submitted {
                    EnvSubmit::Refused(text) => self.notice = Some(Notice::grave(text)),
                    EnvSubmit::Moved => {}
                    EnvSubmit::Filed => self.mode = InputMode::Normal,
                }
            }
            KeyPress::TextAbandon => {
                if let Some(pane) = self.body.config_pane_mut() {
                    pane.abandon_env_typing();
                }
                self.mode = InputMode::Normal;
            }
            _ => {
                if let Some(pane) = self.body.config_pane_mut() {
                    pane.type_env_text(key);
                }
            }
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::pane::ListRow;

    /// A dog's env writes through `Request::SetSheepEnv`, which would name
    /// a sheep that does not exist. It refuses with `Lock::NoWidget`'s own
    /// sentence instead.
    #[test]
    fn enter_on_a_dogs_map_field_refuses_rather_than_opening_an_editor() {
        let mut app = fixtures::app_in_dog_pane();
        let index = app
            .config_pane()
            .expect("the pane is open")
            .fields()
            .fields()
            .iter()
            .position(|field| field.key == "sinks")
            .expect("sinks is a bark field");
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..index {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(
            app.config_pane()
                .expect("still open")
                .env_typing()
                .is_none(),
            "a dog has no env editor"
        );
        let notice = app.notice().expect("a locked row answers").to_string();
        assert!(notice.contains("no editor in this pane"), "{notice}");
    }

    /// Through `Msg::Key`, not `ConfigPane::open_list`: `confirm_field`
    /// has its own gate listing which kinds `Enter` opens, and a test that
    /// called the pane method would pass over it.
    #[test]
    fn enter_on_an_array_row_opens_the_list_sub_screen() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let list = app
            .config_pane()
            .expect("the pane is open")
            .list()
            .expect("enter on an array row opens the sub-screen");
        assert_eq!(list.key(), "args");
        assert_eq!(list.elements(), ["--port", "8080"]);
    }

    /// Every key the sub-screen's own hint names, driven the way an
    /// operator drives them, and the array that lands on the wire at the
    /// end of it.
    #[test]
    fn the_list_sub_screen_edits_removes_and_reorders_one_array() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(app.mode(), InputMode::Text, "e opens the element editor");
        for _ in 0..4 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "9090".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        assert_eq!(
            app.update(Msg::Key(KeyPress::TextApply)),
            Effect::None,
            "applying the editor files; it does not send"
        );
        assert_eq!(
            filed_value(&app, "args"),
            serde_json::json!(["--port", "9090"]),
            "the edited element lands in the whole array"
        );

        let _ = app.update(Msg::Key(KeyPress::StepUp));
        assert_eq!(
            filed_value(&app, "args"),
            serde_json::json!(["9090", "--port"]),
            "K moves the element under the cursor up one place, over the \
             array the edit before it filed"
        );
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(
            filed_value(&app, "args"),
            serde_json::json!(["9090"]),
            "d drops the element under the cursor"
        );

        // One field, one entry, however many keystrokes reached it, and
        // every one of them is in the array the wire carries.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let request = one_wire(close_writing(&mut app));
        let Request::SetSheepField { key, value, .. } = request else {
            panic!("expected SetSheepField, got {request:?}");
        };
        assert_eq!(key, "args");
        assert_eq!(value, serde_json::json!(["9090"]));
    }

    /// A refused element keeps `InputMode::Text` on, files nothing, and puts
    /// the sentence naming the rule in the status bar, grave for the same
    /// reason a scalar field's refusal is: a dog's table is written without
    /// being read.
    ///
    /// `stop_exit_codes` is the list the Flockfile schema puts no bound on
    /// its items, so the rule here is the one every integer list carries:
    /// the element has to be a whole number.
    #[test]
    fn a_refused_list_element_keeps_text_mode_on_and_says_why() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "stop_exit_codes");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(app.mode(), InputMode::Text, "e opens the element editor");
        let _ = app.update(Msg::Key(KeyPress::TextChar('x')));
        assert_eq!(
            app.update(Msg::Key(KeyPress::TextApply)),
            Effect::None,
            "a refusal is not a send"
        );
        assert_eq!(
            app.mode(),
            InputMode::Text,
            "the editor stays open on what was typed"
        );
        let notice = app.notice().expect("a refusal answers");
        assert!(notice.is_grave(), "{notice}");
        assert_eq!(
            notice.to_string(),
            "stop_exit_codes is \"x\", which is not a whole number shep accepts"
        );
        assert!(
            app.config_pane()
                .expect("the pane is open")
                .edits()
                .is_empty(),
            "nothing was filed"
        );
        assert!(
            app.config_pane()
                .expect("the pane is open")
                .list()
                .expect("the sub-screen is still up")
                .typing()
                .is_some(),
            "and the buffer is still in the editor"
        );
    }

    /// A hand-written `ports = [80, "http"]` opens as it is. A dog's writer
    /// has no cell for a list that mixes the two, so the keystroke that
    /// would file one says which element is the trouble, and nothing is
    /// filed that would take the pane's other edits down on close.
    #[test]
    fn a_dogs_integer_list_with_a_stray_string_refuses_to_file_and_names_it() {
        let mut app = fixtures::app_in_dog_pane_over_lists(
            "ports = [80, \"http\"]\nhosts = [\"a.example\"]\n".to_owned(),
        );
        pane_to(&mut app, "ports");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        for typed in "443".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let notice = app.notice().expect("the refusal answers");
        assert!(notice.is_grave(), "{notice}");
        assert_eq!(
            notice.to_string(),
            "ports holds \"http\", which is not an integer; remove it before changing ports"
        );
        let pane = app.config_pane().expect("the pane is open");
        assert!(pane.edits().is_empty(), "nothing was filed");

        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(
            filed_value(&app, "ports"),
            serde_json::json!([80]),
            "removing the odd element files the rest"
        );
    }

    /// `Escape` backs out of the sub-screen and leaves the pane up, the
    /// same one-level-at-a-time rule the env screen follows.
    #[test]
    fn escape_leaves_the_list_sub_screen_before_it_closes_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().unwrap().list().is_none());
        assert!(app.config_pane().is_some(), "the pane is still open");
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
    }

    /// `u` is on the sub-screen's own key hint, so it has to do something
    /// there. It drops the field's entry and the rows go back to the
    /// array the shepherd sent.
    #[test]
    fn u_undoes_a_list_edit_from_inside_the_sub_screen() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(filed_value(&app, "args"), serde_json::json!(["--port"]));

        let _ = app.update(Msg::Key(KeyPress::Undo));
        assert!(
            app.config_pane()
                .expect("the pane is open")
                .edits()
                .is_empty(),
            "u drops the entry the removal filed"
        );
        assert_eq!(
            app.config_pane()
                .expect("the pane is open")
                .list()
                .expect("the sub-screen is still up")
                .elements(),
            ["--port", "8080"],
            "the rows show the array that was restored"
        );
    }

    /// A write re-reads the whole config, so without the carry the
    /// sub-screen would shut on the operator's own keystroke.
    #[test]
    fn the_list_sub_screen_survives_the_refresh_a_write_triggers() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        refresh_config(&mut app, &[]);
        let list = app
            .config_pane()
            .expect("the pane is open")
            .list()
            .expect("the sub-screen rides across the refresh");
        assert_eq!(list.key(), "args");
        assert_eq!(
            list.cursor(),
            Some(ListRow::New),
            "a cursor past the end lands on the row where enter destroys nothing"
        );
    }
}
