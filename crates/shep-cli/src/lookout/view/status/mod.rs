//! The three chrome lines: the title, the link banner, and the status bar.
//!
//! Every sentence here is literal: nothing about damage gets charming. The
//! frozen banner, the drop notice and the refusal all live here.

mod action_gate;
mod key_hint;
mod status_layout;
#[cfg(test)]
mod testing;
pub(super) use action_gate::{CONTROL_ENABLED_LABEL, READ_ONLY_LABEL};
pub use status_layout::{banner_line, rule_line, status_line};
