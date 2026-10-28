//! The destination grammar for reaching a host over `ssh`.
//!
//! A host is reached by handing its name to `ssh` as an argument, so a
//! destination that starts with `-` would be read as an option
//! (`-oProxyCommand=...` runs a command). The grammar lives here once, and
//! every caller holds a [`Destination`] rather than a checked `&str`.

use core::fmt;

/// Longest destination accepted, in bytes.
pub const MAX_DESTINATION_BYTES: usize = 255;

/// A host to reach over `ssh`, as an optional `user@` and a host.
///
/// Each part is one or more of ASCII letters, digits, `.`, `_` and `-`.
/// Neither starts with `-`. Ports, IPv6 literals and `ssh://` URIs are
/// refused: they belong in `~/.ssh/config` as an alias.
///
/// `Debug` is derived: a destination names a machine, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Destination(String);

/// Why [`Destination::parse`] refused a destination.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationError {
    /// The text was empty.
    Empty,
    /// The text was over [`MAX_DESTINATION_BYTES`]; carries its length in bytes.
    TooLong(usize),
    /// The text held a `:`, which would be a port, an IPv6 literal or a URI.
    Colon,
    /// The text held more than one `@`.
    ExtraAt,
    /// The user or the host was empty (`@host`, `user@`).
    EmptyPart,
    /// A part started with `-`, which `ssh` would read as an option.
    LeadingDash,
    /// A character outside letters, digits, `.`, `_`, `-` and the one `@`.
    InvalidChar(char),
}

impl fmt::Display for DestinationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a destination cannot be empty"),
            Self::TooLong(len) => write!(
                f,
                "destination is {len} bytes, over the {MAX_DESTINATION_BYTES}-byte limit"
            ),
            Self::Colon => f.write_str(
                "a destination cannot contain `:`; ports and jump hosts go in `~/.ssh/config`",
            ),
            Self::ExtraAt => f.write_str("a destination can contain at most one `@`"),
            Self::EmptyPart => {
                f.write_str("the user and the host of a destination cannot be empty")
            }
            Self::LeadingDash => {
                f.write_str("a destination cannot start with `-`, which ssh reads as an option")
            }
            Self::InvalidChar(ch) => write!(
                f,
                "{ch:?} is not allowed in a destination; use letters, digits, `.`, `_` and `-`"
            ),
        }
    }
}

impl core::error::Error for DestinationError {}

/// Checks one side of the `@`: non-empty, not an option, in the character set.
fn check_part(part: &str) -> Result<(), DestinationError> {
    if part.is_empty() {
        return Err(DestinationError::EmptyPart);
    }
    if part.starts_with('-') {
        return Err(DestinationError::LeadingDash);
    }
    match part
        .chars()
        .find(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')))
    {
        Some(ch) => Err(DestinationError::InvalidChar(ch)),
        None => Ok(()),
    }
}

impl Destination {
    /// Checks `text` against the destination grammar and holds it as written.
    ///
    /// # Errors
    ///
    /// - [`DestinationError::Empty`]: `text` is empty.
    /// - [`DestinationError::TooLong`]: `text` is over [`MAX_DESTINATION_BYTES`].
    /// - [`DestinationError::Colon`]: `text` holds a `:`.
    /// - [`DestinationError::ExtraAt`]: `text` holds a second `@`.
    /// - [`DestinationError::EmptyPart`]: the user or the host is empty.
    /// - [`DestinationError::LeadingDash`]: the user or the host starts with `-`.
    /// - [`DestinationError::InvalidChar`]: `text` holds any other character
    ///   outside the grammar, such as whitespace, a control character, `/`,
    ///   `%` or a bracket.
    pub fn parse(text: &str) -> Result<Self, DestinationError> {
        if text.is_empty() {
            return Err(DestinationError::Empty);
        }
        if text.len() > MAX_DESTINATION_BYTES {
            return Err(DestinationError::TooLong(text.len()));
        }
        if text.contains(':') {
            return Err(DestinationError::Colon);
        }
        let mut parts = text.split('@');
        let first = parts.next().unwrap_or_default();
        let second = parts.next();
        if parts.next().is_some() {
            return Err(DestinationError::ExtraAt);
        }
        check_part(first)?;
        if let Some(host) = second {
            check_part(host)?;
        }
        Ok(Self(text.to_owned()))
    }

    /// The destination exactly as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Destination {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(text: &str) -> DestinationError {
        Destination::parse(text).expect_err(text)
    }

    #[test]
    fn a_plain_name_a_dotted_name_and_a_user_at_name_are_accepted() {
        for text in [
            "web1",
            "web-1.example.com",
            "db_host",
            "ada@web1",
            "ada.l@10.0.0.1",
        ] {
            assert!(Destination::parse(text).is_ok(), "{text}");
        }
    }

    #[test]
    fn a_destination_ssh_could_read_as_an_option_is_refused() {
        for text in ["-oProxyCommand=x", "-J", "--", "ada@-x", "-a@host"] {
            let err = refused(text);
            assert!(
                matches!(
                    err,
                    DestinationError::LeadingDash | DestinationError::InvalidChar(_)
                ),
                "{text}: {err:?}"
            );
        }
        assert_eq!(refused("-J"), DestinationError::LeadingDash);
        assert_eq!(refused("ada@-x"), DestinationError::LeadingDash);
    }

    #[test]
    fn whitespace_a_colon_a_slash_a_percent_a_bracket_a_second_at_and_a_control_character_are_refused()
     {
        for text in [
            "web 1",
            "web\t1",
            "web\n",
            "host:22",
            "[::1]",
            "ssh://host",
            "a/b",
            "web%eth0",
            "[host]",
            "a\u{0}b",
            "a\u{7f}b",
            "web\u{e9}",
        ] {
            assert!(Destination::parse(text).is_err(), "{text:?}");
        }
        assert_eq!(refused("a@b@c"), DestinationError::ExtraAt);
        assert_eq!(refused("host:22"), DestinationError::Colon);
        assert_eq!(refused("web 1"), DestinationError::InvalidChar(' '));
        assert!(refused("host:22").to_string().contains("~/.ssh/config"));
    }

    #[test]
    fn an_empty_or_over_long_destination_is_refused() {
        assert_eq!(refused(""), DestinationError::Empty);
        assert_eq!(refused("@host"), DestinationError::EmptyPart);
        assert_eq!(refused("ada@"), DestinationError::EmptyPart);
        assert!(Destination::parse(&"a".repeat(MAX_DESTINATION_BYTES)).is_ok());
        assert_eq!(
            refused(&"a".repeat(MAX_DESTINATION_BYTES + 1)),
            DestinationError::TooLong(MAX_DESTINATION_BYTES + 1)
        );
    }

    #[test]
    fn the_text_round_trips_exactly_as_written() {
        let text = "Ada@Web-1.Example.COM";
        let destination = Destination::parse(text).unwrap();
        assert_eq!(destination.as_str(), text);
        assert_eq!(destination.to_string(), text);
        assert_eq!(destination.as_ref(), text);
    }
}
