//! Writing, deleting, revealing and copying a secret.

use super::*;

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
    pub(super) fn secrets_confirm(&mut self) -> Effect {
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

    /// `D`: arms the removal of the selected key's value in the current
    /// tab's environment. Refuses a provider row and a read-only lookout,
    /// mirroring [`Self::secrets_confirm`]'s own two checks, and refuses
    /// silently on the `+ new key` affordance and on a selection folded out
    /// of view: neither names a real, visible key to delete, the same gate
    /// [`Self::reveal_selected`] applies before a read.
    ///
    /// A row taking its value from the `all` slot refuses too
    /// ([`ALL_SLOT_REFUSAL`]), before it arms rather than after the write.
    pub(super) fn arm_secret_delete(&mut self) -> Effect {
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        if !pane.visible_row_indices().contains(&pane.selected) {
            return Effect::None;
        }
        let Some(row) = pane.model.rows.get(pane.selected).cloned() else {
            return Effect::None;
        };
        let environment = pane.environment().map(str::to_string);
        if matches!(row.source, Source::Namespace(_)) {
            self.notice = Some(Notice {
                text: PROVIDER_ROW_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        if environment.is_some_and(|tab| deletes_the_all_slot(&row, &tab)) {
            self.notice = Some(Notice {
                text: ALL_SLOT_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let now = self.now;
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        pane.armed = Some(ArmedDelete {
            key: row.key,
            at: now,
        });
        Effect::None
    }

    /// `Enter` on the secrets pane while [`SecretsPane::armed`] holds a key:
    /// sends the delete and disarms.
    pub(super) fn confirm_secret_delete(&mut self) -> Effect {
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        let Some(ArmedDelete { key, .. }) = pane.armed.take() else {
            return Effect::None;
        };
        let Some(environment) = pane.environment().map(str::to_string) else {
            return Effect::None;
        };
        // The same refusal the arm already made, taken again on the last
        // step before an unrecoverable write. No keypress reaches here with
        // an `all` row armed today, since a tab move and a reload both
        // disarm, so this is depth rather than a live path.
        let refuses = pane
            .model
            .rows
            .iter()
            .find(|row| row.key == key)
            .is_some_and(|row| deletes_the_all_slot(row, &environment));
        if refuses {
            self.notice = Some(Notice {
                text: ALL_SLOT_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(authority) = self.authorize_write() else {
            return Effect::None;
        };
        Effect::WriteSecret(
            SecretEdit {
                key,
                environment,
                value: None,
            },
            authority,
        )
    }

    /// Clears an armed delete, and says whether one was there.
    ///
    /// Called once per keypress, from [`Self::on_secrets_key`]'s own head,
    /// so every key but the confirm and the quit cancels. Its answer is
    /// `Escape`'s cue not to also close the pane on the same press.
    pub(super) fn disarm_secret_delete(&mut self) -> bool {
        self.secrets_pane_mut()
            .is_some_and(|pane| pane.armed.take().is_some())
    }

    /// The secrets pane's own text keymap, in force while
    /// [`SecretsPane::typing`] owns [`InputMode::Text`].
    pub(super) fn on_secrets_text_key(&mut self, key: KeyPress) -> Effect {
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

    /// Takes any revealed value off the screen, on any screen: every
    /// trigger calls this rather than reaching for [`SecretsPane::hide`]
    /// through a pane it first has to find.
    pub(super) fn hide_revealed(&mut self) {
        if let Some(pane) = self.secrets_pane_mut() {
            pane.hide();
        }
    }

    /// Whether `[secrets] allow_read` lets this pane show a value.
    ///
    /// Read off the model the last [`Effect::LoadSecrets`] built, so the
    /// answer is the one `shep.toml` gave when the rows were gathered and
    /// the pane never opens that file itself. Fails closed everywhere it
    /// cannot be answered: a missing key, an unreadable file
    /// (`crate::lookout::secrets::model`) and no open pane all read as `false`.
    pub(crate) fn reveal_gate_open(&self) -> bool {
        matches!(&self.body, Body::Secrets(pane) if pane.model.allow_read)
    }

    /// `v`'s answer: a read of the selected row's stored value, or a refusal
    /// naming the gate and the file.
    ///
    /// The value is not on screen when this returns. [`Self::on_revealed`]
    /// puts it there once the read lands.
    ///
    /// A visibility check on `pane.selected` here, because `move_by` cannot
    /// keep it inside the visible set when that set is empty: every group
    /// folded away and no operator row standing leaves `selected` naming a
    /// hidden row, and nothing else writes it back. Refused silently,
    /// the same answer every other `v` press against a pane that is not
    /// open gives.
    pub(super) fn reveal_selected(&mut self) -> Effect {
        if !self.reveal_gate_open() {
            self.notice = Some(Notice {
                text: crate::commands::secret::HOW_TO_ALLOW_READ.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        if !pane.visible_row_indices().contains(&pane.selected) {
            return Effect::None;
        }
        let (Some(row), Some(environment)) = (
            pane.model.rows.get(pane.selected).cloned(),
            pane.environment().map(str::to_string),
        ) else {
            return Effect::None;
        };
        // Through `hide`, so a value already on screen goes now rather than
        // sitting there under a read that answers for another key.
        pane.hide();
        pane.pending_reveal = Some(row.key.clone());
        Effect::RevealSecret {
            store: pane.model.store.clone(),
            provider_cache: pane.model.provider_cache.clone(),
            row,
            environment,
        }
    }

    /// `y`'s answer: the already-revealed value, on its way to
    /// [`Effect::CopyToClipboard`], or the `allow_read` refusal.
    ///
    /// A reveal by another route, so it takes [`Self::reveal_gate_open`]'s
    /// own gate rather than a second one, and it copies what
    /// [`SecretsPane::reveal`] already holds on screen rather than reading
    /// the store afresh: a fresh read would let `y` show a value the
    /// operator never asked [`KeyPress::Reveal`] to put on screen, past the
    /// same gate a reveal takes.
    ///
    /// Silent, not the `allow_read` refusal, when the gate is open but
    /// nothing is revealed: the gate is not what is missing there, the same
    /// silence [`Self::reveal_selected`] falls back to for an unrelated
    /// selection.
    pub(super) fn copy_revealed(&mut self) -> Effect {
        if !self.reveal_gate_open() {
            self.notice = Some(Notice {
                text: crate::commands::secret::HOW_TO_ALLOW_READ.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(value) = self
            .secrets_pane_mut()
            .and_then(|pane| pane.reveal.as_ref())
            .map(|reveal| reveal.value.clone())
        else {
            return Effect::None;
        };
        self.notice = Some(Notice {
            text: COPY_SENT_NOTICE.to_string(),
            grave: false,
        });
        Effect::CopyToClipboard(ClipboardValue(value))
    }

    /// An [`Effect::RevealSecret`] has landed.
    ///
    /// Drawn only when the reveal is still the one that was asked for: the
    /// gate can have shut under a fresh model, the tab can have moved, and
    /// every clear trigger drops the pending key. A value that reached the
    /// screen past any of those would be a value nobody asked for, which
    /// for a shut gate is the failure the gate exists to stop.
    pub(super) fn on_revealed(
        &mut self,
        key: &str,
        environment: &str,
        value: Option<RevealedValue>,
    ) {
        let gate_open = self.reveal_gate_open();
        let until = self.now + REVEAL_HOLDS;
        let Some(pane) = self.secrets_pane_mut() else {
            return;
        };
        if pane.pending_reveal.as_deref() != Some(key) || pane.environment() != Some(environment) {
            return;
        }
        pane.pending_reveal = None;
        let Some(RevealedValue(value)) = value.filter(|_| gate_open) else {
            return;
        };
        pane.reveal = Some(Reveal {
            key: key.to_string(),
            value,
            until,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::secrets::SecretRow;
    use crate::lookout::secrets::Source;
    use crate::lookout::view::fixtures;

    /// The value on screen, or `None`. Reads the pane rather than the
    /// rendered frame: these tests are about when a value is held, and the
    /// drawing of it has its own tests in `view::secrets`.
    fn reveal_of(app: &App) -> Option<&Reveal> {
        match app.body() {
            Body::Secrets(pane) => pane.reveal.as_ref(),
            _ => None,
        }
    }

    /// The pane's own open input, or `None`.
    fn typing_of(app: &App) -> Option<&Typing> {
        match app.body() {
            Body::Secrets(pane) => pane.typing.as_ref(),
            _ => None,
        }
    }

    /// The status bar's current line, rendered, or `None`.
    fn notice_of(app: &App) -> Option<String> {
        app.notice().map(ToString::to_string)
    }

    /// The key an armed delete names, or `None`.
    fn armed_of(app: &App) -> Option<String> {
        match app.body() {
            Body::Secrets(pane) => pane.armed.as_ref().map(|a| a.key.clone()),
            _ => None,
        }
    }

    /// Walks [`fixtures::app_with_secrets`]'s cursor onto `SET_EVERYWHERE`,
    /// whose only slot is `all` while the tab names `production`.
    ///
    /// # Panics
    /// If the cursor did not land on a row taking its value from `all`.
    #[track_caller]
    fn select_the_all_slot_row(app: &mut App) {
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is not open");
        };
        let row = &pane.model.rows[pane.selected];
        assert_eq!(row.key, "SET_EVERYWHERE");
        assert_eq!(row.in_force.as_deref(), Some("all"));
        assert_eq!(pane.environment(), Some("production"));
    }

    /// How many rows the table currently holds, for the test proving a
    /// failed write redraws nothing.
    fn row_count(app: &App) -> usize {
        match app.body() {
            Body::Secrets(pane) => pane.model.rows.len(),
            _ => 0,
        }
    }

    /// The environment the pane's own tab currently names.
    ///
    /// # Panics
    /// If the pane is not open.
    #[track_caller]
    fn second_tab_of(app: &App) -> String {
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is not open");
        };
        pane.model.environments[pane.tab].clone()
    }

    /// `G`: the pane's own cursor scheme already lands on the trailing
    /// `+ new key` row, so this is `SelectLast` rather than a second way to
    /// reach it.
    fn select_new_key_row(app: &mut App) {
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
    }

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

    #[test]
    fn enter_sets_when_nothing_is_armed_and_confirms_when_something_is() {
        let mut idle = fixtures::app_with_secrets_and_control();

        let _ = idle.update(Msg::Key(KeyPress::Confirm));

        assert!(
            typing_of(&idle).is_some(),
            "unarmed Enter opens the value input"
        );

        let mut armed = fixtures::app_with_secrets_and_control();
        let _ = armed.update(Msg::Key(KeyPress::SecretDelete));

        let effect = armed.update(Msg::Key(KeyPress::Confirm));

        assert!(
            typing_of(&armed).is_none(),
            "armed Enter must not also open the input"
        );
        let Effect::WriteSecret(edit, _) = effect else {
            panic!("expected the delete, got {effect:?}");
        };
        assert_eq!(edit.key, "DB_PASSWORD");
        assert_eq!(edit.value, None, "None is what removes the slot");
    }

    #[test]
    fn escape_disarms_before_it_closes_the_pane() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        let _ = app.update(Msg::Key(KeyPress::Escape));

        assert!(
            matches!(app.body(), Body::Secrets(_)),
            "the pane stays open"
        );
        assert!(armed_of(&app).is_none(), "and the arm is gone");

        let _ = app.update(Msg::Key(KeyPress::Escape));

        assert!(
            matches!(app.body(), Body::FlockTable),
            "a second Escape closes it"
        );
    }

    #[test]
    fn moving_the_selection_disarms() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        let _ = app.update(Msg::Key(KeyPress::SelectDown));

        assert!(
            armed_of(&app).is_none(),
            "an arm must not follow the cursor onto another key"
        );
    }

    /// The armed prompt says "enter confirms, any other key cancels", so
    /// every key but the confirm and the quit has to cancel. `v`, `y` and
    /// `z` are the three that used to leave the arm standing under the
    /// sentence promising they would not.
    #[test]
    fn any_key_but_the_confirm_and_the_quit_disarms() {
        for key in [
            KeyPress::Reveal,
            KeyPress::Copy,
            KeyPress::Collapse,
            KeyPress::Refresh,
            KeyPress::Help,
            KeyPress::Settings,
            KeyPress::Bleats,
        ] {
            let mut app = fixtures::app_armed_to_delete_a_secret();

            let _ = app.update(Msg::Key(key));

            assert!(
                armed_of(&app).is_none(),
                "{key:?} left the delete armed while the bar promised it cancelled"
            );
        }
    }

    #[test]
    fn quit_still_quits_while_a_delete_is_armed() {
        let mut app = fixtures::app_armed_to_delete_a_secret();

        let effect = app.update(Msg::Key(KeyPress::Quit));

        assert!(matches!(effect, Effect::Quit));
    }

    #[test]
    fn moving_the_tab_disarms() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        let _ = app.update(Msg::Key(KeyPress::TabNext));

        assert!(
            armed_of(&app).is_none(),
            "an arm must not follow a tab move onto another environment"
        );
    }

    #[test]
    fn a_reload_disarms() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        assert!(armed_of(&app).is_some(), "the delete armed");

        let _ = app.update(Msg::Secrets {
            environment: "all".to_string(),
            result: Ok(Box::default()),
        });

        assert!(
            armed_of(&app).is_none(),
            "a fresh read describes the store as it is now, not the arm"
        );
    }

    /// `Help`'s own arm here never mentions `armed`: `was_armed`, computed
    /// before the match for every key but `Confirm` and `Quit`, already
    /// disarms it.
    #[test]
    fn h_disarms_a_secret_delete_before_opening_the_overlay() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        assert!(armed_of(&app).is_some(), "the delete armed");

        let _ = app.update(Msg::Key(KeyPress::Help));

        assert!(
            armed_of(&app).is_none(),
            "h did not disarm the pending delete"
        );
        assert!(app.keymap_open(), "h did not open the overlay");
    }

    /// An armed delete is the fourth armed thing in this module the tick
    /// expires, mirroring the config pane's own `armed_at` at
    /// `app.rs:1999-2005`.
    #[test]
    fn an_armed_delete_expires_like_every_other_armed_thing() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        assert!(armed_of(&app).is_some(), "the delete armed");

        let later = Instant::now() + CONFIRM_EXPIRY;
        let _ = app.update(Msg::Tick { now: later });

        assert!(armed_of(&app).is_none(), "it did not expire");
    }

    #[test]
    fn an_armed_delete_survives_a_tick_just_before_the_deadline() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        assert!(armed_of(&app).is_some(), "the delete armed");

        let almost = Instant::now() + CONFIRM_EXPIRY - Duration::from_secs(1);
        let _ = app.update(Msg::Tick { now: almost });

        assert!(armed_of(&app).is_some(), "not yet ten seconds");
    }

    #[test]
    fn a_delete_refuses_without_the_control_gate() {
        let mut app = fixtures::app_with_secrets_read_only();

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(armed_of(&app).is_none());
        assert!(notice_of(&app).is_some_and(|n| n.contains("read-only")));
    }

    #[test]
    fn a_provider_row_refuses_a_delete() {
        let mut app = fixtures::app_with_a_pushed_secret_selected_and_control();

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(armed_of(&app).is_none());
        assert!(notice_of(&app).is_some_and(|n| n.contains("pushed by a dog")));
    }

    /// The row's value comes from the `all` slot, so an unset against the
    /// tab's own environment would remove nothing and report success, and an
    /// unset against `all` would change every environment at once.
    #[test]
    fn a_value_that_comes_from_the_all_slot_refuses_a_delete_on_a_named_tab() {
        let mut app = fixtures::app_with_secrets_on_a_named_tab_and_control();
        select_the_all_slot_row(&mut app);

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(armed_of(&app).is_none(), "it must refuse before it arms");
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("`all` slot")
                && n.contains("every environment")
                && n.contains("`all` tab")),
            "the notice says where the value lives, what removing it costs, \
             and how to do it deliberately"
        );
    }

    #[test]
    fn the_all_tab_still_deletes_an_all_slot() {
        let mut app = fixtures::app_with_secrets_and_control();

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        let effect = app.update(Msg::Key(KeyPress::Confirm));

        let Effect::WriteSecret(edit, _) = effect else {
            panic!("expected a write, got {effect:?}");
        };
        assert_eq!(edit.key, "DB_PASSWORD");
        assert_eq!(edit.environment, "all");
        assert!(edit.value.is_none(), "a delete sends no value");
    }

    #[test]
    fn an_unset_that_removed_nothing_says_so_rather_than_reporting_success() {
        let mut app = fixtures::app_with_secrets_and_control();

        let effect = app.update(Msg::SecretWritten { result: Ok(false) });

        assert!(
            matches!(effect, Effect::None),
            "nothing changed, so nothing is re-read"
        );
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("nothing to remove")),
            "a delete that removed nothing must not read as a delete that worked"
        );
    }

    #[test]
    fn a_write_that_changed_the_store_re_reads_it() {
        let mut app = fixtures::app_with_secrets_and_control();

        let effect = app.update(Msg::SecretWritten { result: Ok(true) });

        assert!(matches!(effect, Effect::LoadSecrets));
        assert!(notice_of(&app).is_none(), "and says nothing about it");
    }

    #[test]
    fn d_does_not_arm_the_new_key_row() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert!(
            matches!(app.body(), Body::Secrets(pane) if pane.selected_is_new_key_row()),
            "sanity: `G` landed on the affordance"
        );

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(
            armed_of(&app).is_none(),
            "there is no value there to delete"
        );
    }

    /// The gate is why the round trip needs a guard at all: a value drawn
    /// after `allow_read` went false is the failure the gate exists to
    /// stop, and nothing hides a reveal when a fresh model arrives.
    #[test]
    fn a_reveal_that_lands_after_the_gate_closed_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let answer = fixtures::ask_to_reveal(&mut app);
        let mut shut = fixtures::secrets_model(dir.path(), false);
        shut.environments = vec!["all".to_string(), "production".to_string()];
        let _ = app.update(Msg::Secrets {
            environment: "production".to_string(),
            result: Ok(Box::new(shut)),
        });

        let _ = app.update(answer);

        assert!(reveal_of(&app).is_none(), "the gate shut while it was read");
    }

    /// A selection move or a tab move, each hiding through
    /// [`SecretsPane::hide`]: the pane is still on screen, still pending
    /// nothing, and a late answer has to find that out rather than land on
    /// a row or a tab the operator has moved past.
    #[test]
    fn a_reveal_that_lands_after_its_reason_went_away_shows_nothing() {
        for (name, press) in [
            ("selection", KeyPress::SelectDown),
            ("tab", KeyPress::TabNext),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
            let answer = fixtures::ask_to_reveal(&mut app);
            let _ = app.update(Msg::Key(press));

            let _ = app.update(answer);

            assert!(
                reveal_of(&app).is_none(),
                "{name} moved on and the answer put the value back"
            );
        }
    }

    /// `close` and `escape` do not leave `SecretsPane` in place the way a
    /// selection or a tab move does: they replace `self.body` with
    /// `Body::FlockTable` outright, so a late answer landing there has
    /// nowhere to write and would show nothing whether or not the guard
    /// works. Reopening the pane before delivering it puts a real
    /// `SecretsPane` back on screen, one with no pending reveal of its
    /// own, so the guard actually has something to refuse.
    #[test]
    fn a_reveal_that_lands_after_the_pane_closed_and_reopened_shows_nothing() {
        for (name, press) in [("close", KeyPress::Secrets), ("escape", KeyPress::Escape)] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
            let answer = fixtures::ask_to_reveal(&mut app);
            let _ = app.update(Msg::Key(press));

            let _ = app.update(Msg::Key(KeyPress::Secrets));
            let _ = app.update(Msg::Secrets {
                environment: "production".to_string(),
                result: Ok(Box::new(fixtures::secrets_model(dir.path(), true))),
            });
            let _ = app.update(answer);

            assert!(
                reveal_of(&app).is_none(),
                "{name} reopened a pane the stale answer names no pending read for"
            );
        }
    }

    /// A tab change reloads, so the answer can arrive against a pane whose
    /// rows are a different environment's: the echo, not the key alone,
    /// says whether it is still the answer that was asked for.
    #[test]
    fn a_reveal_that_lands_for_another_environment_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let Msg::Revealed { key, value, .. } = fixtures::ask_to_reveal(&mut app) else {
            panic!("a reveal answers with a value");
        };

        let _ = app.update(Msg::Revealed {
            key,
            environment: "ci".to_string(),
            value,
        });

        assert!(reveal_of(&app).is_none(), "that is another tab's value");
    }

    #[test]
    fn v_reveals_only_when_allow_read_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut shut = fixtures::app_with_secrets_and_reads(dir.path(), false);

        let effect = shut.update(Msg::Key(KeyPress::Reveal));

        assert_eq!(effect, Effect::None, "a shut gate does not read the store");
        assert!(reveal_of(&shut).is_none(), "the gate is shut");
        assert!(
            shut.notice()
                .is_some_and(|notice| notice.to_string().contains("allow_read")),
            "and it says which gate and where"
        );

        let mut open = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let answer = fixtures::ask_to_reveal(&mut open);

        let _ = open.update(answer);

        assert_eq!(
            reveal_of(&open).map(|reveal| reveal.key.as_str()),
            Some("DB_PASSWORD")
        );
        assert_eq!(
            reveal_of(&open).map(|reveal| reveal.value.as_str()),
            Some(fixtures::REVEALED_VALUE),
            "the value comes off the store, not out of the model"
        );
    }

    #[test]
    fn copying_says_it_was_sent_rather_than_that_it_arrived() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());

        let _ = app.update(Msg::Key(KeyPress::Copy));

        let notice = notice_of(&app).expect("a notice");
        assert!(notice.contains("sent to the terminal"), "got {notice:?}");
        assert!(
            !notice.contains("copied"),
            "OSC 52 is write-only and many terminals refuse it, so claiming \
             success is a claim nothing can check: {notice:?}"
        );
    }

    #[test]
    fn copy_needs_a_revealed_value_rather_than_reading_the_store_behind_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), false);

        let _ = app.update(Msg::Key(KeyPress::Copy));

        assert!(
            notice_of(&app).is_some_and(|n| n.contains("allow_read")),
            "copy is a reveal by another route and takes the same gate"
        );
    }

    #[test]
    fn copy_carries_the_revealed_value_to_the_effect() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());

        let Effect::CopyToClipboard(value) = app.update(Msg::Key(KeyPress::Copy)) else {
            panic!("an open gate over a revealed value copies it");
        };

        assert_eq!(value.0, fixtures::REVEALED_VALUE);
    }

    #[test]
    fn a_reveal_clears_on_every_one_of_its_triggers_that_exists_yet() {
        for (name, press) in [
            ("k", KeyPress::SelectUp),
            ("j", KeyPress::SelectDown),
            ("g", KeyPress::SelectFirst),
            ("G", KeyPress::SelectLast),
            ("shift-tab", KeyPress::TabPrev),
            ("tab", KeyPress::TabNext),
            ("escape", KeyPress::Escape),
            ("close", KeyPress::Secrets),
            ("refresh", KeyPress::Refresh),
            ("quit", KeyPress::Quit),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_revealing(dir.path());

            let _ = app.update(Msg::Key(press));

            assert!(reveal_of(&app).is_none(), "{name} left the value on screen");
        }

        let dir = tempfile::tempdir().unwrap();
        let mut timed = fixtures::app_revealing(dir.path());
        let start = timed.now();

        let _ = timed.update(Msg::Tick {
            now: start + REVEAL_HOLDS,
        });

        assert!(
            reveal_of(&timed).is_none(),
            "the tenth second is the last one, so the value is gone by it"
        );
    }

    /// Stops a clear-on-every-tick implementation passing the test above for
    /// the wrong reason.
    #[test]
    fn a_reveal_survives_the_tick_before_it_expires() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();

        let _ = app.update(Msg::Tick {
            now: start + REVEAL_HOLDS - Duration::from_millis(1),
        });

        assert!(
            reveal_of(&app).is_some(),
            "clearing early makes the countdown a lie"
        );
    }

    /// The expiry rides the tick's own clock, not `self.now`, which stops
    /// advancing on a dead link. A value that outlived a link failure would
    /// sit on screen until the operator pressed something.
    #[test]
    fn a_frozen_link_does_not_hold_a_value_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        let _ = app.update(Msg::Tick {
            now: start + REVEAL_HOLDS,
        });

        assert!(reveal_of(&app).is_none());
    }

    #[test]
    fn a_key_with_no_value_in_this_tab_reveals_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("secrets.json");
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        // The same key, set only in an environment this tab is not showing:
        // `secrets::get` would find a value under `ci` and must not be asked
        // for one.
        let _ = app.update(Msg::Secrets {
            environment: "production".to_string(),
            result: Ok(Box::new(SecretsModel {
                environments: vec!["all".to_string(), "production".to_string()],
                rows: vec![SecretRow {
                    key: "DB_PASSWORD".to_string(),
                    source: Source::Operator,
                    in_force: None,
                    set_in: vec!["ci".to_string()],
                    byte_len: None,
                    readers: Vec::new(),
                }],
                allow_read: true,
                store,
                ..SecretsModel::default()
            })),
        });

        let answer = fixtures::ask_to_reveal(&mut app);
        let _ = app.update(answer);

        assert!(
            reveal_of(&app).is_none(),
            "nothing resolves here, so there is nothing to show"
        );
    }
}
