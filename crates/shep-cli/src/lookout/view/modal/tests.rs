use super::*;
use crate::commands::dogs::{Prompt, Review};
use crate::lookout::app::{KeyPress, Modal, Msg};
use crate::lookout::view::fixtures;
use crate::lookout::view::testing::draw_to;
use crate::output::width::text_columns;

const BOX_CORNER: char = '▛';
const KEYS: &str = "enter yes   any other key no";

/// What `run_ui` does before each draw: tells the layer how much of the text
/// the box cannot show.
fn drawn(app: &mut App, width: u16, height: u16) -> String {
    app.note_modal_reach(scroll_reach(app, Rect::new(0, 0, width, height)));
    draw_to(app, width, height)
}

fn prompt_with(note: Option<&str>) -> Prompt {
    Prompt::for_review("metrics", "adopt", &Review::Undeclared, note)
        .expect("an undeclared dog is asked")
}

/// Thirty permission lines in the shape `Prompt::for_review` lists them.
fn long_listing() -> String {
    (0..30)
        .map(|n| format!("  perm-{n:02} (optional): reason number {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_within(frame: &str, width: usize) {
    for line in frame.lines() {
        assert!(text_columns(line) <= width, "{line:?} in\n{frame}");
    }
}

#[test]
fn a_grant_with_a_note_draws_boxed_at_80_columns_over_a_muted_body() {
    let mut app = fixtures::full_app();
    app.open_modal(Modal::grant(
        1,
        prompt_with(Some("note: it will be built with cargo")),
    ));

    let frame = drawn(&mut app, 80, 24);

    assert!(frame.contains(BOX_CORNER), "{frame}");
    assert!(
        frame.contains("metrics declares nothing, runs with full access."),
        "{frame}"
    );
    assert!(
        frame.contains("note: it will be built with cargo"),
        "{frame}"
    );
    assert!(frame.contains("adopt metrics with full access?"), "{frame}");
    assert!(!frame.contains("[y/N]"), "{frame}");
    let keys = frame.lines().filter(|line| line.contains(KEYS)).count();
    assert_eq!(keys, 2, "the box's last line and the status bar:\n{frame}");
    assert!(
        frame.contains("shep lookout"),
        "the body under it is gone:\n{frame}"
    );
    assert_within(&frame, 80);
}

#[test]
fn a_grant_with_a_note_draws_borderless_at_the_narrow_floor() {
    let mut app = fixtures::full_app();
    app.open_modal(Modal::grant(
        1,
        prompt_with(Some("note: it will be built with cargo")),
    ));
    let width = crate::lookout::view::MIN_TERM_WIDTH;

    let frame = drawn(&mut app, width, 14);

    assert!(!frame.contains(BOX_CORNER), "{frame}");
    let flat = frame.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("note: it will be built with cargo"),
        "{frame}"
    );
    assert!(flat.contains("adopt metrics with full access?"), "{frame}");
    assert!(frame.contains(KEYS), "{frame}");
    assert_within(&frame, usize::from(width));
}

fn long_grant(app: &mut App) {
    app.open_modal(Modal::grant(2, prompt_with(Some(&long_listing()))));
}

#[test]
fn a_long_permission_list_scrolls_to_its_question_at_80_columns() {
    let mut app = fixtures::full_app();
    long_grant(&mut app);

    let top = drawn(&mut app, 80, 24);
    assert!(top.contains("perm-00"), "{top}");
    assert!(!top.contains("perm-29"), "{top}");
    assert!(!top.contains("adopt metrics with full access?"), "{top}");
    assert!(top.contains("j/k g/G scroll   enter yes"), "{top}");

    app.update(Msg::Key(KeyPress::SelectLast));
    let bottom = drawn(&mut app, 80, 24);
    assert!(!bottom.contains("perm-00"), "{bottom}");
    assert!(bottom.contains("perm-29"), "{bottom}");
    assert!(
        bottom.contains("adopt metrics with full access?"),
        "{bottom}"
    );
    assert!(app.modal().is_some());

    app.update(Msg::Key(KeyPress::SelectFirst));
    assert!(drawn(&mut app, 80, 24).contains("perm-00"));
    assert_within(&top, 80);
    assert_within(&bottom, 80);
}

#[test]
fn a_long_permission_list_scrolls_one_line_at_a_time_at_the_narrow_floor() {
    let mut app = fixtures::full_app();
    long_grant(&mut app);
    let width = crate::lookout::view::MIN_TERM_WIDTH;

    let top = drawn(&mut app, width, 10);
    assert!(top.contains("declares nothing"), "{top}");
    assert!(!top.contains(BOX_CORNER), "{top}");

    app.update(Msg::Key(KeyPress::SelectDown));
    let down = drawn(&mut app, width, 10);
    assert!(!down.contains("declares nothing"), "{down}");

    app.update(Msg::Key(KeyPress::SelectLast));
    let bottom = drawn(&mut app, width, 10);
    let flat = bottom.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("adopt metrics with full access?"), "{bottom}");
    for frame in [&top, &down, &bottom] {
        assert_within(frame, usize::from(width));
    }
}

#[test]
fn a_word_wider_than_the_box_wraps_rather_than_being_cut() {
    let mut app = fixtures::full_app();
    let word = "x".repeat(40);
    app.open_modal(Modal::output(3, "cargo", &word));

    let frame = drawn(&mut app, 33, 10);

    let xs: usize = frame.chars().filter(|ch| *ch == 'x').count();
    assert_eq!(xs, 40, "{frame}");
}

#[test]
fn an_output_modal_draws_its_title_and_text_at_80_columns() {
    let mut app = fixtures::full_app();
    app.open_modal(Modal::output(
        3,
        "install of metrics failed",
        "error: could not compile `metrics`\n\u{1b}[31mcaused by: boom\u{1b}[0m",
    ));

    let frame = drawn(&mut app, 80, 24);

    assert!(frame.contains(BOX_CORNER), "{frame}");
    assert!(frame.contains("install of metrics failed"), "{frame}");
    assert!(
        frame.contains("error: could not compile `metrics`"),
        "{frame}"
    );
    assert!(frame.contains("[31mcaused by: boom[0m"), "{frame}");
    assert!(!frame.contains('\u{1b}'), "{frame}");
    assert!(frame.contains("any key closes"), "{frame}");
    assert_within(&frame, 80);
}

#[test]
fn an_output_modal_draws_borderless_and_scrolls_to_its_cut_mark_at_the_narrow_floor() {
    let mut app = fixtures::full_app();
    let lines = (0..2000).map(|n| format!("line {n}")).collect::<Vec<_>>();
    app.open_modal(Modal::output(3, "cargo", &lines.join("\n")));
    let width = crate::lookout::view::MIN_TERM_WIDTH;

    let top = drawn(&mut app, width, 10);
    assert!(!top.contains(BOX_CORNER), "{top}");
    assert!(top.contains("cargo"), "{top}");
    assert!(top.contains("line 0"), "{top}");

    app.update(Msg::Key(KeyPress::SelectLast));
    let bottom = drawn(&mut app, width, 10);
    assert!(bottom.contains("... (cut)"), "{bottom}");
    assert_within(&top, usize::from(width));
    assert_within(&bottom, usize::from(width));
}

#[test]
fn a_frame_with_no_modal_is_the_dashboard_alone() {
    let app = fixtures::full_app();

    let frame = draw_to(&app, 80, 24);

    assert!(!frame.contains(BOX_CORNER), "{frame}");
    assert!(!frame.contains(KEYS), "{frame}");
    assert_eq!(scroll_reach(&app, Rect::new(0, 0, 80, 24)), 0);
}
