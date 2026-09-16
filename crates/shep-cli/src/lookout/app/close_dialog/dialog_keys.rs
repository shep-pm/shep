use super::super::*;

impl App {
    /// The dialog's own keymap: `R` and `L` write and hold their verb until
    /// the writes are answered ([`Self::answer_close`]), `c` writes and
    /// holds nothing. `Escape` closes the dialog and not the pane, which is
    /// the difference from the menu this replaces: `esc` here means keep
    /// editing, so the filed set stays filed and nothing is written.
    pub(in crate::lookout::app) fn on_close_dialog_key(&mut self, key: KeyPress) -> Effect {
        match key {
                KeyPress::Quit => Effect::Quit,
                KeyPress::Action(verb @ (ActionVerb::Reload | ActionVerb::Restart)) => {
                    self.answer_close(Some(verb))
                }
                KeyPress::Continue => self.answer_close(None),
                KeyPress::Escape => {
                    self.close_dialog = None;
                    Effect::None
                }
                // `Help` among them: the dialog owns the keyboard until it is
                // answered, and the keymap overlay is not an exception to that.
                KeyPress::Action(ActionVerb::Stop)
                | KeyPress::SelectUp
                | KeyPress::SelectDown
                | KeyPress::SelectFirst
                | KeyPress::SelectLast
                | KeyPress::Refresh
                | KeyPress::Confirm
                | KeyPress::Edit
                | KeyPress::Cycle
                | KeyPress::Help
                | KeyPress::Settings
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
                | KeyPress::Collapse => Effect::None,
                KeyPress::StreamCycle
                | KeyPress::LevelCycle
                | KeyPress::PageDown
                | KeyPress::PageUp
                | KeyPress::FollowToggle
                | KeyPress::WrapToggle
                | KeyPress::MatchNext
                | KeyPress::MatchPrev
                | KeyPress::Bleats
                // `NextGroup`/`Group`/`Undo` belong to the config pane: no
                // other screen has groups to walk or a filed edit set to undo.
                | KeyPress::NextGroup
                | KeyPress::Group(_)
                | KeyPress::Undo => Effect::None,
            }
    }
}
