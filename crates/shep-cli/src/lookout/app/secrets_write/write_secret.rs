use super::super::*;

impl App {
    /// `Enter` on the secrets pane while nothing is armed: opens the
    /// `+ new key` row's name input, or the selected key's value input.
    /// Refuses a provider row (read-only here) and a read-only lookout,
    /// each with its own reason, through [`Self::authorize_write`] for the
    /// second.
    ///
    /// The gate is checked before the input opens, not just before the
    /// write: a refusal that arrived only once a whole value was typed
    /// would waste every one of those keystrokes for nothing.
    pub(in crate::lookout::app) fn secrets_confirm(&mut self) -> Effect {
        // `environments` empty is the placeholder model `KeyPress::Secrets`
        // opens with, before the first `Msg::Secrets` lands: `selected` (0)
        // and `model.rows.len()` (also 0) coincide there by having no rows
        // yet either, the same trap `Msg::Secrets`'s own clamp guards
        // against. A real load never has an empty `environments`: the model
        // always carries `all`, even over an empty store.
        let is_new_key_row = matches!(
            &self.body,
            Body::Secrets(pane)
                if !pane.model.environments.is_empty() && pane.selected_is_new_key_row()
        );
        if is_new_key_row {
            if self.authorize_write().is_none() {
                return Effect::None;
            }
            let Some(pane) = self.secrets_pane_mut() else {
                return Effect::None;
            };
            pane.typing = Some(Typing {
                what: TypingWhat::NewKey,
                buffer: String::new(),
            });
            self.mode = InputMode::Text;
            return Effect::None;
        }
        let Some(row) = (match &self.body {
            Body::Secrets(pane) => pane.model.rows.get(pane.selected).cloned(),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        if matches!(row.source, Source::Namespace(_)) {
            self.notice = Some(Notice {
                text: PROVIDER_ROW_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        // Seeded empty, never with the stored value: showing it here would
        // put a secret on screen with none of the reveal gate's ten-second
        // limit or its own `[secrets] allow_read` check.
        pane.typing = Some(Typing {
            what: TypingWhat::ValueFor(row.key),
            buffer: String::new(),
        });
        self.mode = InputMode::Text;
        Effect::None
    }

    /// The secrets pane's own text keymap, in force while
    /// [`SecretsPane::typing`] owns [`InputMode::Text`].
    pub(in crate::lookout::app) fn on_secrets_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(typing) = self
                    .secrets_pane_mut()
                    .and_then(|pane| pane.typing.as_mut())
                {
                    typing.buffer.push(typed);
                }
            }
            KeyPress::TextBackspace => {
                if let Some(typing) = self
                    .secrets_pane_mut()
                    .and_then(|pane| pane.typing.as_mut())
                {
                    typing.buffer.pop();
                }
            }
            KeyPress::TextApply => return self.apply_secrets_text(),
            KeyPress::TextAbandon => {
                if let Some(pane) = self.secrets_pane_mut() {
                    pane.typing = None;
                }
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// `TextApply` on the secrets pane's own input.
    ///
    /// The name step hands straight to the value step rather than writing
    /// anything on its own. The value step validates against the store's
    /// own rules ([`shep_core::secrets::is_name`],
    /// [`shep_core::secrets::MAX_VALUE_BYTES`]) rather than a second copy of
    /// either, and raises [`Effect::WriteSecret`] only once both the
    /// grammar and the control gate hold; a refusal reopens the same input
    /// with what was typed still in it, so neither costs a retype.
    fn apply_secrets_text(&mut self) -> Effect {
        let Some(typing) = self.secrets_pane_mut().and_then(|pane| pane.typing.take()) else {
            return Effect::None;
        };
        match typing.what {
            TypingWhat::NewKey => {
                let name = typing.buffer;
                if shep_core::secrets::is_name(&name) {
                    if let Some(pane) = self.secrets_pane_mut() {
                        pane.typing = Some(Typing {
                            what: TypingWhat::ValueFor(name),
                            buffer: String::new(),
                        });
                    }
                    return Effect::None;
                }
                self.notice = Some(Notice {
                    text: format!("{name:?} is not a valid key: {NEW_KEY_GRAMMAR}"),
                    grave: true,
                });
                if let Some(pane) = self.secrets_pane_mut() {
                    pane.typing = Some(Typing {
                        what: TypingWhat::NewKey,
                        buffer: name,
                    });
                }
                Effect::None
            }
            TypingWhat::ValueFor(key) => {
                let value = typing.buffer;
                if value.len() > shep_core::secrets::MAX_VALUE_BYTES {
                    self.notice = Some(Notice {
                        text: format!(
                            "value is {} bytes, over the {}-byte limit",
                            value.len(),
                            shep_core::secrets::MAX_VALUE_BYTES
                        ),
                        grave: true,
                    });
                    if let Some(pane) = self.secrets_pane_mut() {
                        pane.typing = Some(Typing {
                            what: TypingWhat::ValueFor(key),
                            buffer: value,
                        });
                    }
                    return Effect::None;
                }
                let environment = match &self.body {
                    Body::Secrets(pane) => pane.environment().unwrap_or_default().to_string(),
                    Body::FlockTable
                    | Body::Settings(_)
                    | Body::ConfigPane(_)
                    | Body::Bleats(_)
                    | Body::Sheep(_) => return Effect::None,
                };
                let Some(authority) = self.authorize_write() else {
                    return Effect::None;
                };
                self.mode = InputMode::Normal;
                Effect::WriteSecret(
                    SecretEdit {
                        key,
                        environment,
                        value: Some(value),
                    },
                    authority,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use super::super::testing::*;
    use crate::lookout::view::fixtures;

    #[test]
    fn enter_opens_the_value_input_seeded_empty() {
        let mut app = fixtures::app_with_secrets_and_control();

        let _ = app.update(Msg::Key(KeyPress::Confirm));

        let typing = typing_of(&app).expect("the input is open");
        assert_eq!(typing.what, TypingWhat::ValueFor("DB_PASSWORD".into()));
        assert_eq!(
            typing.buffer, "",
            "seeding it with the stored value would put a secret on screen \
                 that `v` and its gate exist to control"
        );
    }

    #[test]
    fn a_write_refuses_without_the_control_gate() {
        let mut app = fixtures::app_with_secrets_read_only();

        let _ = app.update(Msg::Key(KeyPress::Confirm));

        assert!(typing_of(&app).is_none());
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("read-only")),
            "the existing refusal, not a second one"
        );
    }

    #[test]
    fn a_value_over_the_cap_is_refused_at_the_input_not_at_the_file() {
        let mut app = fixtures::app_typing_a_value();
        for _ in 0..=shep_core::secrets::MAX_VALUE_BYTES {
            let _ = app.update(Msg::Key(KeyPress::TextChar('x')));
        }

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(matches!(effect, Effect::None), "nothing reached the file");
        assert!(notice_of(&app).is_some_and(|n| n.contains("4096")));
    }

    #[test]
    fn a_key_outside_the_grammar_is_refused_with_the_grammar() {
        let mut app = fixtures::app_typing_a_new_key();
        for c in ".bad".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(matches!(effect, Effect::None));
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("not starting with a dot")),
            "the refusal states the rule, not just that it failed"
        );
    }

    #[test]
    fn a_value_lands_in_the_tabs_own_environment() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::TabNext));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for c in "s3cret".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        let Effect::WriteSecret(edit, _) = effect else {
            panic!("expected a write, got {effect:?}");
        };
        assert_eq!(edit.environment, second_tab_of(&app));
        assert_eq!(edit.value.as_deref(), Some("s3cret"));
    }

    #[test]
    fn a_successful_write_takes_a_revealed_value_off_the_screen() {
        let mut app = fixtures::app_revealing_with_control();

        let _ = app.update(Msg::SecretWritten { result: Ok(true) });

        assert!(
            reveal_of(&app).is_none(),
            "the value on screen belonged to what the store held before the write"
        );
    }

    #[test]
    fn a_failed_write_says_why_and_leaves_the_table_alone() {
        let mut app = fixtures::app_with_secrets_and_control();
        let before = row_count(&app);

        let _ = app.update(Msg::SecretWritten {
            result: Err("permission denied (os error 13)".to_string()),
        });

        assert!(
            notice_of(&app).is_some_and(|n| n.contains("permission denied")),
            "the operator gets the reason, not a silent no-op"
        );
        assert_eq!(row_count(&app), before, "and nothing is redrawn as changed");
    }

    #[test]
    fn the_new_key_row_opens_a_name_input_and_then_a_value_input() {
        let mut app = fixtures::app_with_secrets_and_control();
        select_new_key_row(&mut app);

        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(
            typing_of(&app).map(|t| t.what.clone()),
            Some(TypingWhat::NewKey)
        );

        for c in "NEW_KEY".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(
            matches!(effect, Effect::None),
            "naming a key writes nothing on its own"
        );
        assert_eq!(
            typing_of(&app).map(|t| t.what.clone()),
            Some(TypingWhat::ValueFor("NEW_KEY".into())),
            "the name input hands straight over to the value input"
        );
    }

    #[test]
    fn a_provider_row_refuses_a_write() {
        let mut app = fixtures::app_with_a_pushed_secret_selected_and_control();

        let _ = app.update(Msg::Key(KeyPress::Confirm));

        assert!(typing_of(&app).is_none());
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("pushed by a dog")),
            "and it says why rather than doing nothing"
        );
    }
}
