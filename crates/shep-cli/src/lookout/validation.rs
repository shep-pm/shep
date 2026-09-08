//! What a field accepts, in the operator's words.
//!
//! Two sources, and the per-field one wins outright rather than merging:
//! a field writing its own `accepts` gets those and none from the table.
//! One rule beats merge semantics nobody can predict.
//!
//! The table lives here rather than beside the parsers because it is
//! rendering copy. It is safe there only because its own tests feed every
//! listed form back through the real parser.

use super::field::{Field, FieldKind, ValueKind};

/// One line of the table: what to print, and the strings that prove it.
///
/// `Debug` is derived (IR-41): two `&'static str`s, no value from a live
/// flock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Form {
    /// What the panel prints.
    pub text: &'static str,
    /// Values the claim is checked against in this module's tests, one for
    /// each form the text names.
    pub examples: &'static [&'static str],
}

/// The forms an [`ValueKind::UpDuration`] field accepts.
pub const DURATION_FORMS: &[Form] = &[
    Form {
        text: "500ms, 2s, 5m, 1h",
        examples: &["500ms", "2s", "5m", "1h"],
    },
    Form {
        text: "a bare number is milliseconds",
        examples: &["250"],
    },
];

/// The forms an [`ValueKind::UpDuration`] field refuses.
pub const DURATION_REFUSALS: &[Form] = &[
    Form {
        text: "a negative",
        examples: &["-1s"],
    },
    Form {
        text: "a unit shep does not know",
        examples: &["3 fortnights"],
    },
];

/// The forms an [`ValueKind::MemSize`] field accepts.
pub const MEMORY_FORMS: &[Form] = &[
    Form {
        text: "512M, 2G",
        examples: &["512M", "2G"],
    },
    Form {
        text: "a bare number is bytes",
        examples: &["1048576"],
    },
];

/// The forms a [`FieldKind::Bool`] field accepts.
///
/// This table is not parser-backed: there is no `shep_core` grammar for a
/// bool, so no test parses these examples.
pub const BOOL_FORMS: &[Form] = &[Form {
    text: "true or false",
    examples: &["true", "false"],
}];

/// The forms a [`FieldKind::Integer`] field accepts.
///
/// This table is not parser-backed: there is no `shep_core` grammar for an
/// integer, so no test parses these examples.
pub const INTEGER_FORMS: &[Form] = &[Form {
    text: "a whole number",
    examples: &["3"],
}];

/// What a field's explanation panel prints under VALIDATION.
///
/// `Debug` is derived (IR-41): rendering copy, not a live value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bullets {
    /// What the field accepts, one clause each.
    pub accepts: Vec<String>,
    /// What the field refuses, one clause each.
    pub refuses: Vec<String>,
}

impl Bullets {
    /// Whether there is nothing to show, in which case the panel draws no
    /// VALIDATION heading rather than an empty one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.accepts.is_empty() && self.refuses.is_empty()
    }
}

/// The type table's forms for `field`, keyed on [`Field::value_kind`] first
/// and [`Field::kind`] second.
fn table_for(field: &Field) -> (&'static [Form], &'static [Form]) {
    match field.value_kind {
        Some(ValueKind::UpDuration) => return (DURATION_FORMS, DURATION_REFUSALS),
        Some(ValueKind::MemSize) => return (MEMORY_FORMS, &[]),
        None => {}
    }
    match field.kind {
        FieldKind::Bool => (BOOL_FORMS, &[]),
        FieldKind::Integer => (INTEGER_FORMS, &[]),
        _ => (&[], &[]),
    }
}

/// The accepted and refused forms for `field`: its own `accepts`/`refuses`
/// when it carries any, else the type table's. The per-field lists win
/// outright rather than merging with the table.
#[must_use]
pub fn bullets(field: &Field) -> Bullets {
    if !field.accepts.is_empty() || !field.refuses.is_empty() {
        return Bullets {
            accepts: field.accepts.clone(),
            refuses: field.refuses.clone(),
        };
    }
    let (accepts, refuses) = table_for(field);
    Bullets {
        accepts: accepts.iter().map(|f| f.text.to_owned()).collect(),
        refuses: refuses.iter().map(|f| f.text.to_owned()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shep_core::values::{MemSize, UpDuration};

    /// The table is only safe away from the parsers because this test
    /// feeds it back through them. Every accepted form must parse and
    /// every refused form must not, so the table cannot go on claiming a
    /// form the parser has stopped taking.
    #[test]
    fn every_accepted_duration_form_parses() {
        for form in DURATION_FORMS {
            for example in form.examples {
                assert!(
                    example.parse::<UpDuration>().is_ok(),
                    "{} is listed as accepted but does not parse",
                    example
                );
            }
        }
    }

    #[test]
    fn every_refused_duration_form_fails_to_parse() {
        for form in DURATION_REFUSALS {
            for example in form.examples {
                assert!(
                    example.parse::<UpDuration>().is_err(),
                    "{} is listed as refused but parses",
                    example
                );
            }
        }
    }

    #[test]
    fn every_accepted_memory_form_parses() {
        for form in MEMORY_FORMS {
            for example in form.examples {
                assert!(
                    example.parse::<MemSize>().is_ok(),
                    "{} is listed as accepted but does not parse",
                    example
                );
            }
        }
    }

    /// Every form the text names has a corresponding example in examples,
    /// since that is the hole being closed. Exempt DURATION_REFUSALS and
    /// INTEGER_FORMS: DURATION_REFUSALS describes a single category in prose
    /// and takes one example each, and INTEGER_FORMS is not parser-backed.
    #[test]
    fn every_form_the_text_names_has_an_example() {
        for form in DURATION_FORMS {
            let comma_count = form.text.matches(',').count();
            let expected_count = comma_count + 1;
            assert!(
                expected_count <= form.examples.len(),
                "text \"{}\" names {} forms but examples has only {}",
                form.text,
                expected_count,
                form.examples.len()
            );
        }
        for form in MEMORY_FORMS {
            let comma_count = form.text.matches(',').count();
            let expected_count = comma_count + 1;
            assert!(
                expected_count <= form.examples.len(),
                "text \"{}\" names {} forms but examples has only {}",
                form.text,
                expected_count,
                form.examples.len()
            );
        }
        for form in BOOL_FORMS {
            let comma_count = form.text.matches(',').count();
            let expected_count = comma_count + 1;
            assert!(
                expected_count <= form.examples.len(),
                "text \"{}\" names {} forms but examples has only {}",
                form.text,
                expected_count,
                form.examples.len()
            );
        }
    }

    #[test]
    fn a_field_with_its_own_accepts_replaces_the_type_table() {
        let mut field = text_field("cwd");
        field.value_kind = Some(ValueKind::UpDuration);
        field.accepts = vec!["only this".to_owned()];
        let bullets = bullets(&field);
        assert_eq!(bullets.accepts, vec!["only this".to_owned()]);
    }

    #[test]
    fn a_field_with_no_accepts_takes_the_type_table() {
        let mut field = text_field("min_uptime");
        field.value_kind = Some(ValueKind::UpDuration);
        let bullets = bullets(&field);
        assert!(bullets.accepts.len() > 1);
    }

    /// A plain string field the type table says nothing about renders no
    /// VALIDATION heading rather than an empty one.
    #[test]
    fn a_plain_text_field_with_no_annotation_has_no_bullets() {
        let bullets = bullets(&text_field("fold"));
        assert!(bullets.is_empty());
    }

    /// A `Field` built by hand, [`FieldKind::Text`] and every new member
    /// empty, so a test only sets the one thing it means to check.
    fn text_field(key: &str) -> Field {
        Field {
            key: key.to_owned(),
            help: key.to_owned(),
            group: None,
            kind: FieldKind::Text,
            value_kind: None,
            default: None,
            secret: false,
            editable: true,
            example: None,
            accepts: Vec::new(),
            refuses: Vec::new(),
            neighbours: Vec::new(),
        }
    }
}
