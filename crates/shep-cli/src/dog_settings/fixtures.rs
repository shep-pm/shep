//! Test fixtures shared by this module's halves.
//!
//! One place rather than one copy each: [`super::refusal`]'s tests, [`super::numbers`]'s,
//! [`super::patterns`]'s and [`super::forms`]'s all need a `Field` with exactly one
//! member set, and a helper that drifts between two copies is a test that
//! passes for the wrong reason.

use crate::lookout::field::{Bounds, Field, FieldKind};

/// A `Field` built by hand, [`FieldKind::Text`] and every other member
/// empty, so a test only sets the one thing it means to check.
pub(super) fn text_field(key: &str) -> Field {
    Field {
        key: key.to_owned(),
        help: key.to_owned(),
        group: None,
        kind: FieldKind::Text,
        value_kind: None,
        default: None,
        default_value: None,
        secret: false,
        editable: true,
        example: None,
        accepts: Vec::new(),
        refuses: Vec::new(),
        neighbours: Vec::new(),
        bounds: Bounds::default(),
        apply: None,
    }
}
