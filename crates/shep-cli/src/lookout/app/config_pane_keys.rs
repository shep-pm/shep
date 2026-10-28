//! The keyboard over an open config pane: the one dispatch that decides which
//! handler answers a key.
//!
//! Moved whole out of `config_pane` under IR-48's 1000-line ceiling: the arms,
//! their order and their comments are what they were, and the handlers this
//! dispatches to live where they already did, in [`crate::lookout::app::close_dialog`],
//! [`crate::lookout::app::pane_list`] and [`crate::lookout::app::pane_dogs`].

use super::*;

impl App {
    /// The config pane's own keymap, in force for as long as
    /// [`Self::config_pane`] is `Some`.
    ///
    /// Movement walks fields, `r` re-reads, `space` cycles the row under the
    /// cursor, `Enter` or `e` edits it, `u` undoes the newest edit, and
    /// `Escape` asks the close dialog's question if there is one: see
    /// [`Self::reread_before_closing`]. Nothing is armed here.
    pub(super) fn on_pane_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
        // An armed env delete is cancelled by every key but the confirm and a
        // quit, the secrets pane's rule, and its status line promises exactly
        // that. The arm is taken before the key is dispatched so `Escape` can
        // eat its own press rather than also closing the pane behind it.
        let was_armed = !matches!(key, KeyPress::Confirm | KeyPress::Quit)
            && self
                .config_pane_mut()
                .is_some_and(ConfigPane::disarm_env_delete);
        if self.close_dialog.is_some() {
            return self.on_close_dialog_key(key);
        }
        if self.config_pane().is_some_and(|pane| pane.list().is_some()) {
            return self.on_list_key(key);
        }
        if let Some(effect) = self.on_dogs_tab_key(key) {
            return effect;
        }
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            // Unreachable: the guard above already returned. Kept rather
            // than a wildcard, because every other arm here is named on
            // purpose, and a wildcard would defeat that for every variant,
            // not just this one.
            KeyPress::Quit => return Effect::Quit,
            // Backs out one level at a time: the close dialog's own
            // question first, if there is one, else the pane. `Escape`
            // closes rather than cascading to a filter clear or a quit.
            // Nothing leaves the pane until the question is answered.
            KeyPress::Escape => {
                // An armed delete eats the first `Escape`, as it does on the
                // secrets pane: a delete waiting on a confirm is a state the
                // operator should see cleared before anything else moves.
                if was_armed {
                    return Effect::None;
                }
                if let Some(text) = self.config_pane().and_then(ConfigPane::incomplete_union) {
                    self.notice = Some(Notice::grave(text));
                    return Effect::None;
                }
                // Tables are read again first: the dialog's `c`, `R` and `L`
                // write them too. See [`Self::on_close_reread`].
                if let Some(effect) = self.reread_before_closing() {
                    return effect;
                }
                if let Some(dialog) = self.close_offer() {
                    self.close_dialog = Some(dialog);
                    return Effect::None;
                }
                return self.write_and_close();
            }
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if let Some(pane) = self.config_pane_mut() {
                    match key {
                        KeyPress::SelectUp => pane.move_by(-1),
                        KeyPress::SelectDown => pane.move_by(1),
                        KeyPress::SelectFirst => pane.move_to_first(),
                        KeyPress::SelectLast => pane.move_to_last(),
                        _ => unreachable!(),
                    }
                }
            }
            // Re-reads the same sheep, so an override applied from another
            // window shows up. The cursor survives it: see
            // `Self::on_sheep_config`.
            KeyPress::Refresh => return self.reread_pane(),
            KeyPress::Cycle => return self.cycle_field(),
            KeyPress::Confirm => return self.confirm_field(),
            // `e` does exactly what `Enter` does here: an operator who
            // opened the pane with `e` should not have to learn a second
            // key to use it. Except on a table, which `Enter` opens and
            // closes and which holds nothing for `e` to edit.
            KeyPress::Edit => {
                if self.config_pane().is_some_and(|pane| !pane.cursor_on_table()) {
                    return self.confirm_field();
                }
            }
            // On a dog's pane `←` and `→` close and open the table under the
            // cursor; a sheep pane has no tables and they do nothing.
            KeyPress::TabPrev => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.close_table();
                }
            }
            KeyPress::TabNext => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.open_table();
                }
            }
            KeyPress::Help => {
                self.open_keymap();
            }
            // `d` restores the field under the cursor to its default. Does
            // nothing on an env row or `+ add a key`: unsetting a key
            // entirely is a different act from restoring a default, and
            // the spec does not ask for it.
            KeyPress::Remove => return self.restore_default(),
            // `D` arms the removal of the env key under the cursor, `Enter`
            // confirms it, and every other key cancels. The cursor is asked
            // before the gate is: a field row names no key to remove, so `D`
            // is inert there. The gate is checked on the arm, not the confirm.
            KeyPress::ArmDelete => {
                let names_a_key = self
                    .config_pane()
                    .is_some_and(|pane| pane.cursor_env_key_name().is_some());
                if names_a_key {
                    let now = self.now;
                    if self.authorize_write().is_some()
                        && let Some(pane) = self.config_pane_mut()
                    {
                        pane.arm_env_delete(now);
                    }
                }
            }
            KeyPress::Action(_)
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextEdit(_)
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::Collapse
            // Bound only in the close dialog; with none up, `c` is a stray
            // key the same way an action key is.
            | KeyPress::Continue => {}
            // The key/value editor's two field steps. `map_key` only raises
            // them in `InputMode::Text`, which `on_key` branches on before
            // this match is reached, so nothing here can see them.
            KeyPress::NextField | KeyPress::PrevField => {}
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats => {}
            // Drops the newest edit and nothing else. No control gate: a
            // key that unfiles something cannot write, and refusing it
            // behind a closed gate would leave an edit the gate already
            // refused to file.
            KeyPress::Undo => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.undo_edit();
                }
            }
            // `tab` and `shift-tab` walk the groups; a digit jumps straight
            // to one. All reset the cursor to the group's first field, so
            // `j`/`k` never start on a row the new tab does not draw. The
            // `dogs` tab lists its dogs the first time it comes up.
            KeyPress::NextGroup => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.next_group();
                    pane.move_to_first();
                }
                return self.dogs_tab_effect();
            }
            KeyPress::PrevGroup => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.prev_group();
                    pane.move_to_first();
                }
                return self.dogs_tab_effect();
            }
            KeyPress::Group(digit) => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.set_group(digit);
                    pane.move_to_first();
                }
                return self.dogs_tab_effect();
            }
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::lookout::view::fixtures;

    /// Whether the config pane holds an armed env delete.
    fn armed(app: &App) -> Option<String> {
        app.config_pane()
            .expect("the pane is open")
            .armed_env()
            .map(|armed| armed.key.clone())
    }

    /// `D` on a row that names no key to remove is inert, and inert here means
    /// silent: the gate's "read-only" sentence answers a write the operator
    /// actually asked for, and no `D` on a field row ever did.
    #[test]
    fn d_on_a_field_row_arms_nothing_and_says_nothing() {
        let mut app = fixtures::app_in_sheep_pane_read_only();
        assert!(
            app.config_pane()
                .expect("the pane is open")
                .cursor_env_key_name()
                .is_none(),
            "sanity: the cursor is on a field"
        );

        let effect = app.update(Msg::Key(KeyPress::ArmDelete));

        assert!(matches!(effect, Effect::None));
        assert!(armed(&app).is_none(), "nothing armed");
        assert!(app.notice().is_none(), "and nothing said");
    }

    /// The same key on a row that does name a key still refuses through the
    /// gate, with the reason the gate gives.
    #[test]
    fn d_on_an_env_row_refuses_through_the_gate_with_its_reason() {
        let mut app = fixtures::app_in_sheep_pane_read_only();
        fixtures::select_env_key(&mut app, "DB_HOST");

        let _ = app.update(Msg::Key(KeyPress::ArmDelete));

        assert!(armed(&app).is_none(), "the gate closed the arm");
        let notice = app.notice().map(ToString::to_string);
        assert!(
            notice
                .as_deref()
                .is_some_and(|notice| notice.contains("read-only")),
            "and named the gate: {notice:?}"
        );
    }
}
