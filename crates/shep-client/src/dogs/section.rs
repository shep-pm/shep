//! A dog's section of `dogs.toml`, parsed without ever quoting it.
//!
//! The TOML parser's own message prints the offending line, and a dog's
//! section routinely holds a webhook token. [`SectionError`] keeps only
//! the line number, so no caller can print a credential by accident.

use core::fmt;

use serde::de::DeserializeOwned;

/// A dog's section that does not fit its config type, reported by where
/// and never by what.
///
/// Carries no source: the parser's error is the thing that quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionError {
    section: String,
    line: Option<usize>,
}

impl SectionError {
    /// The `[<name>]` section that did not fit.
    #[must_use]
    pub fn section(&self) -> &str {
        &self.section
    }

    /// The line the parser stopped at, counting from the one under the
    /// section's header, when it named a place.
    #[must_use]
    pub fn line(&self) -> Option<usize> {
        self.line
    }

    /// [`shep_core::exit::INVALID_CONFIG`], the code a dog stopping on its
    /// own section exits with.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        shep_core::exit::INVALID_CONFIG
    }
}

impl fmt::Display for SectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { section, line } = self;
        match line {
            Some(line) => write!(
                f,
                "line {line} under [{section}] in dogs.toml does not fit this dog's settings"
            ),
            None => write!(
                f,
                "[{section}] in dogs.toml does not fit this dog's settings"
            ),
        }
    }
}

impl core::error::Error for SectionError {}

/// `text`, the `[<section>]` the shepherd served, parsed into `T`, or
/// `T::default()` when it is empty, which is how a dog with no section
/// is answered.
///
/// # Errors
///
/// [`SectionError`] when `text` does not fit `T`, naming the line.
pub fn parse_section<T>(section: &str, text: &str) -> Result<T, SectionError>
where
    T: DeserializeOwned + Default,
{
    if text.is_empty() {
        return Ok(T::default());
    }
    toml::from_str(text).map_err(|err| SectionError {
        section: section.to_owned(),
        line: err
            .span()
            .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, serde::Deserialize, PartialEq)]
    #[serde(deny_unknown_fields, default)]
    struct Settings {
        port: u16,
        webhook: String,
    }

    #[test]
    fn an_empty_section_parses_to_the_defaults_even_with_a_required_field() {
        #[derive(Debug, Default, serde::Deserialize, PartialEq)]
        struct Required {
            port: u16,
        }
        assert!(
            toml::from_str::<Required>("").is_err(),
            "the fixture must refuse an empty document, or this proves nothing"
        );
        assert_eq!(
            parse_section::<Required>("metrics", "").unwrap(),
            Required::default()
        );
    }

    #[test]
    fn a_section_that_fits_parses() {
        let parsed = parse_section::<Settings>("bark", "port = 9000\n").unwrap();
        assert_eq!(parsed.port, 9000);
    }

    /// Every shape the parser can refuse a value in, with a credential
    /// written where it would be quoted.
    #[test]
    fn a_refusal_names_the_line_and_never_the_value() {
        let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
        let refused = [
            (format!("webhook = \"{secret}\"\nport = \"{secret}\"\n"), 2),
            (format!("port = 1\n\n{secret} = 1\n"), 3),
            (format!("webhook = [\"{secret}\"]\n"), 1),
        ];
        for (text, line) in refused {
            let err = parse_section::<Settings>("bark", &text).unwrap_err();
            assert_eq!(err.line(), Some(line), "{text}");
            for shown in [err.to_string(), format!("{err:?}")] {
                assert!(!shown.contains("super-secret-token"), "{shown}");
            }
        }
    }

    #[test]
    fn the_message_names_the_line_and_the_section() {
        let err = parse_section::<Settings>("metrics", "port = \"x\"\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "line 1 under [metrics] in dogs.toml does not fit this dog's settings"
        );
        assert_eq!(err.exit_code(), shep_core::exit::INVALID_CONFIG);
    }
}
