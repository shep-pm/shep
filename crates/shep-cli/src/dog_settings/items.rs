//! The gate for one element of a list field: [`super::item_refusal`]
//! and the rules an item schema can publish.
//!
//! The rules come from the item schema, which is a different schema
//! from the one the dog published about the array, and an element with
//! no rule on it is accepted exactly as a scalar field its schema says
//! nothing about is.

use serde_json::Value;

use super::numbers::{integer_of, range_refusal};
use super::patterns::pattern_refusal;
use super::{Refusal, enum_refusal, shown_value, takes_one_of};
use crate::lookout::field::{Bounds, ListItem};

/// Why one typed element of a list field was not filed, in the operator's
/// words.
///
/// The rules are the item schema's own, not the dog's about the array, and
/// an element with no rule on it is accepted. A [`ListItem::Choice`] element
/// goes through [`takes_one_of`], which the pane never reaches. `secret` is
/// resolved once, so a rule added later cannot echo a credential.
#[must_use]
pub fn item_refusal(
    key: &str,
    item: &ListItem,
    bounds: &Bounds,
    typed: &str,
    secret: bool,
) -> Option<Refusal> {
    let shown = shown_value(secret, typed);
    let value = match item {
        ListItem::Integer => match integer_of(key, bounds, typed, shown) {
            Ok(number) => {
                if let Some(refused) = range_refusal(key, bounds, shown, number) {
                    return Some(refused);
                }
                Value::from(number)
            }
            Err(refused) => return Some(refused),
        },
        // A closed set is chosen from, not typed, so it is the whole rule on
        // the element: the same reason a scalar [`crate::lookout::field::FieldKind::Choice`]
        // never reaches [`super::refusal`]'s grammar arms.
        ListItem::Choice(names) => {
            if !names.iter().any(|name| name == typed) {
                return Some(takes_one_of(key, shown, names));
            }
            Value::String(typed.to_owned())
        }
        ListItem::Text => {
            if let Some(refused) = min_length_refusal(key, bounds, typed, shown) {
                return Some(refused);
            }
            if let Some(refused) =
                pattern_refusal(key, bounds.pattern.as_deref(), &[], typed, shown)
            {
                return Some(refused);
            }
            Value::String(typed.to_owned())
        }
    };
    enum_refusal(key, bounds, &value, shown)
}

/// [`item_refusal`]'s `minLength` arm. The count is of characters, so an
/// element with one accented letter is one character long rather than two.
fn min_length_refusal(key: &str, bounds: &Bounds, typed: &str, shown: &str) -> Option<Refusal> {
    let least = bounds.min_length?;
    (typed.chars().count() < least).then(|| Refusal {
        text: format!("{key} is \"{shown}\"; {key} needs at least {least} characters"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rule an item schema can publish, with the value it takes and
    /// the one it turns down, and the whole sentence for the refused one.
    ///
    /// `minLength` is checked at exactly its bound, because a rule that
    /// refused its own minimum would be a field nothing can be set to.
    #[test]
    fn every_item_rule_refuses_one_element_and_takes_the_next() {
        let min_length = Bounds {
            min_length: Some(3),
            ..Default::default()
        };
        let pattern = Bounds {
            pattern: Some("^[a-z]+$".to_owned()),
            ..Default::default()
        };
        let words = Bounds {
            choices: vec![serde_json::json!("ask"), serde_json::json!("auto")],
            ..Default::default()
        };
        let numbers = Bounds {
            choices: vec![
                serde_json::json!(1),
                serde_json::json!(2),
                serde_json::json!(3),
            ],
            ..Default::default()
        };
        let range = Bounds {
            minimum: Some(1),
            maximum: Some(64),
            ..Default::default()
        };
        let cases = [
            (
                "minLength",
                ListItem::Text,
                &min_length,
                "abc",
                "ab",
                "tags is \"ab\"; tags needs at least 3 characters",
            ),
            (
                "pattern",
                ListItem::Text,
                &pattern,
                "abc",
                "Ab1",
                "tags is \"Ab1\", which it does not take; try it matches ^[a-z]+$",
            ),
            (
                "enum",
                ListItem::Text,
                &words,
                "auto",
                "never",
                "tags is \"never\"; tags takes one of ask, auto",
            ),
            (
                "integer enum",
                ListItem::Integer,
                &numbers,
                "2",
                "4",
                "tags is \"4\"; tags takes one of 1, 2, 3",
            ),
            (
                "minimum",
                ListItem::Integer,
                &range,
                "1",
                "0",
                "tags is \"0\"; tags starts at 1",
            ),
            (
                "maximum",
                ListItem::Integer,
                &range,
                "64",
                "65",
                "tags is \"65\"; tags stops at 64",
            ),
        ];
        for (rule, item, bounds, accepted, refused, sentence) in cases {
            assert_eq!(
                item_refusal("tags", &item, bounds, accepted, false),
                None,
                "{rule}: {accepted} is the value the rule takes"
            );
            let refused = item_refusal("tags", &item, bounds, refused, false).expect(rule);
            assert_eq!(refused.text, sentence, "{rule}");
        }
    }

    /// An `enum` member a schema wrote with a decimal point is the number
    /// the pane parsed, and a `Value` comparison says otherwise:
    /// `serde_json` keeps `1` and `1.0` as two numbers, and JSON Schema
    /// means one of them. A dog publishing `enum: [1.0, 2.0]` for a
    /// `Vec<i64>` had its own values refused.
    #[test]
    fn an_enum_number_written_with_a_decimal_point_still_takes_the_integer() {
        let bounds = Bounds {
            choices: vec![serde_json::json!(1.0), serde_json::json!(2.0)],
            ..Default::default()
        };
        for typed in ["1", "2"] {
            assert_eq!(
                item_refusal("levels", &ListItem::Integer, &bounds, typed, false),
                None,
                "{typed} is the value the schema wrote"
            );
        }
        let refused = item_refusal("levels", &ListItem::Integer, &bounds, "3", false)
            .expect("3 is not in the set");
        assert_eq!(
            refused.text,
            "levels is \"3\"; levels takes one of 1.0, 2.0"
        );

        // The other direction holds: a set holding a fraction does not take
        // the whole number beside it.
        let halves = Bounds {
            choices: vec![serde_json::json!(1.5)],
            ..Default::default()
        };
        let refused =
            item_refusal("levels", &ListItem::Integer, &halves, "1", false).expect("1 is not 1.5");
        assert_eq!(refused.text, "levels is \"1\"; levels takes one of 1.5");
    }

    /// The one thing an integer list refuses even with no bounds on its
    /// items, and the sentence the scalar editor already uses for it.
    #[test]
    fn an_integer_element_that_is_not_a_number_is_refused_out_loud() {
        let bounds = Bounds::default();
        let refused = item_refusal(
            "stop_exit_codes",
            &ListItem::Integer,
            &bounds,
            "lots",
            false,
        )
        .expect("lots is not a number");
        assert_eq!(
            refused.text,
            "stop_exit_codes is \"lots\", which is not a whole number shep accepts"
        );
        assert_eq!(
            item_refusal("stop_exit_codes", &ListItem::Integer, &bounds, "-1", false),
            None,
            "a negative exit code is a whole number"
        );
    }

    /// A list whose items are a plain string, which is every list the
    /// Flockfile schema publishes today. Nothing is refused, and the gate
    /// is not shep's chance to invent a grammar for a dog's array.
    #[test]
    fn a_list_with_no_item_rules_refuses_nothing() {
        let bounds = Bounds::default();
        for typed in ["a", "!!! anything", ""] {
            assert_eq!(
                item_refusal("args", &ListItem::Text, &bounds, typed, false),
                None,
                "{typed}"
            );
        }
    }

    /// The count is of characters, not bytes. `"ab\u{00e9}"` is four bytes
    /// and three characters, so a byte count would have taken it and a
    /// character count refuses it.
    #[test]
    fn a_min_length_counts_characters_and_not_bytes() {
        let bounds = Bounds {
            min_length: Some(4),
            ..Default::default()
        };
        assert_eq!(
            item_refusal("tags", &ListItem::Text, &bounds, "abc\u{00e9}", false),
            None,
            "four characters, five bytes"
        );
        let refused = item_refusal("tags", &ListItem::Text, &bounds, "ab\u{00e9}", false)
            .expect("three characters is under four");
        assert_eq!(
            refused.text,
            "tags is \"ab\u{00e9}\"; tags needs at least 4 characters"
        );
    }

    /// A dog that ships a broken regex in its item schema has a bug in its
    /// schema, and the list editor fails open on it for [`super::refusal`]'s
    /// reason: refusing every element would lock an operator out of a
    /// setting over somebody else's typo.
    #[test]
    fn a_list_pattern_that_does_not_compile_refuses_nothing() {
        let broken = ["^[", "a-z"].concat();
        assert!(
            regex::Regex::new(&broken).is_err(),
            "the fixture must be a pattern regex really refuses"
        );
        let bounds = Bounds {
            pattern: Some(broken),
            ..Default::default()
        };
        assert_eq!(
            item_refusal("tags", &ListItem::Text, &bounds, "ab", false),
            None,
            "a pattern that does not compile bounds nothing"
        );
    }

    /// A list of credentials reaches a refusal the same two doors a scalar
    /// does, and neither may print the element: the list sub-screen draws
    /// what is typed, and this sentence is the one place a value typed into
    /// it would be repeated.
    #[test]
    fn a_secret_lists_refusal_names_the_marker_and_never_the_element() {
        const CREDENTIAL: &str = "tok_HUNTER2";
        let pattern = Bounds {
            pattern: Some("^sk-".to_owned()),
            ..Default::default()
        };
        let refused = item_refusal("tokens", &ListItem::Text, &pattern, CREDENTIAL, true)
            .expect("the pattern refuses it");
        assert_eq!(
            refused.text,
            "tokens is \"<set>\", which it does not take; try it matches ^sk-"
        );

        let min_length = Bounds {
            min_length: Some(32),
            ..Default::default()
        };
        let refused = item_refusal("tokens", &ListItem::Text, &min_length, CREDENTIAL, true)
            .expect("too short");
        assert!(!refused.text.contains(CREDENTIAL), "{}", refused.text);
        assert!(refused.text.contains("<set>"), "{}", refused.text);

        let choices = Bounds {
            choices: vec![serde_json::json!("ask"), serde_json::json!("auto")],
            ..Default::default()
        };
        let refused = item_refusal("tokens", &ListItem::Text, &choices, CREDENTIAL, true)
            .expect("not a member");
        assert!(!refused.text.contains(CREDENTIAL), "{}", refused.text);

        let range = Bounds {
            minimum: Some(1000),
            ..Default::default()
        };
        let refused =
            item_refusal("pins", &ListItem::Integer, &range, "7", true).expect("below the floor");
        assert_eq!(refused.text, "pins is \"<set>\"; pins starts at 1000");
    }

    /// The control on the test above: an element of a list that is not a
    /// credential is quoted back, since redacting everything would pass
    /// that test while making every refusal useless.
    #[test]
    fn an_ordinary_element_is_still_quoted_back() {
        let bounds = Bounds {
            pattern: Some("^eu-".to_owned()),
            ..Default::default()
        };
        assert!(
            item_refusal("regions", &ListItem::Text, &bounds, "Europe", false)
                .expect("refused")
                .text
                .contains("Europe"),
        );
    }
}

/// A closed set spelled as a `oneOf` of `const`s publishes no `enum`, so
/// `bounds.choices` holds nothing and the `enum` arm has nothing to check
/// against. The names live in the item itself, and this is the door that
/// has to read them: the pane toggles a choice list or types it as
/// `ListItem::Text`, and only the table door hands an element to this arm.
#[test]
fn a_choice_element_is_held_to_the_names_the_item_carries() {
    let names = vec!["ask".to_owned(), "auto".to_owned()];
    let bounds = Bounds::default();
    for typed in ["ask", "auto"] {
        assert_eq!(
            item_refusal(
                "modes",
                &ListItem::Choice(names.clone()),
                &bounds,
                typed,
                false
            ),
            None,
            "{typed} is one of the names"
        );
    }
    let refused = item_refusal(
        "modes",
        &ListItem::Choice(names.clone()),
        &bounds,
        "never",
        false,
    )
    .expect("never is not one of the names");
    assert_eq!(
        refused.text,
        "modes is \"never\"; modes takes one of ask, auto"
    );

    let refused = item_refusal(
        "modes",
        &ListItem::Choice(names),
        &bounds,
        "tok_HUNTER2",
        true,
    )
    .expect("a credential is not one of the names");
    assert_eq!(
        refused.text,
        "modes is \"<set>\"; modes takes one of ask, auto"
    );
}
