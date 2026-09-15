//! Editing a field in the config pane: filing an edit, undoing it, and the
//! batch that goes out on close.

use super::*;

impl App {
    /// One `Request::SetSheepField` reply.
    ///
    /// Reaches for no pane: the write went out on the keypress that closed
    /// it, so this lands with the dashboard on screen. A success still
    /// re-reads the config, using `Effect::Send` so the re-read gets
    /// `Msg::Unsent` handling for free, and [`Self::on_sheep_config`]
    /// drops the answer when nobody is waiting for it.
    ///
    /// Every sentence names the field, refusals included. A close sends
    /// the whole set at once, so several of these can land in a row and a
    /// refusal that named only the sheep would not say which write failed.
    ///
    /// The success sentence also names the new value when
    /// [`FieldValue::safe_summary`] says that is safe: without it, setting
    /// `reuse_port` to `true` and setting it back to `false` print the
    /// identical sentence, which is wrong on its own rather than merely
    /// incomplete.
    ///
    /// `pending` is the shepherd's answer, not this pane's guess: it knows
    /// about fields like `autostart` that `apply_group` cannot derive.
    ///
    /// `warning` rides the same sentence rather than a second notice: the
    /// write still landed, so this is one more clause about it, on the same
    /// terms `pending`'s own `", and waits for..."` clause already sets.
    pub(super) fn on_field_applied(
        &mut self,
        name: &str,
        key: &str,
        value: &FieldValue,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::SheepFieldSet {
                pending, warning, ..
            }) => {
                let key_text = match value.safe_summary() {
                    Some(v) => format!("{key} set to {v}"),
                    None => format!("{key} is set"),
                };
                let mut text = if pending {
                    format!("{name}: {key_text}, and waits for `shep reload {name}`")
                } else {
                    format!("{name}: {key_text}")
                };
                if let Some(warning) = warning {
                    text = format!("{text}; {warning}");
                }
                self.notice = Some(Notice { text, grave: false });
                Effect::Send(Sent::SheepConfig {
                    name: name.to_owned(),
                })
            }
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{name}: {key}: the shepherd answered something this lookout does not \
                         understand"
                    ),
                    grave: true,
                });
                Effect::None
            }
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {key}: {}", err.message),
                    grave: true,
                });
                Effect::None
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {key}: {other}"),
                    grave: true,
                });
                Effect::None
            }
        }
    }

    /// Everything the open pane has filed, as the requests that carry it,
    /// leaving the pane holding nothing.
    ///
    /// Empty for no pane, for an empty set, and for a dog whose section
    /// stopped parsing between the read and the keystroke, which is
    /// reported rather than sent as an empty table.
    ///
    /// A sheep's set is one request per entry and a dog's is one request
    /// for the lot: `Request::SetDogConfig` replaces the whole table, so
    /// a batch of edits to one dog is one write. See
    /// `ConfigPane::edited_section_with`.
    pub(super) fn take_pane_writes(&mut self) -> Vec<Sent> {
        // `WriteAuthority::granted`, not `Self::authorize_write`: the gate
        // is checked on the keystroke that files an edit, so a read-only
        // pane reaches here with an empty set and must not be told off for
        // leaving.
        let Some(authority) = WriteAuthority::granted(self) else {
            return Vec::new();
        };
        // A direct field match, not `Self::config_pane_mut`: that helper
        // borrows the whole struct for as long as `pane` lives, and this
        // function still needs `self.next_write_ticket` while it is in
        // scope.
        let Some(pane) = (match &mut self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Vec::new();
        };
        let edits = pane.close();
        if edits.is_empty() {
            return Vec::new();
        }
        let mut ticket = self.next_write_ticket;
        let sent = match pane.target().clone() {
            PaneTarget::Dog { name, .. } => match pane.edited_section_with(&edits) {
                Some(toml) => {
                    let section = vec![Sent::SetDogSection {
                        name,
                        ticket,
                        toml: toml.into(),
                        authority,
                    }];
                    ticket += 1;
                    section
                }
                None => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its section in dogs.toml does not parse"),
                        grave: true,
                    });
                    return Vec::new();
                }
            },
            PaneTarget::Sheep { name } => {
                let writes = edits.into_writes();
                let mut requests = Vec::with_capacity(writes.len());
                for edit in writes {
                    let this = ticket;
                    ticket += 1;
                    requests.push(match edit {
                        PaneEdit::SetEnv { key, value } => Sent::SetEnv {
                            name: name.clone(),
                            ticket: this,
                            key,
                            value,
                            authority,
                        },
                        // No client-side validation, deliberately: the
                        // daemon already re-normalizes untrusted input, so
                        // a weaker copy here would drift the moment
                        // `AppConfig` grows a field. An empty buffer on a
                        // non-nullable field files `null`, refused as
                        // `InvalidConfig`.
                        PaneEdit::Set { key, value } => Sent::ApplyField {
                            name: name.clone(),
                            ticket: this,
                            key,
                            value,
                            authority,
                        },
                    });
                }
                requests
            }
        };
        // One ticket per request that goes out, and never reused: the
        // counter is what keeps two `Sent` values for the same field
        // distinguishable.
        self.next_write_ticket = ticket;
        sent
    }

    /// `space` on the config pane. Arms the next value for the row under
    /// the cursor, or refuses and says why.
    ///
    /// The gate is [`Self::authorize_write`], the same one every settings
    /// write passes and for the same reason: a keystroke that changes a
    /// running flock's config needs the fat-finger catch a keystroke that
    /// stops a sheep has.
    pub(super) fn cycle_field(&mut self) -> Effect {
        // The lock is checked ahead of the control gate, the same order
        // `confirm_field` takes: it is the more specific fact, and
        // `--allow-control` would not change it. A screen that answers
        // one question two ways teaches an operator to believe neither.
        if let Some((key, lock)) = self
            .config_pane()
            .and_then(ConfigPane::cursor_lock)
            .map(|(key, lock)| (key.to_owned(), lock))
        {
            self.notice = Some(Notice {
                text: Self::lock_refusal(&key, lock),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.cycle();
        }
        Effect::None
    }

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
    pub(super) fn confirm_field(&mut self) -> Effect {
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

    /// `d` on the config pane's field list. Files an edit that removes the
    /// operator's value for the field under the cursor, so the stored
    /// default shows through once sent: the same verb the list sub-screen's
    /// `d` performs on an element, which is why both carry
    /// [`KeyPress::Remove`].
    ///
    /// Same lock-then-control order as [`Self::cycle_field`] and
    /// [`Self::confirm_field`]: the lock names the more specific reason and
    /// `--allow-control` would not change it. Does nothing on an env row,
    /// on `+ add a key`, or with no row at all: [`ConfigPane::cursor_kind`]
    /// is `None` for exactly those, and there is no field to restore.
    pub(super) fn restore_default(&mut self) -> Effect {
        if let Some((key, lock)) = self
            .config_pane()
            .and_then(ConfigPane::cursor_lock)
            .map(|(key, lock)| (key.to_owned(), lock))
        {
            self.notice = Some(Notice {
                text: Self::lock_refusal(&key, lock),
                grave: true,
            });
            return Effect::None;
        }
        if self
            .config_pane()
            .and_then(ConfigPane::cursor_kind)
            .is_none()
        {
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.file_default();
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;

    /// One field from each of the groups the pane draws, since the
    /// daemon's routing classification is per field. `rpc.rs`'s
    /// `a_field_edit_is_reported_as_an_operator_override` asserts the
    /// other half, where the marker is actually built.
    #[test]
    fn one_edit_reaches_the_wire_as_a_single_field_override() {
        for key in [
            "autorestart", // control
            "watch",       // process
            "merge_logs",  // inputs
        ] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            assert_eq!(
                app.config_pane().unwrap().edits().len(),
                1,
                "{key}: space files an edit and sends nothing"
            );
            let request = one_wire(close_writing(&mut app));
            let Request::SetSheepField {
                name,
                key: sent,
                value,
            } = request
            else {
                panic!("{key}: expected SetSheepField, got {request:?}");
            };
            assert_eq!(name, "web", "{key}");
            assert_eq!(sent, key, "{key}");
            assert!(value.is_boolean(), "{key}: {value}");
        }
    }

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

    /// A reply for a write from an earlier pane can land while a new one
    /// is open and being typed into. On the env screen the discarded
    /// buffer is a secret the operator cannot read back.
    #[test]
    fn a_reply_landing_mid_edit_leaves_the_buffer_and_the_keyboard_alone() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let mut batch = wire_batch(close_writing(&mut app));
        let sent = batch.remove(0);
        // The pane is reopened and the operator starts typing while the
        // first write is still out.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for typed in "/srv".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        assert_eq!(app.mode(), InputMode::Text);
        let _ = app.update(Msg::Unsent { sent });
        assert_eq!(app.mode(), InputMode::Text, "the keyboard is not stranded");
        let typing = app
            .config_pane()
            .unwrap()
            .typing()
            .expect("the buffer survives the reply");
        assert_eq!(typing.key, "cwd");
        assert!(typing.buffer.ends_with("/srv"), "{}", typing.buffer);
    }

    /// A landed write asks for a re-read, and the re-read rebuilds the
    /// whole `ConfigPane`, editor included.
    #[test]
    fn a_refresh_that_drops_an_open_editor_puts_the_keyboard_back() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text);
        refresh_config(&mut app, &["DB_HOST", "LOG_LEVEL"]);
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.config_pane().unwrap().typing().is_none());
    }

    /// The whole shape of the pane in one test: a keystroke files, and
    /// nothing reaches the shepherd for it.
    #[test]
    fn cycling_a_bool_files_an_edit_and_sends_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Cycle));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    #[test]
    fn undo_drops_the_edit_it_filed() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        app.update(Msg::Key(KeyPress::Undo));
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    #[test]
    fn cycling_a_bool_back_to_its_stored_value_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "a round trip back to the stored value is not an edit"
        );
    }

    /// `d` on an overridden field files the schema's own `default` rather
    /// than [`Value::Null`]: `max_restarts` is a plain `u32`, not an
    /// `Option<u32>`, and the shepherd's deserializer refuses `null` for
    /// one of those. The Flockfile schema's default for `max_restarts` is
    /// `16`.
    #[test]
    fn d_restores_an_overridden_field_to_its_schema_default() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            app.config_pane().unwrap().is_overridden("max_restarts"),
            "the fixture overrides max_restarts"
        );
        pane_to(&mut app, "max_restarts");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_eq!(filed_value(&app, "max_restarts"), serde_json::json!(16));

        let request = one_wire(close_writing(&mut app));
        let Request::SetSheepField { key, value, .. } = request else {
            panic!("expected SetSheepField, got {request:?}");
        };
        assert_eq!(key, "max_restarts");
        assert_eq!(value, serde_json::json!(16));
    }

    /// A field the operator has not overridden is already showing its
    /// default, so `d` files nothing: an edit that changes nothing would
    /// still be counted by the title band.
    #[test]
    fn d_on_a_field_already_at_its_default_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            !app.config_pane().unwrap().is_overridden("autorestart"),
            "the fixture does not override autorestart"
        );
        pane_to(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    /// A field flipped this session, even one the shepherd never
    /// overrode, is no longer showing its default: `d` restores the
    /// schema default rather than leaving the flipped value in place. For
    /// a field the shepherd never overrode, the schema default and what
    /// the shepherd already holds are the same value, so restoring it
    /// exactly cancels the flip: the fresh edit drops rather than being
    /// replaced by a second one.
    #[test]
    fn d_after_cycling_a_fresh_value_restores_the_default_anyway() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            !app.config_pane().unwrap().is_overridden("autostart"),
            "the fixture does not override autostart"
        );
        pane_to(&mut app, "autostart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_ne!(
            filed_value(&app, "autostart"),
            serde_json::Value::Null,
            "the flip files the opposite of the stored value"
        );

        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "the schema default for autostart is the fixture's own stored value, \
             so restoring it cancels the flip rather than filing a second edit"
        );
    }

    /// A field that is already `(unset)` on the shepherd's own side has
    /// nowhere further to fall: cancelling a fresh, unsent edit to it
    /// files nothing rather than a `Null` edit that would just repeat what
    /// the shepherd already has.
    #[test]
    fn d_after_typing_an_unset_field_leaves_nothing_filed() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        fixtures::type_into_the_open_editor(&mut app, "/srv/api");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);

        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "cwd was already (unset), so cancelling the typed edit leaves nothing to send"
        );
    }

    /// The lock wins over the control gate here too: `d` refuses a
    /// Structural field with the same sentence `space` and `Enter` give it.
    #[test]
    fn d_refuses_a_locked_field_with_its_lock_sentence() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "instances");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        let notice = app.notice().expect("a locked row answers").to_string();
        assert!(notice.contains("`shep stock`"), "{notice}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    /// A read-only pane refuses `d` the same way it refuses `space` and
    /// `Enter`: on the keystroke that would file the edit, not on a later
    /// close that would try to send it.
    #[test]
    fn d_refuses_when_the_pane_is_read_only() {
        let mut app = fixtures::app_in_sheep_pane();
        pane_to(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert_eq!(
            app.notice().map(ToString::to_string),
            Some(READ_ONLY_REFUSAL.to_string())
        );
    }

    /// `d` means nothing on an env row: unsetting a key entirely is a
    /// different act from restoring a default, and the spec does not ask
    /// for it.
    #[test]
    fn d_on_an_env_row_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    #[test]
    fn escape_sends_every_filed_edit_at_once() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let effect = close_writing(&mut app);
        let Effect::SendAll(sent) = effect else {
            panic!("wanted a batch, got {effect:?}");
        };
        assert_eq!(sent.len(), 2);
        assert!(
            app.config_pane().is_none(),
            "the pane closes on the same key"
        );
    }

    #[test]
    fn escape_with_nothing_filed_closes_and_sends_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().is_none());
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

    /// A refusal now lands after the pane has gone, so its arm must not
    /// assume a pane is open.
    #[test]
    fn a_refused_write_notices_after_the_pane_has_closed() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let Effect::SendAll(mut sent) = close_writing(&mut app) else {
            panic!("wanted a batch");
        };
        let first = sent.remove(0);
        assert!(app.config_pane().is_none());
        app.update(Msg::Replied {
            sent: first,
            result: Err(fixtures::a_refusal()),
        });
        let notice = app
            .notice()
            .map(ToString::to_string)
            .expect("a refusal says so");
        assert!(
            notice.contains("cwd"),
            "the notice names the field: {notice}"
        );
    }

    /// Validation runs on entry, so the set is always sendable. An integer
    /// field mid-word holds the editor open rather than filing a bad value,
    /// which is what `apply_typing` already does today.
    #[test]
    fn a_value_that_does_not_parse_never_joins_the_set() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "max_restarts");
        app.update(Msg::Key(KeyPress::Confirm));
        for typed in "not a number".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert_eq!(app.mode(), InputMode::Text, "the editor stays open");
    }

    /// The set is the operator's and the values are the shepherd's.
    #[test]
    fn a_config_re_read_replaces_the_values_and_keeps_the_edits() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
            panic!("refresh asks the shepherd for the config again");
        };
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert_eq!(app.config_pane().unwrap().edits().len(), 2);
    }

    /// Two edits, one close, two requests, each naming its own key: a
    /// batch is not one write carrying a map, so neither entry can carry
    /// the other's value.
    #[test]
    fn a_batch_names_each_key_in_its_own_request() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let requests = wire_all(close_writing(&mut app));
        let named: Vec<String> = requests
            .iter()
            .map(|request| match request {
                Request::SetSheepField { key, .. } => key.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(named, vec!["autorestart".to_owned(), "watch".to_owned()]);
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

    /// Nothing is armed, so nothing eats a keystroke: the next key does
    /// its own job and the filed edit stays filed.
    #[test]
    fn a_key_after_an_edit_does_its_own_job_and_keeps_the_edit() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let before = app.config_pane().unwrap().view().cursor();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
        assert_eq!(
            app.config_pane().unwrap().view().cursor(),
            before + 1,
            "the movement key moves"
        );
        assert_eq!(
            app.config_pane().unwrap().edits().len(),
            1,
            "and does not cancel the edit"
        );
    }

    /// A filed edit is not a question waiting for an answer, so no timer
    /// takes it away: an operator who walked off must not come back to
    /// work silently discarded.
    #[test]
    fn a_filed_edit_outlives_the_confirm_budget() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Tick {
            now: Instant::now() + CONFIRM_EXPIRY,
        });
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    /// `u` drops the newest edit and leaves the older one filed.
    #[test]
    fn u_undoes_the_newest_edit_only() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Key(KeyPress::Undo));
        let requests = wire_all(close_writing(&mut app));
        let named: Vec<String> = requests
            .iter()
            .map(|request| match request {
                Request::SetSheepField { key, .. } => key.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(named, vec!["autorestart".to_owned()]);
    }

    /// `pending` is the shepherd's answer, reported verbatim rather than
    /// re-derived from `apply_group`: the two disagree on `autostart`, and
    /// on a field whose config subset would not normalize.
    #[test]
    fn a_landed_write_re_reads_the_config_and_reports_what_the_shepherd_said() {
        for (pending, wanted) in [(false, "set to false"), (true, "shep reload")] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, "autorestart");
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            let mut batch = wire_batch(close_writing(&mut app));
            let effect = app.update(Msg::Replied {
                sent: batch.remove(0),
                result: Ok(Response::SheepFieldSet {
                    name: "web".to_owned(),
                    key: "autorestart".to_owned(),
                    pending,
                    warning: None,
                }),
            });
            assert_eq!(
                effect,
                Effect::Send(Sent::SheepConfig {
                    name: "web".to_owned()
                }),
                "a landed write re-reads what the shepherd now holds"
            );
            let notice = app.notice().expect("the outcome is reported");
            assert!(!notice.is_grave(), "{notice:?}");
            assert!(notice.to_string().contains(wanted), "{notice:?}");
        }
    }

    /// A `cwd`/`script`/`out_file`/`err_file` warning rides the same
    /// notice as the write it came back on, not a second one: the write
    /// still landed, and `grave` stays `false` since this is advisory.
    #[test]
    fn a_path_warning_rides_the_same_notice_as_the_write() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        fixtures::type_into_the_open_editor(&mut app, "/does/not/exist");
        // `cwd` needs a respawn, so `esc` raises the close dialog rather
        // than writing on the keypress. `c` is the answer that writes and
        // leaves the sheep alone, which is the same batch this test always
        // read, reached through the question the dialog now asks first.
        app.update(Msg::Key(KeyPress::Escape));
        let mut batch = wire_batch(app.update(Msg::Key(KeyPress::Continue)));
        let effect = app.update(Msg::Replied {
            sent: batch.remove(0),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_owned(),
                key: "cwd".to_owned(),
                pending: true,
                warning: Some("/does/not/exist does not exist yet".to_owned()),
            }),
        });
        // The re-read still goes out. This is the only test that answers
        // with a warning at all, so discarding the effect here would let a
        // refactor gate the re-read on there not being one.
        assert!(
            matches!(effect, Effect::Send(Sent::SheepConfig { .. })),
            "{effect:?}"
        );
        let notice = app.notice().expect("the outcome is reported");
        assert!(!notice.is_grave(), "{notice:?}");
        let text = notice.to_string();
        assert!(text.contains("shep reload"), "{text}");
        assert!(text.contains("does not exist yet"), "{text}");
    }

    /// Every refusal this door can meet is an `Err`, which is why
    /// `Response::SheepFieldSet` carries no `refused` field: two ways to
    /// say no is one a client forgets to check.
    #[test]
    fn a_refused_write_is_reported_and_does_not_re_read() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let mut batch = wire_batch(close_writing(&mut app));
        let effect = app.update(Msg::Replied {
            sent: batch.remove(0),
            result: Err(fixtures::a_refusal()),
        });
        assert_eq!(effect, Effect::None, "a refusal does not re-read");
        let notice = app.notice().expect("the refusal is reported");
        assert!(notice.is_grave());
        assert!(
            notice.to_string().contains("the store is locked"),
            "{notice:?}"
        );
    }

    /// Asserted on the whole `Effect`, since that is what a diagnostic
    /// would print. Nothing between here and the wire unwraps the value.
    #[test]
    fn a_write_effects_debug_names_no_value() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..40 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "/home/ada/secret-project".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let field = close_writing(&mut app);
        assert_eq!(
            format!("{field:?}"),
            "SendAll([ApplyField { name: \"web\", ticket: 0, key: \"cwd\", \
             value: FieldValue(<string>), authority: WriteAuthority(()) }])"
        );

        let mut app = fixtures::app_in_sheep_pane_with_control();
        // `SelectLast` lands on `+ add a key`; two steps up is `DB_HOST`,
        // the fixture's first env key.
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::Env(0)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for typed in "hunter2".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        // One `Escape`, not two: there is no sub-screen level to back out
        // of any more, so the write goes out once the close dialog it
        // raises (this fixture parks `kill_signal`) is answered.
        let env = close_writing(&mut app);
        assert_eq!(
            format!("{env:?}"),
            "SendAll([SetEnv { name: \"web\", ticket: 0, key: \"DB_HOST\", \
             value: Some(EnvValue(<7 bytes>)), authority: WriteAuthority(()) }])"
        );
    }

    /// Two writes to the same field in one batch is unreachable, since
    /// the set holds one entry per key, but two closes of two panes are
    /// not: the counter is monotonic and never reused, so no two `Sent`
    /// values are ever equal.
    #[test]
    fn no_two_writes_share_a_ticket() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let first = wire_batch(close_writing(&mut app));
        assert_ne!(first[0], first[1], "two entries are two tickets");

        // The same lookout, a second pane. `app_in_sheep_pane_with_control`
        // parks a field, so the close above stopped on the apply menu and
        // this `Escape` is what leaves it.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_owned(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let second = wire_batch(close_writing(&mut app));
        assert_ne!(
            first[0], second[0],
            "a second close does not reuse the first's tickets"
        );
    }

    /// The pending set is the operator's and the values are the
    /// shepherd's: a re-read replaces one and keeps the other.
    #[test]
    fn a_refresh_replaces_the_values_and_keeps_the_edits() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        refresh_config(&mut app, &[]);
        assert_eq!(
            app.config_pane().unwrap().edits().len(),
            1,
            "the edit survives the rebuild"
        );
        let request = one_wire(close_writing(&mut app));
        let Request::SetSheepField { key, .. } = request else {
            panic!("{request:?}");
        };
        assert_eq!(key, "autorestart");
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
