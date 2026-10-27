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
    /// Movement walks fields, `r` re-reads, `space` cycles the row under
    /// the cursor, `Enter` or `e` edits it, `u` undoes the newest edit,
    /// and `Escape` asks the close dialog's question if there is one to
    /// ask, else writes everything filed and leaves. A dog's table is read
    /// again before that write: see [`Self::reread_before_closing`]. `h`
    /// raises the keymap overlay; see `KeyPress::Help`'s arm below.
    /// Everything else is named rather than wildcarded, so a stray variant
    /// cannot fall silently into an arm that ignores it.
    ///
    /// Nothing is armed here and no key is eaten. A keystroke that edits
    /// files into the pane's own set and sends nothing, so a stray one
    /// costs an `u` rather than a write to a running sheep.
    pub(super) fn on_pane_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
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
            // than folded into a silent group or replaced with a wildcard,
            // because every other arm here is named on purpose (a stray
            // `KeyPress` variant should not fall through unnoticed), and a
            // wildcard would defeat that for every variant, not just this
            // one. If the guard above is ever removed, this is what Quit
            // still does.
            KeyPress::Quit => return Effect::Quit,
            // Backs out one level at a time: the close dialog's own
            // question first, if there is one, else the pane. `Escape`
            // closes rather than cascading to a filter clear or a quit,
            // exactly as it does on the settings screen.
            //
            // The dialog is asked before anything is taken: `esc` used to
            // write first and ask second, which missed the very edit that
            // made this pane's `Escape` worth asking about. Now nothing
            // leaves the pane until the question is answered, one way or
            // another.
            //
            // One press, not two: the field's help draws unconditionally
            // now, so nothing waits behind a second `esc` for it.
            KeyPress::Escape => {
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
                let writes = self.take_pane_writes();
                self.close_pane();
                if !writes.is_empty() {
                    return Effect::SendAll(writes);
                }
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
            | KeyPress::SecretDelete
            | KeyPress::Collapse
            // Bound only in the close dialog; with none up, `c` is a stray
            // key the same way an action key is.
            | KeyPress::Continue => {}
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
            // `j`/`k` never start on a row the new tab does not draw.
            //
            // The `dogs` tab lists its dogs the first time it comes up.
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
