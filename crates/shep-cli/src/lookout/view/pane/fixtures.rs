//! Panes and readers the tests in this module's siblings build on.
//!
//! One place rather than one copy per file: the same `web` sheep is the
//! subject of tests in nearly every one of them, and a fixture that drifts
//! between two copies is a test that passes for the wrong reason.

use ratatui::text::Line;

use super::super::super::pane::ConfigPane;
use super::super::fixtures;

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
