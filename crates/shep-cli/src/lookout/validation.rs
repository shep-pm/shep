//! What a field accepts, in the operator's words.
//!
//! Two sources, and the per-field one wins outright rather than merging:
//! a field writing its own `accepts` gets those and none from the table.
//! One rule beats merge semantics nobody can predict.
//!
//! The table lives here rather than beside the parsers because it is
//! rendering copy. It is safe there only because its own tests feed every
//! listed form back through the real parser. The per-field lists are backed
//! one crate down, where shep-core hands `normalize` a value for every
//! refusal it makes, and names the enforcer for the few decided at spawn.
//!
//! [`refusal`] is the other half, and it is here rather than in the pane so
//! that the sentence an operator reads and the check that stops the write
//! are built from one table. A dog's section is written by the shepherd
//! without being read, so there is no second gate behind this one.

use shep_core::values::{MemSize, UpDuration};

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

/// What a refusal prints in place of a secret's value.
///
/// The same marker the pane already draws for a secret it holds, so an
/// operator reads one word for one idea rather than learning a second.
pub const REDACTED: &str = "<set>";

/// What may be printed for `typed`: [`REDACTED`] when the field carries a
/// credential, the value itself otherwise.
///
/// Every sentence this module builds resolves its value here, so a refusal
/// added later cannot echo a secret by forgetting to ask. The rule is one
/// line and the reason it is a function is that it has two callers.
///
/// `pub(super)`: `ConfigPane::apply_typing` builds one refusal of its own
/// through [`not_a_whole_number`], and has to resolve the value the same
/// way.
#[must_use]
pub(super) fn shown_value<'a>(field: &Field, typed: &'a str) -> &'a str {
    if field.secret { REDACTED } else { typed }
}

/// Why a typed buffer was not filed, in the operator's words.
///
/// One sentence, ready for the status bar: it names the key, quotes what
/// was typed, and says what the field does take. The house shape every
/// other refusal in the pane follows.
///
/// `Debug` is derived (IR-41), which is safe only because [`refusal`]
/// redacts before it builds one: a secret's value never reaches `text`,
/// so there is nothing here for a hand-written impl to hide. Both halves
/// are pinned by exact-string tests below.
///
/// The value quoted for a field that is not secret is one the operator
/// just typed, and the row beside it is already drawing the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The sentence to print.
    pub text: String,
}

/// Whether `field` takes `typed`, and why not when it does not.
///
/// Three checks, and a field reaches at most one of them:
///
/// - A [`FieldKind::Integer`] buffer must parse as an `i64` and sit inside
///   whatever `minimum` and `maximum` the schema published.
/// - A string field naming one of shep-core's own grammars goes through
///   that type's `FromStr`, which is the enforcer itself rather than a
///   second opinion about it.
/// - Any other string field with a `pattern` is matched against it. This
///   is the only door a third-party dog's own grammar has, since shep has
///   no parser for a type it has never heard of.
///
/// [`None`] for everything else, which is most fields: a plain string with
/// no pattern takes any string, and shep is not the authority on a dog's
/// config beyond what that dog published.
///
/// Fails open on a `pattern` that does not compile. A dog that ships a
/// broken regex has a bug in its schema, and refusing every value for that
/// field would lock an operator out of a setting on the strength of
/// somebody else's typo.
///
/// The empty buffer never reaches here: it means "unset", which is
/// `ConfigPane::apply_typing`'s own case and not a value to check.
#[must_use]
pub fn refusal(field: &Field, typed: &str) -> Option<Refusal> {
    if typed.is_empty() {
        return None;
    }
    // Every path below quotes `typed` back at the operator, and
    // `App::on_pane_text_key` paints the sentence in the status bar. For a
    // secret that is the one place in this pane the value would appear:
    // `ConfigPane::begin_typing` seeds a secret's editor empty and the row
    // renders `<set>`, both so the value is never drawn. A schema decides
    // this, not shep, so it is reachable rather than theoretical: a dog
    // publishing `pattern` or a `$ref` grammar on a field it also marks
    // `x-shep-secret` sends a credential straight through `pattern_refusal`.
    let shown = shown_value(field, typed);
    if field.kind == FieldKind::Integer {
        return integer_refusal(field, typed, shown);
    }
    match field.value_kind {
        Some(ValueKind::UpDuration) => typed
            .parse::<UpDuration>()
            .is_err()
            .then(|| refuse(&field.key, shown, "a duration", DURATION_FORMS)),
        Some(ValueKind::MemSize) => typed
            .parse::<MemSize>()
            .is_err()
            .then(|| refuse(&field.key, shown, "a size", MEMORY_FORMS)),
        None => pattern_refusal(field, typed, shown),
    }
}

/// [`refusal`] for a [`FieldKind::Integer`] field: the empty range, the
/// parse, then the schema's own floor and ceiling.
///
/// `shown` is what may be printed, which is `typed` for an ordinary field
/// and [`REDACTED`] for a secret one. `typed` is still what gets parsed.
fn integer_refusal(field: &Field, typed: &str, shown: &str) -> Option<Refusal> {
    let key = &field.key;
    // Ahead of the parse, because it is true of every value: a field whose
    // schema admits no integer cannot be answered, and saying "starts at 3"
    // to somebody who then types 3 and is told it "stops at 1" explains
    // nothing.
    if field.bounds.unsatisfiable {
        return Some(Refusal {
            text: format!("{key} takes no value: its schema asks for a range with nothing in it"),
        });
    }
    let Ok(number) = typed.parse::<i64>() else {
        return Some(not_a_whole_number(key, shown));
    };
    match (field.bounds.minimum, field.bounds.maximum) {
        (Some(min), _) if number < min => Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} starts at {min}"),
        }),
        (_, Some(max)) if number > max => Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} stops at {max}"),
        }),
        _ => None,
    }
}

/// [`refusal`] for a string field whose schema published a `pattern` and
/// whose type shep does not know: the dog's own grammar, enforced as the
/// dog wrote it.
///
/// The field's own `accepts` is what the refusal names when it has any,
/// since that is the spelling its author wrote for an operator. The raw
/// pattern is the fallback, which is worse copy than a sentence and better
/// than the silence it replaced.
fn pattern_refusal(field: &Field, typed: &str, shown: &str) -> Option<Refusal> {
    let pattern = field.bounds.pattern.as_deref()?;
    let compiled = regex::Regex::new(pattern).ok()?;
    if compiled.is_match(typed) {
        return None;
    }
    let takes = if field.accepts.is_empty() {
        format!("it matches {pattern}")
    } else {
        field.accepts.join(", ")
    };
    Some(Refusal {
        text: format!(
            "{} is \"{shown}\", which it does not take; try {takes}",
            field.key
        ),
    })
}

/// The refusal for a [`FieldKind::Integer`] buffer that is not a number.
///
/// Not built from [`INTEGER_FORMS`] through [`refuse`], which would append
/// "try a whole number" to a sentence that has just said "is not a whole
/// number". The table's own text still draws the panel, where "a whole
/// number" is the whole answer and reads correctly.
///
/// `pub(super)`: `ConfigPane::apply_typing`'s own integer arm answers with
/// this, so the one sentence has one spelling.
///
/// `shown` is what may be printed: [`REDACTED`] rather than the value when
/// the field is a credential. Callers pass what [`refusal`] resolved.
pub(super) fn not_a_whole_number(key: &str, shown: &str) -> Refusal {
    Refusal {
        text: format!("{key} is \"{shown}\", which is not a whole number shep accepts"),
    }
}

/// One refusal sentence, with what the field takes read off the same table
/// the explanation panel prints. Sharing the table is the point: a form
/// added to one is added to the other, and neither can name a spelling the
/// parser has stopped taking.
///
/// `shown` is what may be printed, which [`shown_value`] has already
/// resolved. Naming the parameter for the raw buffer invited a caller to
/// pass one.
fn refuse(key: &str, shown: &str, what: &str, forms: &[Form]) -> Refusal {
    let takes: Vec<&str> = forms.iter().map(|form| form.text).collect();
    Refusal {
        text: format!(
            "{key} is \"{shown}\", which is not {what} shep accepts; try {}",
            takes.join(", ")
        ),
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

    /// The three arms of `table_for` that no other test reached: a bool
    /// and an integer are keyed on `Field::kind` rather than on a
    /// `value_kind`, and the size table's refusals are new. Every one of
    /// them could have been swapped for the empty pair without failing
    /// anything.
    #[test]
    fn every_type_table_reaches_the_panel() {
        let cases = [
            (FieldKind::Bool, None, BOOL_FORMS, &[] as &[Form]),
            (FieldKind::Integer, None, INTEGER_FORMS, &[]),
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

    /// The one rule both refusal doors share. `ConfigPane::apply_typing`
    /// builds its own sentence for an integer it cannot convert, so the
    /// redaction cannot live inside [`refusal`] alone.
    #[test]
    fn the_shown_value_of_a_secret_is_the_marker_and_never_the_buffer() {
        let mut field = text_field("token");
        assert_eq!(shown_value(&field, "sk_live_HUNTER2"), "sk_live_HUNTER2");
        field.secret = true;
        assert_eq!(shown_value(&field, "sk_live_HUNTER2"), REDACTED);
        assert!(
            !not_a_whole_number(&field.key, shown_value(&field, "sk_live_HUNTER2"))
                .text
                .contains("HUNTER2")
        );
    }

    /// Driven through the same schema shape a real dog publishes:
    /// `shep-log-rotate` spells `max_age` as `Option<UpDuration>`, so the
    /// grammar sits two hops from the property, behind an `anyOf` and a
    /// `$ref`.
    ///
    /// `1d` is the natural spelling of a retention window and shep's
    /// grammar tops out at hours (`docs/specs/deferred.md`), so the panel
    /// must not offer it and the gate must refuse it.
    #[test]
    fn a_dogs_nullable_duration_refuses_a_day() {
        let field = duration_field();
        assert_eq!(
            field.value_kind,
            Some(ValueKind::UpDuration),
            "the $ref must resolve through the anyOf or this test proves nothing"
        );
        assert!(
            "1d".parse::<UpDuration>().is_err(),
            "1d must be outside the grammar or there is nothing to refuse"
        );
        let refused = refusal(&field, "1d").expect("1d is not a duration shep parses");
        assert_eq!(
            refused.text,
            "max_age is \"1d\", which is not a duration shep accepts; \
             try 500ms, 2s, 5m, 1h, a bare number is milliseconds"
        );
    }

    /// The passing case, which is what tells this gate apart from one that
    /// refuses everything.
    #[test]
    fn the_spellings_the_panel_offers_are_the_spellings_it_takes() {
        let field = duration_field();
        for accepted in DURATION_FORMS.iter().flat_map(|form| form.examples) {
            assert_eq!(
                refusal(&field, accepted),
                None,
                "{accepted} is printed as accepted and must be filed"
            );
        }
        for refused in DURATION_REFUSALS.iter().flat_map(|form| form.examples) {
            assert!(
                refusal(&field, refused).is_some(),
                "{refused} is printed as refused and must not be filed"
            );
        }
    }

    /// The size half, which had no test of its own here: it was reached
    /// only through `pane/edit.rs`'s `max_memory` case, so a change to
    /// that test's field would have taken this arm's coverage with it
    /// without failing anything.
    ///
    /// `10MB` is the spelling to refuse rather than a nonsense word.
    /// shep's grammar is `10M`, so `10MB` is what somebody writes who
    /// knows what they mean, and the sentence has to be the one that tells
    /// them the unit is a single letter.
    #[test]
    fn a_size_field_refuses_a_spelling_shep_does_not_take() {
        let mut field = text_field("max_size");
        field.value_kind = Some(ValueKind::MemSize);
        assert_eq!(
            refusal(&field, "10MB")
                .expect("10MB is not shep's spelling")
                .text,
            "max_size is \"10MB\", which is not a size shep accepts; \
             try 512M, 2G, a bare number is bytes"
        );
        for accepted in MEMORY_FORMS.iter().flat_map(|form| form.examples) {
            assert_eq!(
                refusal(&field, accepted),
                None,
                "{accepted} is printed as accepted and must be filed"
            );
        }
    }

    /// A secret's value must not reach the status bar, and both doors a
    /// schema opens onto this gate have to be shut: a `pattern`, and a
    /// `$ref` grammar. `ConfigPane::begin_typing` seeds a secret's editor
    /// empty and its row draws `<set>`, so a refusal echoing the buffer
    /// would be the one place in the pane a credential appears.
    ///
    /// Exact strings, both for the sentence and for the `Debug`, which is
    /// what IR-41 asks for on anything that can carry one.
    #[test]
    fn a_secrets_value_never_reaches_the_sentence_or_the_debug() {
        const CREDENTIAL: &str = "sk_live_HUNTER2";

        let mut pattern_door = text_field("token");
        pattern_door.secret = true;
        pattern_door.bounds.pattern = Some("^sk-".to_owned());
        let refused = refusal(&pattern_door, CREDENTIAL).expect("the pattern refuses it");
        assert_eq!(
            refused.text,
            "token is \"<set>\", which it does not take; try it matches ^sk-"
        );
        assert_eq!(
            format!("{refused:?}"),
            "Refusal { text: \"token is \\\"<set>\\\", which it does not take; try it matches ^sk-\" }"
        );

        let mut grammar_door = text_field("api_key");
        grammar_door.secret = true;
        grammar_door.value_kind = Some(ValueKind::UpDuration);
        let refused = refusal(&grammar_door, CREDENTIAL).expect("not a duration");
        assert!(!refused.text.contains(CREDENTIAL), "{}", refused.text);
        assert!(refused.text.contains("<set>"), "{}", refused.text);

        let mut integer_door = text_field("pin");
        integer_door.secret = true;
        integer_door.kind = FieldKind::Integer;
        let refused = refusal(&integer_door, CREDENTIAL).expect("not a number");
        assert_eq!(
            refused.text,
            "pin is \"<set>\", which is not a whole number shep accepts"
        );

        let mut bounded = text_field("pin");
        bounded.secret = true;
        bounded.kind = FieldKind::Integer;
        bounded.bounds.minimum = Some(1000);
        let refused = refusal(&bounded, "7").expect("below the floor");
        assert_eq!(refused.text, "pin is \"<set>\"; pin starts at 1000");
    }

    /// The control: a field that is not a credential still quotes what was
    /// typed, since redacting everything would pass the test above while
    /// making every refusal useless.
    #[test]
    fn an_ordinary_field_still_quotes_what_was_typed() {
        let mut field = text_field("region");
        field.bounds.pattern = Some("^eu-".to_owned());
        assert!(
            refusal(&field, "Europe")
                .expect("refused")
                .text
                .contains("Europe"),
        );
    }

    /// A range with nothing in it is said once, rather than as a floor and
    /// a ceiling the operator is bounced between. A dog writing
    /// `range(min = 3, max = 1)` is the realistic way to get one.
    #[test]
    fn a_range_with_nothing_in_it_says_so() {
        let mut field = text_field("workers");
        field.kind = FieldKind::Integer;
        field.bounds.minimum = Some(3);
        field.bounds.maximum = Some(1);
        field.bounds.unsatisfiable = true;
        for typed in ["1", "2", "3", "not a number"] {
            assert_eq!(
                refusal(&field, typed).expect("nothing is acceptable").text,
                "workers takes no value: its schema asks for a range with nothing in it",
                "{typed}"
            );
        }
    }

    /// An empty buffer is `ConfigPane::apply_typing`'s unset case, not a
    /// value, and this gate must not take it over.
    #[test]
    fn an_empty_buffer_is_not_a_refusal() {
        assert_eq!(refusal(&duration_field(), ""), None);
    }

    /// A dog's own grammar, which shep has no parser for: the `pattern` is
    /// the only thing that can refuse it, and the sentence names the
    /// field's own `accepts` rather than a regex when it has any.
    #[test]
    fn a_dogs_own_pattern_refuses_what_it_does_not_match() {
        let mut field = text_field("region");
        field.bounds.pattern = Some("^[a-z]{2}-[a-z]+-[0-9]$".to_owned());
        assert_eq!(
            refusal(&field, "eu-west-1"),
            None,
            "the pattern's own shape"
        );
        let refused = refusal(&field, "Europe").expect("Europe does not match");
        assert_eq!(
            refused.text,
            "region is \"Europe\", which it does not take; try it matches ^[a-z]{2}-[a-z]+-[0-9]$"
        );

        field.accepts = vec!["a region like eu-west-1".to_owned()];
        let refused = refusal(&field, "Europe").expect("still refused");
        assert_eq!(
            refused.text,
            "region is \"Europe\", which it does not take; try a region like eu-west-1",
            "the author's own words win over the regex"
        );
    }

    /// A dog that ships a regex that does not compile has a bug in its
    /// schema. Refusing every value for that field would lock an operator
    /// out of a setting over somebody else's typo, so the gate fails open.
    #[test]
    fn a_pattern_that_does_not_compile_refuses_nothing() {
        // Spelled in two pieces because `clippy::invalid_regex` reads a
        // literal handed to `Regex::new` and refuses to compile the file
        // over the very thing this test needs to be broken.
        let broken = ["^[", "a-z"].concat();
        assert!(
            regex::Regex::new(&broken).is_err(),
            "the fixture must be a pattern regex really refuses"
        );
        let mut field = text_field("region");
        field.bounds.pattern = Some(broken);
        assert_eq!(refusal(&field, "anything at all"), None);
    }

    /// `keep = 0` deletes every rotation the moment it is made, and
    /// `shep-log-rotate` publishes `minimum: 1` in its schema saying so.
    /// Its comment there names this exact failure: without the floor the
    /// pane offers a value the dog refuses on its next tick, in a file the
    /// operator has already saved and moved on from.
    #[test]
    fn an_integers_schema_floor_and_ceiling_are_enforced() {
        let mut field = text_field("keep");
        field.kind = FieldKind::Integer;
        field.bounds.minimum = Some(1);
        field.bounds.maximum = Some(64);
        assert_eq!(refusal(&field, "1"), None, "the floor itself is allowed");
        assert_eq!(refusal(&field, "64"), None, "the ceiling itself is allowed");
        // Whole sentences, because a fragment cannot see that the value
        // beside it was rendered bare while every other refusal quoted it.
        let refused = refusal(&field, "0").expect("0 is below the floor");
        assert_eq!(refused.text, "keep is \"0\"; keep starts at 1");
        let refused = refusal(&field, "65").expect("65 is above the ceiling");
        assert_eq!(refused.text, "keep is \"65\"; keep stops at 64");
    }

    /// An integer with no bounds still has to be an integer, and the
    /// sentence says so rather than leaving the editor open in silence.
    #[test]
    fn an_integer_field_refuses_text_out_loud() {
        let mut field = text_field("max_restarts");
        field.kind = FieldKind::Integer;
        let refused = refusal(&field, "lots").expect("lots is not a number");
        // The whole sentence, because a `contains("whole number")` passed
        // one that went on to say "try a whole number".
        assert_eq!(
            refused.text,
            "max_restarts is \"lots\", which is not a whole number shep accepts"
        );
    }

    /// Most fields are a plain string their schema says nothing about, and
    /// this gate is not shep's chance to invent a grammar for a dog's
    /// config. Nothing is refused there.
    #[test]
    fn a_field_with_no_grammar_and_no_pattern_refuses_nothing() {
        assert_eq!(refusal(&text_field("cwd"), "/srv/web"), None);
        assert_eq!(refusal(&text_field("cwd"), "!!! anything"), None);
    }

    /// A [`ValueKind::UpDuration`] field built the way a dog's schema
    /// spells an optional one: `anyOf: [{$ref}, {type: null}]`, with the
    /// grammar in `$defs`.
    fn duration_field() -> Field {
        let schema = serde_json::json!({
            "$defs": {
                "UpDuration": { "type": "string", "pattern": r"^\d+(ms|h|m|s)?$" },
            },
            "properties": {
                "max_age": {
                    "anyOf": [{ "$ref": "#/$defs/UpDuration" }, { "type": "null" }],
                    "description": "Also rotate this long after the last rotation.",
                },
            },
        });
        crate::lookout::pane::ConfigPane::dog("log-rotate".to_owned(), None, schema, String::new())
            .fields()
            .by_key("max_age")
            .expect("the schema declares max_age")
            .clone()
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
            default_value: None,
            secret: false,
            editable: true,
            example: None,
            accepts: Vec::new(),
            refuses: Vec::new(),
            neighbours: Vec::new(),
            bounds: crate::lookout::field::Bounds::default(),
        }
    }
}
