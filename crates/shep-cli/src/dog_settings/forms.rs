//! The type table: what each kind of field takes and turns down, in the
//! operator's words.
//!
//! Rendering copy rather than logic. The duration and memory halves are
//! safe because this module's own tests feed every form they list back
//! through `shep_core`'s real parser. `BOOL_FORMS`, `INTEGER_FORMS` and
//! `NUMBER_FORMS` have no such grammar to check against, and each says so.
//!
//! [`bullets`] is what a field's explanation panel prints, and
//! [`super::refusal`] names the same table in its status-bar sentence.

use crate::lookout::field::{Field, FieldKind, ValueKind};

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
        text: "500ms, 2s, 5m, 1h, 7d",
        examples: &["500ms", "2s", "5m", "1h", "7d"],
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

/// The forms a [`ValueKind::MemSize`] field refuses.
///
/// shep's grammar is `^\d+(G|M|K)?$`, so all three are spellings somebody
/// writes who knows the size they mean. A negative is left out, unlike
/// [`DURATION_REFUSALS`]: a duration refuses a sign by its own rule after
/// parsing, where `-1` is only one more character the size grammar has no
/// place for.
pub const MEMORY_REFUSALS: &[Form] = &[
    Form {
        text: "a two-letter unit",
        examples: &["10MB", "2GB"],
    },
    Form {
        text: "a lowercase unit",
        examples: &["512m", "2g"],
    },
    Form {
        text: "a fraction",
        examples: &["1.5G"],
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

/// The forms a [`FieldKind::Number`] field accepts.
///
/// This table is not parser-backed: there is no `shep_core` grammar for a
/// decimal, so no test parses these examples. `refusal` parses them with
/// `f64::from_str`, the parser the gate actually uses, and its own test
/// feeds them back through it.
pub const NUMBER_FORMS: &[Form] = &[Form {
    text: "a number",
    examples: &["10", "2.5"],
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
        Some(ValueKind::MemSize) => return (MEMORY_FORMS, MEMORY_REFUSALS),
        None => {}
    }
    match field.kind {
        FieldKind::Bool => (BOOL_FORMS, &[]),
        FieldKind::Integer => (INTEGER_FORMS, &[]),
        FieldKind::Number => (NUMBER_FORMS, &[]),
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
    use super::super::fixtures::text_field;
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

    #[test]
    fn every_refused_memory_form_fails_to_parse() {
        for form in MEMORY_REFUSALS {
            for example in form.examples {
                assert!(
                    example.parse::<MemSize>().is_err(),
                    "{} is listed as refused but parses",
                    example
                );
            }
        }
    }

    /// How many forms a `text` names.
    ///
    /// Commas and the word "or", because `BOOL_FORMS` separates with the
    /// second and a comma count alone reads "true or false" as one form.
    fn forms_named(text: &str) -> usize {
        text.split(',').flat_map(|part| part.split(" or ")).count()
    }

    /// Every form the text names has a corresponding example in examples,
    /// since that is the hole being closed. A text may name several forms,
    /// and every one of them has to be provable from the examples list.
    #[test]
    fn every_form_the_text_names_has_an_example() {
        for (name, forms) in [
            ("DURATION_FORMS", DURATION_FORMS),
            ("DURATION_REFUSALS", DURATION_REFUSALS),
            ("MEMORY_FORMS", MEMORY_FORMS),
            ("MEMORY_REFUSALS", MEMORY_REFUSALS),
            ("BOOL_FORMS", BOOL_FORMS),
            ("INTEGER_FORMS", INTEGER_FORMS),
            ("NUMBER_FORMS", NUMBER_FORMS),
        ] {
            for form in forms {
                assert!(
                    forms_named(form.text) <= form.examples.len(),
                    "{name}: text \"{}\" names {} forms but examples has only {}",
                    form.text,
                    forms_named(form.text),
                    form.examples.len()
                );
            }
        }
    }

    /// A decimal has no `shep_core` grammar to check against, so the check is
    /// against the parser [`super::refusal`] actually uses: every example the
    /// table offers must be a finite `f64`, since a non-finite one is refused
    /// by name and a panel offering one would be a form the gate turns down.
    #[test]
    fn every_accepted_number_form_parses_as_a_finite_float() {
        for form in NUMBER_FORMS {
            for example in form.examples {
                let parsed = example.parse::<f64>();
                assert!(
                    parsed.is_ok_and(|value| value.is_finite()),
                    "{example} is listed as accepted and the gate must file it"
                );
            }
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

    /// The arms of `table_for` that no other test reached: a bool, an integer
    /// and a decimal are keyed on [`Field::kind`] rather than on a
    /// `value_kind`, and the size table's refusals are new. Every one of
    /// them could have been swapped for the empty pair without failing
    /// anything.
    #[test]
    fn every_type_table_reaches_the_panel() {
        let cases = [
            (FieldKind::Bool, None, BOOL_FORMS, &[] as &[Form]),
            (FieldKind::Integer, None, INTEGER_FORMS, &[]),
            (FieldKind::Number, None, NUMBER_FORMS, &[]),
            (
                FieldKind::Text,
                Some(ValueKind::MemSize),
                MEMORY_FORMS,
                MEMORY_REFUSALS,
            ),
            (
                FieldKind::Text,
                Some(ValueKind::UpDuration),
                DURATION_FORMS,
                DURATION_REFUSALS,
            ),
        ];
        for (kind, value_kind, accepts, refuses) in cases {
            let mut field = text_field("max_memory");
            field.kind = kind.clone();
            field.value_kind = value_kind;
            let bullets = bullets(&field);
            let texts = |forms: &[Form]| -> Vec<String> {
                forms.iter().map(|form| form.text.to_owned()).collect()
            };
            assert_eq!(bullets.accepts, texts(accepts), "{kind:?} accepts");
            assert_eq!(bullets.refuses, texts(refuses), "{kind:?} refuses");
        }
    }
}
