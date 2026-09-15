//! Fixtures and helpers shared by this module's tests.

use super::super::*;

/// The value on screen, or `None`. Reads the pane rather than the
/// rendered frame: these tests are about when a value is held, and the
/// drawing of it has its own tests in `view::secrets`.
pub(super) fn reveal_of(app: &App) -> Option<&Reveal> {
    match app.body() {
        Body::Secrets(pane) => pane.reveal.as_ref(),
        _ => None,
    }
}

/// The pane's own open input, or `None`.
pub(super) fn typing_of(app: &App) -> Option<&Typing> {
    match app.body() {
        Body::Secrets(pane) => pane.typing.as_ref(),
        _ => None,
    }
}

/// The status bar's current line, rendered, or `None`.
pub(super) fn notice_of(app: &App) -> Option<String> {
    app.notice().map(ToString::to_string)
}

/// The key an armed delete names, or `None`.
pub(super) fn armed_of(app: &App) -> Option<String> {
    match app.body() {
        Body::Secrets(pane) => pane.armed.as_ref().map(|a| a.key.clone()),
        _ => None,
    }
}

/// Walks [`fixtures::app_with_secrets`]'s cursor onto `SET_EVERYWHERE`,
/// whose only slot is `all` while the tab names `production`.
///
/// # Panics
/// If the cursor did not land on a row taking its value from `all`.
#[track_caller]
pub(super) fn select_the_all_slot_row(app: &mut App) {
    let _ = app.update(Msg::Key(KeyPress::SelectDown));
    let _ = app.update(Msg::Key(KeyPress::SelectDown));
    let Body::Secrets(pane) = app.body() else {
        panic!("pane is not open");
    };
    let row = &pane.model.rows[pane.selected];
    assert_eq!(row.key, "SET_EVERYWHERE");
    assert_eq!(row.in_force.as_deref(), Some("all"));
    assert_eq!(pane.environment(), Some("production"));
}

/// How many rows the table currently holds, for the test proving a
/// failed write redraws nothing.
pub(super) fn row_count(app: &App) -> usize {
    match app.body() {
        Body::Secrets(pane) => pane.model.rows.len(),
        _ => 0,
    }
}

/// The environment the pane's own tab currently names.
///
/// # Panics
/// If the pane is not open.
#[track_caller]
pub(super) fn second_tab_of(app: &App) -> String {
    let Body::Secrets(pane) = app.body() else {
        panic!("pane is not open");
    };
    pane.model.environments[pane.tab].clone()
}

/// `G`: the pane's own cursor scheme already lands on the trailing
/// `+ new key` row, so this is `SelectLast` rather than a second way to
/// reach it.
pub(super) fn select_new_key_row(app: &mut App) {
    let _ = app.update(Msg::Key(KeyPress::SelectLast));
}
