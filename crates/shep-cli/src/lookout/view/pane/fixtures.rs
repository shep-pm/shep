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
