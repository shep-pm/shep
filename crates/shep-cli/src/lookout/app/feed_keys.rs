//! The keys the two feed screens share: the bleats pane and the sheep pane's
//! embedded feed claim the same keys ahead of their own keymaps, cycle the
//! same two axes, and type into the same match box.

use super::*;

/// Which feed axis a cycle key moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedAxis {
    /// `o`: whose stream's lines survive.
    Stream,
    /// `m`: the minimum level a line must meet.
    Level,
}

/// Cycle `axis` on `pane`'s feed, one step forward.
///
/// The stream axis runs `None` (both streams) -> `Out` -> `Err` -> `None`.
/// The level axis runs every `Level` in ascending order and back to `None`:
/// a cycle that skipped `None` would leave an operator who set a floor
/// unable to see an unclassifiable line again without closing the pane.
fn cycle_feed_axis(pane: &mut BleatsPane, axis: FeedAxis) {
    match axis {
        FeedAxis::Stream => {
            let next = match pane.filters().stream {
                None => Some(Stream::Out),
                Some(Stream::Out) => Some(Stream::Err),
                Some(Stream::Err) => None,
            };
            pane.set_stream(next);
        }
        FeedAxis::Level => {
            let next = match pane.filters().min_level {
                None => Some(Level::Trace),
                Some(Level::Trace) => Some(Level::Debug),
                Some(Level::Debug) => Some(Level::Info),
                Some(Level::Info) => Some(Level::Warn),
                Some(Level::Warn) => Some(Level::Error),
                Some(Level::Error) => None,
            };
            pane.set_min_level(next);
        }
    }
}

impl App {
    /// The feed keys both screens claim, run ahead of each pane's own keymap.
    /// `feed` names the feed they act on: [`Self::bleats_pane_mut`] for the
    /// full-screen pane, [`Self::sheep_feed_mut`] for the embedded one.
    ///
    /// `Some` for a key this function owns, `None` for the caller's keymap to
    /// answer. A key added here is answered on both screens at once.
    pub(super) fn on_feed_key(
        &mut self,
        key: KeyPress,
        feed: impl FnOnce(&mut Self) -> Option<&mut BleatsPane>,
    ) -> Option<Effect> {
        match key {
            // `o` and `m`: the two cycle axes, one step forward each.
            KeyPress::StreamCycle => {
                if let Some(pane) = feed(self) {
                    cycle_feed_axis(pane, FeedAxis::Stream);
                }
                Some(Effect::None)
            }
            KeyPress::LevelCycle => {
                if let Some(pane) = feed(self) {
                    cycle_feed_axis(pane, FeedAxis::Level);
                }
                Some(Effect::None)
            }
            // `/`: opens the match box. `begin_match_edit` remembers what the
            // axis held, so an abandoned edit restores it rather than losing
            // it, and the keystrokes that follow reach
            // [`Self::on_feed_text_key`] through [`Self::on_text_key`].
            KeyPress::FilterStart => {
                if let Some(pane) = feed(self) {
                    pane.begin_match_edit();
                }
                self.mode = InputMode::Text;
                Some(Effect::None)
            }
            _ => None,
        }
    }

    /// The armed-confirm prelude, run ahead of the dashboard's keymap and
    /// ahead of the sheep pane's.
    ///
    /// A cancelling keypress is consumed: a stray `j` cancels the confirm
    /// rather than also moving the selection the next Enter would act on.
    /// `Ctrl-C` is the one key a cancel does not consume. `Some` when
    /// something was armed and this key went to it.
    pub(super) fn on_armed_key(&mut self, key: KeyPress) -> Option<Effect> {
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.stage == Stage::Armed)
        {
            if key == KeyPress::Confirm {
                return Some(self.confirm());
            }
            if key == KeyPress::Quit {
                return Some(Effect::Quit);
            }
            self.action = None;
            return Some(Effect::None);
        }
        None
    }

    /// The match box, in force while a feed owns [`InputMode::Text`]. `feed`
    /// names the pane the box is typed into.
    ///
    /// Typing narrows live through [`BleatsPane::set_match`], while
    /// [`KeyPress::TextAbandon`] restores what [`BleatsPane::begin_match_edit`]
    /// saw rather than clearing an axis that may already hold a chip.
    pub(super) fn on_feed_text_key(
        &mut self,
        key: KeyPress,
        feed: impl FnOnce(&mut Self) -> Option<&mut BleatsPane>,
    ) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::TextChar(_) | KeyPress::TextBackspace | KeyPress::TextEdit(_) => {
                if let Some(pane) = feed(self) {
                    pane.edit_match_box(|text| text.handle(key));
                }
                Effect::None
            }
            KeyPress::TextApply => {
                self.mode = InputMode::Normal;
                if let Some(pane) = feed(self) {
                    pane.commit_match_edit();
                }
                Effect::None
            }
            KeyPress::TextAbandon => {
                self.mode = InputMode::Normal;
                if let Some(pane) = feed(self) {
                    pane.abandon_match_edit();
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::pane_bleats::{Filters, MatchKind};
    use crate::lookout::tail::Tail;

    /// A feed with lines that each fail exactly one axis, so a script that
    /// sets all three visibly narrows something.
    fn tail() -> Tail {
        Tail {
            lines: vec![
                fixtures::line(Stream::Err, "ERROR pool exhausted"),
                fixtures::line(Stream::Out, "INFO pool warming"),
                fixtures::line(Stream::Err, "ERROR disk full"),
            ],
            missed_lines: 0,
            missed_bytes: 0,
            read_bytes: 128,
            note: None,
        }
    }

    /// The keys both feed screens answer, in the order an operator presses
    /// them: two `o`s, three `m`s, a match box typed into and applied, a
    /// second box abandoned, then `f` and `w`.
    fn feed_script() -> Vec<KeyPress> {
        vec![
            KeyPress::StreamCycle,
            KeyPress::StreamCycle,
            KeyPress::LevelCycle,
            KeyPress::LevelCycle,
            KeyPress::LevelCycle,
            KeyPress::FilterStart,
            KeyPress::TextChar('p'),
            KeyPress::TextChar('o'),
            KeyPress::TextChar('x'),
            KeyPress::TextBackspace,
            KeyPress::TextApply,
            KeyPress::FilterStart,
            KeyPress::TextChar('z'),
            KeyPress::TextAbandon,
            KeyPress::FollowToggle,
            KeyPress::WrapToggle,
        ]
    }

    /// The feed state both screens can be asked for: the axes, in the order
    /// they were set, plus the follow and wrap flags and whatever the match
    /// box holds. The pane's own scroll and column figures are left out: the
    /// full-screen pane scrolls and the embedded feed does not.
    fn feed_state(pane: &BleatsPane) -> (Filters, bool, bool, Option<String>) {
        (
            pane.filters().clone(),
            pane.following(),
            pane.wrapped(),
            pane.match_editing().map(|text| text.as_str().to_string()),
        )
    }

    /// One key script, run through the full-screen bleats pane and through
    /// the sheep pane's embedded feed, on the same sheep over the same feed.
    ///
    /// Asserts the two answers are equal and that the script actually set
    /// something: two feeds left untouched compare equal too, and that pass
    /// would mean nothing.
    #[test]
    fn the_bleats_pane_and_the_sheep_feed_answer_the_same_keys_the_same_way() {
        let mut bleats = fixtures::with_feed(tail());
        let _ = bleats.update(Msg::Key(KeyPress::Bleats));
        for key in feed_script() {
            bleats.update(Msg::Key(key));
        }
        let bleats_state = feed_state(bleats.bleats_pane().expect("the pane is open"));

        let mut sheep = fixtures::with_feed(tail());
        let _ = sheep.update(Msg::Key(KeyPress::Confirm));
        for key in feed_script() {
            sheep.update(Msg::Key(key));
        }
        let sheep_state = feed_state(sheep.sheep_pane().expect("the pane is open").feed());

        assert_eq!(bleats_state, sheep_state);
        assert_eq!(bleats.mode(), sheep.mode(), "both closed the box");

        let (filters, following, wrapped, box_text) = &bleats_state;
        assert_eq!(filters.stream, Some(Stream::Err), "two o's");
        assert_eq!(filters.min_level, Some(Level::Info), "three m's");
        assert_eq!(filters.match_text(), Some("po"), "the typed match survived");
        assert_eq!(filters.match_kind(), Some(MatchKind::Literal));
        assert!(!following, "f turned following off");
        assert!(wrapped, "w turned wrapping on");
        assert!(box_text.is_none(), "the box closed on the abandon");
    }

    /// What the dispatcher claims, read on both screens: the three feed keys
    /// are answered there, and a key it leaves go falls to the pane's own
    /// keymap. A pane wiring one of the three on its own would have to answer
    /// for it here too.
    #[test]
    fn the_feed_dispatcher_claims_the_same_keys_on_both_screens() {
        let mut bleats = fixtures::with_feed(tail());
        let _ = bleats.update(Msg::Key(KeyPress::Bleats));
        let mut sheep = fixtures::with_feed(tail());
        let _ = sheep.update(Msg::Key(KeyPress::Confirm));

        for key in [
            KeyPress::StreamCycle,
            KeyPress::LevelCycle,
            KeyPress::FilterStart,
        ] {
            assert!(
                bleats.on_feed_key(key, App::bleats_pane_mut).is_some(),
                "the bleats pane answers {key:?} through the dispatcher"
            );
            assert!(
                sheep.on_feed_key(key, App::sheep_feed_mut).is_some(),
                "and so does the embedded feed"
            );
        }
        assert_eq!(bleats.mode(), InputMode::Text);
        assert_eq!(sheep.mode(), InputMode::Text, "both opened their own box");

        for key in [KeyPress::Escape, KeyPress::SelectDown, KeyPress::MatchPrev] {
            assert!(bleats.on_feed_key(key, App::bleats_pane_mut).is_none());
            assert!(sheep.on_feed_key(key, App::sheep_feed_mut).is_none());
        }
    }

    /// The prelude both screens run: `Enter` confirms, everything else
    /// cancels and is consumed, on the dashboard and inside the sheep pane.
    #[test]
    fn an_armed_confirm_is_answered_the_same_way_on_both_screens() {
        for open_pane in [false, true] {
            let mut app = allowed();
            if open_pane {
                let _ = app.update(Msg::Key(KeyPress::Confirm));
                assert!(matches!(app.body(), Body::Sheep(_)), "the pane opened");
            }
            let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_some(), "armed");

            let before = app.selected();
            assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
            assert!(app.action().is_none(), "the stray j cancelled");
            assert_eq!(app.selected(), before, "and was consumed doing it");

            let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
                panic!("Enter sends the armed action");
            };
            assert!(matches!(sent, Sent::Action { .. }), "{sent:?}");
        }
    }
}
