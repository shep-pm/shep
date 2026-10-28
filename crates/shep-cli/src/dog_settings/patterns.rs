//! The door a third-party dog's own grammar has: [`pattern_refusal`].
//!
//! shep has no parser for a type it has never heard of, so a `pattern`
//! is the only thing that can refuse it, and it is enforced as the dog
//! wrote it.

use super::Refusal;

/// [`super::refusal`] for a string field whose schema published a `pattern`
/// and whose type shep does not know: the dog's own grammar, as it wrote it.
///
/// The field's own `accepts` is what the refusal names when it has any,
/// since that is the spelling its author wrote for an operator, and the raw
/// pattern is the fallback. Fails open on a `pattern` that does not compile.
pub(super) fn pattern_refusal(
    key: &str,
    pattern: Option<&str>,
    accepts: &[String],
    typed: &str,
    shown: &str,
) -> Option<Refusal> {
    let compiled = regex::Regex::new(pattern?).ok()?;
    if compiled.is_match(typed) {
        return None;
    }
    let takes = if accepts.is_empty() {
        format!("it matches {}", pattern?)
    } else {
        accepts.join(", ")
    };
    Some(Refusal {
        text: format!("{key} is \"{shown}\", which it does not take; try {takes}"),
    })
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::text_field;
    use super::super::refusal;

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
}
