//! What one app's log lines look like, when shep's own reading of them is
//! wrong.
//!
//! A client classifies a line so an operator can filter by level. Its
//! built-in reading looks for a level word near the start of the line, which
//! an app writing `{"severity":"ERROR"}` or `11:02:03.144 [main] SEVERE`
//! never satisfies. [`LevelRule`] lets the app say what its own lines look
//! like instead: an ordered list of patterns, first match wins.

use core::fmt;

use serde::{Deserialize, Serialize};

/// Largest compiled program a [`LevelRule`] pattern may produce.
///
/// 1 MiB, the bound `ProcessSelector` already puts on a peer-supplied
/// regex. A pattern reaches this crate from a Flockfile and from
/// `Request::Start`, so the memory one costs is not this process's to
/// choose, and regex's own default is unbounded in practice.
const PATTERN_SIZE_LIMIT: usize = 1 << 20;

/// The level one log line announces.
///
/// Ordered lowest first, so `level >= minimum` reads the way an operator
/// setting a floor expects. `Display` renders the spelling a Flockfile
/// writes.
//
// Not `config::LogLevel`, which is the shepherd's own verbosity. This is a
// level a sheep claims for one of its own lines, and shep only reads it.
// wire format: changing these strings is a breaking change.
//
// No `#[non_exhaustive]` (IR-20). These five are the universal set every
// logging library agrees on, so growth is not anticipated; a sixth would
// also change what every operator's stored filter means, which is a break
// whatever this attribute says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LineLevel {
    /// The lowest, and the only one an app has to opt into emitting.
    Trace,
    /// Below `Info`, and the usual floor for an app's own noise.
    Debug,
    /// The default an operator reads when nothing is filtered.
    Info,
    /// Something an operator should look at, without the sheep being broken.
    Warn,
    /// The highest. A line saying it is fatal is not saying something milder
    /// than one saying error.
    Error,
}

impl fmt::Display for LineLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        })
    }
}

/// One rule: the lines this pattern matches carry this level.
///
/// The pattern is a regex, matched against the whole line, exactly as
/// written: `(?i)` makes one case-insensitive, and nothing is anchored for
/// you.
//
// `Debug` is derived (IR-41). A pattern and a level describe an app's
// output rather than carrying any of it.
// wire format: changing field names is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct LevelRule {
    /// The regex a line must match for this rule to claim it.
    pub pattern: String,
    /// The level a matching line is given.
    pub level: LineLevel,
}

/// A compiled [`LevelRule`] list, ready to classify lines.
///
/// Build one with [`Self::compile`]. Rules are tried in the order they were
/// declared and the first match wins, so an operator orders them rather than
/// discovering which of two overlapping patterns a map happened to hold
/// first.
#[derive(Debug)]
pub struct LevelMatcher {
    rules: Vec<(regex::Regex, LineLevel)>,
}

impl LevelMatcher {
    /// Compiles `rules` in order.
    ///
    /// An empty slice compiles to an empty matcher, which classifies
    /// nothing. What a caller does with that is its own decision: a client
    /// reading an app that declared no rules falls back to its own reading
    /// of the line.
    ///
    /// # Errors
    /// - [`LevelRuleError::EmptyPattern`] if a rule's pattern is empty.
    /// - [`LevelRuleError::BadPattern`] if a pattern does not compile, or
    ///   compiles to a program over [`PATTERN_SIZE_LIMIT`].
    pub fn compile(rules: &[LevelRule]) -> Result<Self, LevelRuleError> {
        let mut compiled = Vec::with_capacity(rules.len());
        for rule in rules {
            if rule.pattern.is_empty() {
                return Err(LevelRuleError::EmptyPattern { level: rule.level });
            }
            let regex = regex::RegexBuilder::new(&rule.pattern)
                .size_limit(PATTERN_SIZE_LIMIT)
                .build()
                .map_err(|err| LevelRuleError::BadPattern {
                    pattern: rule.pattern.clone(),
                    reason: err.to_string(),
                })?;
            compiled.push((regex, rule.level));
        }
        Ok(Self { rules: compiled })
    }

    /// Whether no rule was declared, so this matcher can never classify
    /// anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The level the first matching rule gives `line`, or `None` when none
    /// matches.
    ///
    /// `None` is an ordinary answer and never means "below the minimum": an
    /// app's output is arbitrary text and most of it announces no level at
    /// all.
    #[must_use]
    pub fn level_of(&self, line: &str) -> Option<LineLevel> {
        self.rules
            .iter()
            .find(|(regex, _)| regex.is_match(line))
            .map(|(_, level)| *level)
    }
}

/// Why a [`LevelRule`] list was refused.
///
/// `#[non_exhaustive]`: shep-core is published, and a rule grammar that
/// grows a second kind of pattern would add a variant here with no version
/// to warn an out-of-tree match about it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LevelRuleError {
    /// A rule's pattern is empty. An empty regex matches every line, so the
    /// rule would claim the whole feed and leave every rule after it dead.
    EmptyPattern {
        /// The level that rule would have given, so the refusal names which
        /// entry to edit.
        level: LineLevel,
    },
    /// A pattern does not compile, or compiles to a program over the size
    /// bound this crate puts on a pattern it did not write.
    BadPattern {
        /// The pattern as the user wrote it.
        pattern: String,
        /// regex's own rendered reason.
        reason: String,
    },
}

impl fmt::Display for LevelRuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPattern { level } => write!(
                f,
                "the `{level}` level rule has an empty pattern, which would match every line"
            ),
            Self::BadPattern { pattern, reason } => {
                write!(f, "invalid level rule pattern `{pattern}`: {reason}")
            }
        }
    }
}

impl core::error::Error for LevelRuleError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_order_from_trace_up_to_error() {
        assert!(LineLevel::Trace < LineLevel::Debug);
        assert!(LineLevel::Debug < LineLevel::Info);
        assert!(LineLevel::Info < LineLevel::Warn);
        assert!(LineLevel::Warn < LineLevel::Error);
    }

    /// The wire spelling, which a Flockfile writes and an older peer reads.
    #[test]
    fn a_level_serializes_as_its_lowercase_name() {
        let rule = LevelRule {
            pattern: "x".to_string(),
            level: LineLevel::Warn,
        };
        assert_eq!(
            serde_json::to_string(&rule).unwrap(),
            r#"{"pattern":"x","level":"warn"}"#
        );
    }

    #[test]
    fn a_rule_claims_the_lines_its_pattern_matches() {
        let matcher = LevelMatcher::compile(&[LevelRule {
            pattern: r#""severity":"ERROR""#.to_string(),
            level: LineLevel::Error,
        }])
        .unwrap();
        assert_eq!(
            matcher.level_of(r#"{"severity":"ERROR","msg":"boom"}"#),
            Some(LineLevel::Error)
        );
        assert_eq!(matcher.level_of(r#"{"severity":"INFO"}"#), None);
    }

    /// The whole reason the list is ordered rather than a map: two patterns
    /// can match one line, and the operator decides which wins.
    #[test]
    fn the_first_matching_rule_wins() {
        let rules = |first, second| {
            vec![
                LevelRule {
                    pattern: "boom".to_string(),
                    level: first,
                },
                LevelRule {
                    pattern: "b".to_string(),
                    level: second,
                },
            ]
        };
        let matcher = LevelMatcher::compile(&rules(LineLevel::Error, LineLevel::Debug)).unwrap();
        assert_eq!(matcher.level_of("boom"), Some(LineLevel::Error));

        let reversed = LevelMatcher::compile(&rules(LineLevel::Debug, LineLevel::Error)).unwrap();
        assert_eq!(reversed.level_of("boom"), Some(LineLevel::Debug));
    }

    /// A line no rule matches is unclassified, not the lowest level: a
    /// filter that treated it as a miss would hide a bare `println!`.
    #[test]
    fn a_line_no_rule_matches_has_no_level() {
        let matcher = LevelMatcher::compile(&[LevelRule {
            pattern: "^ERROR".to_string(),
            level: LineLevel::Error,
        }])
        .unwrap();
        assert_eq!(matcher.level_of("listening on 8080"), None);
    }

    #[test]
    fn no_rules_compiles_to_a_matcher_that_classifies_nothing() {
        let matcher = LevelMatcher::compile(&[]).unwrap();
        assert!(matcher.is_empty());
        assert_eq!(matcher.level_of("ERROR everything is on fire"), None);
    }

    /// Case is the pattern's own business. Refusing to fold it is what lets
    /// an app tell `ERROR` apart from a message mentioning an error.
    #[test]
    fn a_pattern_matches_case_exactly_unless_it_asks_not_to() {
        let exact = LevelMatcher::compile(&[LevelRule {
            pattern: "^ERROR".to_string(),
            level: LineLevel::Error,
        }])
        .unwrap();
        assert_eq!(exact.level_of("error: nope"), None);

        let folded = LevelMatcher::compile(&[LevelRule {
            pattern: "(?i)^error".to_string(),
            level: LineLevel::Error,
        }])
        .unwrap();
        assert_eq!(folded.level_of("error: nope"), Some(LineLevel::Error));
    }

    #[test]
    fn an_empty_pattern_is_refused() {
        let err = LevelMatcher::compile(&[LevelRule {
            pattern: String::new(),
            level: LineLevel::Warn,
        }])
        .unwrap_err();
        assert_eq!(
            err,
            LevelRuleError::EmptyPattern {
                level: LineLevel::Warn
            }
        );
        assert!(err.to_string().contains("warn"), "{err}");
    }

    #[test]
    fn a_pattern_regex_refuses_is_refused() {
        let err = LevelMatcher::compile(&[LevelRule {
            pattern: "[unclosed".to_string(),
            level: LineLevel::Error,
        }])
        .unwrap_err();
        assert!(
            matches!(&err, LevelRuleError::BadPattern { pattern, .. } if pattern == "[unclosed"),
            "{err:?}"
        );
    }

    /// The bound exists because a pattern arrives from a peer. Without it
    /// one Flockfile line can cost the process hundreds of megabytes.
    #[test]
    fn a_pattern_over_the_size_bound_is_refused() {
        let err = LevelMatcher::compile(&[LevelRule {
            pattern: format!("(a{}){{10000}}", "|b".repeat(100_000)),
            level: LineLevel::Error,
        }])
        .unwrap_err();
        assert!(matches!(err, LevelRuleError::BadPattern { .. }), "{err:?}");
    }
}
