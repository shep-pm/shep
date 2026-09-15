//! The settings screen's keyboard: the cursor, the cycle, the armed candidate
//! and the write.

use super::*;

impl App {
    /// What a landed settings write answers with: re-read `shep.toml`, unless
    /// the screen has an edit of its own in flight.
    ///
    /// [`Msg::Settings`] rebuilds the whole screen, so a re-read raised by a
    /// reply the screen was no longer waiting on would throw away the edit it
    /// is waiting on instead. That edit's own reply re-reads a moment later,
    /// and carries both writes' work with it.
    pub(super) fn reread_settings(&self) -> Effect {
        match self.settings() {
            Some(settings) if settings.pending.is_some() => Effect::None,
            _ => Effect::LoadSettings,
        }
    }

    /// Cancels an armed settings candidate, answering whether it did.
    ///
    /// Five arms of the settings handler spend a key this way rather than
    /// acting: `Settings`/`Escape`, `Refresh`, `Edit`, `Help`, and the four
    /// `Select*` arms together. A change to what cancelling means, a
    /// confirmation, a different field cleared, has to land here or the
    /// handler starts disagreeing with itself about what an armed prompt
    /// eats.
    ///
    /// A `bool` rather than an `Effect`, so each caller keeps its own reason
    /// for returning: the arms are identical in what they cancel and
    /// different in what they would otherwise have done.
    fn disarm_settings_candidate(&mut self) -> bool {
        if let Some(settings) = self.settings_mut()
            && settings.is_armed()
        {
            settings.pending = None;
            return true;
        }
        false
    }

    /// The settings screen's own keymap, in force while [`Self::settings`] is
    /// `Some`. Everything not named here is ignored, an action key included.
    pub(super) fn on_settings_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
        match key {
            KeyPress::Quit => return Effect::Quit,
            // Both close, but an armed confirm eats the first one, the
            // cancel-before-act rule the dashboard follows. `Escape` closing
            // rather than quitting is where this screen swaps that cascade.
            KeyPress::Settings | KeyPress::Escape => {
                if !self.disarm_settings_candidate() {
                    self.body = Body::FlockTable;
                }
            }
            // An armed candidate eats the first movement key rather than also
            // moving: the next reflexive Enter would otherwise apply an edit to
            // a row the operator lost track of. `Sent` is untouched.
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if !self.disarm_settings_candidate()
                    && let Some(settings) = self.settings_mut()
                {
                    match key {
                        KeyPress::SelectUp => settings.move_by(-1),
                        KeyPress::SelectDown => settings.move_by(1),
                        KeyPress::SelectFirst => settings.move_to_first(),
                        KeyPress::SelectLast => settings.move_to_last(),
                        _ => unreachable!(),
                    }
                }
            }
            KeyPress::Cycle => return self.cycle_setting(),
            KeyPress::Confirm => return self.confirm_setting(),
            // Re-reads `shep.toml`, so another process's write shows up, and
            // the cursor survives. An armed candidate eats this key too.
            KeyPress::Refresh => {
                if self.disarm_settings_candidate() {
                    return Effect::None;
                }
                return Effect::LoadSettings;
            }
            // The probe, not the open, same reasoning as `on_key`'s `e`:
            // the pane shows the dog's real schema and section or nothing.
            // An armed candidate eats it first, like every other key here.
            KeyPress::Edit => {
                if self.disarm_settings_candidate() {
                    return Effect::None;
                }
                return self.probe_dog_schema();
            }
            // An armed candidate eats this too, on the same terms as
            // `Refresh` and `Edit` above: a prompt left standing behind the
            // overlay is a value the operator cannot see to confirm or
            // cancel, so `h` cancels it and is consumed rather than also
            // opening the overlay.
            KeyPress::Help => {
                if self.disarm_settings_candidate() {
                    return Effect::None;
                }
                return self.open_keymap();
            }
            // Unreachable from here, named so a new variant cannot fall
            // silently into an arm that ignores it.
            KeyPress::Action(_)
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse => {}
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats
            // `NextGroup`/`Group`/`Undo`/`Continue` belong to the config
            // pane: no other screen has groups to walk, a filed edit set
            // to undo, or a close dialog to answer.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo
            | KeyPress::Continue => {}
        }
        Effect::None
    }

    /// `space` on the settings screen: arms a candidate for the cursor's row,
    /// or refuses through [`Self::authorize_write`].
    fn cycle_setting(&mut self) -> Effect {
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(cursor) = self.settings().and_then(Settings::cursor) else {
            return Effect::None;
        };
        match cursor {
            SettingsRow::Scalar(field) => self.cycle_scalar(field),
            SettingsRow::Dog(index) => self.cycle_dog(index),
        }
    }

    /// `space` on one of the six scalar rows. Re-arms when a candidate is
    /// already armed, so a second `space` walks one step further along the
    /// cycle. Does nothing on the two free-text fields.
    ///
    /// Replaces a [`Pending::Sent`] outright rather than refusing over it:
    /// the write it names is local file I/O the operator need not wait on,
    /// and its answer can no longer reach the edit armed here. See
    /// [`Settings::pending`].
    fn cycle_scalar(&mut self, field: SettingField) -> Effect {
        // See the comment in `Self::take_pane_writes`: a direct field
        // match, not `Self::settings_mut`, so `self.now` stays reachable
        // below.
        let Some(settings) = (match &mut self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        let Some(value) = settings.next_candidate(field) else {
            return Effect::None;
        };
        let source = settings.source_of(field);
        let text = confirm_text(field, &value, source);
        settings.pending = Some(Pending::Armed {
            edit: SettingEdit::Set { field, value },
            text,
            at: self.now,
        });
        Effect::None
    }

    /// The operator's `Enter` on the settings screen. On a free-text row with
    /// nothing pending it opens [`Pending::Typing`] and switches
    /// [`InputMode::Text`] on; on an armed candidate it sends and moves to
    /// [`Pending::Sent`]; anything else is untouched.
    ///
    /// Both acting cases go through [`Self::authorize_write`]. Opening the
    /// editor is gated as well as applying it, so the refusal arrives before a
    /// whole socket path is typed.
    fn confirm_setting(&mut self) -> Effect {
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

    /// The settings editor's own text keymap, in force while a
    /// [`Pending::Typing`] owns [`InputMode::Text`]. The buffer is never
    /// trimmed.
    ///
    /// `TextApply` arms rather than writes: an empty buffer becomes
    /// [`SettingEdit::Unset`], anything else [`SettingEdit::Set`], and the next
    /// `Enter` sends it. `TextAbandon` leaves the screen open.
    pub(super) fn on_settings_text_key(&mut self, key: KeyPress) -> Effect {
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
    use crate::commands::settings::ScalarView;
    use crate::lookout::view::fixtures;

    #[test]
    fn s_asks_for_the_file_before_the_screen_opens() {
        let mut app = fixtures::full_app();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Settings)),
            Effect::LoadSettings
        );
        assert!(
            app.settings().is_none(),
            "nothing opens until the read lands"
        );
    }

    #[test]
    fn the_screen_opens_when_the_read_lands() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(app.settings().is_some());
    }

    #[test]
    fn a_read_that_failed_says_so_and_leaves_the_dashboard_up() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Err("no such file".into()),
        });
        assert!(app.settings().is_none());
        let notice = app.notice().expect("a failed read has to say so");
        assert!(notice.is_grave());
        assert!(notice.to_string().contains("no such file"));
    }

    #[test]
    fn s_closes_the_screen_again() {
        let mut app = fixtures::app_in_settings();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        assert!(app.settings().is_none());
    }

    /// From the dashboard with no filter `Esc` quits; from here it must not.
    #[test]
    fn escape_closes_the_screen_and_never_quits() {
        let mut app = fixtures::app_in_settings();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert!(app.settings().is_none());
    }

    #[test]
    fn the_flock_cursor_and_the_filter_survive_the_swap() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        for c in "web".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let selected = app.selected();
        let filter = app.filter().to_string();

        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        let _ = app.update(Msg::Key(KeyPress::Settings));

        assert_eq!(app.selected(), selected);
        assert_eq!(app.filter(), filter);
    }

    #[test]
    fn the_settings_cursor_starts_at_the_first_row_on_every_open() {
        let mut app = fixtures::app_in_settings();
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });

        let first = app.settings().unwrap().rows()[0];
        assert_eq!(app.settings().unwrap().cursor(), Some(first));
    }

    #[test]
    fn the_cursor_moves_through_the_scalars_and_into_the_dogs() {
        let mut app = fixtures::app_in_settings();
        let rows = app.settings().unwrap().rows();
        for _ in 0..rows.len() - 1 {
            let _ = app.update(Msg::Key(KeyPress::SelectDown));
        }
        assert_eq!(
            app.settings().unwrap().cursor(),
            Some(*rows.last().unwrap())
        );
        // and it stops rather than wrapping
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.settings().unwrap().cursor(),
            Some(*rows.last().unwrap())
        );
    }

    #[test]
    fn an_action_key_from_the_dashboard_is_unreachable_while_the_screen_is_up() {
        let mut app = fixtures::app_in_settings_with_control();
        // `x` is the stop key on the dashboard. In here it is not an action.
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_none(), "no sheep confirm can arm from here");
    }

    /// Every key sequence that ends in a write, not one key: a gate guarding
    /// `space` alone leaves the free-text editor's route reaching
    /// `WriteSetting` on a read-only lookout.
    ///
    /// The refusal is checked per keypress rather than at the end, because
    /// `on_settings_key` clears `self.notice` on every key.
    #[test]
    fn no_key_route_writes_the_config_while_the_gate_is_closed() {
        use KeyPress::{Confirm, Cycle, SelectDown, TextApply, TextChar};

        // `Settings::rows` puts the six scalars first, then the fixture's two
        // dogs: two `SelectDown`s reach `socket`, six the first dog row.
        let routes: &[(&str, &[KeyPress])] = &[
            ("space on a cycled scalar", &[Cycle, Confirm]),
            (
                "the socket editor",
                &[
                    SelectDown,
                    SelectDown,
                    Confirm,
                    TextChar('/'),
                    TextChar('x'),
                    TextApply,
                    Confirm,
                ],
            ),
            (
                "the max_cron_sleep editor",
                &[
                    SelectDown,
                    SelectDown,
                    SelectDown,
                    Confirm,
                    TextChar('9'),
                    TextChar('s'),
                    TextApply,
                    Confirm,
                ],
            ),
            (
                "the whistle gate",
                &[
                    SelectDown, SelectDown, SelectDown, SelectDown, Cycle, Confirm,
                ],
            ),
            (
                "space on a dog row",
                &[
                    SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, Cycle,
                    Confirm,
                ],
            ),
        ];

        for (what, keys) in routes {
            let mut app = fixtures::app_in_settings(); // Control::ReadOnly
            let mut refused = false;
            for key in *keys {
                let effect = app.update(Msg::Key(*key));
                assert!(
                    !matches!(
                        effect,
                        Effect::WriteSetting { .. } | Effect::WriteDog { .. }
                    ),
                    "{what}: a read-only lookout reached {effect:?}"
                );
                refused |= app.notice().is_some_and(Notice::is_grave);
            }
            assert!(refused, "{what}: the refusal has to say why");
            assert!(
                app.settings().unwrap().pending().is_none(),
                "{what}: nothing is left armed"
            );
            assert!(
                app.settings().unwrap().typing().is_none(),
                "{what}: no editor is left open"
            );
        }
    }

    /// The half that keeps the closed-gate test honest: a gate that refused
    /// everything would pass it and be useless.
    #[test]
    fn every_one_of_those_routes_writes_once_the_gate_is_open() {
        use KeyPress::{Confirm, Cycle, SelectDown, TextApply, TextChar};

        let routes: &[(&str, &[KeyPress])] = &[
            ("space on a cycled scalar", &[Cycle, Confirm]),
            (
                "the socket editor",
                &[
                    SelectDown,
                    SelectDown,
                    Confirm,
                    TextChar('/'),
                    TextChar('x'),
                    TextApply,
                    Confirm,
                ],
            ),
            (
                "space on a dog row",
                &[
                    SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, Cycle,
                    Confirm,
                ],
            ),
        ];

        for (what, keys) in routes {
            let mut app = fixtures::app_in_settings_with_control();
            let mut wrote = false;
            for key in *keys {
                let effect = app.update(Msg::Key(*key));
                wrote |= matches!(
                    effect,
                    Effect::WriteSetting { .. } | Effect::WriteDog { .. }
                );
            }
            assert!(wrote, "{what}: an open gate has to reach the write");
        }
    }

    #[test]
    fn a_read_only_lookout_opens_the_screen_and_refuses_the_edit_key() {
        let mut app = fixtures::app_in_settings(); // Control::ReadOnly
        assert!(app.settings().is_some(), "reading shep.toml is not gated");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let notice = app.notice().expect("the refusal has to say why");
        assert!(notice.is_grave());
    }

    #[test]
    fn space_arms_a_candidate_without_changing_the_row() {
        let mut app = fixtures::app_in_settings_with_control();
        let before = app.settings().unwrap().snapshot().log_level.value.clone();

        assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);

        assert_eq!(
            app.settings().unwrap().snapshot().log_level.value,
            before,
            "arming is a question, so the row still shows what the file says"
        );
        assert!(app.settings().unwrap().pending().is_some());
    }

    /// Six log levels and one cycle key: without re-arming, the fourth needs a
    /// cancel in between.
    #[test]
    fn space_advances_the_candidate_rather_than_needing_a_cancel() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let first = app.settings().unwrap().pending().unwrap().text.to_string();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let second = app.settings().unwrap().pending().unwrap().text.to_string();
        assert_ne!(first, second);
    }

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

    /// With `$SHEP_STYLE=bare` over a file saying `full`, cycling the resolved
    /// value would propose `full`: a no-op write, reported as a change.
    #[test]
    fn the_style_cycle_starts_from_the_file_and_not_the_level_in_force() {
        let mut app = fixtures::app_in_settings_with_shadowed_style(StyleSource::Env);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(
            text.contains("set style level to plain"),
            "the file says full, so one step is plain: {text}"
        );
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

    /// The divergence from the sheep confirm, which `disarm_on_link_change`
    /// clears: a settings edit is local file I/O.
    #[test]
    fn a_lost_link_leaves_a_scalar_confirm_armed() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert!(
            app.settings().unwrap().pending().is_some(),
            "a scalar never leaves the machine, so a dead shepherd is irrelevant to it"
        );
    }

    /// Off the raw tick rather than `self.now`, which stops advancing once the
    /// link is lost.
    #[test]
    fn a_settings_confirm_expires_on_a_frozen_dashboard() {
        let (mut app, start) = fixtures::app_in_settings_at();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let _ = app.update(Msg::Tick {
            now: start + CONFIRM_EXPIRY,
        });
        assert!(app.settings().unwrap().pending().is_none());
    }

    #[test]
    fn escape_cancels_the_confirm_before_it_closes_the_screen() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.settings().unwrap().pending().is_none());
        assert!(
            app.settings().is_some(),
            "the first Esc cancels, it does not close"
        );
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.settings().is_none());
    }

    /// `s` raises `Effect::LoadSettings` while `body` is still `Body::FlockTable`,
    /// so `x` reaches `arm()`. Once the read lands, `on_settings_key` no-ops
    /// `Confirm`, so nothing could resolve the armed action.
    #[test]
    fn opening_the_screen_clears_an_action_armed_while_the_read_was_in_flight() {
        let mut app = fixtures::allowed_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(
            app.action().is_some(),
            "the arm must still succeed before the read lands"
        );
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(
            app.action().is_none(),
            "no armed action may survive the screen opening"
        );
    }

    /// `on_key` checks the text mode ahead of its settings branch, so a box
    /// left open would eat every key the settings keymap owns. The query itself
    /// is kept.
    #[test]
    fn opening_the_screen_closes_a_filter_box_left_open_while_the_read_was_in_flight() {
        let mut app = fixtures::allowed_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        let _ = app.update(Msg::Key(KeyPress::TextChar('w')));
        let _ = app.update(Msg::Key(KeyPress::TextChar('e')));
        assert_eq!(
            app.mode(),
            InputMode::Text,
            "the box is open before the read lands"
        );
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(app.settings().is_some(), "the screen opened");
        assert_eq!(
            app.mode(),
            InputMode::Normal,
            "the box must not survive the screen opening"
        );
        assert_eq!(app.filter(), "we", "the typed query is kept, not discarded");
    }

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

    #[test]
    fn movement_cancels_an_armed_candidate_rather_than_also_moving() {
        let mut app = fixtures::app_in_settings_with_control(); // cursor on log_level
        let before = app.settings().unwrap().cursor();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "space must arm before this test means anything"
        );
        let effect = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(effect, Effect::None, "a cancel must not also move");
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the armed candidate must not survive the movement key"
        );
        assert_eq!(
            app.settings().unwrap().cursor(),
            before,
            "the cursor must not also move on the same keypress"
        );
    }

    /// `Edit` cancels an armed candidate instead of probing the dog's
    /// schema, same as `Refresh` and `Help` cancel instead of their own
    /// actions.
    ///
    /// `Effect::None` is the whole assertion on the effect side: a cancel
    /// that also probed would hand the operator a schema they never asked
    /// for, on a keypress they spent undoing something else.
    #[test]
    fn edit_cancels_an_armed_candidate_rather_than_probing_a_schema() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "space must arm before this test means anything"
        );
        let effect = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(effect, Effect::None, "a cancel must not also probe");
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the armed candidate must not survive `e`"
        );
    }

    #[test]
    fn refresh_cancels_an_armed_candidate_rather_than_silently_dropping_it() {
        let mut app = fixtures::app_in_settings_with_control(); // cursor on log_level
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "space must arm before this test means anything"
        );
        let effect = app.update(Msg::Key(KeyPress::Refresh));
        assert_eq!(
            effect,
            Effect::None,
            "a cancel must not also raise a reload"
        );
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the armed candidate must not survive `r`"
        );
    }

    #[test]
    fn escape_cancels_an_armed_candidate_rather_than_closing_the_screen() {
        let mut app = fixtures::app_in_settings_with_control(); // cursor on log_level
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "space must arm before this test means anything"
        );
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(effect, Effect::None, "a cancel must not also close");
        assert!(
            app.settings().is_some(),
            "the screen must stay open for the cancel to be seen"
        );
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the armed candidate must not survive escape"
        );
    }
}
