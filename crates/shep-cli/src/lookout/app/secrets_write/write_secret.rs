use super::super::*;

impl App {
    /// `Enter` on the secrets pane while nothing is armed: opens the `+ new
    /// key` row's editor on its KEY field, or the selected key's editor on its
    /// VALUE field. Refuses a provider row (read-only here) and a read-only
    /// lookout, each with its own reason, through [`Self::authorize_write`] for
    /// the second.
    ///
    /// The gate is checked before the editor opens, not just before the write:
    /// a refusal that arrived only once a whole value was typed would waste
    /// every one of those keystrokes for nothing.
    ///
    /// The editor is the same [`KeyValues`] a sheep's env rows open, so `Tab`,
    /// `Shift-Tab`, `Enter` and `Esc` mean the same thing on both panes.
    pub(in crate::lookout::app) fn secrets_confirm(&mut self) -> Effect {
        // `environments` empty is the placeholder model `KeyPress::Secrets`
        // opens with, before the first `Msg::Secrets` lands: `selected` (0)
        // and `model.rows.len()` (also 0) coincide there by having no rows
        // yet either, the same trap `Msg::Secrets`'s own clamp guards against.
        // A real load never has an empty `environments`: the model always
        // carries `all`, even over an empty store.
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
            pane.typing = Some(KeyValues::add(Grammar::Secret));
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
        if self.refuse_provider_row(&row) {
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        // Seeded empty, never with the stored value: showing it here would put
        // a secret on screen with none of the reveal gate's ten-second limit or
        // its own `[secrets] allow_read` check.
        pane.typing = Some(KeyValues::under(Grammar::Secret, &row.key));
        self.mode = InputMode::Text;
        Effect::None
    }

    /// The secrets pane's own text keymap, in force while
    /// [`SecretsPane::typing`] owns [`InputMode::Text`].
    ///
    /// One [`KeyValues`] and one keymap, the same pair
    /// [`App::on_env_text_key`] runs over a sheep's env rows.
    pub(in crate::lookout::app) fn on_secrets_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::NextField | KeyPress::PrevField => {
                let back = key == KeyPress::PrevField;
                let refused = self
                    .secrets_pane_mut()
                    .and_then(|pane| step_typing(pane.typing.as_mut(), back));
                if let Some(refused) = refused {
                    self.notice = Some(Notice::grave(refused));
                }
            }
            KeyPress::TextApply => return self.apply_secrets_text(),
            KeyPress::TextAbandon => {
                if let Some(pane) = self.secrets_pane_mut() {
                    pane.typing = None;
                }
                self.mode = InputMode::Normal;
            }
            _ => {
                if let Some(typing) = self
                    .secrets_pane_mut()
                    .and_then(|pane| pane.typing.as_mut())
                {
                    typing.handle(key);
                }
            }
        }
        Effect::None
    }

    /// `TextApply` on the secrets pane's own editor.
    ///
    /// `Enter` on KEY checks the name and moves to VALUE, writing nothing;
    /// `Enter` on VALUE submits both. The name grammar is the store's own
    /// ([`shep_core::secrets::is_name`], reached through [`Grammar::Secret`])
    /// and the value cap is [`shep_core::secrets::MAX_VALUE_BYTES`], each
    /// checked rather than copied. A refusal reopens the same editor with what
    /// was typed still in it, so neither costs a retype.
    ///
    /// A KEY that no longer names the row it was opened under is a rename: it
    /// raises one [`Effect::WriteSecrets`] carrying both halves, so one `Enter`
    /// moves the key instead of asking the operator to delete it and add it
    /// again.
    fn apply_secrets_text(&mut self) -> Effect {
        let Some(mut editor) = self.secrets_pane_mut().and_then(|pane| pane.typing.take()) else {
            return Effect::None;
        };
        let Some(refused) = (match editor.submit() {
            Outcome::Submitted => None,
            Outcome::Moved => {
                // Enter on KEY is the move `Tab` makes: the editor stays open,
                // now on VALUE, holding the name that was just typed.
                self.reopen_secret_editor(editor);
                return Effect::None;
            }
            Outcome::Refused(refusal) => Some(refusal.sentence(editor.key().as_str())),
        }) else {
            return self.write_secret(editor);
        };
        self.reopen_secret_editor(editor);
        self.notice = Some(Notice::grave(refused));
        Effect::None
    }

    /// The submit that passed every check the editor itself makes: the value
    /// cap and the tab the write belongs to, each refused the way the editor's
    /// own refusals are, with what was typed kept.
    fn write_secret(&mut self, editor: KeyValues) -> Effect {
        let typed_len = editor.value().as_str().len();
        if typed_len > shep_core::secrets::MAX_VALUE_BYTES {
            self.reopen_secret_editor(editor);
            self.notice = Some(Notice::grave(format!(
                "value is {typed_len} bytes, over the {}-byte limit",
                shep_core::secrets::MAX_VALUE_BYTES
            )));
            return Effect::None;
        }
        // No tab showing is no environment to write to: the empty name is a
        // slot no tab reads. Unreachable through the keymap today, since
        // opening this editor needs a loaded model and a loaded model always
        // carries `all`.
        let Some(environment) = (match &self.body {
            Body::Secrets(pane) => pane.environment().map(str::to_string),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Sheep(_) => None,
        }) else {
            self.reopen_secret_editor(editor);
            return Effect::None;
        };
        let Some(authority) = self.authorize_write() else {
            self.reopen_secret_editor(editor);
            return Effect::None;
        };
        self.mode = InputMode::Normal;
        let key = editor.key().as_str().to_owned();
        let value = Some(editor.value().as_str().to_owned());
        let Some(under) = editor.opened_under().map(str::to_owned) else {
            return Effect::WriteSecret(
                SecretEdit {
                    key,
                    environment,
                    value,
                },
                authority,
            );
        };
        if under == key {
            return Effect::WriteSecret(
                SecretEdit {
                    key,
                    environment,
                    value,
                },
                authority,
            );
        }
        Effect::WriteSecrets(
            vec![
                SecretEdit {
                    key,
                    environment: environment.clone(),
                    value,
                },
                // The removal goes last: a rename that stopped here would have
                // left the value reachable under its new name, and a rename
                // that stopped at the removal would have lost it.
                SecretEdit {
                    key: under,
                    environment,
                    value: None,
                },
            ],
            authority,
        )
    }

    /// Puts an editor back on the pane with what was typed still in it.
    ///
    /// Every refusal path calls it, so a refused keystroke never costs a
    /// retype, the way the too-long value's refusal always has.
    fn reopen_secret_editor(&mut self, editor: KeyValues) {
        if let Some(pane) = self.secrets_pane_mut() {
            pane.typing = Some(editor);
        }
    }
}

/// `Tab` or `Shift-Tab` on an open editor: the sentence a KEY the pane's own
/// grammar refuses raises, with the cursor left on KEY, or [`None`] when the
/// cursor moved.
fn step_typing(typing: Option<&mut KeyValues>, back: bool) -> Option<String> {
    let typing = typing?;
    match typing.step(back) {
        Outcome::Refused(refusal) => Some(refusal.sentence(typing.key().as_str())),
        Outcome::Moved | Outcome::Submitted => None,
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use super::super::testing::*;
    use crate::lookout::keyvalue::Field;
    use crate::lookout::view::fixtures;

    #[test]
    fn enter_opens_the_value_input_seeded_empty() {
        let mut app = fixtures::app_with_secrets_and_control();

        let _ = app.update(Msg::Key(KeyPress::Confirm));

        let typing = typing_of(&app).expect("the input is open");
        assert_eq!(typing.opened_under(), Some("DB_PASSWORD"));
        assert_eq!(typing.focus(), Field::Value);
        assert_eq!(
            typing.value().as_str(),
            "",
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

    /// The name is refused as KEY loses focus, which is what `TextApply` on
    /// KEY means here, and the cursor stays on KEY with the name still typed.
    #[test]
    fn a_key_outside_the_grammar_is_refused_as_the_field_loses_focus() {
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
        let typing = typing_of(&app).expect("the editor stayed open");
        assert_eq!(typing.focus(), Field::Key, "the cursor is kept on KEY");
        assert_eq!(typing.key().as_str(), ".bad", "and so is what was typed");
        assert_eq!(app.mode(), InputMode::Text);

        // `Tab` is the other way out of KEY and takes the same refusal.
        let effect = app.update(Msg::Key(KeyPress::NextField));
        assert!(matches!(effect, Effect::None));
        assert_eq!(typing_of(&app).expect("still open").focus(), Field::Key);
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

    /// A reload's environment union can shrink while a value input is open,
    /// and `SecretsPane::environment` over an empty list names no tab. The
    /// write refuses rather than naming the environment `""`.
    #[test]
    fn a_value_with_no_environment_tab_writes_nothing() {
        let mut app = fixtures::app_typing_a_value();
        let _ = app.update(Msg::Secrets {
            environment: "all".to_string(),
            result: Ok(Box::default()),
        });
        assert!(
            typing_of(&app).is_some_and(|typing| typing.opened_under() == Some("DB_PASSWORD")),
            "sanity: the input outlived the load that took every tab away"
        );

        for c in "s3cret".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(
            !matches!(effect, Effect::WriteSecret(..) | Effect::WriteSecrets(..)),
            "no tab is showing, so nothing here names an environment to write to: {effect:?}"
        );
        assert!(matches!(effect, Effect::None));
        assert!(
            notice_of(&app).is_none(),
            "silent, the way a missing tab refuses a reveal and a delete"
        );
        let typing = typing_of(&app).expect("the refusal keeps what was typed");
        assert_eq!(typing.opened_under(), Some("DB_PASSWORD"));
        assert_eq!(typing.value().as_str(), "s3cret");
        assert_eq!(
            app.mode(),
            InputMode::Text,
            "and the input is still the live one, not a copy off the screen"
        );
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

    /// `Enter` on KEY is the move `Tab` makes: the name writes nothing on its
    /// own, it just hands the editor to VALUE with the name kept.
    #[test]
    fn the_new_key_row_opens_on_key_and_hands_over_to_value() {
        let mut app = fixtures::app_with_secrets_and_control();
        select_new_key_row(&mut app);

        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let typing = typing_of(&app).expect("the editor is open");
        assert_eq!(typing.opened_under(), None, "an add row is under no key");
        assert_eq!(typing.focus(), Field::Key);

        for c in "NEW_KEY".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(
            matches!(effect, Effect::None),
            "naming a key writes nothing on its own"
        );
        let typing = typing_of(&app).expect("the editor is still open");
        assert_eq!(typing.focus(), Field::Value);
        assert_eq!(typing.key().as_str(), "NEW_KEY");
        assert_eq!(app.mode(), InputMode::Text);
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

    /// A pasted secret goes through the same box as a typed one, and nothing
    /// that prints a message or a `Debug` carries it.
    #[test]
    fn a_pasted_value_is_never_printed_in_a_notice_or_a_debug() {
        let mut app = fixtures::app_typing_a_value();
        let paste = Msg::Paste(Pasted::new("hunter2\nsecond".to_string()));

        assert_eq!(format!("{paste:?}"), "Paste(Pasted(<14 bytes>))");
        let _ = app.update(paste);

        let typing = typing_of(&app).expect("the newline did not apply the input");
        assert_eq!(typing.value().as_str(), "hunter2second");
        assert_eq!(
            format!("{typing:?}"),
            "KeyValues { add: false, key: <11 chars>, value: <13 chars>, focus: Value }"
        );
        assert_eq!(notice_of(&app), None);
    }

    /// The cursor keys edit a secret in the middle too.
    #[test]
    fn a_secret_value_can_be_corrected_at_the_cursor() {
        let mut app = fixtures::app_typing_a_value();
        for typed in "xz".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextEdit(TextEdit::Left)));
        let _ = app.update(Msg::Key(KeyPress::TextChar('y')));

        assert_eq!(typing_of(&app).unwrap().value().as_str(), "xyz");
    }

    /// `Shift-Tab` from a row's value reaches the key it is under, and a KEY
    /// that no longer names that row is a rename: one effect, both halves.
    #[test]
    fn renaming_a_key_writes_the_new_name_and_the_removal_of_the_old_one() {
        let mut app = fixtures::app_typing_a_value();
        let _ = app.update(Msg::Key(KeyPress::PrevField));
        let _ = app.update(Msg::Key(KeyPress::TextEdit(TextEdit::DeleteToStart)));
        for c in "DB_PASSPHRASE".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let _ = app.update(Msg::Key(KeyPress::NextField));
        for c in "hunter2".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        let Effect::WriteSecrets(edits, _) = effect else {
            panic!("a rename is one write carrying both halves, got {effect:?}");
        };
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].key, "DB_PASSPHRASE");
        assert_eq!(edits[0].value.as_deref(), Some("hunter2"));
        assert_eq!(edits[1].key, "DB_PASSWORD");
        assert_eq!(edits[1].value, None, "the key it was renamed out of goes");
        assert_eq!(edits[0].environment, edits[1].environment, "same tab");
        assert_eq!(app.mode(), InputMode::Normal, "and the editor closed");
    }

    /// A rename with no value would set an empty key and remove one that had a
    /// value, and no pane is ever told the value it had.
    #[test]
    fn a_rename_with_nothing_typed_in_value_is_refused() {
        let mut app = fixtures::app_typing_a_value();
        let _ = app.update(Msg::Key(KeyPress::PrevField));
        let _ = app.update(Msg::Key(KeyPress::TextEdit(TextEdit::DeleteToStart)));
        for c in "DB_PASSPHRASE".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let _ = app.update(Msg::Key(KeyPress::NextField));

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(matches!(effect, Effect::None), "nothing reached the store");
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("a rename needs a value")),
            "{}",
            notice_of(&app).unwrap_or_default()
        );
        let typing = typing_of(&app).expect("the editor stayed open");
        assert_eq!(typing.key().as_str(), "DB_PASSPHRASE");
        assert_eq!(typing.focus(), Field::Value);
    }

    /// `Shift-Tab` back to KEY and `Tab` forward again is free: the grammar was
    /// already checked on the way out of KEY the first time.
    #[test]
    fn stepping_back_to_key_and_forward_again_refuses_nothing() {
        let mut app = fixtures::app_typing_a_value();
        let _ = app.update(Msg::Key(KeyPress::PrevField));
        assert_eq!(typing_of(&app).expect("open").focus(), Field::Key);

        let effect = app.update(Msg::Key(KeyPress::NextField));

        assert!(matches!(effect, Effect::None));
        assert!(notice_of(&app).is_none());
        assert_eq!(typing_of(&app).expect("open").focus(), Field::Value);
    }
}
