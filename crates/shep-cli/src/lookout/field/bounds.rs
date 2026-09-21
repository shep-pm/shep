//! An integer field's floor and ceiling, read off its schema.
//!
//! Separate from [`super::schema`] because the grammar is its own: JSON
//! Schema writes `minimum` and `maximum` as arbitrary numbers, and a
//! `Field` needs a pair of `i64` bounds plus an answer for a range that
//! holds nothing. Normalising one into the other is the whole job here.

use serde_json::{Map, Value};

use super::schema::resolved;

/// The bounds a property's schema publishes beyond its type, for the two
/// keywords this pane can actually check a keystroke against.
///
/// Read off the resolved schema, so a `$ref` into `$defs` and an
/// `anyOf: [T, null]` are both followed first: `max_age` on a dog that
/// spells it `Option<UpDuration>` carries its pattern two hops away from
/// the property itself.
///
/// Empty for a field whose schema says nothing machine-checkable, which is
/// most of them. A bound this type does not carry is a bound
/// [`super::super::validation::refusal`] cannot enforce, and the pane
/// files the value: shep is not the authority on a dog's own config.
///
/// `Debug` is derived (IR-41): a schema's bounds describe a value without
/// carrying one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bounds {
    /// `pattern`, as the schema wrote it. A dog's own grammar reaches the
    /// pane only through this, since shep has no `FromStr` for a type it
    /// has never heard of.
    pub pattern: Option<String>,
    /// `minimum`, for a [`super::FieldKind::Integer`] field, normalised
    /// to the smallest `i64` the schema accepts. shep-log-rotate's `keep`
    /// publishes one for exactly this reason: without it the pane offers a
    /// `keep = 0` the dog refuses on its next tick, in a file the operator
    /// has already saved and moved on from.
    pub minimum: Option<i64>,
    /// `maximum`, normalised to the largest `i64` the schema accepts.
    pub maximum: Option<i64>,
    /// Whether the schema's own numeric range admits no integer at all.
    ///
    /// A dog that writes `range(min = 3, max = 1)` publishes a field
    /// nothing can set. Without this the pane refuses `1` for being under
    /// the floor and `3` for being over the ceiling, one sentence at a
    /// time, and never says that no answer exists.
    pub unsatisfiable: bool,
}

/// The [`Bounds`] the resolved schema publishes.
///
/// A numeric bound is read as an `f64` and normalised to the integer it
/// actually forbids, because JSON Schema's bounds are inclusive: a
/// `minimum` rounds UP and a `maximum` rounds DOWN, so `minimum: 0.5`
/// forbids `0` and `maximum: 2.5` forbids `3`.
///
/// Reading them with `as_i64` instead dropped both, and a dropped bound is
/// an accepted keystroke: the pane filed a `0` its own schema forbade.
/// Reading them as `f64` also keeps a bound written as `1e30`, which no
/// `i64` satisfies, apart from one written as `1`.
pub(super) fn bounds_of(schema: &Value, defs: &Map<String, Value>) -> Bounds {
    let schema = resolved(schema, defs);
    let minimum = schema.get("minimum").and_then(Value::as_f64).map(f64::ceil);
    let maximum = schema
        .get("maximum")
        .and_then(Value::as_f64)
        .map(f64::floor);
    let (floor, ceiling) = (minimum.and_then(as_bound), maximum.and_then(as_bound));
    Bounds {
        pattern: schema
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::to_owned),
        minimum: floor,
        maximum: ceiling,
        // A bound past an `i64`'s reach is impossible in one direction and
        // says nothing in the other: a `minimum` above `i64::MAX` admits
        // nothing, while one below `i64::MIN` admits every integer there
        // is, which is what `as_bound` already answered `None` for.
        unsatisfiable: minimum.is_some_and(|value| value > i64::MAX as f64)
            || maximum.is_some_and(|value| value < i64::MIN as f64)
            || matches!((floor, ceiling), (Some(low), Some(high)) if low > high),
    }
}

/// `value` as the `i64` bound it names, or [`None`] when it sits outside
/// what an `i64` can hold and so bounds nothing this pane can compare.
///
/// The comparison is in `f64`, which cannot name every `i64` exactly near
/// its ends. A bound within a few units of `i64::MAX` therefore saturates
/// to it rather than being refused, which is the right answer for a field
/// no operator is typing sixteen digits into.
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

    /// Every hop [`resolved`] follows, because a reader that followed one
    /// and not the other reports a nullable field's bounds as absent, and
    /// nothing downstream can tell that from a schema that published none.
    ///
    /// Three spellings of the same `pattern`: inline on the property, one
    /// `$ref` away, and behind the `anyOf: [{$ref}, {type: null}]` that a
    /// dog writes for an `Option<UpDuration>`. That third one is the shape
    /// `shep-log-rotate` publishes, so it is the shape the gate in
    /// [`super::super::validation::refusal`] actually meets.
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

    /// The numeric half, and the reason it is here rather than only in the
    /// gate's own tests: `shep-log-rotate` publishes `minimum: 1` on
    /// `keep`, so the floor an operator is held to is one this reader has
    /// to lift off somebody else's schema.
    ///
    /// A fractional bound is normalised to the integer it forbids, not
    /// dropped: JSON Schema's bounds are inclusive, so a `minimum` rounds
    /// up and a `maximum` rounds down. Dropping them accepted a `0` under
    /// a `minimum` of `0.5`, and the pane filed it.
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

    /// The bounds the real Flockfile schema publishes, so a change to it
    /// that moved one lands here rather than on an operator.
    ///
    /// `max_memory` and `kill_timeout` carry their grammars through
    /// `$defs`, and `max_restarts` a floor of its own. The two string
    /// grammars go through their own `FromStr` in the gate rather than
    /// through these patterns, so this test is about the reader reaching
    /// them at all.
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
            Some(r"^\d+(ms|h|m|s)?$")
        );
        assert_eq!(set.by_key("max_restarts").unwrap().bounds.minimum, Some(0));
        assert_eq!(
            set.by_key("cwd").unwrap().bounds,
            Bounds::default(),
            "a plain path field is held to nothing"
        );
    }
}
