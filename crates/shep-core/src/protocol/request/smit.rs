//! [`Smit`], the validated marker a sheep carries, and its parse error.

use core::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// A short marker a dog attaches to a sheep for `shep flock` to paint.
///
/// shep stores and prints it, never parses it: `▲ main@a1b2c3` is a
/// deploy tool's sentence.
///
/// The grammar: non-empty once whitespace is discounted, at most
/// [`Self::MAX_CHARS`] characters, no [`char::is_control`] character,
/// `\u{1b}` included. Refused, never repaired, and validated here rather
/// than at the renderer: `shep`'s own `output::width::sanitize_cell` keeps
/// a well-formed CSI sequence, since shep's colouring is made of them.
///
/// [`Self::MAX_CHARS`] counts `char`s, not bytes: a byte cap would refuse a
/// legitimate CJK smit at roughly a third of its apparent length.
///
/// `Debug` is derived: a smit carries no secret, so there is nothing to
/// redact.
///
/// # Example
/// ```
/// use shep_core::protocol::Smit;
///
/// assert_eq!("▲ main@a1b2c3".parse::<Smit>()?.as_str(), "▲ main@a1b2c3");
/// assert!("\u{1b}[2Jgone".parse::<Smit>().is_err()); // no escapes
/// # Ok::<(), shep_core::protocol::SmitError>(())
/// ```
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Smit(String);

impl Smit {
    /// The longest a smit may be, in characters.
    pub const MAX_CHARS: usize = 48;

    /// The marker as text, exactly as its publisher sent it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Smit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::str::FromStr for Smit {
    type Err = SmitError;

    /// # Errors
    /// - [`SmitError::Empty`] if the text is nothing but whitespace.
    /// - [`SmitError::TooLong`] if it is over [`Self::MAX_CHARS`] characters.
    /// - [`SmitError::Unprintable`] if it holds a control character.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.trim().is_empty() {
            return Err(SmitError::Empty);
        }
        let chars = text.chars().count();
        if chars > Self::MAX_CHARS {
            return Err(SmitError::TooLong { chars });
        }
        if text.chars().any(char::is_control) {
            return Err(SmitError::Unprintable);
        }
        Ok(Self(text.to_string()))
    }
}

/// Validates on decode: a dog written in another language speaks this wire
/// directly and never runs [`core::str::FromStr`], so a derived impl would
/// let `\u{1b}[2J` reach every listing built from a smit.
impl<'de> Deserialize<'de> for Smit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // String, not &str: a non-borrowing deserializer cannot always borrow
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Why a string is not a [`Smit`].
///
/// `#[non_exhaustive]`: the grammar can gain a reason to reject, and an
/// out-of-tree consumer matching exhaustively would break on a new variant
/// with no version bump to say so. It buys nothing on the wire. This enum is
/// not serialized: a smit that fails to decode arrives as a serde error
/// carrying the [`fmt::Display`] text below.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmitError {
    /// Over [`Smit::MAX_CHARS`] characters; carries the count that was sent.
    TooLong {
        /// How many characters the string held.
        chars: usize,
    },
    /// A control character, `\u{1b}` included.
    Unprintable,
    /// Empty, or nothing but whitespace.
    Empty,
}

impl fmt::Display for SmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { chars } => write!(
                f,
                "a smit is at most {} characters; this one is {chars}",
                Smit::MAX_CHARS
            ),
            Self::Unprintable => {
                f.write_str("a smit may not contain a control character, an escape included")
            }
            Self::Empty => f.write_str("a smit may not be empty"),
        }
    }
}

impl core::error::Error for SmitError {}

#[cfg(test)]
mod tests {
    use super::super::{Envelope, Request, SelectorSpec};
    use super::*;

    /// A dog written in another language speaks this wire directly and never
    /// runs `FromStr`.
    #[test]
    fn a_smit_is_validated_when_it_is_deserialized_not_only_when_parsed() {
        for bad in [
            r#""\u001b[2Jgone""#.to_string(),                    // an escape
            r#""a\nb""#.to_string(),                             // a newline
            r#""""#.to_string(),                                 // empty
            r#""   ""#.to_string(),                              // whitespace
            format!(r#""{}""#, "x".repeat(Smit::MAX_CHARS + 1)), // too long
        ] {
            assert!(
                serde_json::from_str::<Smit>(&bad).is_err(),
                "a daemon must refuse this on the wire: {bad}"
            );
        }
        assert!(serde_json::from_str::<Smit>(r#""\u25b2 main@a1b2c3""#).is_ok());
    }

    /// The hand-written `Deserialize` agrees with the derived `Serialize`
    /// only while the serialize side stays transparent.
    #[test]
    fn a_smit_travels_as_a_bare_string() {
        let smit: Smit = "\u{25b2} main@a1b2c3".parse().expect("valid");
        let json = serde_json::to_string(&smit).unwrap();
        assert_eq!(json, "\"\u{25b2} main@a1b2c3\"");
        assert_eq!(serde_json::from_str::<Smit>(&json).unwrap(), smit);
    }

    #[test]
    fn a_smit_is_capped_in_characters_not_bytes() {
        let cjk = "\u{7f8a}".repeat(Smit::MAX_CHARS);
        assert_eq!(cjk.len(), Smit::MAX_CHARS * 3);
        assert!(cjk.parse::<Smit>().is_ok(), "{cjk}");
        assert_eq!(
            "x".repeat(Smit::MAX_CHARS + 1).parse::<Smit>(),
            Err(SmitError::TooLong {
                chars: Smit::MAX_CHARS + 1
            })
        );
    }

    #[test]
    fn a_smit_is_stored_exactly_as_it_arrived() {
        let padded: Smit = "  main@a1b2c3  ".parse().expect("valid");
        assert_eq!(padded.as_str(), "  main@a1b2c3  ");
        assert_eq!(padded.to_string(), "  main@a1b2c3  ");
    }

    #[test]
    fn v1_fixture_still_deserializes() {
        // Committed byte fixture from protocol v1. If this breaks, bump
        // PROTOCOL_VERSION and record it in the CHANGELOG.
        let fixture = r#"{"id":7,"deadline_ms":null,"body":{"kind":"stop","selector":{"kind":"name","value":"web"}}}"#;
        let env: Envelope = serde_json::from_str(fixture).unwrap();
        assert_eq!(env.id, 7);
        assert!(matches!(
            env.body,
            Request::Stop { selector: SelectorSpec::Name(ref n) } if n == "web"
        ));
    }
}
