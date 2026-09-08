//! The `.env` grammar, and nothing else.
//!
//! Strict on purpose. Every shape this file does not recognise refuses with
//! a line number rather than being guessed at, because the values are
//! credentials and a wrong guess is silent. No variable interpolation: `$`
//! is a literal character everywhere, which is the one place this parser
//! deliberately differs from most `.env` readers. dotenvy resolves `${NAME}`
//! against the reading process's own environment and turns an undefined name
//! into an empty string, which on this path would store a truncated secret.
//!
//! The trailing-comment rule is the subtle one and it is in
//! [`unquoted_value`].

use core::fmt;
use std::collections::BTreeMap;

/// One `KEY=value` pair.
///
/// `Debug` prints the value's length and never the value (IR-41).
/// Exact-string-tested below (`entry_debug_does_not_leak`).
pub(crate) struct Entry {
    /// The key, exactly as written.
    pub key: String,
    /// The value, unquoted and unescaped.
    pub value: String,
    /// The 1-based line the key was on.
    pub line: usize,
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("key", &self.key)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .field("line", &self.line)
            .finish()
    }
}

/// Why one line refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParseReason {
    /// The line has no `=`.
    NoEquals,
    /// The name before the `=` is empty.
    EmptyKey,
    /// A quote opened and the file ended, or a single quote opened and its
    /// line ended.
    UnterminatedQuote,
    /// A backslash inside double quotes followed by something other than
    /// `n`, `r`, `t`, `"` or `\`.
    BadEscape(char),
    /// A ` #` in an unquoted value whose value already contains whitespace,
    /// so the line reads two ways.
    AmbiguousComment,
    /// Something other than whitespace or a comment after a closing quote.
    TrailingText,
    /// A key that an earlier line already set.
    Duplicate {
        /// The line that set it first.
        first: usize,
    },
}

/// One line refused, and why.
///
/// `Debug` is derived: no variant carries a value.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParseError {
    /// The 1-based line.
    pub line: usize,
    /// What was wrong with it.
    pub reason: ParseReason,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: ", self.line)?;
        match &self.reason {
            ParseReason::NoEquals => f.write_str("no `=`; every line is `KEY=value`"),
            ParseReason::EmptyKey => f.write_str("the name before the `=` is empty"),
            ParseReason::UnterminatedQuote => f.write_str("a quote opened and never closed"),
            ParseReason::BadEscape(c) => write!(
                f,
                "`\\{c}` is not an escape; inside double quotes only \\n \\r \\t \\\" \\\\ are"
            ),
            ParseReason::AmbiguousComment => f.write_str(
                "a ` #` after a value that already has spaces in it reads two ways; \
                 quote the value if the `#` is part of it",
            ),
            ParseReason::TrailingText => f.write_str("unexpected text after the closing quote"),
            ParseReason::Duplicate { first } => {
                write!(f, "this key was already set on line {first}")
            }
        }
    }
}

impl core::error::Error for ParseError {}

/// Reads a `.env` into its pairs.
///
/// # Errors
/// [`ParseError`] for the first line this grammar does not accept. No error
/// carries a value.
pub(crate) fn parse(text: &str) -> Result<Vec<Entry>, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();

    let mut entries = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line_number = index + 1;
        let line = lines[index].trim_start();
        if line.is_empty() || line.starts_with('#') {
            index += 1;
            continue;
        }
        let body = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((raw_key, rest)) = body.split_once('=') else {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::NoEquals,
            });
        };
        let key = raw_key.trim_end().to_string();
        if key.is_empty() {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::EmptyKey,
            });
        }
        if let Some(first) = seen.get(&key) {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::Duplicate { first: *first },
            });
        }
        let (value, extra_lines) = read_value(rest, &lines[index + 1..], line_number)?;
        seen.insert(key.clone(), line_number);
        entries.push(Entry {
            key,
            value,
            line: line_number,
        });
        index += 1 + extra_lines;
    }
    Ok(entries)
}

/// Reads one value, returning it and how many further lines it consumed.
///
/// `rest` is everything after the `=`, untrimmed, so that `KEY= # note`
/// still sees the space that makes the `#` a comment.
fn read_value(
    rest: &str,
    following: &[&str],
    line_number: usize,
) -> Result<(String, usize), ParseError> {
    let trimmed = rest.trim_start();
    match trimmed.as_bytes().first() {
        Some(b'\'') => single_quoted(&trimmed[1..], line_number).map(|value| (value, 0)),
        Some(b'"') => double_quoted(&trimmed[1..], following, line_number),
        _ => unquoted_value(rest, line_number).map(|value| (value, 0)),
    }
}

/// An unquoted value: the rest of the line, trimmed, with the trailing
/// comment rule applied.
///
/// A `#` is a comment only when whitespace comes before it *and* the value
/// before that whitespace has no whitespace of its own. `PORT=8080 # dev` is
/// a port and a comment; `KEY=v#3` is a three-character value; and
/// `PASSPHRASE=correct horse battery #4` refuses, because taking the comment
/// truncates a credential and not taking it stores a note.
fn unquoted_value(rest: &str, line_number: usize) -> Result<String, ParseError> {
    let comment_at = rest.char_indices().find(|&(index, c)| {
        c == '#'
            && rest[..index]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
    });
    let Some((index, _)) = comment_at else {
        return Ok(rest.trim().to_string());
    };
    let value = rest[..index].trim();
    if value.chars().any(char::is_whitespace) {
        return Err(ParseError {
            line: line_number,
            reason: ParseReason::AmbiguousComment,
        });
    }
    Ok(value.to_string())
}

/// A single-quoted value: literal, no escapes, closes on its own line.
fn single_quoted(after_quote: &str, line_number: usize) -> Result<String, ParseError> {
    let Some(end) = after_quote.find('\'') else {
        return Err(ParseError {
            line: line_number,
            reason: ParseReason::UnterminatedQuote,
        });
    };
    check_tail(&after_quote[end + 1..], line_number)?;
    Ok(after_quote[..end].to_string())
}

/// A double-quoted value: five escapes, and it may span lines.
fn double_quoted(
    after_quote: &str,
    following: &[&str],
    line_number: usize,
) -> Result<(String, usize), ParseError> {
    let mut value = String::new();
    let mut current = after_quote;
    let mut consumed = 0;
    loop {
        let mut chars = current.char_indices();
        while let Some((index, c)) = chars.next() {
            match c {
                '"' => {
                    check_tail(&current[index + 1..], line_number)?;
                    return Ok((value, consumed));
                }
                '\\' => {
                    let Some((_, escaped)) = chars.next() else {
                        return Err(ParseError {
                            line: line_number,
                            reason: ParseReason::UnterminatedQuote,
                        });
                    };
                    value.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        other => {
                            return Err(ParseError {
                                line: line_number,
                                reason: ParseReason::BadEscape(other),
                            });
                        }
                    });
                }
                other => value.push(other),
            }
        }
        let Some(next) = following.get(consumed).copied() else {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::UnterminatedQuote,
            });
        };
        value.push('\n');
        consumed += 1;
        current = next;
    }
}

/// What may follow a closing quote: whitespace, then nothing or a comment.
fn check_tail(tail: &str, line_number: usize) -> Result<(), ParseError> {
    let tail = tail.trim();
    if tail.is_empty() || tail.starts_with('#') {
        return Ok(());
    }
    Err(ParseError {
        line: line_number,
        reason: ParseReason::TrailingText,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_and_values(text: &str) -> Vec<(String, String)> {
        parse(text)
            .expect("this fixture parses")
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect()
    }

    #[test]
    fn plain_pairs_comments_and_blanks() {
        let parsed = keys_and_values("# a note\n\nNODE_ENV=production\nPORT=8080\n");
        assert_eq!(
            parsed,
            [
                ("NODE_ENV".to_string(), "production".to_string()),
                ("PORT".to_string(), "8080".to_string()),
            ]
        );
    }

    #[test]
    fn export_prefix_and_spaces_around_the_equals() {
        let parsed = keys_and_values("export NODE_ENV=production\nPORT = 8080\n");
        assert_eq!(parsed[0].0, "NODE_ENV");
        assert_eq!(parsed[1], ("PORT".to_string(), "8080".to_string()));
    }

    #[test]
    fn a_trailing_comment_is_taken_only_after_a_value_with_no_spaces() {
        assert_eq!(keys_and_values("PORT=8080 # dev\n")[0].1, "8080");
        assert_eq!(keys_and_values("KEY=v#3\n")[0].1, "v#3");
        assert_eq!(keys_and_values("NODE_OPTIONS=--a --b\n")[0].1, "--a --b");
        assert_eq!(keys_and_values("KEY= # note\n")[0].1, "");
    }

    #[test]
    fn an_ambiguous_trailing_comment_refuses() {
        let err = parse("PASSPHRASE=correct horse battery #4\n").unwrap_err();
        assert_eq!(err.line, 1);
        assert!(matches!(err.reason, ParseReason::AmbiguousComment));
        assert!(
            !err.to_string().contains("correct horse"),
            "the message must not carry the value: {err}"
        );
    }

    #[test]
    fn single_quotes_are_literal_and_must_close_on_the_line() {
        assert_eq!(keys_and_values("KEY='a \\n b # c'\n")[0].1, "a \\n b # c");
        let err = parse("KEY='unclosed\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::UnterminatedQuote));
    }

    #[test]
    fn double_quotes_take_escapes_and_may_span_lines() {
        assert_eq!(keys_and_values(r#"KEY="a\nb\"c\\d""#)[0].1, "a\nb\"c\\d");
        assert_eq!(
            keys_and_values("PEM=\"-----BEGIN-----\nline two\n-----END-----\"\n")[0].1,
            "-----BEGIN-----\nline two\n-----END-----"
        );
        let err = parse("KEY=\"a\\qb\"\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::BadEscape('q')));
    }

    #[test]
    fn a_dollar_is_literal() {
        assert_eq!(keys_and_values("TOKEN=${PART}-live\n")[0].1, "${PART}-live");
        assert_eq!(
            keys_and_values("TOKEN=\"${PART}-live\"\n")[0].1,
            "${PART}-live"
        );
    }

    #[test]
    fn a_duplicate_key_refuses_and_names_both_lines() {
        let err = parse("A=1\nB=2\nA=3\n").unwrap_err();
        assert_eq!(err.line, 3);
        assert!(matches!(err.reason, ParseReason::Duplicate { first: 1 }));
    }

    #[test]
    fn a_bom_and_crlf_are_stripped() {
        let parsed = keys_and_values("\u{feff}A=1\r\nB=2\r\n");
        assert_eq!(
            parsed,
            [
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ]
        );
    }

    #[test]
    fn a_line_with_no_equals_refuses() {
        let err = parse("NODE_ENV production\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::NoEquals));
    }

    #[test]
    fn text_after_a_closing_quote_refuses() {
        let err = parse("KEY=\"a\" b\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::TrailingText));
    }

    /// IR-41. A derived `Debug` would print the value in a panic message,
    /// a test failure or a `dbg!`.
    #[test]
    fn entry_debug_does_not_leak() {
        let entry = Entry {
            key: "DB_PASSWORD".to_string(),
            value: "hunter2".to_string(),
            line: 4,
        };
        assert_eq!(
            format!("{entry:?}"),
            "Entry { key: \"DB_PASSWORD\", value: <7 bytes>, line: 4 }"
        );
    }
}
