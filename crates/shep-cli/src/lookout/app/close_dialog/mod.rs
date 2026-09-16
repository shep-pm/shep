//! The dialog a pane raises on the way out, and the verb it holds until every
//! write lands.

mod dialog_keys;
mod dialog_model;
mod verb_commit;
pub use dialog_model::CloseDialog;
pub(super) use verb_commit::HeldAction;
