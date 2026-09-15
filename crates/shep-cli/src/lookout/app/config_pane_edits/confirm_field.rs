use super::super::*;

impl App {
    /// The operator's `Enter` on the config pane. Three meanings, picked in
    /// this order:
    ///
    /// - The cursor is on an env row or `+ add a key`: opens the env
    ///   editor, in place, on the same row.
    /// - The cursor is on an array field: opens the list sub-screen.
    /// - The cursor is on a typed field: opens the editor and switches
    ///   [`InputMode::Text`] on.
    ///
    /// All three go through [`Self::authorize_write`], the editor included,
    /// for the reason [`Self::confirm_setting`]'s own doc gives: the gate
    /// is checked on the keystroke that would file an edit, not on the
    /// close that writes them.
    pub(in crate::lookout::app) fn confirm_field(&mut self) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        if matches!(pane.cursor(), Some(PaneRow::Env(_) | PaneRow::AddEnv)) {
            if self.authorize_write().is_none() {
                return Effect::None;
            }
            if let Some(pane) = self.config_pane_mut() {
                pane.begin_env_typing();
                self.mode = InputMode::Text;
            }
            return Effect::None;
        }
        let Some(kind) = pane.cursor_kind().cloned() else {
            return Effect::None;
        };
        let locked = pane.cursor_lock().map(|(key, lock)| (key.to_owned(), lock));
        let opens = matches!(
            kind,
            FieldKind::List(_) | FieldKind::Text | FieldKind::Integer | FieldKind::Suggested(_)
        );
        // A row `Enter` was never going to open raises nothing at all: a
        // refusal about a key that was never going to act trains an
        // operator to ignore the status bar. A bool and a choice are
        // `space`'s job, and `space` works.
        if !opens && locked.is_none() {
            return Effect::None;
        }
        // The lock is checked ahead of the control gate: it is the more
        // specific of the two answers, and `--allow-control` would not
        // help. Each lock says its own thing; see [`Self::lock_refusal`].
        if let Some((key, lock)) = locked {
            self.notice = Some(Notice {
                text: Self::lock_refusal(&key, lock),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        if matches!(kind, FieldKind::List(_)) {
            pane.open_list();
        } else {
            pane.begin_typing();
            self.mode = InputMode::Text;
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::*;
    use crate::lookout::app::testing::*;

    /// `cwd` and `max_restarts` between them cover text and integer, which
    /// file differently, and an integer sent as a string is refused by the
    /// daemon rather than set.
    #[test]
    fn a_typed_field_reaches_the_wire_as_the_value_that_was_typed() {
        for (key, typed, want) in [
            ("cwd", "/srv/web", serde_json::json!("/srv/web")),
            ("max_restarts", "40", serde_json::json!(40)),
        ] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);
            let _ = app.update(Msg::Key(KeyPress::Confirm));
            assert_eq!(app.mode(), InputMode::Text, "{key}: the editor opens");
            for _ in 0..40 {
                let _ = app.update(Msg::Key(KeyPress::TextBackspace));
            }
            for typed in typed.chars() {
                let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
            }
            let _ = app.update(Msg::Key(KeyPress::TextApply));
            assert_eq!(app.mode(), InputMode::Normal, "{key}: the editor closes");
            let request = one_wire(close_writing(&mut app));
            let Request::SetSheepField {
                key: sent, value, ..
            } = request
            else {
                panic!("{key}: expected SetSheepField, got {request:?}");
            };
            assert_eq!(sent, key, "{key}");
            assert_eq!(value, want, "{key}");
        }
    }

    /// Routing through `ApplyConfig` would not work: no `ResetDepth`
    /// names a single key.
    #[test]
    fn the_env_rows_file_then_set_one_key_and_remove_another() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        // `+ add a key`, under the two keys the fixture's sheep has.
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text, "enter opens the env editor");
        for typed in "API_TOKEN=hunter2".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        assert_eq!(
            app.update(Msg::Key(KeyPress::TextApply)),
            Effect::None,
            "applying the editor files; it does not send"
        );
        assert_eq!(app.mode(), InputMode::Normal);

        // An existing key with an empty buffer removes it. The cursor is
        // still on `+ add a key`, since applying an edit does not move it;
        // two steps up reaches `DB_HOST`, the fixture's first env key.
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::Env(0)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));

        // Both leave together, on the `Escape` that closes the pane.
        let requests = wire_all(close_writing(&mut app));
        assert_eq!(
            requests,
            vec![
                Request::SetSheepEnv {
                    name: "web".to_owned(),
                    key: "API_TOKEN".to_owned(),
                    value: Some("hunter2".to_owned().into()),
                },
                Request::SetSheepEnv {
                    name: "web".to_owned(),
                    key: "DB_HOST".to_owned(),
                    value: None,
                },
            ]
        );
    }

    /// A removal shortens the list, so a cursor carried by index would name
    /// the next key down, and a reflexive second `Enter` would arm a write
    /// against a neighbour nobody chose. A key that is gone lands on
    /// `+ new`, the one row where `Enter` destroys nothing.
    #[test]
    fn the_env_cursor_is_carried_by_key_and_not_by_index_across_a_refresh() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        // `SelectLast` lands on `+ add a key`; one step up is `LOG_LEVEL`,
        // the fixture's second env key.
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(
            app.config_pane().unwrap().cursor_env_key_name(),
            Some("LOG_LEVEL")
        );
        // A refresh that leaves both keys in place keeps the cursor on its
        // own key rather than on row 1.
        refresh_config(&mut app, &["DB_HOST", "LOG_LEVEL"]);
        assert_eq!(
            app.config_pane().unwrap().cursor_env_key_name(),
            Some("LOG_LEVEL")
        );
        // A refresh that removed the key above it keeps it on its own key,
        // which is now row 0. Carrying the index would have moved it to
        // `+ add a key`; carrying nothing would have moved it to `DB_HOST`.
        refresh_config(&mut app, &["LOG_LEVEL"]);
        assert_eq!(
            app.config_pane().unwrap().cursor_env_key_name(),
            Some("LOG_LEVEL")
        );
        // A refresh that removed the cursor's own key lands on `+ add a
        // key`, never on whatever took its place.
        refresh_config(&mut app, &["OTHER"]);
        assert_eq!(app.config_pane().unwrap().cursor_env_key_name(), None);
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
    }

    /// Read-only refuses the first keypress, not the close. Building five
    /// edits and losing them all at `esc` wastes the operator's time.
    #[test]
    fn read_only_refuses_the_first_edit_and_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::ReadOnly);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        assert!(app.config_pane().unwrap().edits().is_empty());
        let notice = app
            .notice()
            .map(ToString::to_string)
            .expect("a refusal says so");
        assert!(notice.contains("read-only"), "{notice}");
    }

    /// `instances` is `Lock::Refused`, since shep takes no config write for
    /// it at all. `liveness_probe` is `Lock::NoWidget`, since this pane
    /// simply has no editor for a nested object. `Lock` exists so one
    /// sentence never covers both.
    #[test]
    fn a_refused_field_and_one_with_no_widget_refuse_for_different_reasons() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "instances");
        assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);
        assert!(app.config_pane().unwrap().edits().is_empty());
        let refused = app.notice().expect("a refusal is raised").to_string();
        assert!(refused.contains("instances"), "{refused}");
        assert!(
            !refused.contains("no editor in this pane"),
            "a field shep refuses is not a field this pane merely lacks a widget for: {refused}"
        );

        pane_to(&mut app, "liveness_probe");
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(app.config_pane().unwrap().edits().is_empty());
        let no_widget = app.notice().expect("a refusal is raised").to_string();
        assert!(no_widget.contains("liveness_probe"), "{no_widget}");
        assert!(
            no_widget.contains("Flockfile"),
            "a shape with no widget is still one a Flockfile writes: {no_widget}"
        );
        assert_ne!(refused, no_widget, "two facts, two sentences");
    }

    #[test]
    fn a_refused_field_names_the_verb_that_owns_it() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "instances");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let said = app.notice().expect("a refusal is raised").to_string();
        assert!(said.contains("`shep stock`"), "{said}");
    }

    /// The lock wins over the control gate: `--allow-control` does not
    /// unlock a Structural field.
    #[test]
    fn space_and_enter_refuse_a_locked_row_with_the_same_sentence() {
        for control in [Control::ReadOnly, Control::Allowed] {
            for key in ["instances", "liveness_probe"] {
                let mut app = fixtures::app_in_sheep_pane();
                app.set_control_for_tests(control);
                pane_to(&mut app, key);
                let _ = app.update(Msg::Key(KeyPress::Cycle));
                let cycled = app.notice().map(ToString::to_string);
                let _ = app.update(Msg::Key(KeyPress::Confirm));
                let confirmed = app.notice().map(ToString::to_string);
                assert_eq!(cycled, confirmed, "{control:?} {key}");
                assert!(
                    cycled.as_deref().is_some_and(|text| text.contains(key)),
                    "{control:?} {key}: {cycled:?}"
                );
                assert_ne!(
                    cycled.as_deref(),
                    Some(READ_ONLY_REFUSAL),
                    "the lock is the more specific fact: {control:?} {key}"
                );
            }
        }
    }

    #[test]
    fn a_read_only_pane_refuses_every_door_that_writes() {
        // One pair per door: `space` cycles, `Enter` opens the text
        // editor.
        for (key, press) in [("autorestart", KeyPress::Cycle), ("cwd", KeyPress::Confirm)] {
            let mut app = fixtures::app_in_sheep_pane();
            pane_to(&mut app, key);
            assert_eq!(app.update(Msg::Key(press)), Effect::None, "{key}");
            assert!(app.config_pane().unwrap().edits().is_empty(), "{key}");
            assert!(app.config_pane().unwrap().typing().is_none(), "{key}");
            assert!(app.config_pane().unwrap().env_typing().is_none(), "{key}");
            assert_eq!(app.mode(), InputMode::Normal, "{key}");
            assert_eq!(
                app.notice().map(ToString::to_string),
                Some(READ_ONLY_REFUSAL.to_string()),
                "{key}"
            );
        }
    }

    /// `Enter` on an env row is the fourth door: it opens the env editor
    /// exactly as `Enter` on a typed field does, so it is gated the same
    /// way.
    #[test]
    fn a_read_only_pane_refuses_enter_on_an_env_row() {
        let mut app = fixtures::app_in_sheep_pane();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert!(app.config_pane().unwrap().env_typing().is_none());
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.notice().map(ToString::to_string),
            Some(READ_ONLY_REFUSAL.to_string())
        );
    }

    /// The pane-level test reaches `begin_typing` directly, so it passes
    /// over a dead key path. This one presses the key.
    #[test]
    fn e_opens_the_editor_on_a_suggested_field() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "kill_signal");
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.config_pane()
                .and_then(ConfigPane::typing)
                .map(|typing| typing.key.as_str()),
            Some("kill_signal")
        );
    }
}
