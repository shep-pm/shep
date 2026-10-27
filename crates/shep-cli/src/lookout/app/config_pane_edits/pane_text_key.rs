use super::super::*;

impl App {
    /// The config pane's own text keymap, in force for as long as one of
    /// its two editors owns [`InputMode::Text`].
    ///
    /// Does not trim the buffer, for the reason
    /// [`Self::on_settings_text_key`]'s own doc gives: this repository does
    /// not widen an accepted input grammar without a basis in the spec.
    ///
    /// Both editors file on `TextApply`, and neither sends: the pane's own
    /// `Escape` writes the whole set.
    pub(in crate::lookout::app) fn on_pane_text_key(&mut self, key: KeyPress) -> Effect {
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        if self
            .config_pane()
            .is_some_and(|pane| pane.focused_dog().is_some())
        {
            return self
                .in_table(|app| app.on_pane_text_key(key))
                .unwrap_or(Effect::None);
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
            KeyPress::TextEdit(edit) => pane.edit_text(edit),
            KeyPress::TextApply => {
                let refused = pane.apply_typing();
                // `apply_typing` keeps the editor open on a buffer the
                // field's own schema refuses, so the mode follows what the
                // pane actually did rather than what the key asked for.
                if pane.typing().is_none() {
                    self.mode = InputMode::Normal;
                }
                // The sentence is the whole point of refusing here: a dog's
                // section is written unread, so an operator who is not told
                // now finds out from the dog's own log, or never.
                if let Some(refused) = refused {
                    self.notice = Some(Notice::grave(refused.text));
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
}
