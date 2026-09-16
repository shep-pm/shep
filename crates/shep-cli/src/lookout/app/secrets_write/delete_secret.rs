use super::super::*;

impl App {
    /// `D`: arms the removal of the selected key's value in the current
    /// tab's environment. Refuses a provider row and a read-only lookout,
    /// mirroring [`Self::secrets_confirm`]'s own two checks, and refuses
    /// silently on the `+ new key` affordance and on a selection folded out
    /// of view: neither names a real, visible key to delete, the same gate
    /// [`Self::reveal_selected`] applies before a read.
    ///
    /// A row taking its value from the `all` slot refuses too
    /// ([`ALL_SLOT_REFUSAL`]), before it arms rather than after the write.
    pub(in crate::lookout::app) fn arm_secret_delete(&mut self) -> Effect {
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
    pub(in crate::lookout::app) fn confirm_secret_delete(&mut self) -> Effect {
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
    pub(in crate::lookout::app) fn disarm_secret_delete(&mut self) -> bool {
        self.secrets_pane_mut()
            .is_some_and(|pane| pane.armed.take().is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::*;

    use super::super::testing::*;
    use crate::lookout::view::fixtures;

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
}
