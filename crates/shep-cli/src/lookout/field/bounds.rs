//! What a schema says a value must satisfy, beyond its type.
//!
//! Separate from [`super::schema`] because the grammar is its own: JSON
//! Schema writes `minimum` and `maximum` as arbitrary numbers, and a
//! `Field` needs a pair of `i64` bounds for an integer, the same bounds as
//! written for a decimal, and an answer for a range that holds nothing.
//! Normalising one into the other is the whole job here.
//!
//! An array's `items` schema is read the same way and hung under
//! [`Bounds::items`], so a list element is checked against its own bounds.

use serde_json::{Map, Number, Value};

use super::schema::resolved;

// A bound `Bounds` does not carry is a bound `crate::dog_settings::refusal`
// cannot enforce, and the pane files the value: shep is not the authority on
// a dog's own config. `Debug` is derived (IR-41): bounds describe a value
// without carrying one.
/// The bounds a property's schema publishes beyond its type, for the
/// keywords this pane can check a keystroke against: `pattern`, `minLength`,
/// `enum`, and the numeric range.
///
/// Read off the resolved schema, so a `$ref` into `$defs` and an
/// `anyOf: [T, null]` are followed first. Empty when the schema says nothing
/// machine-checkable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bounds {
    /// `pattern`, as the schema wrote it. A dog's own grammar reaches the
    /// pane only through this, since shep has no `FromStr` for a type it
    /// has never heard of.
    pub pattern: Option<String>,
    /// `minLength`, counted in characters. Only an array's item schema is
    /// checked against it today: [`crate::dog_settings::refusal`] reads
    /// a scalar's `minLength` and enforces nothing with it.
    pub min_length: Option<usize>,
    /// `minimum` or `exclusiveMinimum`, whichever of the two is tighter,
    /// normalised to the smallest `i64` the schema accepts. shep-log-rotate's
    /// `keep` publishes one for exactly this reason: without it the pane offers
    /// a `keep = 0` the dog refuses on its next tick, in a file the operator has
    /// already saved and moved on from. An integer's strict floor is folded in
    /// here rather than carried beside it, because the two spell one range, and
    /// a gate that read only `minimum` filed a `0` under `exclusiveMinimum: 0`.
    pub minimum: Option<i64>,
    /// `maximum` or `exclusiveMaximum`, whichever is tighter, normalised to the
    /// largest `i64` the schema accepts.
    pub maximum: Option<i64>,
    /// `minimum`, as the schema wrote it, for a [`super::FieldKind::Number`]
    /// field. Kept beside [`Self::minimum`] rather than in place of it: a
    /// decimal field's floor is a decimal, and rounding `0.5` up to `1` would
    /// refuse the value the schema itself names.
    ///
    /// A `Number` rather than an `f64`, so `Bounds` stays an `Eq` type and
    /// renders back as the schema spelled it.
    pub number_minimum: Option<Number>,
    /// `maximum`, as the schema wrote it, for a [`super::FieldKind::Number`]
    /// field.
    pub number_maximum: Option<Number>,
    /// `exclusiveMinimum`, as written. JSON Schema's strict form of a floor,
    /// and the one `shep-kelpie` publishes on `budget.per_item_usd`. Read for a
    /// decimal field: an integer's [`Self::minimum`] already holds the same
    /// keyword folded to the next integer up, and a decimal is held to the value
    /// the schema named rather than to a rounding of it.
    pub number_exclusive_minimum: Option<Number>,
    /// `exclusiveMaximum`, as written. The strict ceiling, read for a decimal
    /// field only, for [`Self::number_exclusive_minimum`]'s reason.
    pub number_exclusive_maximum: Option<Number>,
    /// `enum`, kept as JSON values rather than strings so an integer list's
    /// `enum` of numbers compares against a parsed element. Empty is the
    /// common case and means the schema named no `enum`.
    ///
    /// A scalar's `enum` reaches the operator as a cycle widget
    /// ([`super::FieldKind::Choice`]) rather than through here, so only a
    /// list's item `enum` is checked against these values.
    pub choices: Vec<Value>,
    /// Whether the schema's own numeric range admits no integer at all.
    ///
    /// A dog that writes `range(min = 3, max = 1)` publishes a field
    /// nothing can set. Without this the pane refuses `1` for being under
    /// the floor and `3` for being over the ceiling, one sentence at a
    /// time, and never says that no answer exists.
    pub unsatisfiable: bool,
    /// Whether the schema's own range admits no value at all, read from the
    /// four decimal keywords as written.
    ///
    /// [`Self::unsatisfiable`] answers whether the range admits an *integer*,
    /// which is the wrong question for a decimal field: `minimum: 1.2,
    /// maximum: 1.8` admits no integer and admits `1.5`.
    pub number_unsatisfiable: bool,
    /// For an array field, what its `items` schema publishes, followed
    /// through a `$ref` and an `anyOf: [T, null]` exactly as a scalar's
    /// bounds are. [`None`] for a field that is not an array.
    pub items: Option<Box<Bounds>>,
}

/// The [`Bounds`] the resolved schema publishes.
///
/// A numeric bound is normalised to the integer it actually admits, because
/// JSON Schema's bounds are inclusive: `minimum: 0.5` forbids `0`. The four
/// keywords stay unnormalised beside them for a [`super::FieldKind::Number`]
/// field, and `exclusiveMinimum` and `exclusiveMaximum` are folded in the
/// same two ways. See [`integer_end`].
pub(super) fn bounds_of(schema: &Value, defs: &Map<String, Value>) -> Bounds {
    bounds_opening(schema, defs, &mut Vec::new())
}

/// [`bounds_of`]'s walk. `open` holds the schemas on the current path, by
/// identity, each one twice: as written, and as [`resolved`] reads it.
///
/// An array whose `items` names its own type, which `schemars` emits for a
/// recursive one, publishes no item bounds. The raw form is what makes that
/// hold for a `$defs` entry that is only an alias: a path of resolved values
/// alone never meets `B` again.
fn bounds_opening<'a>(
    schema: &'a Value,
    defs: &'a Map<String, Value>,
    open: &mut Vec<&'a Value>,
) -> Bounds {
    let depth = open.len();
    let written = schema;
    let schema = resolved(schema, defs);
    open.push(written);
    open.push(schema);
    let items = schema
        .get("items")
        .filter(|items| items.is_object())
        .map(|items| resolved(items, defs))
        .filter(|item| !open.iter().any(|seen| core::ptr::eq(*seen, *item)))
        .map(|item| Box::new(bounds_opening(item, defs, open)));
    open.truncate(depth);
    let minimum = integer_end(schema.get("minimum"), End::Floor);
    let exclusive_minimum = integer_end(schema.get("exclusiveMinimum"), End::StrictFloor);
    let maximum = integer_end(schema.get("maximum"), End::Ceiling);
    let exclusive_maximum = integer_end(schema.get("exclusiveMaximum"), End::StrictCeiling);
    let (floor, ceiling) = (
        tighter_floor(minimum.bound, exclusive_minimum.bound),
        tighter_ceiling(maximum.bound, exclusive_maximum.bound),
    );
    // The same keywords, unnormalised, for a field whose values are decimals.
    // A bound the schema wrote as a number is read as one here, so `minimum: 0`
    // and `minimum: 0.5` reach the gate as what they say, and a bound that is
    // not a number at all bounds nothing.
    let number_bound = |keyword: &str| schema.get(keyword).and_then(Value::as_number).cloned();
    let (number_minimum, number_maximum) = (number_bound("minimum"), number_bound("maximum"));
    let (number_exclusive_minimum, number_exclusive_maximum) = (
        number_bound("exclusiveMinimum"),
        number_bound("exclusiveMaximum"),
    );
    let number_holds_nothing = number_range_is_empty(
        number_minimum.as_ref(),
        number_maximum.as_ref(),
        number_exclusive_minimum.as_ref(),
        number_exclusive_maximum.as_ref(),
    );
    Bounds {
        pattern: schema
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::to_owned),
        // A `minLength` that is not a whole non-negative number is not a
        // `minLength` JSON Schema allows, and dropping it fails open.
        min_length: schema
            .get("minLength")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok()),
        minimum: floor,
        maximum: ceiling,
        number_minimum,
        number_maximum,
        number_exclusive_minimum,
        number_exclusive_maximum,
        choices: schema
            .get("enum")
            .and_then(Value::as_array)
            .map(|values| values.to_vec())
            .unwrap_or_default(),
        // A bound past an `i64`'s reach is impossible in one direction and
        // says nothing in the other: a `minimum` above `i64::MAX` admits
        // nothing, while one below `i64::MIN` admits every integer there
        // is, which is what `as_bound` already answered `None` for.
        unsatisfiable: minimum.empty
            || exclusive_minimum.empty
            || maximum.empty
            || exclusive_maximum.empty
            || matches!((floor, ceiling), (Some(low), Some(high)) if low > high),
        number_unsatisfiable: number_holds_nothing,
        items,
    }
}

/// Which end of an integer field's range a numeric keyword names, and whether
/// the value it carries is inside that range or just outside it.
#[derive(Clone, Copy)]
enum End {
    /// `minimum`: the value itself is admitted.
    Floor,
    /// `exclusiveMinimum`: the value itself is not.
    StrictFloor,
    /// `maximum`: the value itself is admitted.
    Ceiling,
    /// `exclusiveMaximum`: the value itself is not.
    StrictCeiling,
}

/// What one keyword says about an integer field's range.
struct IntegerEnd {
    /// The smallest, or largest, `i64` the keyword admits, [`None`] when it
    /// bounds nothing this pane can compare.
    bound: Option<i64>,
    /// The keyword admits no `i64` at all: a strict floor sitting on
    /// `i64::MAX`, or a floor past every `i64`.
    empty: bool,
}

/// `keyword` as the integer end it names.
///
/// Three readings, in the order they are exact: a whole number an `i64`
/// holds gets `i64` arithmetic; a whole number too large for an `i64` but
/// whole as a float converts once and gets the same; a fraction is rounded
/// through `f64`, the strict spellings stepping one past the rounding. A
/// keyword that is not a number bounds nothing.
fn integer_end(keyword: Option<&Value>, end: End) -> IntegerEnd {
    let Some(number) = keyword.and_then(Value::as_number) else {
        return IntegerEnd {
            bound: None,
            empty: false,
        };
    };
    if let Some(written) = number.as_i64() {
        return exact(written, end);
    }
    let Some(value) = number.as_f64() else {
        return IntegerEnd {
            bound: None,
            empty: false,
        };
    };
    // Below `2^63` an integral `f64` names one integer exactly, so the cast is a
    // reading rather than a rounding. `i64::MAX as f64` is `2^63`, one past the
    // largest `i64`, so the test is strict at the top and not at the bottom.
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value < i64::MAX as f64 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "guarded: the value is integral and inside an i64's reach"
        )]
        return exact(value as i64, end);
    }
    let rounded = match end {
        End::Floor => value.ceil(),
        End::StrictFloor => value.floor() + 1.0,
        End::Ceiling => value.floor(),
        End::StrictCeiling => value.ceil() - 1.0,
    };
    IntegerEnd {
        bound: as_bound(rounded),
        // A floor at or above `2^63` sits past every `i64` and admits nothing,
        // while one below `i64::MIN` bounds nothing, since every `i64` is above
        // it. A ceiling is the mirror image.
        empty: match end {
            End::Floor | End::StrictFloor => value >= i64::MAX as f64,
            End::Ceiling | End::StrictCeiling => value <= i64::MIN as f64,
        },
    }
}

/// The end `written` names, computed in `i64`.
///
/// A strict floor on `i64::MAX` has no integer above it and a strict ceiling on
/// `i64::MIN` has none below it, which is a range with nothing in it rather than
/// a bound to enforce.
fn exact(written: i64, end: End) -> IntegerEnd {
    let bound = match end {
        End::Floor | End::Ceiling => Some(written),
        End::StrictFloor => written.checked_add(1),
        End::StrictCeiling => written.checked_sub(1),
    };
    IntegerEnd {
        bound,
        empty: bound.is_none(),
    }
}

/// The tighter of two integer floors: the higher of the two, since both bound
/// the same field and a schema that publishes `minimum: 3` beside
/// `exclusiveMinimum: 5` means 6 up.
fn tighter_floor(minimum: Option<i64>, exclusive_minimum: Option<i64>) -> Option<i64> {
    match (minimum, exclusive_minimum) {
        (Some(low), Some(strict)) => Some(low.max(strict)),
        (Some(low), None) => Some(low),
        (None, Some(strict)) => Some(strict),
        (None, None) => None,
    }
}

/// The tighter of two integer ceilings: the lower of the two, for
/// [`tighter_floor`]'s reason.
fn tighter_ceiling(maximum: Option<i64>, exclusive_maximum: Option<i64>) -> Option<i64> {
    match (maximum, exclusive_maximum) {
        (Some(high), Some(strict)) => Some(high.min(strict)),
        (Some(high), None) => Some(high),
        (None, Some(strict)) => Some(strict),
        (None, None) => None,
    }
}

/// Whether the four decimal bounds admit no value.
///
/// The range is `[minimum, maximum]` crossed with `(exclusiveMinimum,
/// exclusiveMaximum)`, and it is empty when any lower end fails to sit below
/// any upper end as far as strictness allows: `minimum: 2.5, maximum: 1.5`
/// crosses outright, and `minimum: 5, exclusiveMaximum: 5` meets at the one
/// value the ceiling forbids.
fn number_range_is_empty(
    minimum: Option<&Number>,
    maximum: Option<&Number>,
    exclusive_minimum: Option<&Number>,
    exclusive_maximum: Option<&Number>,
) -> bool {
    let low = minimum.and_then(Number::as_f64);
    let high = maximum.and_then(Number::as_f64);
    let above = exclusive_minimum.and_then(Number::as_f64);
    let below = exclusive_maximum.and_then(Number::as_f64);
    matches!((low, high), (Some(l), Some(h)) if l > h)
        || matches!((low, below), (Some(l), Some(h)) if l >= h)
        || matches!((above, high), (Some(l), Some(h)) if l >= h)
        || matches!((above, below), (Some(l), Some(h)) if l >= h)
}

/// `value` as the `i64` bound it names, or [`None`] when it sits outside
/// what an `i64` can hold and so bounds nothing this pane can compare.
///
/// [`integer_end`] reaches this only for a fraction, or a whole number past
/// an `i64`'s reach. The comparison is in `f64`, which cannot name every
/// `i64` exactly near its ends, so a bound within a few units of `i64::MAX`
/// saturates to it rather than being refused.
fn as_bound(value: f64) -> Option<i64> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "guarded by the range check, and a float-to-int cast saturates"
    )]
    (value >= i64::MIN as f64 && value <= i64::MAX as f64).then_some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::super::FieldSet;
    use super::super::fixtures::{props, real_field_set};
    use super::*;
    use serde_json::json;

    /// Three spellings of the same `pattern`: inline on the property, one
    /// `$ref` away, and behind the `anyOf: [{$ref}, {type: null}]` a dog
    /// writes for an `Option<UpDuration>`. That third is what
    /// `shep-log-rotate` publishes, so it is the shape
    /// [`crate::dog_settings::refusal`] actually meets.
    #[test]
    fn a_pattern_is_read_through_every_hop_a_schema_can_put_it_behind() {
        let defs = props(json!({
            "UpDuration": { "type": "string", "pattern": r"^\d+(ms|h|m|s)?$" },
        }));
        let p = props(json!({
            "inline": { "type": "string", "pattern": r"^\d+(ms|h|m|s)?$" },
            "one_hop": { "$ref": "#/$defs/UpDuration" },
            "two_hops": { "anyOf": [{ "$ref": "#/$defs/UpDuration" }, { "type": "null" }] },
            "plain": { "type": "string" },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        for key in ["inline", "one_hop", "two_hops"] {
            assert_eq!(
                set.by_key(key).unwrap().bounds.pattern.as_deref(),
                Some(r"^\d+(ms|h|m|s)?$"),
                "{key}"
            );
        }
        // The negative control: bounds that came back non-empty for a
        // schema publishing none would pass every assertion above.
        assert_eq!(set.by_key("plain").unwrap().bounds, Bounds::default());
    }

    /// `shep-log-rotate` publishes `minimum: 1` on `keep`, so the floor an
    /// operator is held to is one this reader lifts off somebody else's
    /// schema.
    ///
    /// A fractional bound is normalised to the integer it forbids: JSON
    /// Schema's bounds are inclusive, so a `minimum` rounds up and a
    /// `maximum` rounds down.
    #[test]
    fn integer_bounds_are_read_and_a_fractional_one_is_normalised() {
        let p = props(json!({
            "keep": { "type": "integer", "minimum": 1 },
            "ratio": { "type": "integer", "minimum": 0.5, "maximum": 2.5 },
            "both": { "type": "integer", "minimum": -3, "maximum": 64 },
            "plain": { "type": "integer" },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        assert_eq!(set.by_key("keep").unwrap().bounds.minimum, Some(1));
        assert_eq!(set.by_key("both").unwrap().bounds.minimum, Some(-3));
        assert_eq!(set.by_key("both").unwrap().bounds.maximum, Some(64));
        let ratio = &set.by_key("ratio").unwrap().bounds;
        assert_eq!(ratio.minimum, Some(1), "0.5 rounds up, so 0 is refused");
        assert_eq!(ratio.maximum, Some(2), "2.5 rounds down, so 3 is refused");
        assert!(!ratio.unsatisfiable);
        assert_eq!(set.by_key("plain").unwrap().bounds, Bounds::default());
    }

    /// The bounds a [`super::FieldKind::Number`] field is held to, kept as the
    /// schema wrote them. A decimal field's floor is a decimal: normalising
    /// `minimum: 0.5` up to `1` would refuse `0.5`, the value the schema
    /// names, and `exclusiveMinimum: 0` is the spelling `shep-kelpie`'s
    /// `budget.per_item_usd` publishes, which the integer pair does not read
    /// at all.
    #[test]
    fn a_number_fields_bounds_are_kept_as_the_schema_wrote_them() {
        let p = props(json!({
            "per_item_usd": { "type": "number", "format": "double", "exclusiveMinimum": 0 },
            "ratio": { "type": "number", "minimum": 0.5, "maximum": 2.5 },
            "below": { "type": "number", "exclusiveMaximum": 10 },
            "plain": { "type": "number" },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let bounds = |key: &str| set.by_key(key).expect(key).bounds.clone();
        let bound = |value: Value| value.as_number().expect("a number").clone();
        assert_eq!(
            bounds("per_item_usd").number_exclusive_minimum,
            Some(bound(json!(0)))
        );
        assert_eq!(
            (
                bounds("ratio").number_minimum,
                bounds("ratio").number_maximum
            ),
            (Some(bound(json!(0.5))), Some(bound(json!(2.5)))),
            "a decimal bound is not rounded"
        );
        assert_eq!(
            bounds("below").number_exclusive_maximum,
            Some(bound(json!(10)))
        );
        assert_eq!(
            bounds("ratio"),
            Bounds {
                number_minimum: Some(bound(json!(0.5))),
                number_maximum: Some(bound(json!(2.5))),
                // The integer pair sits beside these and is what an
                // [`super::FieldKind::Integer`] field is held to.
                minimum: Some(1),
                maximum: Some(2),
                ..Default::default()
            },
            "both halves are read, and neither replaces the other"
        );
        assert_eq!(
            bounds("plain"),
            Bounds::default(),
            "no bound, nothing to check"
        );
    }

    /// `exclusiveMinimum` and `exclusiveMaximum` are the same two keywords
    /// spelled strict, and an integer gate that read only `minimum` and `maximum`
    /// enforced neither: the pane filed a `0` under `exclusiveMinimum: 0`, which
    /// is how `shep-kelpie` says "a cap, and not a cap of nothing". For an
    /// integer the strict spelling names the next integer in, and where the
    /// schema publishes both it is the tighter of the two that binds.
    #[test]
    fn an_integers_exclusive_bounds_fold_into_the_pair_the_gate_reads() {
        let p = props(json!({
            "above_zero": { "type": "integer", "exclusiveMinimum": 0 },
            "below_ten": { "type": "integer", "exclusiveMaximum": 10 },
            "above_a_fraction": { "type": "integer", "exclusiveMinimum": 1.2 },
            "below_a_fraction": { "type": "integer", "exclusiveMaximum": 9.5 },
            "tighter_of_two": { "type": "integer", "minimum": 3, "exclusiveMinimum": 5 },
            "looser_of_two": { "type": "integer", "minimum": 5, "exclusiveMinimum": 2 },
            "plain": { "type": "integer" },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let bounds = |key: &str| set.by_key(key).expect(key).bounds.clone();
        assert_eq!(
            (bounds("above_zero").minimum, bounds("above_zero").maximum),
            (Some(1), None),
            "the integers above 0 start at 1"
        );
        assert_eq!(
            (bounds("below_ten").minimum, bounds("below_ten").maximum),
            (None, Some(9)),
            "the integers below 10 end at 9"
        );
        assert_eq!(
            bounds("above_a_fraction").minimum,
            Some(2),
            "the integers above 1.2 start at 2"
        );
        assert_eq!(
            bounds("below_a_fraction").maximum,
            Some(9),
            "the integers below 9.5 end at 9"
        );
        assert_eq!(
            bounds("tighter_of_two").minimum,
            Some(6),
            "exclusiveMinimum: 5 binds past minimum: 3"
        );
        assert_eq!(
            bounds("looser_of_two").minimum,
            Some(5),
            "and the tighter of the two is what the field is held to"
        );
        assert_eq!(
            bounds("plain"),
            Bounds::default(),
            "no bound, nothing to fold"
        );
    }

    /// An `f64` cannot name every integer, and `9007199254740992.0 + 1.0` is
    /// `9007199254740992.0`. Folding a strict floor through a float therefore
    /// kept the very value the schema forbade, and the pane filed it. The fold is
    /// `i64` arithmetic for any bound the schema wrote as a whole number, spelled
    /// with a decimal point or not, and a strict bound with no integer past it is
    /// the empty range it names rather than a bound that admits everything.
    #[test]
    fn an_integers_strict_bound_is_folded_past_a_floats_exact_integers() {
        let p = props(json!({
            "past_2_53": { "type": "integer", "exclusiveMinimum": 9007199254740992i64 },
            "past_2_53_as_a_float": {
                "type": "integer", "exclusiveMinimum": 9007199254740992.0
            },
            "under_negative_2_53": { "type": "integer", "exclusiveMaximum": -9007199254740992i64 },
            "strict_at_i64_max": { "type": "integer", "exclusiveMinimum": i64::MAX },
            "strict_at_i64_min": { "type": "integer", "exclusiveMaximum": i64::MIN },
            "inclusive_at_i64_max": { "type": "integer", "minimum": i64::MAX },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let bounds = |key: &str| set.by_key(key).expect(key).bounds.clone();
        assert_eq!(
            bounds("past_2_53").minimum,
            Some(9007199254740993),
            "the next integer up, not the same float read back"
        );
        assert_eq!(
            bounds("past_2_53_as_a_float").minimum,
            Some(9007199254740993),
            "the same integer spelled with a decimal point"
        );
        assert_eq!(
            bounds("under_negative_2_53").maximum,
            Some(-9007199254740993)
        );
        for key in ["past_2_53", "past_2_53_as_a_float", "under_negative_2_53"] {
            assert!(!bounds(key).unsatisfiable, "{key} admits plenty");
        }
        let top = bounds("strict_at_i64_max");
        assert_eq!(top.minimum, None, "no integer sits above i64::MAX");
        assert!(
            top.unsatisfiable,
            "so the schema asks for a range with nothing in it"
        );
        let bottom = bounds("strict_at_i64_min");
        assert_eq!(bottom.maximum, None, "no integer sits below i64::MIN");
        assert!(bottom.unsatisfiable);
        // The control: the same value spelled inclusive is a bound, not an empty
        // range.
        let inclusive = bounds("inclusive_at_i64_max");
        assert_eq!(inclusive.minimum, Some(i64::MAX));
        assert!(!inclusive.unsatisfiable);
    }

    /// The decimal half of the answer [`unsatisfiable`] gives an integer, which
    /// cannot be the same flag: `minimum: 1.2, maximum: 1.8` is empty of integers
    /// and full of decimals. A decimal range empty on its own terms has to be
    /// recorded as one, or the pane bounces an operator between a floor and a
    /// ceiling that cannot both be met.
    #[test]
    fn a_decimal_range_with_nothing_in_it_is_recorded_as_such() {
        let p = props(json!({
            "inverted": { "type": "number", "minimum": 2.5, "maximum": 1.5 },
            "met_at_a_strict_ceiling": { "type": "number", "minimum": 5, "exclusiveMaximum": 5 },
            "met_at_a_strict_floor": { "type": "number", "exclusiveMinimum": 5, "maximum": 5 },
            "strict_and_inverted": { "type": "number", "exclusiveMinimum": 5, "exclusiveMaximum": 5 },
            "meets_inclusively": { "type": "number", "minimum": 5, "maximum": 5 },
            "fractional_but_full": { "type": "number", "minimum": 1.2, "maximum": 1.8 },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        for key in [
            "inverted",
            "met_at_a_strict_ceiling",
            "met_at_a_strict_floor",
            "strict_and_inverted",
        ] {
            assert!(
                set.by_key(key).expect(key).bounds.number_unsatisfiable,
                "{key} admits nothing"
            );
        }
        // The controls: a range holding exactly one value holds a value, and a
        // range with no integer in it is not empty for a decimal field.
        for key in ["meets_inclusively", "fractional_but_full"] {
            assert!(
                !set.by_key(key).expect(key).bounds.number_unsatisfiable,
                "{key}"
            );
        }
        assert!(
            set.by_key("fractional_but_full")
                .expect("fractional_but_full")
                .bounds
                .unsatisfiable,
            "the integer pair beside it is empty, and that is not this field's problem"
        );
    }

    /// A bound the schema did not write as a number bounds nothing, the same
    /// fail-open answer a `pattern` that does not compile gets.
    #[test]
    fn a_numeric_bound_that_is_not_a_number_is_read_as_no_bound() {
        let p = props(json!({
            "ratio": { "type": "number", "minimum": "0.5", "maximum": null },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let bounds = &set.by_key("ratio").expect("ratio").bounds;
        assert_eq!(bounds.number_minimum, None);
        assert_eq!(bounds.number_maximum, None);
    }

    /// A range no integer satisfies, by each of the three routes into one.
    /// The pane says so once rather than bouncing an operator between a
    /// floor and a ceiling that cannot both be met.
    #[test]
    fn a_range_with_nothing_in_it_is_recorded_as_such() {
        let p = props(json!({
            "inverted": { "type": "integer", "minimum": 3, "maximum": 1 },
            "inverted_after_rounding": {
                "type": "integer", "minimum": 1.2, "maximum": 1.8,
            },
            "past_i64": { "type": "integer", "minimum": 1e30 },
            "under_i64": { "type": "integer", "maximum": -1e30 },
            "wide_open": { "type": "integer", "minimum": -1e30, "maximum": 1e30 },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        for key in [
            "inverted",
            "inverted_after_rounding",
            "past_i64",
            "under_i64",
        ] {
            assert!(set.by_key(key).unwrap().bounds.unsatisfiable, "{key}");
        }
        // The other direction of the same overflow bounds nothing: every
        // integer is above -1e30 and below 1e30, so the field is free.
        let open = &set.by_key("wide_open").unwrap().bounds;
        assert!(!open.unsatisfiable);
        assert_eq!((open.minimum, open.maximum), (None, None));
    }

    /// An array's element is held to what the dog published about an
    /// element, which is a different schema from the one it published about
    /// the array. The three spellings of an item schema are the three a
    /// `schemars` answer emits for `Vec<Slug>`, `Vec<Option<Slug>>` and an
    /// inline `Vec` of a plain string.
    #[test]
    fn an_arrays_item_bounds_are_read_inline_through_a_ref_and_through_an_anyof() {
        let defs = props(json!({
            "Slug": { "type": "string", "pattern": "^[a-z-]+$", "minLength": 3 },
            "Count": { "type": "integer", "minimum": 1, "maximum": 64 },
        }));
        let p = props(json!({
            "inline": { "type": "array", "items": { "type": "string", "minLength": 2 } },
            "one_hop": { "type": "array", "items": { "$ref": "#/$defs/Slug" } },
            "two_hops": {
                "type": "array",
                "items": { "anyOf": [{ "$ref": "#/$defs/Slug" }, { "type": "null" }] },
            },
            "numbers": { "type": "array", "items": { "$ref": "#/$defs/Count" } },
            "plain": { "type": "array", "items": { "type": "string" } },
            "scalar": { "type": "string", "minLength": 3 },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        let items = |key: &str| {
            set.by_key(key)
                .expect(key)
                .bounds
                .items
                .as_deref()
                .unwrap_or_else(|| panic!("{key} declares an item schema"))
                .clone()
        };
        assert_eq!(items("inline").min_length, Some(2));
        assert_eq!(items("one_hop").pattern.as_deref(), Some("^[a-z-]+$"));
        assert_eq!(
            items("two_hops").min_length,
            Some(3),
            "the anyOf arm must be resolved before its bounds are read"
        );
        assert_eq!(
            (items("numbers").minimum, items("numbers").maximum),
            (Some(1), Some(64))
        );
        // The negative control: an array of plain strings publishes nothing
        // an element is held to, so a list editor over one refuses nothing.
        assert_eq!(items("plain"), Bounds::default());
        assert_eq!(set.by_key("scalar").unwrap().bounds.items, None);
    }

    /// A `Vec` whose element type is the `Vec` itself, which is what
    /// `schemars` emits for a recursive one. The hop back to a schema
    /// already on the path publishes no item bounds, and reading the field
    /// ends there rather than on the stack.
    #[test]
    fn an_item_schema_that_names_its_own_array_ends_the_walk() {
        let p = props(json!({
            "tree": { "type": "array", "items": { "$ref": "#/$defs/Tree" } },
        }));
        let defs = props(json!({
            "Tree": { "type": "array", "items": { "$ref": "#/$defs/Tree" }, "enum": ["a"] },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        let bounds = &set.by_key("tree").unwrap().bounds;
        let item = bounds.items.as_deref().expect("the array declares items");
        assert_eq!(
            item.choices,
            vec![json!("a")],
            "the def's own bounds are read"
        );
        assert_eq!(
            item.items.as_deref(),
            None,
            "the hop back to a schema already on the path says nothing"
        );
    }

    /// The guard holds each schema as written beside the one it resolves
    /// to. `B` resolves to `A`, so a path that holds only resolved values
    /// never meets `B` again.
    ///
    /// This is a third party's schema, so a runaway walk is an operator's
    /// `shep lookout` opening a dog pane.
    #[test]
    fn an_item_schema_that_is_only_an_alias_for_another_ends_the_walk() {
        let p = props(json!({ "tree": { "$ref": "#/$defs/A" } }));
        let defs = props(json!({
            "A": { "type": "array", "items": { "$ref": "#/$defs/B" }, "minLength": 1 },
            "B": { "$ref": "#/$defs/A" },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        let bounds = &set.by_key("tree").unwrap().bounds;
        assert_eq!(bounds.min_length, Some(1), "the property's own def");
        let item = bounds.items.as_deref().expect("A declares an item schema");
        assert_eq!(
            item.min_length,
            Some(1),
            "the alias resolves to A, whose bounds are read"
        );
        assert_eq!(
            item.items.as_deref(),
            None,
            "the hop back to A says nothing, and the walk stops one level deeper"
        );
    }

    /// The same loop behind two `anyOf: [T, {type: null}]` arms, which is
    /// how a dog spells an optional field, and the shape the finding named.
    #[test]
    fn an_item_schema_behind_two_nullable_aliases_ends_the_walk() {
        let p = props(json!({ "tree": { "$ref": "#/$defs/A" } }));
        let defs = props(json!({
            "A": { "type": "array", "items": { "$ref": "#/$defs/B" }, "minLength": 1 },
            "B": { "anyOf": [{ "$ref": "#/$defs/C" }, { "type": "null" }] },
            "C": { "anyOf": [{ "$ref": "#/$defs/A" }, { "type": "null" }] },
        }));
        let set = FieldSet::from_properties(&p, &defs, &[]);
        let bounds = &set.by_key("tree").unwrap().bounds;
        let item = bounds.items.as_deref().expect("A declares an item schema");
        assert_eq!(item.min_length, Some(1));
        assert_eq!(item.items.as_deref(), None, "the hop back to A ends it");
    }

    /// An `enum` on an item is the one closed set a list editor can meet: a
    /// scalar's is a cycle widget, and an element is typed.
    #[test]
    fn an_items_enum_is_kept_as_the_values_the_schema_wrote() {
        let p = props(json!({
            "tags": { "type": "array", "items": { "enum": ["ask", "auto"] } },
            "levels": { "type": "array", "items": { "enum": [1, 2, 3] } },
            "open": { "type": "array", "items": { "type": "string" } },
        }));
        let set = FieldSet::from_properties(&p, &Default::default(), &[]);
        let items = |key: &str| {
            set.by_key(key)
                .unwrap()
                .bounds
                .items
                .as_deref()
                .expect("items")
                .clone()
        };
        assert_eq!(items("tags").choices, vec![json!("ask"), json!("auto")]);
        assert_eq!(items("levels").choices, vec![json!(1), json!(2), json!(3)]);
        assert!(items("open").choices.is_empty(), "no enum, no choices");
    }

    /// `max_memory` and `kill_timeout` carry their grammars through `$defs`,
    /// and `max_restarts` a floor of its own. The two string grammars go
    /// through their own `FromStr` in the gate rather than through these
    /// patterns, so this test is about the reader reaching them at all.
    #[test]
    fn the_real_schema_publishes_the_bounds_the_gate_reads() {
        let set = real_field_set();
        assert_eq!(
            set.by_key("max_memory").unwrap().bounds.pattern.as_deref(),
            Some(r"^\d+(G|M|K)?$")
        );
        assert_eq!(
            set.by_key("kill_timeout")
                .unwrap()
                .bounds
                .pattern
                .as_deref(),
            Some(r"^\d+(ms|d|h|m|s)?$")
        );
        assert_eq!(set.by_key("max_restarts").unwrap().bounds.minimum, Some(0));
        assert_eq!(
            set.by_key("cwd").unwrap().bounds,
            Bounds::default(),
            "a plain path field is held to nothing"
        );
    }
}
