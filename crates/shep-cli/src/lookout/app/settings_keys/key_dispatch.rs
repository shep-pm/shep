use super::super::*;

impl App {
    /// What a landed settings write answers with: re-read `shep.toml`, unless
    /// the screen has an edit of its own in flight.
    ///
    /// [`Msg::Settings`] rebuilds the whole screen, so a re-read raised by a
    /// reply the screen was no longer waiting on would throw away the edit it
    /// is waiting on instead. That edit's own reply re-reads a moment later,
    /// and carries both writes' work with it.
    pub(in crate::lookout::app) fn reread_settings(&self) -> Effect {
        match self.settings() {
            Some(settings) if settings.pending.is_some() => Effect::None,
            _ => Effect::LoadSettings,
        }
    }

    /// The settings screen's own keymap, in force while [`Self::settings`] is
    /// `Some`. Everything not named here is ignored, an action key included.
    pub(in crate::lookout::app) fn on_settings_key(&mut self, key: KeyPress) -> Effect {
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
}

#[cfg(test)]
mod tests {

    use super::*;

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
}
