//! The top-level key dispatch, the keymap overlay and the filter box.

use super::*;

impl App {
    /// Raises the overlay. Reached from every body's own `Help` arm, so
    /// each body's cancel and dialog guards have already run by the time
    /// this is called: `h` cancels an armed confirm and is consumed, and a
    /// close dialog never reaches its body's match at all.
    pub(super) fn open_keymap(&mut self) -> Effect {
        self.keymap_open = true;
        Effect::None
    }

    /// The overlay's own keymap while it is up.
    ///
    /// Four keystrokes do something across three arms, since `map_key` folds
    /// `h` and `?` into one `Help`, and everything else is swallowed. Swallowing
    /// is the point: the box covers the flock table, so a `j` that reached
    /// the reducer would move a selection the operator cannot see.
    fn on_keymap_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Help | KeyPress::Escape => {
                self.keymap_open = false;
                Effect::None
            }
            _ => Effect::None,
        }
    }

    pub(super) fn on_key(&mut self, key: KeyPress) -> Effect {
        // While the box is open every key is text.
        if self.mode == InputMode::Text {
            return self.on_text_key(key);
        }
        // The overlay owns the keyboard while it is up, ahead of every pane
        // below. Behind text mode, not in front of it: `h` typed into an
        // open filter box is a letter, so the overlay can never be raised
        // from inside one.
        if self.keymap_open {
            return self.on_keymap_key(key);
        }
        // The config pane owns the keyboard while it is open, ahead of the
        // settings screen and the armed-confirm check below. The two
        // screens cannot coexist, so this ordering is a documentation
        // choice, not a correctness one.
        if self.config_pane().is_some() {
            return self.on_pane_key(key);
        }
        // The settings screen owns its own keymap while it is open.
        if self.settings().is_some() {
            return self.on_settings_key(key);
        }
        // The bleats pane owns the keyboard while it is open, the same as
        // the other two full-screen panes above.
        if self.bleats_pane().is_some() {
            return self.on_bleats_key(key);
        }
        // The secrets pane, the same as the three panes above.
        if matches!(self.body, Body::Secrets(_)) {
            return self.on_secrets_key(key);
        }
        // The sheep pane owns the keyboard while it is open, the same as
        // the three full-screen panes above. `Body` holds only one at a
        // time, so this ordering is documentation, not correctness, the
        // same as theirs.
        if self.sheep_pane().is_some() {
            return self.on_sheep_pane_key(key);
        }
        // A cancelling keypress is consumed: a stray `j` cancels the confirm
        // and does not also move the selection, or the next reflexive Enter
        // acts on a target the operator lost track of. Cancelling is silent.
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.stage == Stage::Armed)
        {
            if key == KeyPress::Confirm {
                return self.confirm();
            }
            // The one key the cancel does not consume: an operator whose
            // Ctrl-C does nothing reaches for `kill -9`, past every restore
            // path `crate::lookout::term` has. Quitting discards the confirm.
            if key == KeyPress::Quit {
                return Effect::Quit;
            }
            self.action = None;
            return Effect::None;
        }
        self.notice = None;
        match key {
            KeyPress::Quit => Effect::Quit,
            // The one key whose meaning depends on state, and the bar reads
            // `esc clear` for exactly as long as clearing is what it does.
            KeyPress::Escape => {
                if self.filter.is_empty() {
                    Effect::Quit
                } else {
                    self.set_filter(String::new())
                }
            }
            // Once the link task has ended its poll receiver is gone, so an
            // `Effect::PollNow` would be silence with no reason for it.
            KeyPress::Refresh => {
                if matches!(self.link, Link::Lost { .. }) {
                    self.notice = Some(Notice {
                        text: LINK_GONE.to_string(),
                        grave: true,
                    });
                    return Effect::None;
                }
                Effect::PollNow
            }
            KeyPress::SelectUp => self.select_by(-1),
            KeyPress::SelectDown => self.select_by(1),
            KeyPress::SelectFirst => self.select_at(0, 1),
            KeyPress::SelectLast => self.select_at(self.visible_len().saturating_sub(1), -1),
            KeyPress::Action(verb) => self.arm(verb),
            // An armed confirm (including one already in flight) owns
            // `Enter` before it ever reaches here: the routing rule above
            // fires only on `Stage::Armed`. With nothing armed, `Enter`
            // opens the sheep pane on the selected row, or does nothing on
            // a dog, a group or a fold header, none of which is a sheep to
            // open one on.
            KeyPress::Confirm => self.open_sheep_pane(),
            KeyPress::FilterStart => {
                self.mode = InputMode::Text;
                Effect::None
            }
            // `TextChar`/`TextBackspace`/`TextApply`/`TextAbandon` reach here
            // only from text mode, already branched above. `map_key` also
            // sends `Remove`/`StepUp`/`StepDown` from Normal mode
            // (`d`/`K`/`J`), so those land here too, just inert.
            // `NextGroup`/`Group`/`Undo` belong to the config pane: no other
            // screen has groups to walk or a filed edit set to undo.
            KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo
            | KeyPress::Continue => Effect::None,
            // The read, not the open: the screen opens only once
            // `Msg::Settings` lands.
            KeyPress::Settings => Effect::LoadSettings,
            // Opens with an empty model; `Msg::Secrets` fills it once the
            // read lands. Closing again is `on_secrets_key`'s job, reached
            // only once `self.body` is already `Body::Secrets`, the same
            // split `KeyPress::Settings`/`on_settings_key` uses.
            KeyPress::Secrets => {
                self.body = Body::Secrets(SecretsPane {
                    model: Box::default(),
                    tab: 0,
                    selected: 0,
                    collapsed: HashSet::new(),
                    reveal: None,
                    pending_reveal: None,
                    armed: None,
                    typing: None,
                });
                Effect::LoadSecrets
            }
            // Meaningful only inside the secrets pane, which owns the
            // keyboard while `self.body` is `Body::Secrets`; reached here
            // only from the dashboard, where there is no tab to move and no
            // secret selected to show.
            KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::SecretDelete => Effect::None,
            // Also the read, not the open: the pane shows the shepherd's
            // answer or nothing. `selected_row` is `None` for a group too,
            // but a group's name is what `Request::SheepConfig` wants, so
            // `e` still works on a multi-instance app's default row.
            KeyPress::Edit => self.ask_for_config(),
            // `space` acts only on the settings screen.
            KeyPress::Cycle => Effect::None,
            // Raises the keymap overlay, reached only once the
            // armed-confirm check above has already had its turn: an
            // armed confirm consumes `h` as a cancel rather than letting
            // it reach here.
            KeyPress::Help => self.open_keymap(),
            // Toggles rather than opening: pressing it twice is where it
            // began. But `ByFold` collapses a grouped app to its
            // `RowKey::Group` header alone (`push_fold_group_rows`), so a
            // `RowKey::Sheep` row the flat view was pointing at can vanish
            // from `visible_rows()` even though the id survives in
            // `self.selected`. `reseat` reads `selected_index`, not id
            // survival, so the same fixup `set_filter` uses applies here.
            KeyPress::FoldView => {
                let previous = self.selected_index();
                self.grouping = match self.grouping {
                    Grouping::Flat => Grouping::ByFold,
                    Grouping::ByFold => Grouping::Flat,
                };
                if self.reseat(previous) && !matches!(self.link, Link::Lost { .. }) {
                    // The cursor moved to a different sheep (or a group
                    // standing in for several), so the feed and lambs panes
                    // are about to describe someone else.
                    return Effect::RefreshSelected;
                }
                Effect::None
            }
            // Only a `RowKey::Fold` header answers to this key; anything
            // else, including no selection at all, is a no-op rather than a
            // refusal, the same silence `Cycle` falls back to outside its own
            // screen. Not `Help` any more: the overlay's own screen is every
            // screen, so `h` acts here rather than falling silent. Left
            // naming `Cycle` alone rather than dropped, because the next
            // reader of this arm wants to know a no-op is deliberate.
            KeyPress::Collapse => {
                if let Some(RowKey::Fold(name)) = self.selected()
                    && !self.collapsed_folds.remove(&name)
                {
                    self.collapsed_folds.insert(name);
                }
                Effect::None
            }
            // Opens on the selected sheep, or does nothing without one, the
            // same shape `KeyPress::Edit` follows above.
            KeyPress::Bleats => self.ask_for_bleats(),
            // All four scroll or cycle a bleats-pane axis, and the
            // dashboard has neither a filter axis nor a feed to scroll;
            // named here rather than left to fall through the arm above,
            // the same way `KeyPress::Bleats` would be ignored on a screen
            // with no bleats pane.
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev => Effect::None,
        }
    }

    /// Puts the keyboard back to [`InputMode::Normal`] when no pane editor
    /// owns it any more.
    ///
    /// [`InputMode::Text`] is remembered on `App` while the buffer it
    /// belongs to lives on the pane, so anything that drops or replaces the
    /// pane can leave the two disagreeing, and a lookout in `Text` mode
    /// with nothing to type into eats every keystroke until `Esc`. A
    /// re-read rebuilds the whole `ConfigPane`, which is exactly that, and
    /// a landed write asks for one.
    ///
    /// Called only from paths where the config pane is the screen in
    /// question. The filter box owns `Text` with no marker but the mode
    /// itself, and it cannot be open while the pane is.
    pub(super) fn release_text_mode_if_unowned(&mut self) {
        if self.mode != InputMode::Text {
            return;
        }
        let owned = self
            .config_pane()
            .is_some_and(|pane| pane.typing().is_some() || pane.env_typing().is_some());
        if !owned {
            self.mode = InputMode::Normal;
        }
    }

    /// The text keymap's router: the filter box while the settings screen is
    /// closed, [`Self::on_settings_text_key`]'s editor while it is open. The
    /// two never both own [`InputMode::Text`].
    fn on_text_key(&mut self, key: KeyPress) -> Effect {
        // Six now, and the split is still total: the config pane, the
        // settings screen, the bleats pane, the secrets pane and the sheep
        // pane cannot coexist with each other (`e`, `s` and `S` reach the
        // dashboard only from the dashboard, and `b`/`↵` only from there
        // too), and none of them
        // coexist with the dashboard's own filter box, which `Msg::Settings`'s
        // own arm closed the window on.
        if self.config_pane().is_some() {
            return self.on_pane_text_key(key);
        }
        if self.settings().is_some() {
            return self.on_settings_text_key(key);
        }
        if self.bleats_pane().is_some() {
            return self.on_bleats_text_key(key);
        }
        if matches!(self.body, Body::Secrets(_)) {
            return self.on_secrets_text_key(key);
        }
        if self.sheep_pane().is_some() {
            return self.on_sheep_feed_text_key(key);
        }
        self.on_filter_text_key(key)
    }

    /// The filter box's keymap.
    ///
    /// Ctrl-C still quits: in raw mode it is a key event, not a signal. Does
    /// not clear [`Self::notice`], unlike normal mode, since a notice can be
    /// raised with no keypress involved; the status bar hides it under the box
    /// and shows it again when the box closes.
    fn on_filter_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::TextChar(typed) => {
                let mut query = self.filter.clone();
                query.push(typed);
                self.set_filter(query)
            }
            KeyPress::TextBackspace => {
                let mut query = self.filter.clone();
                query.pop();
                self.set_filter(query)
            }
            KeyPress::TextApply => {
                self.mode = InputMode::Normal;
                Effect::None
            }
            KeyPress::TextAbandon => {
                self.mode = InputMode::Normal;
                self.set_filter(String::new())
            }
            _ => Effect::None,
        }
    }

    /// Replaces the filter and puts the selection back on a visible sheep: a
    /// keystroke that narrows the query can hide the selected one.
    pub(super) fn set_filter(&mut self, query: String) -> Effect {
        if self.filter == query {
            return Effect::None;
        }
        let previous = self.selected_index();
        self.filter = query;
        if self.reseat(previous) && !matches!(self.link, Link::Lost { .. }) {
            // The cursor moved, so the feed and the lambs are about to describe
            // a different sheep.
            return Effect::RefreshSelected;
        }
        Effect::None
    }

    /// The filter as typed, empty when there is none.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Which keymap is currently in force.
    #[must_use]
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// Whether the keymap overlay is up, for `view` to draw.
    #[must_use]
    pub const fn keymap_open(&self) -> bool {
        self.keymap_open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::view::fixtures;

    /// A `DaemonShutdown` is a notice here, where in `bleats` it precedes a
    /// clean exit.
    #[test]
    fn nothing_but_a_keypress_quits() {
        let (mut app, _) = started();
        for msg in [
            Msg::Event(BusEvent::DaemonShutdown),
            Msg::Event(BusEvent::Dropped { count: 1 }),
            Msg::BusLagged { count: 1 },
            Msg::Retrying { attempt: 5 },
            Msg::Frozen {
                at_local: "2026-08-14 14:32:07".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            assert_ne!(app.update(msg), Effect::Quit);
        }
        assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
    }

    /// `Effect::None`, not `RefreshFeed`: clearing a filter only widens the
    /// visible set, so the selection stays seated and `reseat` is a no-op.
    #[test]
    fn esc_clears_the_filter_instead_of_quitting_while_one_is_set() {
        let mut app = filtered("web");
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert_eq!(app.filter(), "");
        assert_eq!(app.rows().len(), 4);
    }

    #[test]
    fn esc_still_quits_with_no_filter_set() {
        let (mut app, _t0) = started();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::Quit);
    }

    #[test]
    fn the_table_narrows_while_the_query_is_still_being_typed() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        assert_eq!(app.mode(), InputMode::Text);
        for letter in ['w', 'e', 'b'] {
            app.update(Msg::Key(KeyPress::TextChar(letter)));
        }
        assert_eq!(app.rows().len(), 1, "narrowed before Enter");
        app.update(Msg::Key(KeyPress::TextApply));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.rows().len(),
            1,
            "and applying changed nothing but the mode"
        );
    }

    #[test]
    fn backspace_widens_the_table_back_out() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        app.update(Msg::Key(KeyPress::TextChar('w')));
        app.update(Msg::Key(KeyPress::TextChar('z')));
        assert_eq!(app.rows().len(), 0);
        app.update(Msg::Key(KeyPress::TextBackspace));
        assert_eq!(
            app.rows().len(),
            2,
            "wz became w, which matches web and worker"
        );
    }

    #[test]
    fn esc_while_editing_clears_the_filter_and_leaves_the_box() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        app.update(Msg::Key(KeyPress::TextChar('w')));
        app.update(Msg::Key(KeyPress::TextAbandon));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(app.filter(), "");
        assert_eq!(app.rows().len(), 3);
    }

    #[test]
    fn opening_the_filter_takes_a_notice_off_the_bar() {
        let (mut app, _t0) = started();
        app.update(Msg::Event(BusEvent::Dropped { count: 3 }));
        assert!(app.notice().is_some());
        app.update(Msg::Key(KeyPress::FilterStart));
        assert!(app.notice().is_none(), "the box is what the bar shows now");
    }

    #[test]
    fn a_notice_raised_while_typing_is_deferred_and_not_destroyed() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        app.update(Msg::Key(KeyPress::TextChar('w')));
        app.update(Msg::Event(BusEvent::DaemonShutdown));
        app.update(Msg::Key(KeyPress::TextChar('e')));
        assert!(
            app.notice().is_some(),
            "typing did not wipe the shepherd's announcement"
        );
        assert_eq!(app.filter(), "we", "and the box kept the query");
    }

    /// `h` raises the keymap overlay from inside the config pane too: the
    /// field help draws unconditionally (`view::pane::top_lines`), so no
    /// key is needed for it and `h` is free for this instead.
    #[test]
    fn h_opens_the_keymap_overlay_in_the_config_pane() {
        let mut app = fixtures::app_in_sheep_pane();
        pane_to(&mut app, "max_memory");
        assert_eq!(app.update(Msg::Key(KeyPress::Help)), Effect::None);
        assert!(app.config_pane().is_some(), "h closed the pane");
        assert!(app.keymap_open(), "h did not raise the overlay");
    }

    /// `h` raises the keymap overlay from the list sub-screen too, reached
    /// the same way `enter_on_an_array_row_opens_the_list_sub_screen` gets
    /// there: `pane_to` an array field, then `Confirm`. `on_pane_key`
    /// routes to `on_list_key` only once `ConfigPane::list()` is `Some`, so
    /// landing here for real is the only way to exercise its own `Help`
    /// arm rather than the pane's.
    #[test]
    fn h_opens_the_keymap_overlay_from_the_list_sub_screen() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            app.config_pane().unwrap().list().is_some(),
            "the list sub-screen did not open"
        );
        assert_eq!(app.update(Msg::Key(KeyPress::Help)), Effect::None);
        assert!(
            app.config_pane().unwrap().list().is_some(),
            "h closed the list sub-screen"
        );
        assert!(app.keymap_open(), "h did not raise the overlay");
    }

    /// The overlay swallows a movement key rather than letting it reach the
    /// table underneath, which the box is covering.
    ///
    /// Asserts on the selection index, not on the effect: a `j` that
    /// returned `Effect::None` and still moved the cursor is exactly the
    /// bug, and an effect-only assertion would pass through it.
    #[test]
    fn the_overlay_swallows_a_movement_key() {
        let mut app = fixtures::full_app();
        let before = app.selected_index();
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open());
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
        assert_eq!(
            app.selected_index(),
            before,
            "j moved the selection behind the overlay"
        );
        assert!(app.keymap_open(), "j closed the overlay as well");
    }

    /// `h`, `?` and `esc` all close it. `?` reaches here as `Help` too, so
    /// this is one variant tested by the route the operator takes.
    #[test]
    fn the_overlay_closes_on_help_and_on_esc() {
        for closer in [KeyPress::Help, KeyPress::Escape] {
            let mut app = fixtures::full_app();
            let _ = app.update(Msg::Key(KeyPress::Help));
            assert!(app.keymap_open());
            let _ = app.update(Msg::Key(closer));
            assert!(!app.keymap_open(), "{closer:?} did not close it");
        }
    }

    /// `q` still quits, the way it does with 1g's dialog up.
    #[test]
    fn quit_still_quits_with_the_overlay_up() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open(), "the overlay did not open");
        assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
    }

    /// A close dialog owns the keyboard, overlay included: `h` with one up
    /// does not open a box over the question.
    #[test]
    fn the_close_dialog_keeps_the_keyboard_from_the_overlay() {
        let mut app = fixtures::app_with_close_dialog();
        assert!(app.close_dialog().is_some());
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(
            !app.keymap_open(),
            "the overlay opened over an unanswered dialog"
        );
    }

    /// An armed confirm is cancelled by `h` and the overlay does not open,
    /// the same rule `any_other_key_cancels_an_action_armed_inside_the_pane`
    /// already states for every other key: a cancelling press is consumed.
    #[test]
    fn h_cancels_an_armed_confirm_instead_of_opening_the_overlay() {
        let mut app = fixtures::full_app();
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_some());
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.action().is_none(), "h did not cancel the confirm");
        assert!(
            !app.keymap_open(),
            "h cancelled and also opened the overlay"
        );
    }

    /// `Help` joins its siblings' `is_armed()` check: a candidate armed with
    /// `Cycle` is cancelled and consumed rather than left standing behind a
    /// box the operator cannot see past, the same ruling
    /// `h_cancels_an_armed_confirm_instead_of_opening_the_overlay` already
    /// pins for the dashboard's own confirm.
    #[test]
    fn h_cancels_an_armed_settings_candidate_instead_of_opening_the_overlay() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(app.settings().unwrap().pending().is_some());
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(
            app.settings().unwrap().pending().is_none(),
            "h did not cancel the armed candidate"
        );
        assert!(
            !app.keymap_open(),
            "h cancelled and also opened the overlay"
        );
    }

    /// The settings screen's own `Help` arm, reached only once
    /// `Msg::Settings` has actually landed and put `self.body` into
    /// `Body::Settings`: pressing `s` alone leaves the dashboard's own arm
    /// in force, which would open the overlay for the wrong reason.
    #[test]
    fn the_overlay_opens_from_the_settings_screen() {
        let mut app = fixtures::app_in_settings();
        assert!(app.settings().is_some(), "the screen did not open");
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open());
    }

    /// The reducer-level property text mode's ordering actually provides: a
    /// `Help` arriving while `mode == InputMode::Text` must not raise the
    /// overlay, however it arrives. Sent directly rather than through
    /// `TextChar`, which `map_key` already resolves before the reducer ever
    /// sees it: `input.rs` covers that side, and it never emits `Help` in
    /// text mode today. This is the reducer's own guard, kept correct
    /// independent of whatever `map_key` does or is later changed to do.
    #[test]
    fn help_in_text_mode_does_not_raise_the_overlay() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        assert_eq!(app.mode(), InputMode::Text);
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(!app.keymap_open(), "h raised the overlay from text mode");
    }

    /// It opens from the three bodies `Confirm`, `Secrets` and `Bleats` set
    /// synchronously (`self.body` changes on the keystroke itself, no reply
    /// needed). `Edit` and `Settings` open their screens only once a reply
    /// lands, so a `Help` pressed right after either still reaches the
    /// dashboard's own arm, which also opens the overlay but proves nothing
    /// about `on_pane_key` or `on_settings_key`: those two get their own
    /// dedicated tests instead
    /// (`h_opens_the_keymap_overlay_in_the_config_pane`,
    /// `the_overlay_opens_from_the_settings_screen`). The list sub-screen is
    /// not a `Body` at all (it is `ConfigPane::list`, nested inside
    /// `Body::ConfigPane`, and reached by a `pane_to` plus `Confirm` rather
    /// than a single opener from the dashboard), so it was never in scope
    /// for this loop either; it gets its own dedicated test too
    /// (`h_opens_the_keymap_overlay_from_the_list_sub_screen`).
    ///
    /// Each iteration asserts it reached the body before pressing `Help`.
    /// Without that the loop is the shape it was narrowed for: an opener
    /// that stops setting `Body` synchronously leaves the dashboard on
    /// screen, `Help` opens the overlay from there, and the loop passes
    /// three times while testing one body. `Edit` and `Settings` did
    /// exactly that before they were dropped from it.
    #[test]
    fn the_overlay_opens_from_every_synchronously_opened_body() {
        /// An opener paired with the predicate that says it landed.
        type Arrival = (KeyPress, fn(&App) -> bool);

        let reached: [Arrival; 3] = [
            (KeyPress::Secrets, |app| app.secrets_pane_is_open()),
            (KeyPress::Bleats, |app| app.bleats_pane().is_some()),
            (KeyPress::Confirm, |app| app.sheep_pane().is_some()),
        ];
        for (opener, arrived) in reached {
            let mut app = fixtures::full_app();
            let _ = app.update(Msg::Key(opener));
            assert!(
                arrived(&app),
                "{opener:?} did not reach its body, so this iteration would \
                 have tested the dashboard"
            );
            let _ = app.update(Msg::Key(KeyPress::Help));
            assert!(
                app.keymap_open(),
                "the overlay did not open after {opener:?}"
            );
        }
    }

    /// And on a frozen dashboard, where a key list is most wanted.
    #[test]
    fn the_overlay_opens_when_the_link_is_gone() {
        let mut app = fixtures::full_app();
        app.update(Msg::Frozen {
            at_local: "2026-08-16 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open());
    }
}
