//! Panes and readers the tests in this module's siblings build on.
//!
//! One place rather than one copy per file: the same `web` sheep is the
//! subject of tests in nearly every one of them, and a fixture that drifts
//! between two copies is a test that passes for the wrong reason.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::super::super::app::App;
use super::super::super::pane::{ConfigPane, PaneRow};
use super::super::fixtures;
use crate::lookout::frames::render_text;

/// The pane the rest of this module renders: `web`, with two overridden
/// fields, one pending and two env keys.
pub(super) fn web_pane() -> ConfigPane {
    ConfigPane::sheep(fixtures::sheep_config_view())
}

/// Every line as a plain string, styles dropped.
pub(super) fn text_of(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

/// A dog pane with one array field marked `x-shep-secret`, two elements
/// already set, cursor opened onto the list sub-screen.
pub(super) fn secret_list_dog_pane() -> ConfigPane {
    let schema = serde_json::json!({
        "properties": {
            "tokens": {
                "type": "array",
                "items": { "type": "string" },
                "x-shep-secret": true,
            }
        }
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "tokens = [\"ab12cd34\", \"ef56gh78\"]\n".into(),
    );
    pane.move_to_key("tokens");
    pane.open_list();
    pane
}
/// A dog whose one string field is `x-shep-secret`, edited once. No
/// Flockfile field is secret today, but a dog's schema can mark one,
/// and the pending-edits section has to mask it the same way
/// [`field_line`] already does for the row it scrolls off of.
pub(super) fn secret_dog_pane_with_an_edit() -> ConfigPane {
    let schema = serde_json::json!({
        "properties": {
            "token": {
                "type": "string",
                "x-shep-secret": true,
            }
        }
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "token = \"ab12cd34\"\n".into(),
    );
    pane.move_to_key("token");
    pane.begin_typing();
    for c in "ef56gh78".chars() {
        pane.type_char(c);
    }
    pane.apply_typing();
    pane
}

/// A sheep whose `args` are `args`, for the list sub-screen's own
/// tests.
pub(super) fn web_with_args(args: &[&str]) -> shep_core::protocol::SheepConfigView {
    let config = shep_core::config::AppConfig {
        name: "web".to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        ..Default::default()
    };
    shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new())
}

/// The pane's cursor, walked onto `key` the way an operator walks it.
/// A thin wrapper: [`fixtures::select_field`] is this exact walk, and
/// this module had its own copy before the tab row gave a field's
/// group somewhere to switch to first.
pub(super) fn pane_to(app: &mut App, key: &str) {
    fixtures::select_field(app, key);
}

/// The whole frame at `height`, 120 columns wide, through the same
/// `note_body_rows` and `draw` the event loop runs before each one.
pub(super) fn screen_at(app: &mut App, height: u16) -> String {
    screen_of(app, 120, height)
}
/// [`screen_at`] at a width of the caller's own choosing.
pub(super) fn screen_of(app: &mut App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    app.note_body_rows(super::super::body_rows(area));
    app.note_body_width(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(app, frame))
        .unwrap();
    render_text(terminal.backend().buffer())
}
/// How many rows the frame marks as selected. One, always.
pub(super) fn marked(text: &str) -> usize {
    text.lines().filter(|line| line.starts_with('>')).count()
}

/// The longest word in the cursor's field help, which is what the blurb
/// tests match on.
///
/// Not the whole help string: the blurb wraps to `BLURB_WRAP`, so a
/// help text longer than the wrap budget appears in no single row and a
/// `contains` against all of it fails for a reason unrelated to what
/// these tests pin. Not the first word either, since "Set" or "The"
/// appears in other rows. The longest word is the one least likely to
/// be split by a wrap or shared with another row.
pub(super) fn blurb_anchor(pane: &ConfigPane) -> String {
    let help = field_help_under_cursor(pane);
    help.split_whitespace()
        .max_by_key(|word| word.len())
        .expect("the field help is empty")
        .to_owned()
}
/// The `help` string of the field under the cursor, whichever it is.
pub(super) fn field_help_under_cursor(pane: &ConfigPane) -> String {
    let Some(PaneRow::Field(index)) = pane.cursor() else {
        panic!("the cursor is not on a field");
    };
    pane.fields().fields()[index].help.clone()
}
