use super::super::super::app::{ActionState, RowKey};

/// The status bar's own label for [`Control::ReadOnly`](crate::lookout::app::Control::ReadOnly), shared with the
/// keymap overlay's gate line so the two cannot drift apart.
pub(in crate::lookout::view) const READ_ONLY_LABEL: &str = "read-only";

/// The status bar's own label for [`Control::Allowed`](crate::lookout::app::Control::Allowed), shared with the
/// keymap overlay's gate line so the two cannot drift apart.
pub(in crate::lookout::view) const CONTROL_ENABLED_LABEL: &str = "control enabled";

/// The confirm prompt's own sentence: which verb, which target, and how to
/// answer.
///
/// A group row is the one place a keypress reaches several processes, so
/// the prompt says how many before the operator commits. A single sheep
/// keeps the `(id N)` form.
pub(super) fn confirm_prompt(action: &ActionState<'_>) -> String {
    match action.target {
        RowKey::Sheep(id) => format!(
            "{} {} (id {id})? enter confirms, any other key cancels",
            action.verb.label(),
            action.name
        ),
        RowKey::Group(name) => {
            let count = action.count;
            format!(
                "{} all {count} instances of {name}? enter confirms, any other key cancels",
                action.verb.label()
            )
        }
        RowKey::Fold(name) => format!(
            "{} all {} sheep in fold {name}? enter confirms, any other key cancels",
            action.verb.label(),
            action.count
        ),
        RowKey::Section(_) => unreachable!("a header is never an action target"),
    }
}

/// The in-flight line: the same verb-and-target naming [`confirm_prompt`]
/// uses, once the request has already gone out.
pub(super) fn in_flight_text(action: &ActionState<'_>) -> String {
    match action.target {
        RowKey::Sheep(id) => format!(
            "{} {} (id {id}): sent, waiting for the shepherd",
            action.verb.label(),
            action.name
        ),
        RowKey::Group(name) => format!(
            "{} all {} instances of {name}: sent, waiting for the shepherd",
            action.verb.label(),
            action.count
        ),
        RowKey::Fold(name) => format!(
            "{} all {} sheep in fold {name}: sent, waiting for the shepherd",
            action.verb.label(),
            action.count
        ),
        RowKey::Section(_) => unreachable!("a header is never an action target"),
    }
}
