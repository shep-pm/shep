/// A log level, ordered so a minimum can be compared against.
///
/// `Ord` is derived and the declaration order is the ordering: `Trace` is
/// the lowest and `Error` the highest, so `level >= minimum` reads the way
/// an operator setting `level >= warn` expects.
///
/// No non-test caller yet: `#[allow(dead_code)]` says so rather than
/// inventing one. Task 3 wires this into the bleats filter.
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
/// Scans the first 4 whitespace-separated words rather than only the
/// first, because a timestamp before the level is the common shape. A
/// level past the fourth word is not found; that is deliberate, not a
/// bug, and a line whose level falls outside the window simply falls
/// back to `None`, which is always safe under decision 3.
///
/// A candidate matches only as a whole word, after surrounding
/// punctuation is stripped, so `information` is not `info`. Digits
/// adjacent to the word are NOT stripped, only punctuation is: a digit
/// touching the word means it was never a level token in the first
/// place (`/error404`, `info2`), and trimming through it would invent a
/// level the line never announced, which decision 3 forbids.
///
/// `None` is the ordinary answer for app output. Callers must not treat it
/// as "below the minimum": see the spec's decision 3.
///
/// No non-test caller yet: `#[allow(dead_code)]` says so rather than
/// inventing one. Task 3 wires this into the bleats filter.
#[must_use]
#[allow(dead_code)]
pub fn level_of(line: &str) -> Option<Level> {
    line.split_whitespace().take(4).find_map(|word| {
        match word
            .trim_matches(|c: char| !c.is_ascii_alphabetic() && !c.is_ascii_digit())
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

    /// The full extra-word variants of warn/error, not just the short
    /// forms. Dropping either from the match still passes every other
    /// test in this file, so each needs its own pin.
    ///
    /// Neither line may contain a second level word. `a fatal error
    /// occurred` pins nothing: drop the `fatal` arm and `find_map` reaches
    /// `error` on the next word for the same answer.
    #[test]
    fn the_long_forms_of_warn_and_error_are_recognised() {
        assert_eq!(level_of("a warning was logged"), Some(Level::Warn));
        assert_eq!(level_of("a fatal crash occurred"), Some(Level::Error));
    }

    /// A digit touching the word means it was never a level token: an
    /// HTTP status glued onto a route, or a word that merely ends in a
    /// digit. Stripping through the digit would invent a level the line
    /// never announced (decision 3 forbids exactly this).
    #[test]
    fn a_digit_touching_the_word_blocks_the_match() {
        assert_eq!(level_of("/error404 requested"), None);
        assert_eq!(level_of("info2 and counting"), None);
        assert_eq!(level_of("GET /info2 200"), None);
    }

    /// Punctuation still strips cleanly on both sides; only digits are
    /// excluded from what trims.
    #[test]
    fn surrounding_punctuation_still_trims() {
        assert_eq!(level_of("[WARN] pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("Error: connection refused"), Some(Level::Error));
    }

    /// The scan window is exactly 4 words: a level at the fourth word is
    /// found, one at the fifth is not.
    #[test]
    fn the_scan_window_is_exactly_four_words() {
        assert_eq!(level_of("a b c warn"), Some(Level::Warn));
        assert_eq!(level_of("a b c d warn"), None);
    }

    #[test]
    fn levels_order_from_trace_up_to_error() {
        assert!(Level::Trace < Level::Debug);
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
    }
}
