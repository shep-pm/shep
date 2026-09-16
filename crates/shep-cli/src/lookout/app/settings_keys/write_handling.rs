use super::super::*;

impl App {
    /// The operator's `Enter` on the settings screen. On a free-text row with
    /// nothing pending it opens [`Pending::Typing`] and switches
    /// [`InputMode::Text`] on; on an armed candidate it sends and moves to
    /// [`Pending::Sent`]; anything else is untouched.
    ///
    /// Both acting cases go through [`Self::authorize_write`]. Opening the
    /// editor is gated as well as applying it, so the refusal arrives before a
    /// whole socket path is typed.
    pub(super) fn confirm_setting(&mut self) -> Effect {
        let Some(settings) = self.settings() else {
            return Effect::None;
        };
        let opens_editor = settings.pending.is_none()
            && matches!(
                settings.cursor(),
                Some(SettingsRow::Scalar(
                    SettingField::Socket | SettingField::MaxCronSleep
                ))
            );
        if !opens_editor && !settings.is_armed() {
            return Effect::None;
        }
        let Some(authority) = self.authorize_write() else {
            return Effect::None;
        };
        let Some(settings) = self.settings_mut() else {
            return Effect::None;
        };
        if opens_editor {
            let Some(SettingsRow::Scalar(field)) = settings.cursor() else {
                return Effect::None;
            };
            let buffer = settings.text_seed(field).to_string();
            settings.pending = Some(Pending::Typing { field, buffer });
            self.mode = InputMode::Text;
            return Effect::None;
        }
        // Minted before the borrow below, and spent on either arm that
        // sends: past the gate above, the pending edit is armed.
        let ticket = self.take_write_ticket();
        let Some(settings) = self.settings_mut() else {
            return Effect::None;
        };
        match settings.pending.take() {
            Some(Pending::Armed { edit, text, .. }) => {
                settings.pending = Some(Pending::Sent { text, ticket });
                Effect::WriteSetting {
                    edit,
                    ticket,
                    authority,
                }
            }
            Some(Pending::DogArmed { edit, text, .. }) => {
                settings.pending = Some(Pending::Sent { text, ticket });
                Effect::WriteDog {
                    edit,
                    ticket,
                    authority,
                }
            }
            other => {
                settings.pending = other;
                Effect::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::*;
    use crate::commands::settings::ScalarView;
    use crate::lookout::view::fixtures;

    #[test]
    fn the_daemon_confirm_names_both_layers_lookout_cannot_see() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("shep daemon reload"), "got: {text}");
        assert!(text.contains("SHEP_LOG_LEVEL"), "got: {text}");
        assert!(text.contains("--log-level"), "got: {text}");
    }

    #[test]
    fn the_whistle_confirm_names_a_whistle_restart_and_not_a_reload() {
        let mut app = fixtures::app_in_settings_on(SettingField::AllowControl);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("shep whistle restarted"), "got: {text}");
        assert!(
            !text.contains("daemon reload"),
            "a whistle key needs no reload: {text}"
        );
    }

    #[test]
    fn the_style_confirm_promises_nothing_beyond_the_next_command() {
        let mut app = fixtures::app_in_settings_on(SettingField::StyleLevel);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("the next command reads it"), "got: {text}");
    }

    /// `style::resolve` is flag over env over config, so with `$SHEP_STYLE` or
    /// `--style` in play the write lands and nothing changes.
    #[test]
    fn a_shadowed_style_confirm_names_the_layer_that_keeps_winning() {
        for (source, layer) in [
            (StyleSource::Env, "$SHEP_STYLE"),
            (StyleSource::Flag, "--style"),
        ] {
            let mut app = fixtures::app_in_settings_with_shadowed_style(source);
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            let text = app.settings().unwrap().pending().unwrap().text.to_string();
            assert!(text.contains(layer), "{source} must name itself: {text}");
            assert!(
                text.contains("keeps winning"),
                "{source} must say what it does: {text}"
            );
            assert!(
                !text.contains("the next command reads it"),
                "{source}: the next command reads {source}, not the file: {text}"
            );
        }
    }

    #[test]
    fn enter_sends_the_armed_edit() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(
            effect,
            Effect::WriteSetting {
                edit: SettingEdit::Set {
                    field: SettingField::LogLevel,
                    ..
                },
                ..
            }
        ));
        assert!(app.settings().unwrap().pending().unwrap().sent);
    }

    #[test]
    fn a_written_edit_updates_the_row_and_its_source() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let SettingEdit::Set {
            value: candidate, ..
        } = edit.clone()
        else {
            panic!("cycling only ever arms Set");
        };

        let effect = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Ok(()),
        });
        assert_eq!(
            effect,
            Effect::LoadSettings,
            "a landed write re-reads rather than hand-folding the row"
        );
        assert!(app.settings().unwrap().pending().is_none());

        // The re-read, which `run_ui` drives through `load_settings`.
        let mut updated = fixtures::settings_snapshot();
        updated.log_level = ScalarView {
            value: candidate,
            source: StyleSource::Config,
        };
        let _ = app.update(Msg::Settings {
            result: Ok(updated.clone()),
        });

        assert_eq!(app.settings().unwrap().snapshot(), &updated);
    }

    #[test]
    fn an_unset_write_returns_the_row_to_the_default() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        assert!(matches!(
            edit,
            SettingEdit::Unset {
                field: SettingField::MaxCronSleep
            }
        ));

        let effect = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Ok(()),
        });
        assert_eq!(effect, Effect::LoadSettings);

        let mut updated = fixtures::settings_snapshot();
        updated.max_cron_sleep = ScalarView {
            value: "30s".to_string(),
            source: StyleSource::Default,
        };
        let _ = app.update(Msg::Settings {
            result: Ok(updated.clone()),
        });

        assert_eq!(app.settings().unwrap().snapshot(), &updated);
    }

    /// A refused write reopens the text editor, and the overlay must not
    /// survive to hide it: `on_key` checks text mode ahead of
    /// `keymap_open`, so a still-open overlay would swallow every key
    /// meant for the reopened editor.
    ///
    /// The ordering in `on_key` is deliberate and documented: `h` typed into
    /// an open filter box is a letter, so the overlay can never be raised
    /// from inside text mode. It says nothing about the other direction, and
    /// `Msg::SettingWritten`'s `Err` arm restores `InputMode::Text` from a
    /// message rather than a keypress.
    ///
    /// Reachable because `is_armed` covers `Pending::Armed` and
    /// `Pending::DogArmed` and not `Pending::Sent`, so `h` with a write in
    /// flight opens the overlay instead of cancelling anything. The reply
    /// then lands, and the operator has a box on screen while every key goes
    /// into a socket path they cannot see.
    ///
    /// It is the shape a green suite cannot see: two messages in an order
    /// no single test sends.
    #[test]
    fn a_refused_write_closes_the_overlay_before_reopening_the_editor() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };

        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(
            app.keymap_open(),
            "a write in flight is not armed, so `h` must open the overlay"
        );

        let _ = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Err("refused".to_string()),
        });

        // Both halves, because `!(a && b)` holds when either is false and
        // this one would pass vacuously if the buffer restoration broke:
        // `typed_text_of` no longer matching means neither line runs and the
        // assertion is satisfied by an editor that never reopened.
        assert!(
            !app.keymap_open(),
            "the overlay is still up while text mode owns the keyboard"
        );
        assert_eq!(
            app.mode,
            InputMode::Text,
            "the editor must reopen, so the operator gets their typed text back"
        );
        assert!(
            matches!(
                app.settings().unwrap().pending,
                Some(Pending::Typing { .. })
            ),
            "the refused write's buffer is what the editor reopens with"
        );
    }

    /// Pins `Msg::Settings`'s `opening` check.
    #[test]
    fn the_cursor_survives_a_landed_writes_reload() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let before = app.settings().unwrap().cursor();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let _ = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Ok(()),
        });
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert_eq!(app.settings().unwrap().cursor(), before);
    }

    #[test]
    fn the_cursor_survives_a_refresh() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let before = app.settings().unwrap().cursor();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::LoadSettings
        );
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert_eq!(app.settings().unwrap().cursor(), before);
    }

    #[test]
    fn a_refused_write_says_why_and_leaves_the_row_alone() {
        let mut app = fixtures::app_in_settings_with_control();
        let before = app.settings().unwrap().snapshot().log_level.clone();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let _ = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Err("max_cron_sleep is 500ms, below the 1s floor".into()),
        });

        assert_eq!(app.settings().unwrap().snapshot().log_level, before);
        let notice = app.notice().unwrap();
        assert!(notice.is_grave());
        assert!(notice.to_string().contains("below the 1s floor"));
    }

    /// Two writes can be in flight at once: `Pending::Sent` eats no key, so
    /// `space` arms a second edit over the first and `Enter` sends it. The
    /// first write's answer must not resolve the second, which is the edit
    /// the screen is actually showing a prompt for.
    #[test]
    fn a_superseded_reply_does_not_resolve_the_edit_that_replaced_it() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting {
            edit: first,
            ticket: first_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the first edit");
        };

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting {
            edit: second,
            ticket: second_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the second edit too");
        };
        assert_ne!(first_ticket, second_ticket, "two writes are two tickets");

        let effect = app.update(Msg::SettingWritten {
            edit: first,
            ticket: first_ticket,
            result: Ok(()),
        });
        assert_eq!(
            effect,
            Effect::None,
            "a re-read here rebuilds the screen and throws the live edit away"
        );
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the second edit is still in flight and still says so"
        );

        let effect = app.update(Msg::SettingWritten {
            edit: second,
            ticket: second_ticket,
            result: Ok(()),
        });
        assert_eq!(effect, Effect::LoadSettings, "its own reply re-reads");
        assert!(app.settings().unwrap().pending().is_none());
    }

    /// The worst of the two: a refusal reopens the editor for a free-text
    /// field, so a superseded one used to replace a live `Pending::Sent`
    /// with a text editor. The live write's own answer then cleared it and
    /// left `InputMode::Text` behind with nothing to type into, which is
    /// the state `a_refused_settings_write_landing_over_a_dog_pane_does_not_arm_text_mode`
    /// exists to keep out by the other door.
    #[test]
    fn a_superseded_refusal_does_not_reopen_the_editor_over_a_live_edit() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting {
            edit: socket,
            ticket: socket_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the socket edit");
        };

        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting {
            edit: level,
            ticket: level_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the log level edit too");
        };

        let _ = app.update(Msg::SettingWritten {
            edit: socket,
            ticket: socket_ticket,
            result: Err("the socket path is too long".into()),
        });
        assert_eq!(
            app.mode(),
            InputMode::Normal,
            "no editor was opened, so no text mode is owed one"
        );
        assert!(app.settings().unwrap().typing().is_none());
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the log level edit is still in flight"
        );
        assert!(
            app.notice().unwrap().to_string().contains("too long"),
            "the refusal is still reported"
        );

        let _ = app.update(Msg::SettingWritten {
            edit: level,
            ticket: level_ticket,
            result: Ok(()),
        });
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.settings().unwrap().pending().is_none());
    }

    /// A dog's ticket has to survive two hops: the file half answers as
    /// `Msg::DogWritten`, which raises `Sent::Dog`, and only the shepherd's
    /// answer to that clears the prompt. An edit armed in between owns the
    /// prompt by then.
    #[test]
    fn a_superseded_dog_reply_does_not_resolve_the_edit_that_replaced_it() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half");
        };
        let Effect::Send(dog) = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Ok(DogSource::BuiltIn),
        }) else {
            panic!("a landed file half must raise the daemon half");
        };

        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { ticket: scalar, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the scalar edit");
        };
        assert_ne!(ticket, scalar, "the toggle and the scalar are two writes");

        let info = ProcessInfo::builder(50, "metrics", ProcStatus::Online)
            .pid(Some(50_000))
            .dog(Some(DogSource::BuiltIn))
            .build();
        let effect = app.update(Msg::Replied {
            sent: dog,
            result: Ok(Response::DogStarted(info)),
        });
        assert_eq!(effect, Effect::None, "the live edit survives a re-read");
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the scalar edit is still in flight and still says so"
        );
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("enable metrics: the shepherd started it"),
            "the toggle still reports what the shepherd did"
        );
    }

    /// The door a refuse-to-arm guard could not close: `Escape` leaves the
    /// screen without cancelling the write, and reopening it builds a fresh
    /// `Settings` with nothing pending. The abandoned write's answer must
    /// still not touch whatever the reopened screen has armed since.
    #[test]
    fn a_reply_for_a_write_the_screen_walked_away_from_touches_nothing() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting {
            edit: socket,
            ticket: socket_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the socket edit");
        };

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.settings().is_none(), "Escape leaves the screen");
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { ticket: level, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the reopened screen's edit");
        };
        assert_ne!(socket_ticket, level, "a reopened screen mints its own");

        let _ = app.update(Msg::SettingWritten {
            edit: socket,
            ticket: socket_ticket,
            result: Err("the socket path is too long".into()),
        });
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.settings().unwrap().typing().is_none());
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the reopened screen's own edit is untouched"
        );
    }
}
