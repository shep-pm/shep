use super::*;
use crate::commands::dogs::{Prompt, Review};
use crate::lookout::view::fixtures;

fn prompt() -> Prompt {
    Prompt::for_review("metrics", "adopt", &Review::Undeclared, None)
        .expect("an undeclared dog is asked")
}

fn grant(tag: u64) -> Modal {
    Modal::grant(tag, prompt())
}

/// What the event loop does after a draw that showed the open modal in full.
fn drawn(app: &mut App) {
    app.note_modal_reach(Some(0));
}

#[test]
fn enter_answers_a_grant_yes_for_its_tag() {
    let mut app = fixtures::full_app();
    app.open_modal(grant(7));
    drawn(&mut app);

    let effect = app.update(Msg::Key(KeyPress::Confirm));

    assert_eq!(effect, Effect::ModalAnswered { tag: 7, yes: true });
    assert!(app.modal().is_none());
}

#[test]
fn any_other_key_answers_a_grant_no_for_its_tag() {
    let others = [
        KeyPress::Quit,
        KeyPress::Escape,
        KeyPress::Refresh,
        KeyPress::SelectDown,
        KeyPress::Action(ActionVerb::Stop),
        KeyPress::TextChar('y'),
        KeyPress::Help,
    ];
    for key in others {
        let mut app = fixtures::full_app();
        app.open_modal(grant(9));
        drawn(&mut app);

        let effect = app.update(Msg::Key(key));

        assert_eq!(
            effect,
            Effect::ModalAnswered { tag: 9, yes: false },
            "{key:?}"
        );
        assert!(app.modal().is_none(), "{key:?}");
    }
}

#[test]
fn a_second_modal_waits_and_shows_once_the_first_is_answered() {
    let mut app = fixtures::full_app();
    app.open_modal(grant(1));
    app.open_modal(Modal::confirm(2, "quit lookout?".to_owned()));
    drawn(&mut app);

    assert!(matches!(app.modal(), Some(Modal::Grant { tag: 1, .. })));

    let first = app.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(first, Effect::ModalAnswered { tag: 1, yes: true });
    assert!(matches!(app.modal(), Some(Modal::Confirm { tag: 2, .. })));

    drawn(&mut app);
    let second = app.update(Msg::Key(KeyPress::Escape));
    assert_eq!(second, Effect::ModalAnswered { tag: 2, yes: false });
    assert!(app.modal().is_none());
}

/// `x` would arm a stop and `q` would quit, `/` would open the filter box,
/// and `s` would load settings, were the dashboard to see them.
#[test]
fn a_dashboard_key_does_nothing_but_answer_the_modal() {
    let dashboard_keys = [
        KeyPress::Quit,
        KeyPress::Action(ActionVerb::Stop),
        KeyPress::FilterStart,
        KeyPress::Settings,
        KeyPress::Secrets,
        KeyPress::Edit,
        KeyPress::Help,
    ];
    for key in dashboard_keys {
        let mut app = fixtures::app_with_a_dog_selected_and_control();
        let before = app.selected_index();
        app.open_modal(Modal::confirm(3, "go on?".to_owned()));
        drawn(&mut app);

        let effect = app.update(Msg::Key(key));

        assert_eq!(
            effect,
            Effect::ModalAnswered { tag: 3, yes: false },
            "{key:?}"
        );
        assert_eq!(app.mode(), InputMode::Normal, "{key:?}");
        assert!(app.action().is_none(), "{key:?} armed an action");
        assert!(!app.keymap_open(), "{key:?} opened the keymap");
        assert!(app.settings().is_none(), "{key:?} opened settings");
        assert_eq!(app.selected_index(), before, "{key:?} moved the cursor");
    }
}

#[test]
fn the_modal_takes_keys_ahead_of_an_open_filter_box() {
    let mut app = fixtures::editing_app("web");
    app.open_modal(Modal::confirm(4, "go on?".to_owned()));
    drawn(&mut app);

    let effect = app.update(Msg::Key(KeyPress::TextApply));

    assert_eq!(effect, Effect::ModalAnswered { tag: 4, yes: true });
    assert_eq!(app.filter(), "web", "Enter reached the filter box");
}

#[test]
fn an_output_modal_closes_on_any_key_and_raises_no_answer() {
    let mut app = fixtures::full_app();
    app.open_modal(Modal::output(5, "cargo", "error: boom"));
    drawn(&mut app);

    let effect = app.update(Msg::Key(KeyPress::Confirm));

    assert_eq!(effect, Effect::None);
    assert!(app.modal().is_none());
}

#[test]
fn output_scrolls_with_the_select_keys_only_when_it_is_taller_than_the_box() {
    let mut app = fixtures::full_app();
    app.open_modal(Modal::output(5, "cargo", "a\nb\nc\nd"));
    app.note_modal_reach(Some(2));

    assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
    assert_eq!(app.modal_scroll(), 1);
    app.update(Msg::Key(KeyPress::SelectDown));
    app.update(Msg::Key(KeyPress::SelectDown));
    assert_eq!(app.modal_scroll(), 2, "scrolling stops at the reach");
    app.update(Msg::Key(KeyPress::SelectUp));
    assert_eq!(app.modal_scroll(), 1);
    app.update(Msg::Key(KeyPress::SelectLast));
    assert_eq!(app.modal_scroll(), 2);
    app.update(Msg::Key(KeyPress::SelectFirst));
    assert_eq!(app.modal_scroll(), 0);
    assert!(app.modal().is_some(), "a scroll key closed the modal");

    app.update(Msg::Key(KeyPress::Quit));
    assert!(app.modal().is_none());
}

#[test]
fn output_that_fits_the_box_closes_on_a_select_key() {
    let mut app = fixtures::full_app();
    app.open_modal(Modal::output(5, "cargo", "a\nb"));
    app.note_modal_reach(Some(0));

    app.update(Msg::Key(KeyPress::SelectDown));

    assert!(app.modal().is_none());
}

fn shown(modal: &Modal) -> &str {
    match modal {
        Modal::Output { text, .. } => text.as_str(),
        other => panic!("not an output modal: {other:?}"),
    }
}

#[test]
fn output_over_the_cap_is_cut_on_a_character_boundary() {
    // 'é' is two bytes, so a cut at byte 4096 would fall inside one.
    let long = "é".repeat(OUTPUT_CAP);

    let modal = Modal::output(1, "cargo", &long);

    let text = shown(&modal);
    assert!(text.ends_with(CUT_MARK), "{text:?}");
    let kept = text.strip_suffix(CUT_MARK).expect("ends with the mark");
    assert_eq!(kept.len(), OUTPUT_CAP, "two-byte characters fit exactly");
    assert!(kept.chars().all(|ch| ch == 'é'));

    let odd = format!("a{}", "é".repeat(OUTPUT_CAP));
    let cut = Modal::output(1, "cargo", &odd);
    let kept = shown(&cut).strip_suffix(CUT_MARK).expect("cut");
    assert_eq!(
        kept.len(),
        OUTPUT_CAP - 1,
        "the straddling character is dropped"
    );
}

#[test]
fn output_at_the_cap_is_not_cut() {
    let exact = "x".repeat(OUTPUT_CAP);

    let modal = Modal::output(1, "cargo", &exact);

    assert_eq!(shown(&modal), exact);
}

#[test]
fn output_never_carries_escapes_or_control_characters() {
    let raw = "\u{1b}[31mred\u{1b}[0m\r\nbell\u{7}\ttab\u{9b}2J\u{202e}end";

    let modal = Modal::output(1, "\u{1b}[1mtitle\u{1b}[0m", raw);

    let Modal::Output { title, text, .. } = &modal else {
        panic!("not an output modal");
    };
    for shown in [title.as_str(), text.as_str()] {
        assert!(
            !shown
                .chars()
                .any(|ch| (ch.is_control() && ch != '\n') || ch == '\u{202e}'),
            "{shown:?}"
        );
    }
    assert_eq!(text.as_str(), "[31mred[0m \nbell tab2Jend");
}

/// Without this a permission list taller than the box could not be read, and
/// every key that would scroll it answers no.
#[test]
fn a_grant_taller_than_the_box_scrolls_on_the_select_keys_and_answers_on_the_rest() {
    let mut app = fixtures::full_app();
    app.open_modal(grant(6));
    app.note_modal_reach(Some(3));

    assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
    assert_eq!(app.modal_scroll(), 1);
    assert!(app.modal().is_some());

    let effect = app.update(Msg::Key(KeyPress::Escape));

    assert_eq!(effect, Effect::ModalAnswered { tag: 6, yes: false });
    assert_eq!(app.modal_scroll(), 0);
}

#[test]
fn a_paste_does_not_reach_a_text_box_under_a_modal() {
    let mut app = fixtures::editing_app("web");
    app.open_modal(Modal::confirm(4, "go on?".to_owned()));
    drawn(&mut app);

    let effect = app.update(Msg::Paste(Pasted::new("abc".to_owned())));

    assert_eq!(effect, Effect::None);
    assert_eq!(app.filter(), "web");
    assert!(app.modal().is_some(), "a paste answered the modal");
}

#[test]
fn a_modal_over_a_text_box_reads_keys_in_the_normal_keymap() {
    let mut app = fixtures::editing_app("web");
    assert_eq!(app.key_mode(), InputMode::Text);

    app.open_modal(grant(8));
    drawn(&mut app);

    assert_eq!(app.key_mode(), InputMode::Normal);
    app.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(app.key_mode(), InputMode::Text, "the box is back");
}

#[test]
fn a_modal_not_yet_drawn_answers_nothing_but_a_quit_still_quits() {
    let mut app = fixtures::full_app();
    app.open_modal(grant(1));

    assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
    assert!(app.modal().is_some());
    assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
}

#[test]
fn the_next_queued_modal_waits_for_its_own_draw_before_a_key_answers_it() {
    let mut app = fixtures::full_app();
    app.open_modal(grant(1));
    app.open_modal(grant(2));
    app.note_modal_reach(Some(0));
    app.update(Msg::Key(KeyPress::Confirm));

    let early = app.update(Msg::Key(KeyPress::SelectDown));

    assert_eq!(early, Effect::None, "answered a grant no one has seen");
    assert!(matches!(app.modal(), Some(Modal::Grant { tag: 2, .. })));
    app.note_modal_reach(Some(3));
    app.update(Msg::Key(KeyPress::SelectDown));
    assert_eq!(app.modal_scroll(), 1);
}

#[test]
fn a_modal_left_off_screen_by_a_small_terminal_keeps_its_question() {
    let mut app = fixtures::full_app();
    app.open_modal(grant(1));
    app.note_modal_reach(Some(0));

    app.note_modal_reach(None);
    let effect = app.update(Msg::Key(KeyPress::Escape));

    assert_eq!(effect, Effect::None);
    assert!(app.modal().is_some());
    app.note_modal_reach(Some(0));
    assert_eq!(
        app.update(Msg::Key(KeyPress::Escape)),
        Effect::ModalAnswered { tag: 1, yes: false }
    );
}
