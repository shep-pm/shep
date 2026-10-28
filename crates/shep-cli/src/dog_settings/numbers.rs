//! The whole-number and decimal arms of [`super::refusal`]: the
//! parse first, then the bounds the schema published.
//!
//! The two arms read different bounds. [`crate::lookout::field::Bounds`]
//! keeps an integer pair normalised for `i64` beside a decimal pair
//! held as the schema wrote it, and each arm applies only its own:
//! `minimum: 1.2, maximum: 1.8` bounds a decimal at 1.2 and 1.8 while
//! it leaves the integer pair empty.

use serde_json::{Number, Value};

use super::{Refusal, enum_refusal, not_a_number, not_a_whole_number, takes_no_value};
use crate::lookout::field::Bounds;

/// [`super::refusal`] for a [`crate::lookout::field::FieldKind::Integer`] field: the empty
/// range, the parse, then the schema's own floor and ceiling, then its `enum`.
///
/// `shown` is what may be printed, which is `typed` for an ordinary field
/// and [`super::REDACTED`] for a secret one. `typed` is still what gets parsed.
///
/// `pub(super)`: [`super::refusal`] runs it for an integer field.
pub(super) fn integer_refusal(
    key: &str,
    bounds: &Bounds,
    typed: &str,
    shown: &str,
) -> Option<Refusal> {
    match integer_of(key, bounds, typed, shown) {
        Ok(number) => match range_refusal(key, bounds, shown, number) {
            Some(refused) => Some(refused),
            None => enum_refusal(key, bounds, &Value::from(number), shown),
        },
        Err(refused) => Some(refused),
    }
}

/// `typed` as the whole number it names, or the sentence saying it names
/// none.
///
/// The empty range is checked ahead of the parse, because it is true of
/// every value: saying "starts at 3" to somebody who then types 3 and is
/// told it "stops at 1" explains nothing. [`super::items`] runs it for one
/// element of an integer list.
pub(super) fn integer_of(
    key: &str,
    bounds: &Bounds,
    typed: &str,
    shown: &str,
) -> Result<i64, Refusal> {
    if bounds.unsatisfiable {
        return Err(takes_no_value(key));
    }
    typed
        .parse::<i64>()
        .map_err(|_| not_a_whole_number(key, shown))
}

/// The floor and ceiling `bounds` sets on an integer already parsed.
///
/// The pair carries `exclusiveMinimum` and `exclusiveMaximum` folded in at
/// the next integer in, by exact `i64` arithmetic, so the sentence names the
/// value the field really starts and stops at. [`super::items`] runs it for
/// one element of an integer list.
pub(super) fn range_refusal(
    key: &str,
    bounds: &Bounds,
    shown: &str,
    number: i64,
) -> Option<Refusal> {
    match (bounds.minimum, bounds.maximum) {
        (Some(min), _) if number < min => Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} starts at {min}"),
        }),
        (_, Some(max)) if number > max => Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} stops at {max}"),
        }),
        _ => None,
    }
}

/// [`super::refusal`] for a [`crate::lookout::field::FieldKind::Number`] field:
/// the empty range, the parse, the four numeric keywords, then its `enum`.
///
/// [`Bounds::number_unsatisfiable`] is the emptiness these bounds assert,
/// not [`Bounds::unsatisfiable`]: `minimum: 1.2, maximum: 1.8` admits `1.5`.
/// A non-finite parse is refused, because `Value::from(f64)` answers `NaN`
/// with `null` and a dog's writer reads that as removing the key.
pub(super) fn number_refusal(
    key: &str,
    bounds: &Bounds,
    typed: &str,
    shown: &str,
) -> Option<Refusal> {
    if bounds.number_unsatisfiable {
        return Some(takes_no_value(key));
    }
    let Ok(number) = typed.parse::<f64>() else {
        return Some(not_a_number(key, shown));
    };
    if !number.is_finite() {
        return Some(not_a_number(key, shown));
    }
    number_range_refusal(key, bounds, shown, number)
        .or_else(|| enum_refusal(key, bounds, &Value::from(number), shown))
}

/// The four bounds `bounds` sets on a decimal already parsed.
///
/// An exclusive bound is checked against the value it names, which an
/// inclusive one is not: `exclusiveMinimum: 0` is how a dog says "a cap, and
/// not a cap of nothing".
fn number_range_refusal(key: &str, bounds: &Bounds, shown: &str, number: f64) -> Option<Refusal> {
    if let Some(low) = as_float(bounds.number_minimum.as_ref())
        && number < low
    {
        return Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} starts at {low}"),
        });
    }
    if let Some(high) = as_float(bounds.number_maximum.as_ref())
        && number > high
    {
        return Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} stops at {high}"),
        });
    }
    if let Some(low) = as_float(bounds.number_exclusive_minimum.as_ref())
        && number <= low
    {
        return Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} must be above {low}"),
        });
    }
    if let Some(high) = as_float(bounds.number_exclusive_maximum.as_ref())
        && number >= high
    {
        return Some(Refusal {
            text: format!("{key} is \"{shown}\"; {key} must be below {high}"),
        });
    }
    None
}

/// A bound kept as the schema wrote it, as the `f64` a comparison needs.
///
/// [`None`] for a bound that is not a JSON number at all, which is the same
/// fail-open answer a `pattern` that does not compile gets.
fn as_float(bound: Option<&Number>) -> Option<f64> {
    bound.and_then(Number::as_f64)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::text_field;
    use super::super::refusal;
    use crate::lookout::field::{Field, FieldKind};

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

    /// `exclusiveMinimum` is the same keyword spelled strict, and the gate read
    /// only `minimum`: a `{"type": "integer", "exclusiveMinimum": 0}` pane filed a
    /// `0`, which is a cap of nothing. The strict spelling now reaches the pair
    /// the integer gate enforces, at the next integer in.
    #[test]
    fn an_integers_exclusive_floor_refuses_zero_and_takes_the_value_above_it() {
        let floor = dog_field(
            "retries",
            serde_json::json!({ "type": "integer", "exclusiveMinimum": 0 }),
        );
        assert_eq!(
            floor.kind,
            FieldKind::Integer,
            "the fixture is an integer field"
        );
        let refused = refusal(&floor, "0").expect("0 is not above a floor of 0");
        assert_eq!(refused.text, "retries is \"0\"; retries starts at 1");
        assert_eq!(
            refusal(&floor, "1"),
            None,
            "the next integer up is what the schema means"
        );
        let ceiling = dog_field(
            "depth",
            serde_json::json!({ "type": "integer", "exclusiveMaximum": 10 }),
        );
        assert_eq!(
            refusal(&ceiling, "9"),
            None,
            "9 is below a ceiling of 10, which the ceiling itself excludes"
        );
        let refused = refusal(&ceiling, "10").expect("10 is not below a ceiling of 10");
        assert_eq!(refused.text, "depth is \"10\"; depth stops at 9");
    }

    /// One field of a dog's schema, built through the same door a real dog's
    /// `--schema` answer reaches the pane by, so the kind and the bounds a
    /// test checks are the ones [`crate::lookout::field`] read off the schema
    /// rather than ones the test wrote by hand.
    fn dog_field(key: &str, property: serde_json::Value) -> Field {
        let schema = serde_json::json!({ "properties": { key: property } });
        crate::lookout::pane::ConfigPane::dog("kelpie".to_owned(), None, schema, String::new())
            .fields()
            .by_key(key)
            .expect("the schema declares the key")
            .clone()
    }

    /// `shep-kelpie`'s `budget.per_item_usd`: a decimal money cap with a floor
    /// that excludes zero, the field this gate exists for.
    fn money_field() -> Field {
        dog_field(
            "per_item_usd",
            serde_json::json!({ "type": "number", "format": "double", "exclusiveMinimum": 0 }),
        )
    }

    /// A decimal field is a typed editor, and this is the only gate between
    /// what was typed and `dogs.toml`. `f64::from_str` takes `NaN`, `inf` and
    /// `-inf`, and `Value::from(f64)` turns a non-finite one into `null`, which
    /// the section writer reads as "remove this key": an operator typing
    /// `NaN` would have lost the key outright.
    #[test]
    fn a_number_field_refuses_a_word_and_a_non_finite_number() {
        let field = money_field();
        assert_eq!(
            field.kind,
            FieldKind::Number,
            "the fixture is a number field"
        );
        assert_eq!(refusal(&field, "2.5"), None, "the field takes a decimal");
        for typed in ["abc", "NaN", "inf", "-inf", "10MB"] {
            let refused = refusal(&field, typed).expect("refused");
            assert_eq!(
                refused.text,
                format!("per_item_usd is \"{typed}\", which is not a number shep accepts"),
                "{typed}"
            );
        }
    }

    /// `exclusiveMinimum: 0` is the spelling `shep-kelpie` publishes, and the
    /// integer pair [`crate::lookout::field::Bounds`] builds beside its own
    /// bounds reads no such thing. A `0` filed there is a cap of nothing.
    #[test]
    fn a_number_fields_exclusive_floor_refuses_zero_and_takes_the_value_above_it() {
        let field = money_field();
        let refused = refusal(&field, "0").expect("0 is not above a floor of 0");
        assert_eq!(
            refused.text,
            "per_item_usd is \"0\"; per_item_usd must be above 0"
        );
        let refused = refusal(&field, "-0.5").expect("a negative is not above the floor");
        assert_eq!(
            refused.text,
            "per_item_usd is \"-0.5\"; per_item_usd must be above 0"
        );
        assert_eq!(
            refusal(&field, "0.01"),
            None,
            "the smallest useful value above the floor"
        );
    }

    /// The inclusive pair, held at exactly its own bounds, and as the decimals
    /// the schema wrote: `minimum: 0.5` takes `0.5`, which the integer
    /// normalisation beside it would refuse.
    #[test]
    fn a_number_fields_minimum_and_maximum_are_checked_as_the_schema_wrote_them() {
        let field = dog_field(
            "ratio",
            serde_json::json!({ "type": "number", "minimum": 0.5, "maximum": 2.5 }),
        );
        for accepted in ["0.5", "2.5", "1"] {
            assert_eq!(refusal(&field, accepted), None, "{accepted}");
        }
        let refused = refusal(&field, "0.4").expect("0.4 is under the floor");
        assert_eq!(refused.text, "ratio is \"0.4\"; ratio starts at 0.5");
        let refused = refusal(&field, "2.6").expect("2.6 is over the ceiling");
        assert_eq!(refused.text, "ratio is \"2.6\"; ratio stops at 2.5");
    }

    /// A decimal field is not held to the integer rounding built beside its
    /// bounds. `minimum: 1.2, maximum: 1.8` admits no integer, so
    /// [`crate::lookout::field::Bounds::unsatisfiable`] is true of it, and
    /// `1.5` is the value the schema actually names: reading the integer
    /// answer here would tell an operator their field takes no value.
    #[test]
    fn a_number_field_is_not_held_to_the_integer_rounding_of_its_bounds() {
        let field = dog_field(
            "ratio",
            serde_json::json!({ "type": "number", "minimum": 1.2, "maximum": 1.8 }),
        );
        assert!(
            field.bounds.unsatisfiable,
            "the integer pair is empty, which is not this field's problem"
        );
        assert_eq!(refusal(&field, "1.5"), None);
        let refused = refusal(&field, "1.9").expect("1.9 is over the ceiling");
        assert_eq!(refused.text, "ratio is \"1.9\"; ratio stops at 1.8");
    }

    /// A ceiling the schema wrote as strict, checked at the value it names.
    #[test]
    fn a_number_fields_exclusive_ceiling_refuses_the_value_it_names() {
        let field = dog_field(
            "cap",
            serde_json::json!({ "type": "number", "exclusiveMaximum": 10 }),
        );
        assert_eq!(refusal(&field, "9.99"), None);
        let refused = refusal(&field, "10").expect("10 is not below a ceiling of 10");
        assert_eq!(refused.text, "cap is \"10\"; cap must be below 10");
    }

    /// The emptiness a decimal field's own bounds assert, which the integer flag
    /// beside them cannot answer: `minimum: 1.2, maximum: 1.8` admits no integer
    /// and admits `1.5`. A range empty on the schema's own terms is said once,
    /// ahead of the parse, rather than as a floor and a ceiling the operator is
    /// bounced between.
    #[test]
    fn a_decimal_range_with_nothing_in_it_says_so() {
        let inverted = dog_field(
            "ratio",
            serde_json::json!({ "type": "number", "minimum": 2.5, "maximum": 1.5 }),
        );
        for typed in ["2", "1.6", "abc"] {
            assert_eq!(
                refusal(&inverted, typed)
                    .expect("nothing is acceptable")
                    .text,
                "ratio takes no value: its schema asks for a range with nothing in it",
                "{typed}"
            );
        }
        let met_at_a_strict_ceiling = dog_field(
            "cap",
            serde_json::json!({ "type": "number", "minimum": 5, "exclusiveMaximum": 5 }),
        );
        assert_eq!(
            refusal(&met_at_a_strict_ceiling, "5")
                .expect("5 is not below a ceiling of 5")
                .text,
            "cap takes no value: its schema asks for a range with nothing in it"
        );
        // The control: a range holding exactly one value holds a value.
        let one_value = dog_field(
            "level",
            serde_json::json!({ "type": "number", "minimum": 5, "maximum": 5 }),
        );
        assert_eq!(
            refusal(&one_value, "5"),
            None,
            "5 is the one value the schema names"
        );
    }

    /// The gate reads the folded bound, so a schema that says "above
    /// 9007199254740992" cannot be answered by typing that number. Folding the
    /// strict floor through an `f64` gave the bound straight back as if it were
    /// the next integer up, because `2^53 + 1.0` is `2^53`.
    #[test]
    fn an_integer_field_refuses_the_strict_bound_a_float_could_not_increment() {
        let field = dog_field(
            "id",
            serde_json::json!({ "type": "integer", "exclusiveMinimum": 9007199254740992i64 }),
        );
        let refused = refusal(&field, "9007199254740992").expect("the bound itself is excluded");
        assert_eq!(
            refused.text,
            "id is \"9007199254740992\"; id starts at 9007199254740993"
        );
        assert_eq!(
            refusal(&field, "9007199254740993"),
            None,
            "the next integer up is the one the schema means"
        );
        let top = dog_field(
            "depth",
            serde_json::json!({ "type": "integer", "exclusiveMinimum": i64::MAX }),
        );
        assert_eq!(
            refusal(&top, "9223372036854775807")
                .expect("no integer sits above i64::MAX")
                .text,
            "depth takes no value: its schema asks for a range with nothing in it"
        );
    }

    /// A set of numbers is not a closed set this pane can draw as a cycle widget:
    /// [`crate::lookout::field::schema::closed_set`] reads strings, so
    /// `{"type": "number", "enum": [1.5, 2.5]}` is a typed field. The gate is then
    /// the only thing between an operator and a `3.5` the published schema
    /// forbids.
    #[test]
    fn a_decimal_field_holds_a_typed_value_to_its_own_numeric_enum() {
        let field = dog_field(
            "ratio",
            serde_json::json!({ "type": "number", "enum": [1.5, 2.5] }),
        );
        assert_eq!(
            field.kind,
            FieldKind::Number,
            "a numeric enum is typed, not cycled"
        );
        assert_eq!(refusal(&field, "1.5"), None, "the value the schema wrote");
        assert_eq!(refusal(&field, "2.5"), None);
        let refused = refusal(&field, "3.5").expect("3.5 is not in the set");
        assert_eq!(
            refused.text,
            "ratio is \"3.5\"; ratio takes one of 1.5, 2.5"
        );
        // The parse still comes first: a buffer that is not a number never
        // reaches the set.
        assert_eq!(
            refusal(&field, "abc").expect("abc is not a number").text,
            "ratio is \"abc\", which is not a number shep accepts"
        );
    }

    /// The same hole on the integer side, which a numeric `enum` reaches the same
    /// way. A range and a set are two rules on one field, and the value breaks
    /// whichever it meets first.
    #[test]
    fn an_integer_field_holds_a_typed_value_to_its_own_numeric_enum() {
        let field = dog_field(
            "level",
            serde_json::json!({ "type": "integer", "enum": [1, 2, 3] }),
        );
        assert_eq!(field.kind, FieldKind::Integer);
        assert_eq!(refusal(&field, "2"), None, "the value the schema wrote");
        let refused = refusal(&field, "4").expect("4 is not in the set");
        assert_eq!(refused.text, "level is \"4\"; level takes one of 1, 2, 3");
        let bounded = dog_field(
            "tier",
            serde_json::json!({ "type": "integer", "minimum": 10, "enum": [1, 2, 3] }),
        );
        let refused = refusal(&bounded, "5").expect("5 is under the floor");
        assert_eq!(refused.text, "tier is \"5\"; tier starts at 10");
        let refused = refusal(&bounded, "11").expect("11 is outside the set");
        assert_eq!(refused.text, "tier is \"11\"; tier takes one of 1, 2, 3");
    }
}
