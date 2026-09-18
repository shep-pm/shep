//! What each parsed key is, and whether shep can hold it.
//!
//! `--only` filters and `--secret` classifies what survives. Both take the
//! same argument grammar the sheep-name selector takes: an anchored glob,
//! which for a pattern with no metacharacter in it is an exact match.
//!
//! A pattern matching nothing refuses the whole import. For `--secret` that
//! is a leak rule rather than a tidiness one: a glob that misses the keys it
//! was aimed at would otherwise write credentials into the override store in
//! the clear and exit 0.

use core::fmt;

use globset::Glob;
use shep_core::secrets::{self, MAX_VALUE_BYTES};

use super::parse::Entry;

/// Substrings that make a name look like a credential.
///
/// Closed and short on purpose, and it decides nothing: a match that no
/// `--secret` pattern claimed is named on stderr and imported anyway. Do
/// not grow it by guessing, which is the rule the pm2 importer's own key
/// lists carry for the same reason.
const SECRETISH: &[&str] = &[
    "PASSWORD",
    "SECRET",
    "TOKEN",
    "KEY",
    "DSN",
    "CREDENTIAL",
    "PRIVATE",
];

/// Which store a key is bound for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// The value goes to `secrets.json`; the sheep's env gets a reference.
    Secret,
    /// The value goes to the sheep's env as it stands.
    Plain,
}

/// One key, its value, and where it is going.
///
/// `Debug` prints the value's length and never the value (IR-41).
pub(crate) struct Planned {
    /// The key.
    pub key: String,
    /// The value.
    pub value: String,
    /// Which store.
    pub class: Class,
}

impl fmt::Debug for Planned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Planned")
            .field("key", &self.key)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .field("class", &self.class)
            .finish()
    }
}

/// Everything the import is about to write, and what it wants to warn about.
///
/// `Debug` counts the entries rather than listing them: each one holds a
/// value (IR-41). `unnamed` is keys only and prints in full.
pub(crate) struct ImportPlan {
    /// Every key that survived `--only`, in the file's own order.
    pub entries: Vec<Planned>,
    /// Keys whose names look like credentials that no `--secret` claimed.
    pub unnamed: Vec<String>,
}

impl fmt::Debug for ImportPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportPlan")
            .field("entries", &format_args!("<{} entries>", self.entries.len()))
            .field("unnamed", &self.unnamed)
            .finish()
    }
}

/// Why a plan could not be built.
///
/// `Debug` is derived: no variant carries a value.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlanError {
    /// A pattern globset would not compile.
    BadPattern {
        /// The pattern as typed.
        pattern: String,
        /// globset's own message.
        message: String,
    },
    /// A pattern that matched none of the file's keys.
    MatchedNothing {
        /// The pattern as typed.
        pattern: String,
    },
    /// The file held no keys, or `--only` left none.
    NothingToImport,
    /// A key classified secret that the secret store's grammar refuses.
    KeyNotStorable {
        /// The key.
        key: String,
    },
    /// A value classified secret that is over the store's cap.
    ValueTooLong {
        /// The key.
        key: String,
        /// Its length in bytes.
        len: usize,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadPattern { pattern, message } => {
                write!(f, "`{pattern}` is not a valid pattern: {message}")
            }
            Self::MatchedNothing { pattern } => write!(
                f,
                "`{pattern}` matched no key in the file; nothing was imported"
            ),
            Self::NothingToImport => f.write_str("the file holds no keys to import"),
            Self::KeyNotStorable { key } => write!(
                f,
                "`{key}` cannot be a secret: a key is letters, digits, `.`, `_` and `-`, \
                 at most {} bytes, and does not start with a dot",
                secrets::MAX_KEY_BYTES
            ),
            Self::ValueTooLong { key, len } => write!(
                f,
                "`{key}` is {len} bytes, over the {MAX_VALUE_BYTES}-byte limit for a secret"
            ),
        }
    }
}

impl core::error::Error for PlanError {}

/// Filters, classifies, and checks the store's limits.
///
/// # Errors
/// [`PlanError`], for a pattern that will not compile or matches nothing, a
/// file with nothing left to import, or a secret shep cannot hold. No error
/// carries a value.
pub(crate) fn build(
    entries: Vec<Entry>,
    only: &[String],
    secret: &[String],
) -> Result<ImportPlan, PlanError> {
    let kept: Vec<Entry> = if only.is_empty() {
        entries
    } else {
        let matched = select(&entries, only)?;
        entries
            .into_iter()
            .filter(|entry| matched.contains(&entry.key))
            .collect()
    };
    if kept.is_empty() {
        return Err(PlanError::NothingToImport);
    }

    let secrets_named = select(&kept, secret)?;

    let mut planned = Vec::with_capacity(kept.len());
    let mut unnamed = Vec::new();
    for entry in kept {
        let class = if secrets_named.contains(&entry.key) {
            if !secrets::is_name(&entry.key) {
                return Err(PlanError::KeyNotStorable { key: entry.key });
            }
            if entry.value.len() > MAX_VALUE_BYTES {
                return Err(PlanError::ValueTooLong {
                    key: entry.key,
                    len: entry.value.len(),
                });
            }
            Class::Secret
        } else {
            if looks_secret(&entry.key) {
                unnamed.push(entry.key.clone());
            }
            Class::Plain
        };
        planned.push(Planned {
            key: entry.key,
            value: entry.value,
            class,
        });
    }
    Ok(ImportPlan {
        entries: planned,
        unnamed,
    })
}

/// The keys `patterns` match, refusing any pattern that matches none.
fn select(entries: &[Entry], patterns: &[String]) -> Result<Vec<String>, PlanError> {
    let mut matched = Vec::new();
    for pattern in patterns {
        let glob = Glob::new(pattern)
            .map_err(|err| PlanError::BadPattern {
                pattern: pattern.clone(),
                message: err.to_string(),
            })?
            .compile_matcher();
        let hits: Vec<String> = entries
            .iter()
            .filter(|entry| glob.is_match(&entry.key))
            .map(|entry| entry.key.clone())
            .collect();
        if hits.is_empty() {
            return Err(PlanError::MatchedNothing {
                pattern: pattern.clone(),
            });
        }
        matched.extend(hits);
    }
    Ok(matched)
}

/// Whether a name reads like a credential.
fn looks_secret(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRETISH.iter().any(|needle| upper.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::import::dotenv::parse;

    fn entries(text: &str) -> Vec<parse::Entry> {
        parse::parse(text).expect("this fixture parses")
    }

    const SAMPLE: &str =
        "NODE_ENV=production\nPORT=8080\nDB_PASSWORD=hunter2\nSTRIPE_TOKEN=sk_live\n";

    fn classes(plan: &ImportPlan) -> Vec<(&str, bool)> {
        plan.entries
            .iter()
            .map(|planned| (planned.key.as_str(), matches!(planned.class, Class::Secret)))
            .collect()
    }

    #[test]
    fn everything_is_plain_by_default() {
        let plan = build(entries(SAMPLE), &[], &[]).unwrap();
        assert_eq!(plan.entries.len(), 4);
        assert!(plan.entries.iter().all(|e| matches!(e.class, Class::Plain)));
    }

    #[test]
    fn an_exact_key_and_a_glob_both_classify() {
        let plan = build(
            entries(SAMPLE),
            &[],
            &["DB_PASSWORD".to_string(), "*_TOKEN".to_string()],
        )
        .unwrap();
        assert_eq!(
            classes(&plan),
            [
                ("NODE_ENV", false),
                ("PORT", false),
                ("DB_PASSWORD", true),
                ("STRIPE_TOKEN", true),
            ]
        );
    }

    #[test]
    fn only_filters_before_secret_classifies() {
        let plan = build(
            entries(SAMPLE),
            &["DB_*".to_string()],
            &["DB_PASSWORD".to_string()],
        )
        .unwrap();
        assert_eq!(classes(&plan), [("DB_PASSWORD", true)]);
    }

    #[test]
    fn a_glob_is_anchored() {
        let plan = build(
            entries("MY_DB_URL=x\nDB_URL=y\n"),
            &["DB_*".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(classes(&plan), [("DB_URL", false)]);
    }

    #[test]
    fn a_pattern_matching_nothing_refuses() {
        let err = build(entries(SAMPLE), &[], &["DB_*NOPE".to_string()]).unwrap_err();
        assert!(matches!(err, PlanError::MatchedNothing { .. }));
        let err = build(entries(SAMPLE), &["absent".to_string()], &[]).unwrap_err();
        assert!(matches!(err, PlanError::MatchedNothing { .. }));
    }

    #[test]
    fn a_secretish_name_nobody_named_is_reported() {
        let plan = build(entries(SAMPLE), &[], &["DB_PASSWORD".to_string()]).unwrap();
        assert_eq!(plan.unnamed, ["STRIPE_TOKEN"]);
    }

    #[test]
    fn a_secret_key_shep_cannot_store_refuses() {
        let err = build(entries("A$B=1\n"), &[], &["*".to_string()]).unwrap_err();
        assert!(matches!(err, PlanError::KeyNotStorable { .. }));
    }

    #[test]
    fn a_secret_value_over_the_cap_refuses() {
        let long = "x".repeat(shep_core::secrets::MAX_VALUE_BYTES + 1);
        let err = build(entries(&format!("BIG={long}\n")), &[], &["BIG".to_string()]).unwrap_err();
        assert!(matches!(err, PlanError::ValueTooLong { .. }));
    }

    #[test]
    fn an_empty_file_refuses() {
        let err = build(entries("# nothing here\n"), &[], &[]).unwrap_err();
        assert!(matches!(err, PlanError::NothingToImport));
    }

    /// IR-41.
    #[test]
    fn planned_and_plan_debug_do_not_leak() {
        let plan = build(
            entries("DB_PASSWORD=hunter2\n"),
            &[],
            &["DB_PASSWORD".to_string()],
        )
        .unwrap();
        assert_eq!(
            format!("{:?}", plan.entries[0]),
            "Planned { key: \"DB_PASSWORD\", value: <7 bytes>, class: Secret }"
        );
        assert_eq!(
            format!("{plan:?}"),
            "ImportPlan { entries: <1 entries>, unnamed: [] }"
        );
    }

    /// No `PlanError` carries a value, so none of them can print one.
    #[test]
    fn no_plan_error_prints_a_value() {
        let long = "s3cret".repeat(1000);
        let err = build(entries(&format!("BIG={long}\n")), &[], &["BIG".to_string()]).unwrap_err();
        assert!(!err.to_string().contains("s3cret"), "{err}");
    }
}
