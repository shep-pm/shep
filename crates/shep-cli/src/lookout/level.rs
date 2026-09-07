/// A log level, ordered so a minimum can be compared against.
///
/// `Ord` is derived and the declaration order is the ordering: `Trace` is
/// the lowest and `Error` the highest, so `level >= minimum` reads the way
/// an operator setting `level >= warn` expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// The level a line announces, or `None` when it announces none.
///
/// Scans the first few whitespace-separated words rather than only the
/// first, because a timestamp before the level is the common shape. A
/// candidate matches only as a whole word, after trailing punctuation is
/// stripped, so `information` is not `info`.
///
/// `None` is the ordinary answer for app output. Callers must not treat it
/// as "below the minimum": see the spec's decision 3.
#[must_use]
#[allow(dead_code)]
pub fn level_of(line: &str) -> Option<Level> {
    line.split_whitespace().take(4).find_map(|word| {
        match word
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .to_ascii_lowercase()
            .as_str()
        {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" | "fatal" => Some(Level::Error),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_level_word_is_found_whatever_its_case() {
        assert_eq!(level_of("WARN pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("warn pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("Error: connection refused"), Some(Level::Error));
    }

    /// A timestamp before the level is the common shape, so the search looks
    /// past a prefix rather than only at the first word.
    #[test]
    fn a_level_after_a_timestamp_is_still_found() {
        assert_eq!(
            level_of("2026-09-08T11:02:03Z INFO listening on 8080"),
            Some(Level::Info)
        );
    }

    /// The whole reason the filter shows unclassifiable lines: most app
    /// output looks like this.
    #[test]
    fn a_line_with_no_level_word_has_no_level() {
        assert_eq!(level_of("listening on 8080"), None);
        assert_eq!(level_of(""), None);
    }

    /// A word that merely contains a level name is not a level. Without
    /// this, "information" and "errors:" both read as levels.
    #[test]
    fn a_level_name_inside_a_longer_word_is_not_a_level() {
        assert_eq!(level_of("information about the pool"), None);
        assert_eq!(level_of("warnings are disabled"), None);
    }

    #[test]
    fn levels_order_from_trace_up_to_error() {
        assert!(Level::Trace < Level::Debug);
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
    }
}
