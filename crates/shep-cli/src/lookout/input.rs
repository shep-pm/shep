//! `crossterm::event::Event` -> [`KeyPress`]. The whole crossterm-typed edge
//! of the keyboard, kept in one small file so `super::app` never imports a
//! terminal crate and its reducer tests never construct one.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use super::app::{ActionVerb, InputMode, KeyPress};

/// The [`KeyPress`] this event means under `mode`, or `None` for a key
/// lookout does not bind there.
///
/// Only `KeyEventKind::Press` counts: a terminal that reports repeats and
/// releases would otherwise fire an action once per repeat of a held key.
/// `Ctrl-C` is a binding in either mode, since raw mode delivers it as an
/// ordinary key event and there is no `SIGINT` to catch. `Ctrl-D`/`Ctrl-U`
/// are `Normal`-only, so paging the bleats pane never fires behind an
/// operator's back while they are typing a match.
#[must_use]
pub fn map_key(event: &Event, mode: InputMode) -> Option<KeyPress> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(KeyPress::Quit),
            // Guarded on `mode` here, unlike every other binding in this
            // branch: this CONTROL match runs ahead of the `InputMode::Text`
            // check below, so an unguarded `ctrl-d`/`ctrl-u` would page the
            // pane behind an operator's back while they are typing a match
            // into the bleats box. `ctrl-c` stays unguarded on purpose — it
            // quits from the box too, the same way it always has.
            KeyCode::Char('d') if mode == InputMode::Normal => Some(KeyPress::PageDown),
            KeyCode::Char('u') if mode == InputMode::Normal => Some(KeyPress::PageUp),
            _ => None,
        };
    }
    if mode == InputMode::Text {
        return match key.code {
            // SHIFT stays: crossterm delivers a capital as `Char('W')` with
            // SHIFT set. ALT is filtered, since `Alt-w` is never a letter.
            KeyCode::Char(typed) if !key.modifiers.contains(KeyModifiers::ALT) => {
                Some(KeyPress::TextChar(typed))
            }
            KeyCode::Backspace => Some(KeyPress::TextBackspace),
            KeyCode::Enter => Some(KeyPress::TextApply),
            KeyCode::Esc => Some(KeyPress::TextAbandon),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Some(KeyPress::Quit),
        KeyCode::Esc => Some(KeyPress::Escape),
        KeyCode::Char('/') => Some(KeyPress::FilterStart),
        KeyCode::Char('j') | KeyCode::Down => Some(KeyPress::SelectDown),
        KeyCode::Char('k') | KeyCode::Up => Some(KeyPress::SelectUp),
        KeyCode::Char('g') | KeyCode::Home => Some(KeyPress::SelectFirst),
        KeyCode::Char('G') | KeyCode::End => Some(KeyPress::SelectLast),
        KeyCode::Char('r') => Some(KeyPress::Refresh),
        KeyCode::Char('x') => Some(KeyPress::Action(ActionVerb::Stop)),
        KeyCode::Char('R') => Some(KeyPress::Action(ActionVerb::Restart)),
        KeyCode::Char('L') => Some(KeyPress::Action(ActionVerb::Reload)),
        KeyCode::Char('s') => Some(KeyPress::Settings),
        KeyCode::Char('S') => Some(KeyPress::Secrets),
        KeyCode::Char('v') => Some(KeyPress::Reveal),
        KeyCode::Char('y') => Some(KeyPress::Copy),
        KeyCode::Left => Some(KeyPress::TabPrev),
        KeyCode::Right => Some(KeyPress::TabNext),
        // The secrets pane's tab row names these two in so many words.
        // Nothing else in either keymap binds them, so they cost nothing
        // elsewhere: on the dashboard a tab move means nothing and lands on
        // `Effect::None`. Crossterm delivers shift-tab as its own `BackTab`.
        KeyCode::Tab => Some(KeyPress::TabNext),
        KeyCode::BackTab => Some(KeyPress::TabPrev),
        KeyCode::Char('e') => Some(KeyPress::Edit),
        KeyCode::Char('h') => Some(KeyPress::Help),
        KeyCode::Char(' ') => Some(KeyPress::Cycle),
        KeyCode::Char('d') => Some(KeyPress::ListRemove),
        KeyCode::Char('D') => Some(KeyPress::SecretDelete),
        KeyCode::Char('K') => Some(KeyPress::ListMoveUp),
        KeyCode::Char('J') => Some(KeyPress::ListMoveDown),
        KeyCode::Char('F') => Some(KeyPress::FoldView),
        KeyCode::Char('z') => Some(KeyPress::Collapse),
        KeyCode::Char('b') => Some(KeyPress::Bleats),
        KeyCode::Char('o') => Some(KeyPress::StreamCycle),
        KeyCode::Char('m') => Some(KeyPress::LevelCycle),
        KeyCode::Char('f') => Some(KeyPress::FollowToggle),
        KeyCode::Char('w') => Some(KeyPress::WrapToggle),
        KeyCode::Char('n') => Some(KeyPress::MatchNext),
        KeyCode::Char('N') => Some(KeyPress::MatchPrev),
        KeyCode::Enter => Some(KeyPress::Confirm),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn every_bound_key_resolves_to_its_press() {
        assert_eq!(
            map_key(&key(KeyCode::Char('q')), InputMode::Normal),
            Some(KeyPress::Quit)
        );
        assert_eq!(
            map_key(&key(KeyCode::Esc), InputMode::Normal),
            Some(KeyPress::Escape)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('j')), InputMode::Normal),
            Some(KeyPress::SelectDown)
        );
        assert_eq!(
            map_key(&key(KeyCode::Down), InputMode::Normal),
            Some(KeyPress::SelectDown)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('k')), InputMode::Normal),
            Some(KeyPress::SelectUp)
        );
        assert_eq!(
            map_key(&key(KeyCode::Up), InputMode::Normal),
            Some(KeyPress::SelectUp)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('g')), InputMode::Normal),
            Some(KeyPress::SelectFirst)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('G')), InputMode::Normal),
            Some(KeyPress::SelectLast)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('r')), InputMode::Normal),
            Some(KeyPress::Refresh)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('x')), InputMode::Normal),
            Some(KeyPress::Action(ActionVerb::Stop))
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('R')), InputMode::Normal),
            Some(KeyPress::Action(ActionVerb::Restart))
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('L')), InputMode::Normal),
            Some(KeyPress::Action(ActionVerb::Reload))
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('s')), InputMode::Normal),
            Some(KeyPress::Settings)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char(' ')), InputMode::Normal),
            Some(KeyPress::Cycle)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('e')), InputMode::Normal),
            Some(KeyPress::Edit)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('E')), InputMode::Normal),
            None,
            "the config pane is lower-case `e`; `E` is unbound"
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('h')), InputMode::Normal),
            Some(KeyPress::Help)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('d')), InputMode::Normal),
            Some(KeyPress::ListRemove)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('D')), InputMode::Normal),
            Some(KeyPress::SecretDelete)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('K')), InputMode::Normal),
            Some(KeyPress::ListMoveUp)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('J')), InputMode::Normal),
            Some(KeyPress::ListMoveDown)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('F')), InputMode::Normal),
            Some(KeyPress::FoldView)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('z')), InputMode::Normal),
            Some(KeyPress::Collapse)
        );
    }

    /// `b` opens the full-screen bleats pane. Pinned because `map_key`
    /// dispatches on mode rather than pane, so a key taken here is taken
    /// everywhere in `Normal`.
    #[test]
    fn b_opens_the_bleats_pane() {
        assert_eq!(
            map_key(&key(KeyCode::Char('b')), InputMode::Normal),
            Some(KeyPress::Bleats)
        );
    }

    #[test]
    fn the_movement_keys_are_unchanged_and_now_mean_selection() {
        assert_eq!(
            map_key(&key(KeyCode::Char('j')), InputMode::Normal),
            Some(KeyPress::SelectDown)
        );
        assert_eq!(
            map_key(&key(KeyCode::Down), InputMode::Normal),
            Some(KeyPress::SelectDown)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('k')), InputMode::Normal),
            Some(KeyPress::SelectUp)
        );
        assert_eq!(
            map_key(&key(KeyCode::Up), InputMode::Normal),
            Some(KeyPress::SelectUp)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('g')), InputMode::Normal),
            Some(KeyPress::SelectFirst)
        );
        assert_eq!(
            map_key(&key(KeyCode::Home), InputMode::Normal),
            Some(KeyPress::SelectFirst)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('G')), InputMode::Normal),
            Some(KeyPress::SelectLast)
        );
        assert_eq!(
            map_key(&key(KeyCode::End), InputMode::Normal),
            Some(KeyPress::SelectLast)
        );
    }

    #[test]
    fn capital_s_opens_the_secrets_pane_and_lower_s_still_opens_settings() {
        assert_eq!(
            map_key(&key(KeyCode::Char('S')), InputMode::Normal),
            Some(KeyPress::Secrets)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('s')), InputMode::Normal),
            Some(KeyPress::Settings),
            "the settings screen keeps its own key"
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('g')), InputMode::Normal),
            Some(KeyPress::SelectFirst),
            "the frame wanted `g` for secrets; `g` is still SelectFirst"
        );
    }

    #[test]
    fn lower_v_is_the_reveal() {
        assert_eq!(
            map_key(&key(KeyCode::Char('v')), InputMode::Normal),
            Some(KeyPress::Reveal)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('V')), InputMode::Normal),
            None,
            "one key puts a value on screen, and it is not a shifted one"
        );
    }

    #[test]
    fn the_arrow_keys_move_the_environment_tab() {
        assert_eq!(
            map_key(&key(KeyCode::Left), InputMode::Normal),
            Some(KeyPress::TabPrev)
        );
        assert_eq!(
            map_key(&key(KeyCode::Right), InputMode::Normal),
            Some(KeyPress::TabNext)
        );
        assert_eq!(
            map_key(&key(KeyCode::Up), InputMode::Normal),
            Some(KeyPress::SelectUp),
            "the vertical arrows keep the meaning they already have"
        );
    }

    /// The secrets pane's tab row reads `<-/-> or tab`, and a caption that
    /// names a key nothing binds is a caption that lies.
    #[test]
    fn tab_and_shift_tab_move_the_environment_tab_the_caption_names_them() {
        assert_eq!(
            map_key(&key(KeyCode::Tab), InputMode::Normal),
            Some(KeyPress::TabNext)
        );
        assert_eq!(
            map_key(
                &Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
                InputMode::Normal
            ),
            Some(KeyPress::TabPrev),
            "crossterm delivers shift-tab as `BackTab`, with SHIFT set"
        );
        assert_eq!(
            map_key(&key(KeyCode::Tab), InputMode::Text),
            None,
            "and neither reaches an open input, where a tab is not a character"
        );
    }

    #[test]
    fn ctrl_c_quits_because_raw_mode_swallows_the_signal() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(map_key(&event, InputMode::Normal), Some(KeyPress::Quit));
        assert_eq!(map_key(&key(KeyCode::Char('c')), InputMode::Normal), None);
    }

    #[test]
    fn only_a_press_counts() {
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(map_key(&Event::Key(release), InputMode::Normal), None);

        let mut repeat = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(map_key(&Event::Key(repeat), InputMode::Normal), None);
    }

    #[test]
    fn typing_q_while_editing_types_a_letter() {
        assert_eq!(
            map_key(&key(KeyCode::Char('q')), InputMode::Text),
            Some(KeyPress::TextChar('q'))
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('q')), InputMode::Normal),
            Some(KeyPress::Quit)
        );
    }

    #[test]
    fn the_text_mode_binds_exactly_the_box_s_keys() {
        assert_eq!(
            map_key(&key(KeyCode::Backspace), InputMode::Text),
            Some(KeyPress::TextBackspace)
        );
        assert_eq!(
            map_key(&key(KeyCode::Enter), InputMode::Text),
            Some(KeyPress::TextApply)
        );
        assert_eq!(
            map_key(&key(KeyCode::Esc), InputMode::Text),
            Some(KeyPress::TextAbandon)
        );
        let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(map_key(&ctrl_c, InputMode::Text), Some(KeyPress::Quit));
        assert_eq!(map_key(&key(KeyCode::F(5)), InputMode::Text), None);
    }

    #[test]
    fn a_shifted_letter_is_still_a_letter_in_the_box() {
        let shifted = Event::Key(KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT));
        assert_eq!(
            map_key(&shifted, InputMode::Text),
            Some(KeyPress::TextChar('W'))
        );
    }

    /// `o` and `m` are global bindings, taken here so the bleats pane can
    /// cycle its stream and minimum-level axes; a key taken in `map_key` is
    /// taken everywhere in `Normal`, the same note `b_opens_the_bleats_pane`
    /// makes.
    #[test]
    fn o_and_m_cycle_the_stream_and_level_axes() {
        assert_eq!(
            map_key(&key(KeyCode::Char('o')), InputMode::Normal),
            Some(KeyPress::StreamCycle)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('m')), InputMode::Normal),
            Some(KeyPress::LevelCycle)
        );
    }

    /// `f` cycles the bleats pane's follow flag; a global binding, ignored
    /// on the dashboard the same way `o`/`m` are.
    #[test]
    fn f_toggles_follow() {
        assert_eq!(
            map_key(&key(KeyCode::Char('f')), InputMode::Normal),
            Some(KeyPress::FollowToggle)
        );
    }

    /// `ctrl-d`/`ctrl-u` page the bleats pane, and only fire in `Normal`:
    /// unguarded, they would page the pane behind an operator's back while
    /// the match box owns `InputMode::Text`.
    #[test]
    fn ctrl_d_and_ctrl_u_page_only_in_normal_mode() {
        let ctrl_d = Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        let ctrl_u = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(
            map_key(&ctrl_d, InputMode::Normal),
            Some(KeyPress::PageDown)
        );
        assert_eq!(map_key(&ctrl_u, InputMode::Normal), Some(KeyPress::PageUp));
        assert_eq!(map_key(&ctrl_d, InputMode::Text), None);
        assert_eq!(map_key(&ctrl_u, InputMode::Text), None);
    }

    #[test]
    fn slash_opens_the_filter_in_normal_mode() {
        assert_eq!(
            map_key(&key(KeyCode::Char('/')), InputMode::Normal),
            Some(KeyPress::FilterStart)
        );
    }

    /// `w` toggles the bleats pane's wrap; `n`/`N` step between matches. All
    /// three are global bindings, the same way `o`/`m`/`f` are: `map_key`
    /// dispatches on mode alone, ignored on the dashboard.
    #[test]
    fn w_and_n_and_shift_n_are_bound() {
        assert_eq!(
            map_key(&key(KeyCode::Char('w')), InputMode::Normal),
            Some(KeyPress::WrapToggle)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('n')), InputMode::Normal),
            Some(KeyPress::MatchNext)
        );
        assert_eq!(
            map_key(&key(KeyCode::Char('N')), InputMode::Normal),
            Some(KeyPress::MatchPrev)
        );
    }
}
