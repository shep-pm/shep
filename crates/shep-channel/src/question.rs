//! The grammar of a question a sheep puts to the operator, and of the
//! answer that comes back.
//!
//! Every type here checks its input the same way whether it is built in
//! Rust or read off the wire, so a hostile frame cannot reach the
//! operator's terminal with a control character.

use serde::{Deserialize, Serialize};

/// The most characters a note may hold.
const NOTE_MAX_CHARS: usize = 500;
/// The most characters an answer to a text question may hold.
const ANSWER_MAX_CHARS: usize = 1000;
/// The most characters a `via` may hold.
const VIA_MAX_CHARS: usize = 64;
/// The most characters a `who` may hold.
const WHO_MAX_CHARS: usize = 128;

/// Why a string is not a valid part of a question or an answer.
// Library crate: a new rule in the grammar is a new variant.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionError {
    /// The field is the empty string.
    Empty {
        /// The wire key of the field.
        field: &'static str,
    },
    /// The field has more than `max` characters.
    TooLong {
        /// The wire key of the field.
        field: &'static str,
        /// The most characters the field may hold.
        max: usize,
        /// How many characters it has.
        chars: usize,
    },
    /// The field holds a control character the field does not allow.
    ControlCharacter {
        /// The wire key of the field.
        field: &'static str,
    },
    /// A question id holds a character outside letters, digits, `.`, `_`
    /// and `-`.
    IdCharacter {
        /// The first character that is not allowed.
        found: char,
    },
    /// The answer to a yes-or-no question is neither `yes` nor `no`.
    NotYesOrNo {
        /// What the answer said instead.
        found: String,
    },
    /// An answer to a text question carries a note.
    NoteOnText,
    /// The question takes a kind of answer this build does not know.
    UnknownTakes,
}

/// The words that name a field in an error message.
fn noun(field: &str) -> &str {
    match field {
        "question" => "a question id",
        "text" => "a question's text",
        "answer" => "an answer",
        "note" => "a note",
        "via" => "`via`",
        "who" => "`who`",
        other => other,
    }
}

impl core::fmt::Display for QuestionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty { field } => write!(f, "{} cannot be empty", noun(field)),
            Self::TooLong { field, max, chars } => write!(
                f,
                "{} holds at most {max} characters, and this one has {chars}",
                noun(field)
            ),
            Self::ControlCharacter { field } => {
                write!(f, "{} cannot hold a control character", noun(field))
            }
            Self::IdCharacter { found } => write!(
                f,
                "a question id takes letters, digits, `.`, `_` and `-`, not `{found}`"
            ),
            Self::NotYesOrNo { found } => {
                write!(f, "this question takes `yes` or `no`, not `{found}`")
            }
            Self::NoteOnText => f.write_str("a note goes with a yes or no answer, not a text one"),
            Self::UnknownTakes => {
                f.write_str("this question takes a kind of answer this shepherd does not know")
            }
        }
    }
}

impl core::error::Error for QuestionError {}

/// The one place the character rules live, so the six fields cannot drift.
fn check_text(
    field: &'static str,
    text: &str,
    max: usize,
    newline: bool,
) -> Result<(), QuestionError> {
    if text.is_empty() {
        return Err(QuestionError::Empty { field });
    }
    let chars = text.chars().count();
    if chars > max {
        return Err(QuestionError::TooLong { field, max, chars });
    }
    if text
        .chars()
        .any(|c| c.is_control() && !(newline && c == '\n'))
    {
        return Err(QuestionError::ControlCharacter { field });
    }
    Ok(())
}

/// Checks the name of the channel an operator answered through.
///
/// # Errors
///
/// [`QuestionError`] when `via` is empty, has more than 64 characters or
/// holds a control character.
pub fn check_via(via: &str) -> Result<(), QuestionError> {
    check_text("via", via, VIA_MAX_CHARS, false)
}

/// Checks the name of the operator who answered.
///
/// # Errors
///
/// [`QuestionError`] when `who` is empty, has more than 128 characters or
/// holds a control character.
pub fn check_who(who: &str) -> Result<(), QuestionError> {
    check_text("who", who, WHO_MAX_CHARS, false)
}

/// Implements what a checked string newtype shares: the wire conversions,
/// `as_str` and `Display`.
macro_rules! checked_string {
    ($name:ident) => {
        impl TryFrom<String> for $name {
            type Error = QuestionError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl $name {
            /// The checked text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

/// The name a sheep gives one question, so its answer can find it.
///
/// Holds one to [`QuestionId::MAX_CHARS`] characters from `A-Z`, `a-z`,
/// `0-9`, `.`, `_` and `-`, whether built with [`QuestionId::new`] or read
/// off the wire.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QuestionId(String);

impl QuestionId {
    /// The most characters an id may hold.
    pub const MAX_CHARS: usize = 32;

    /// An id, checked against the grammar.
    ///
    /// # Errors
    ///
    /// - [`QuestionError::Empty`] when `id` is empty.
    /// - [`QuestionError::TooLong`] when `id` has more than
    ///   [`Self::MAX_CHARS`] characters.
    /// - [`QuestionError::IdCharacter`] on the first character outside
    ///   `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`.
    pub fn new(id: impl Into<String>) -> Result<Self, QuestionError> {
        let id = id.into();
        check_text("question", &id, Self::MAX_CHARS, false)?;
        if let Some(found) = id
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        {
            return Err(QuestionError::IdCharacter { found });
        }
        Ok(Self(id))
    }
}

checked_string!(QuestionId);

/// The words of a question, shown to the operator as written.
///
/// Holds one to [`QuestionText::MAX_CHARS`] characters. A newline is
/// allowed; every other control character is refused.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QuestionText(String);

impl QuestionText {
    /// The most characters a question may hold.
    pub const MAX_CHARS: usize = 1000;

    /// A question's text, checked against the grammar.
    ///
    /// # Errors
    ///
    /// - [`QuestionError::Empty`] when `text` is empty.
    /// - [`QuestionError::TooLong`] when `text` has more than
    ///   [`Self::MAX_CHARS`] characters.
    /// - [`QuestionError::ControlCharacter`] when `text` holds a control
    ///   character other than a newline.
    pub fn new(text: impl Into<String>) -> Result<Self, QuestionError> {
        let text = text.into();
        check_text("text", &text, Self::MAX_CHARS, true)?;
        Ok(Self(text))
    }
}

checked_string!(QuestionText);

/// What kind of answer a question takes.
// wire format: changing these strings is a breaking change
// A `choice` kind is anticipated, so a new kind must not break a match.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Takes {
    /// `yes` or `no`, with an optional note.
    YesNo,
    /// Free text, with no note.
    Text,
    /// A kind this build has not been taught.
    ///
    /// Only ever produced by decoding: an unknown string falls through to
    /// this variant via `#[serde(other)]` instead of failing the whole
    /// frame. Nothing constructs one to send, and the shepherd drops an
    /// `ask` carrying it. That is a call-site invariant rather than a
    /// type-level one: `#[serde(other)]` governs decoding only, so
    /// serializing this would emit `"unrecognized"`.
    #[serde(other)]
    Unrecognized,
}

impl Takes {
    /// Checks an answer and its note against what this question takes.
    ///
    /// # Errors
    ///
    /// - [`QuestionError::NotYesOrNo`] when a yes-or-no answer is neither.
    /// - [`QuestionError::NoteOnText`] when a text answer carries a note.
    /// - [`QuestionError::UnknownTakes`] for [`Takes::Unrecognized`], which
    ///   no answer fits.
    /// - [`QuestionError`] from the character rules when the note (at most
    ///   500 characters) or a text answer (at most 1000, newlines allowed)
    ///   breaks them.
    pub fn check(self, answer: &str, note: Option<&str>) -> Result<(), QuestionError> {
        match self {
            Self::YesNo => {
                if answer != "yes" && answer != "no" {
                    return Err(QuestionError::NotYesOrNo {
                        found: answer.to_string(),
                    });
                }
                note.map_or(Ok(()), |note| {
                    check_text("note", note, NOTE_MAX_CHARS, false)
                })
            }
            Self::Text => {
                if note.is_some() {
                    return Err(QuestionError::NoteOnText);
                }
                check_text("answer", answer, ANSWER_MAX_CHARS, true)
            }
            Self::Unrecognized => Err(QuestionError::UnknownTakes),
        }
    }
}

/// The operator's answer to one question.
///
/// Fields are not checked here: the shepherd checks an answer against the
/// question's [`Takes`] before it sends one.
// wire format: changing this is a breaking change
// Library crate: an answer may gain a field.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The question this answers.
    pub question: QuestionId,
    /// The answer: `yes` or `no`, or text.
    pub answer: String,
    /// The operator's reason, for a yes-or-no answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The channel the operator answered through, such as `discord`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Who answered, as that channel names them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
}

impl Answer {
    /// An answer to `question`, with no note, `via` or `who`.
    #[must_use]
    pub fn new(question: QuestionId, answer: impl Into<String>) -> Self {
        Self {
            question,
            answer: answer.into(),
            note: None,
            via: None,
            who: None,
        }
    }

    /// Adds the operator's note.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Adds the channel the operator answered through.
    #[must_use]
    pub fn with_via(mut self, via: impl Into<String>) -> Self {
        self.via = Some(via.into());
        self
    }

    /// Adds who answered.
    #[must_use]
    pub fn with_who(mut self, who: impl Into<String>) -> Self {
        self.who = Some(who.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_id_takes_letters_digits_dot_underscore_and_dash() {
        assert!(QuestionId::new("koji-3.retry_2").is_ok());
        assert_eq!(
            QuestionId::new(""),
            Err(QuestionError::Empty { field: "question" })
        );
        assert_eq!(
            QuestionId::new("a b"),
            Err(QuestionError::IdCharacter { found: ' ' })
        );
        assert_eq!(
            QuestionId::new("ü"),
            Err(QuestionError::IdCharacter { found: 'ü' })
        );
        assert!(QuestionId::new("x".repeat(32)).is_ok());
        assert_eq!(
            QuestionId::new("x".repeat(33)),
            Err(QuestionError::TooLong {
                field: "question",
                max: 32,
                chars: 33
            })
        );
    }

    #[test]
    fn question_text_keeps_newlines_and_refuses_every_other_control_character() {
        assert!(QuestionText::new("Merge #12?\nCI is green.").is_ok());
        assert_eq!(
            QuestionText::new("a\rb"),
            Err(QuestionError::ControlCharacter { field: "text" })
        );
        assert_eq!(
            QuestionText::new("\u{1b}[2J"),
            Err(QuestionError::ControlCharacter { field: "text" })
        );
        assert_eq!(
            QuestionText::new(""),
            Err(QuestionError::Empty { field: "text" })
        );
        assert!(
            QuestionText::new("é".repeat(1000)).is_ok(),
            "counted in characters, not bytes"
        );
        assert!(QuestionText::new("é".repeat(1001)).is_err());
    }

    #[test]
    fn a_yes_no_question_takes_exactly_yes_or_no_and_an_optional_note() {
        assert_eq!(Takes::YesNo.check("yes", None), Ok(()));
        assert_eq!(Takes::YesNo.check("no", Some("rebase first")), Ok(()));
        assert_eq!(
            Takes::YesNo.check("Yes", None),
            Err(QuestionError::NotYesOrNo {
                found: "Yes".to_string()
            })
        );
        assert_eq!(
            Takes::YesNo.check("no", Some(&"n".repeat(501))),
            Err(QuestionError::TooLong {
                field: "note",
                max: 500,
                chars: 501
            })
        );
        assert_eq!(
            Takes::YesNo.check("no", Some("a\nb")),
            Err(QuestionError::ControlCharacter { field: "note" })
        );
    }

    #[test]
    fn a_text_question_takes_any_text_and_no_note() {
        assert_eq!(Takes::Text.check("call it kelpie-probe", None), Ok(()));
        assert_eq!(Takes::Text.check("two\nlines", None), Ok(()));
        assert_eq!(
            Takes::Text.check("", None),
            Err(QuestionError::Empty { field: "answer" })
        );
        assert_eq!(
            Takes::Text.check("x", Some("y")),
            Err(QuestionError::NoteOnText)
        );
    }

    #[test]
    fn via_and_who_are_short_and_carry_no_control_character() {
        assert!(check_via("discord").is_ok());
        assert!(check_via(&"v".repeat(65)).is_err());
        assert!(check_who(&"w".repeat(128)).is_ok());
        assert!(check_who("<@1>\n").is_err());
    }

    #[test]
    fn a_question_off_the_wire_is_checked_like_one_built_in_rust() {
        let bad = r#"{"kind":"ask","question":"a b","text":"x","takes":"yes-no"}"#;
        assert!(serde_json::from_str::<crate::ChildMessage>(bad).is_err());
    }

    #[test]
    fn an_unknown_takes_decodes_as_unrecognized_and_no_answer_fits_it() {
        let unknown = r#"{"kind":"ask","question":"q","text":"x","takes":"maybe"}"#;
        let crate::ChildMessage::Ask { takes, .. } =
            serde_json::from_str::<crate::ChildMessage>(unknown).unwrap()
        else {
            panic!("an ask with an unknown `takes` must still decode as an ask");
        };
        assert_eq!(takes, Takes::Unrecognized);
        assert_eq!(
            Takes::Unrecognized.check("yes", None),
            Err(QuestionError::UnknownTakes)
        );
    }

    #[test]
    fn every_error_names_its_field_in_plain_words() {
        assert_eq!(
            QuestionError::TooLong {
                field: "note",
                max: 500,
                chars: 501
            }
            .to_string(),
            "a note holds at most 500 characters, and this one has 501"
        );
        assert_eq!(
            QuestionError::NotYesOrNo {
                found: "maybe".to_string()
            }
            .to_string(),
            "this question takes `yes` or `no`, not `maybe`"
        );
        for (field, noun) in [
            ("question", "a question id"),
            ("text", "a question's text"),
            ("answer", "an answer"),
            ("note", "a note"),
            ("via", "`via`"),
            ("who", "`who`"),
        ] {
            assert_eq!(
                QuestionError::Empty { field }.to_string(),
                format!("{noun} cannot be empty")
            );
        }
    }
}
