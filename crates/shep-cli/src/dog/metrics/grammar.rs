//! The grammar and the caps a channel sample has to satisfy before the
//! metrics dog keeps it.
//!
//! `shep_channel` checks nothing here on purpose: its own doc says the
//! metrics dog enforces the grammar. This is that enforcement, and it runs
//! at ingestion, so a refusal is counted once rather than once per scrape.

use std::fmt;

/// Longest metric name the dog accepts, in bytes. Prometheus sets no limit of
/// its own; this one stops one app's runaway name from growing every scrape.
pub(super) const MAX_NAME_BYTES: usize = 128;

/// Longest label name the dog accepts, in bytes.
pub(super) const MAX_LABEL_NAME_BYTES: usize = 64;

/// Longest label value the dog accepts, in bytes. A label is a lookup key in
/// a scraper's dashboard, so one carrying a payload is a mistake.
pub(super) const MAX_LABEL_VALUE_BYTES: usize = 256;

/// Longest help text the dog accepts, in bytes.
pub(super) const MAX_HELP_BYTES: usize = 512;

/// Most labels on one sample.
pub(super) const MAX_LABELS: usize = 16;

/// Most series one sender may hold. A new series is refused rather than an
/// old one evicted, so a flapping label cannot push out a stable series.
pub(super) const MAX_SERIES_PER_SENDER: usize = 1024;

/// Metric names this dog serves itself. A sender may not write one.
pub(super) const RESERVED_NAME_PREFIX: &str = "shep_";

/// The label names this dog writes for a sender. A sample may not carry one,
/// which is what keeps `sheep="web"` meaning one thing in every series.
pub(super) const SENDER_LABELS: [&str; 5] = ["sheep", "id", "fold", "dog", "source"];

/// The counter that says how many samples this dog refused, by reason.
pub(super) const REFUSED_TOTAL: &str = "shep_channel_samples_refused_total";

/// Why the dog refused a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RefusalReason {
    /// The name starts with `shep_`, which is this dog's own namespace.
    ReservedName,
    /// The name is not `[a-zA-Z_:][a-zA-Z0-9_:]*`, or is over
    /// [`MAX_NAME_BYTES`].
    InvalidName,
    /// A label name is not `[a-zA-Z_][a-zA-Z0-9_]*`, starts with `__`, or is
    /// over [`MAX_LABEL_NAME_BYTES`].
    InvalidLabel,
    /// A label name is one of [`SENDER_LABELS`].
    ReservedLabel,
    /// More than [`MAX_LABELS`] labels, a label value over
    /// [`MAX_LABEL_VALUE_BYTES`], or help text over [`MAX_HELP_BYTES`].
    TooLong,
    /// The sender already holds the series cap and this is a new series.
    SeriesCap,
    /// Another sample already holds this name as the other kind.
    TypeConflict,
}

impl RefusalReason {
    /// The spelling in `reason="..."` and in the dog's own log line, so an
    /// app author can grep for the one they hit.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ReservedName => "reserved_name",
            Self::InvalidName => "invalid_name",
            Self::InvalidLabel => "invalid_label",
            Self::ReservedLabel => "reserved_label",
            Self::TooLong => "too_long",
            Self::SeriesCap => "series_cap",
            Self::TypeConflict => "type_conflict",
        }
    }

    /// What to tell the author whose sample vanished, since a metric that
    /// simply never appears is otherwise silent.
    fn explain(self) -> &'static str {
        match self {
            Self::ReservedName => "the `shep_` prefix belongs to the shepherd",
            Self::InvalidName => {
                "the name must match `[a-zA-Z_:][a-zA-Z0-9_:]*` and stay under 128 bytes"
            }
            Self::InvalidLabel => {
                "a label name must match `[a-zA-Z_][a-zA-Z0-9_]*`, stay under 64 bytes and not start with `__`"
            }
            Self::ReservedLabel => "the label name is one this dog writes for the sender",
            Self::TooLong => {
                "the sample is over 16 labels, or a label value or help text is over its cap"
            }
            Self::SeriesCap => {
                "this sender already holds 1024 series, and a stable series is never evicted for a new one"
            }
            Self::TypeConflict => "this name is already held as the other kind",
        }
    }
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One sample the dog refused.
///
/// The feed prints one line per sender and reason through this type's
/// [`fmt::Display`], which is what makes a refused metric visible to the
/// author who wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    /// The sender the sample came from, as its `ProcessInfo` id.
    pub(crate) sender: u32,
    /// The name it carried.
    name: String,
    pub(crate) reason: RefusalReason,
    /// Whether this is the first refusal of this reason from this sender.
    pub(crate) first: bool,
}

impl Refusal {
    pub(crate) fn new(sender: u32, name: &str, reason: RefusalReason, first: bool) -> Self {
        Self {
            sender,
            name: name.to_string(),
            reason,
            first,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refused a sample from sender {} named {:?} ({}): {}",
            self.sender,
            // Quoted: a name is app-supplied text and could carry a newline,
            // which in this dog's log reads as a second line.
            self.name,
            self.reason,
            self.reason.explain(),
        )
    }
}

/// The first rule this sample breaks, if it breaks one.
///
/// `labels` is sorted, which is the order the wire's `BTreeMap` gives it.
pub(crate) fn refusal_for(
    name: &str,
    labels: &[(String, String)],
    help: Option<&str>,
) -> Option<RefusalReason> {
    if name.starts_with(RESERVED_NAME_PREFIX) {
        return Some(RefusalReason::ReservedName);
    }
    if name.len() > MAX_NAME_BYTES || !valid_metric_name(name) {
        return Some(RefusalReason::InvalidName);
    }
    for (key, _) in labels {
        if key.starts_with("__") || key.len() > MAX_LABEL_NAME_BYTES || !valid_label_name(key) {
            return Some(RefusalReason::InvalidLabel);
        }
        if SENDER_LABELS.contains(&key.as_str()) {
            return Some(RefusalReason::ReservedLabel);
        }
    }
    if labels.len() > MAX_LABELS
        || labels
            .iter()
            .any(|(_, value)| value.len() > MAX_LABEL_VALUE_BYTES)
        || help.is_some_and(|text| text.len() > MAX_HELP_BYTES)
    {
        return Some(RefusalReason::TooLong);
    }
    None
}

/// Whether `name` matches `[a-zA-Z_:][a-zA-Z0-9_:]*`. Non-ASCII never matches:
/// the Prometheus text format's grammar is ASCII.
fn valid_metric_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == ':')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':')
}

/// Whether a label name matches `[a-zA-Z_][a-zA-Z0-9_]*`.
fn valid_label_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    /// Every spelling the published grammar allows, at every cap exactly.
    /// Fails if a check widens or narrows past what `docs/shepherd-channel.md`
    /// publishes.
    #[test]
    fn the_published_grammar_is_accepted_at_its_edges() {
        let at_the_name_cap = "n".repeat(MAX_NAME_BYTES);
        let at_the_label_cap = "k".repeat(MAX_LABEL_NAME_BYTES);
        let at_the_value_cap = "v".repeat(MAX_LABEL_VALUE_BYTES);
        let at_the_help_cap = "h".repeat(MAX_HELP_BYTES);
        let at_the_label_count = (0..MAX_LABELS)
            .map(|i| (format!("k{i}"), at_the_value_cap.clone()))
            .collect::<Vec<_>>();

        for name in [
            "rps",
            "_rps",
            "r:ps",
            "r_ps2",
            "a:b_c9",
            at_the_name_cap.as_str(),
        ] {
            assert_eq!(
                refusal_for(name, &[], None),
                None,
                "{name} must be accepted"
            );
        }
        assert_eq!(
            refusal_for("queue_depth", &at_the_label_count, Some(&at_the_help_cap)),
            None,
            "16 labels, a 256-byte value and 512 bytes of help is the widest legal sample"
        );
        assert_eq!(
            refusal_for(
                "queue_depth",
                &labels(&[("_key", "v"), (&at_the_label_cap, "v")]),
                None
            ),
            None,
            "a leading single underscore is the grammar's own spelling"
        );
    }

    #[test]
    fn a_name_the_dog_already_uses_is_refused() {
        assert_eq!(
            refusal_for("shep_sheep_uptime_seconds", &[], None),
            Some(RefusalReason::ReservedName)
        );
        // The prefix is the rule, so a name nobody serves is refused too.
        assert_eq!(
            refusal_for("shep_mine", &[], None),
            Some(RefusalReason::ReservedName)
        );
    }

    #[test]
    fn a_name_outside_the_grammar_or_the_cap_is_refused() {
        let too_long = "n".repeat(MAX_NAME_BYTES + 1);
        for name in ["9lives", "r ps", "r-ps", "", "\u{e9}lan", too_long.as_str()] {
            assert_eq!(
                refusal_for(name, &[], None),
                Some(RefusalReason::InvalidName),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_label_name_outside_the_grammar_or_the_cap_is_refused() {
        let too_long = "k".repeat(MAX_LABEL_NAME_BYTES + 1);
        for key in [
            "9key",
            "a key",
            "a-key",
            "",
            "__internal",
            too_long.as_str(),
        ] {
            assert_eq!(
                refusal_for("queue_depth", &labels(&[(key, "v")]), None),
                Some(RefusalReason::InvalidLabel),
                "{key:?}"
            );
        }
    }

    #[test]
    fn a_label_named_like_a_sender_label_is_refused() {
        for key in SENDER_LABELS {
            assert_eq!(
                refusal_for("queue_depth", &labels(&[(key, "web")]), None),
                Some(RefusalReason::ReservedLabel),
                "{key:?}"
            );
        }
    }

    #[test]
    fn a_sample_over_a_cap_is_refused() {
        let too_many = (0..=MAX_LABELS)
            .map(|i| (format!("k{i}"), "v".to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            refusal_for("queue_depth", &too_many, None),
            Some(RefusalReason::TooLong)
        );
        let long_value = "v".repeat(MAX_LABEL_VALUE_BYTES + 1);
        assert_eq!(
            refusal_for("queue_depth", &labels(&[("model", &long_value)]), None),
            Some(RefusalReason::TooLong)
        );
        let long_help = "h".repeat(MAX_HELP_BYTES + 1);
        assert_eq!(
            refusal_for("queue_depth", &[], Some(&long_help)),
            Some(RefusalReason::TooLong)
        );
    }

    /// The reason is the app author's only clue, so it has to name the rule
    /// and the cap it broke.
    #[test]
    fn a_refusal_names_the_rule_it_broke() {
        let refusal = Refusal::new(7, "r ps", RefusalReason::InvalidName, true);
        let line = refusal.to_string();
        assert_eq!(
            line,
            "refused a sample from sender 7 named \"r ps\" (invalid_name): the name must match `[a-zA-Z_:][a-zA-Z0-9_:]*` and stay under 128 bytes"
        );
        // A name is app-supplied text: quoted, so a newline in one cannot
        // forge a second log line.
        assert_eq!(
            Refusal::new(1, "a\nb", RefusalReason::TooLong, true).to_string(),
            "refused a sample from sender 1 named \"a\\nb\" (too_long): the sample is over 16 labels, or a label value or help text is over its cap"
        );
    }
}
