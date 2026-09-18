use super::super::*;

impl App {
    /// The settings editor's own text keymap, in force while a
    /// [`Pending::Typing`] owns [`InputMode::Text`]. The buffer is never
    /// trimmed.
    ///
    /// `TextApply` arms rather than writes: an empty buffer becomes
    /// [`SettingEdit::Unset`], anything else [`SettingEdit::Set`], and the next
    /// `Enter` sends it. `TextAbandon` leaves the screen open.
    pub(in crate::lookout::app) fn on_settings_text_key(&mut self, key: KeyPress) -> Effect {
        let now = self.now;
        let Some(settings) = self.settings_mut() else {
            return Effect::None;
        };
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(Pending::Typing { buffer, .. }) = settings.pending.as_mut() {
                    buffer.push(typed);
                }
            }
            KeyPress::TextBackspace => {
                if let Some(Pending::Typing { buffer, .. }) = settings.pending.as_mut() {
                    buffer.pop();
                }
            }
            KeyPress::TextApply => {
                if let Some(Pending::Typing { field, buffer }) = settings.pending.take() {
                    let edit = if buffer.is_empty() {
                        SettingEdit::Unset { field }
                    } else {
                        SettingEdit::Set {
                            field,
                            value: buffer,
                        }
                    };
                    let text = confirm_text_for_edit(&edit);
                    settings.pending = Some(Pending::Armed {
                        edit,
                        text,
                        at: now,
                    });
                }
                self.mode = InputMode::Normal;
            }
            KeyPress::TextAbandon => {
                settings.pending = None;
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::lookout::view::fixtures;

    #[test]
    fn enter_on_a_text_row_opens_the_editor_seeded_with_the_current_value() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let (field, buffer) = app.settings().unwrap().typing().expect("the editor opens");
        assert_eq!(*field, SettingField::MaxCronSleep);
        assert_eq!(buffer, "30s");
    }

    #[test]
    fn typing_then_enter_arms_rather_than_writing() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..3 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for c in "45s".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        assert_eq!(app.update(Msg::Key(KeyPress::TextApply)), Effect::None);
        let prompt = app.settings().unwrap().pending().unwrap();
        assert!(
            !prompt.sent,
            "the editor arms; a second Enter is what sends"
        );
        assert!(prompt.text.contains("45s"), "got: {}", prompt.text);
        assert!(
            prompt.text.contains("SHEP_MAX_CRON_SLEEP"),
            "got: {}",
            prompt.text
        );
    }

    #[test]
    fn an_empty_editor_arms_an_unset() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.starts_with("unset max_cron_sleep?"), "got: {text}");
    }

    #[test]
    fn the_socket_confirm_rules_out_the_reload_it_would_otherwise_imply() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("stopped and started"), "got: {text}");
        assert!(text.contains("a reload will not move it"), "got: {text}");
    }

    /// A refusal is discovered under the lock, so it lands after the confirm,
    /// and the typed text has to survive it.
    #[test]
    fn a_refused_write_reopens_the_editor_with_the_text_intact() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..3 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for c in "500ms".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let _ = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Err("max_cron_sleep is 500ms, below the 1s floor".into()),
        });

        let (_, buffer) = app
            .settings()
            .unwrap()
            .typing()
            .expect("the editor reopens");
        assert_eq!(buffer, "500ms");
        assert_eq!(
            app.mode(),
            InputMode::Text,
            "a reopened editor owns the keyboard, or the text is unreachable"
        );
        assert!(
            app.notice()
                .unwrap()
                .to_string()
                .contains("below the 1s floor")
        );
    }

    #[test]
    fn escape_abandons_the_editor_and_keeps_the_screen_open() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextAbandon));
        assert!(app.settings().unwrap().typing().is_none());
        assert!(app.settings().is_some());
    }

    #[test]
    fn a_closed_scalar_has_no_editor() {
        let mut app = fixtures::app_in_settings_with_control(); // on log_level
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            app.settings().unwrap().typing().is_none(),
            "log_level is a cycle, not a text field"
        );
    }
}
