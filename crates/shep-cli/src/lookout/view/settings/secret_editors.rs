//! What the settings screen draws while a secret is being typed: the prompt
//! under the table and the status bar, which both echo the open editor.

use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::super::fixtures;
use super::super::status::status_layout::status_line;
use crate::lookout::app::{App, KeyPress, Msg};
use crate::lookout::frames::render_text;
use crate::lookout::textbox::TextEdit;

const BLOCKS: &str = "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}";

fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| super::super::draw(app, frame))
        .unwrap();
    render_text(terminal.backend().buffer())
}

fn bar(app: &App) -> String {
    fixtures::rendered(&status_line(app, 200))
}

fn type_text(app: &mut App, text: &str) {
    for typed in text.chars() {
        let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
}

/// `otel` open over a schema with a secret `token` and a plain `label`.
fn app_on_a_dog_with_a_secret() -> App {
    let (_dir, _paths, mut app) = fixtures::settings_over(fixtures::OTEL_BARE);
    fixtures::open_adopted_dog_row_with(
        &mut app,
        "otel",
        Path::new("/usr/local/bin/shep-otel"),
        serde_json::json!({
            "type": "object",
            "properties": {
                "token": { "type": "string", "x-shep-secret": true },
                "label": { "type": "string" },
            },
        }),
        "",
    );
    app
}

/// The editor a dog's own field opens, once `hunter2` is typed in it.
fn typing_into(app: &mut App, field: &str) {
    fixtures::select_setting(app, &format!("dog.otel.{field}"));
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(app, "hunter2");
}

#[test]
fn a_dogs_secret_field_is_masked_under_the_table_and_on_the_bar() {
    let mut app = app_on_a_dog_with_a_secret();
    typing_into(&mut app, "token");

    for drawn in [screen(&app), bar(&app)] {
        assert!(!drawn.contains("hunter2"), "the secret is drawn: {drawn}");
        assert!(drawn.contains("otel token"), "{drawn}");
        assert!(drawn.contains(&format!("{BLOCKS}\u{258f}")), "{drawn}");
    }
}

#[test]
fn ctrl_r_shows_a_dogs_secret_field_and_a_second_press_hides_it() {
    let mut app = app_on_a_dog_with_a_secret();
    typing_into(&mut app, "token");

    let _ = app.update(Msg::Key(KeyPress::TextEdit(TextEdit::Reveal)));
    for drawn in [screen(&app), bar(&app)] {
        assert!(drawn.contains("hunter2\u{258f}"), "{drawn}");
    }
    let _ = app.update(Msg::Key(KeyPress::TextEdit(TextEdit::Reveal)));
    assert!(!screen(&app).contains("hunter2"));
}

#[test]
fn a_dogs_plain_field_is_still_drawn_as_typed() {
    let mut app = app_on_a_dog_with_a_secret();
    typing_into(&mut app, "label");

    for drawn in [screen(&app), bar(&app)] {
        assert!(drawn.contains("hunter2\u{258f}"), "{drawn}");
    }
}

/// A dog's env value is as much a secret as a sheep's, and its KEY is a name.
/// This editor draws under the table only: the bar keeps its filter line.
#[test]
fn a_dogs_env_value_is_masked_under_the_table() {
    let (_dir, _paths, mut app) = fixtures::settings_over(fixtures::OTEL_WITH_ENV);
    fixtures::open_adopted_dog_row(&mut app, "otel", Path::new("/usr/local/bin/shep-otel"));
    fixtures::select_setting(&mut app, "dog.otel.env+");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(&mut app, "TOKEN");
    assert!(screen(&app).contains("TOKEN\u{258f}"), "{}", screen(&app));
    let _ = app.update(Msg::Key(KeyPress::TextApply));
    type_text(&mut app, "hunter2");

    let drawn = screen(&app);
    assert!(!drawn.contains("hunter2"), "the secret is drawn: {drawn}");
    assert!(drawn.contains(&format!("{BLOCKS}\u{258f}")), "{drawn}");
    assert!(!bar(&app).contains("hunter2"), "{}", bar(&app));
    let _ = app.update(Msg::Key(KeyPress::TextEdit(TextEdit::Reveal)));
    assert!(screen(&app).contains("hunter2\u{258f}"));
}
