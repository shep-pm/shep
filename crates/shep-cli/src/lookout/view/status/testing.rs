//! Fixtures and helpers shared by this module's tests.

use super::super::super::app::App;

/// The pane's cursor, walked onto `key` the way an operator walks it.
/// A thin wrapper: [`super::super::fixtures::select_field`] is this
/// exact walk, and this module had its own copy before the tab row
/// gave a field's group somewhere to switch to first.
pub(super) fn pane_to(app: &mut App, key: &str) {
    super::super::fixtures::select_field(app, key);
}

/// What the pane has filed for `args`, or [`None`].
pub(super) fn filed_args(app: &App) -> Option<serde_json::Value> {
    use super::super::super::edits::EditKey;
    use super::super::super::pane::PaneEdit;
    match app
        .config_pane()?
        .edits()
        .get(&EditKey::Field("args".to_owned()))?
        .edit()
    {
        PaneEdit::Set { value, .. } => Some(value.as_value().clone()),
        PaneEdit::SetEnv { .. } => None,
    }
}
