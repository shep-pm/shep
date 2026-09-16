use super::super::*;

impl App {
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
    pub(super) fn disarm_settings_candidate(&mut self) -> bool {
        if let Some(settings) = self.settings_mut()
            && settings.is_armed()
        {
            settings.pending = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::lookout::view::fixtures;

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
